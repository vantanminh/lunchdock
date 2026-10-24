//! Disk tier for cached objects.
//!
//! Each object is one file. `index.json` records the key, host, path, size, and
//! last access so eviction and purge do not scan every file. A missing index is
//! rebuilt from the files. Objects stay on disk across a process restart.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8] = b"RDISK1";
const OVERHEAD: u64 = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    hash: String,
    id: String,
    host: String,
    path: String,
    size: u64,
    last_access: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct IndexFile {
    objects: Vec<IndexEntry>,
}

struct Inner {
    objects: HashMap<String, IndexEntry>,
    total: u64,
    tick: u64,
}

pub struct DiskStore {
    dir: PathBuf,
    inner: Mutex<Inner>,
    budget: AtomicU64,
    evictions: AtomicU64,
}

pub struct DiskObject {
    pub meta: (Vec<u8>, Vec<u8>),
    pub body: Bytes,
    pub host: String,
    pub path: String,
}

impl DiskStore {
    /// Open `dir`, creating it. A budget of zero stores nothing new and drops
    /// whatever is already there when [`Self::set_budget`] is called with zero.
    pub fn open(dir: impl Into<PathBuf>, budget: u64) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(dir.join("objects"))?;
        let objects = load_index(&dir);
        let total = objects.values().map(|entry| entry.size).sum();
        let tick = objects
            .values()
            .map(|entry| entry.last_access)
            .max()
            .unwrap_or(0);
        let store = Self {
            dir,
            inner: Mutex::new(Inner {
                objects,
                total,
                tick,
            }),
            budget: AtomicU64::new(budget),
            evictions: AtomicU64::new(0),
        };
        store.evict();
        store.persist();
        Ok(store)
    }

    pub fn set_budget(&self, budget: u64) {
        self.budget.store(budget, Ordering::Relaxed);
        self.evict();
        self.persist();
    }

    pub fn budget(&self) -> u64 {
        self.budget.load(Ordering::Relaxed)
    }

    pub fn evictions(&self) -> u64 {
        self.evictions.load(Ordering::Relaxed)
    }

    pub fn totals(&self) -> (u64, u64) {
        let inner = self.lock();
        (inner.objects.len() as u64, inner.total)
    }

    pub fn get(&self, hash: &str) -> Option<DiskObject> {
        let (id, host, path) = {
            let mut inner = self.lock();
            inner.tick += 1;
            let tick = inner.tick;
            let entry = inner.objects.get_mut(hash)?;
            entry.last_access = tick;
            (entry.id.clone(), entry.host.clone(), entry.path.clone())
        };
        match read_object(&self.object_path(&id)) {
            Ok(StoredObject { meta, body, .. }) => Some(DiskObject {
                meta,
                body,
                host,
                path,
            }),
            Err(_) => {
                self.remove(hash);
                None
            }
        }
    }

    pub fn insert(
        &self,
        hash: &str,
        meta: &(Vec<u8>, Vec<u8>),
        body: &[u8],
        host: &str,
        path: &str,
    ) {
        let budget = self.budget();
        if budget == 0 {
            return;
        }
        let size = body.len() as u64
            + (meta.0.len() + meta.1.len()) as u64
            + (host.len() + path.len()) as u64
            + OVERHEAD;
        if size > budget {
            return;
        }
        let id = file_id(hash);
        if write_object(&self.object_path(&id), hash, host, path, meta, body).is_err() {
            return;
        }
        {
            let mut inner = self.lock();
            inner.tick += 1;
            let entry = IndexEntry {
                hash: hash.to_owned(),
                id,
                host: host.to_owned(),
                path: path.to_owned(),
                size,
                last_access: inner.tick,
            };
            inner.total += size;
            if let Some(old) = inner.objects.insert(hash.to_owned(), entry) {
                inner.total = inner.total.saturating_sub(old.size);
                if old.id != file_id(hash) {
                    let _ = fs::remove_file(self.object_path(&old.id));
                }
            }
        }
        self.evict();
        self.persist();
    }

    pub fn update_meta(&self, hash: &str, meta: &(Vec<u8>, Vec<u8>)) -> bool {
        let Some(object) = self.get(hash) else {
            return false;
        };
        self.insert(hash, meta, &object.body, &object.host, &object.path);
        true
    }

    pub fn remove(&self, hash: &str) -> bool {
        let removed = {
            let mut inner = self.lock();
            inner.objects.remove(hash).map(|entry| {
                inner.total = inner.total.saturating_sub(entry.size);
                entry.id
            })
        };
        let Some(id) = removed else {
            return false;
        };
        let _ = fs::remove_file(self.object_path(&id));
        self.persist();
        true
    }

    pub fn purge_where(&self, host: &str, select: impl Fn(&str) -> bool) -> u64 {
        let doomed: Vec<String> = {
            let inner = self.lock();
            inner
                .objects
                .values()
                .filter(|entry| entry.host == host && select(&entry.path))
                .map(|entry| entry.hash.clone())
                .collect()
        };
        let count = doomed.len() as u64;
        for hash in doomed {
            self.remove(&hash);
        }
        count
    }

    pub fn retain_hosts(&self, keep: impl Fn(&str) -> bool) -> u64 {
        let doomed: Vec<String> = {
            let inner = self.lock();
            inner
                .objects
                .values()
                .filter(|entry| !keep(&entry.host))
                .map(|entry| entry.hash.clone())
                .collect()
        };
        let count = doomed.len() as u64;
        for hash in doomed {
            self.remove(&hash);
        }
        count
    }

    fn object_path(&self, id: &str) -> PathBuf {
        self.dir.join("objects").join(id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn evict(&self) {
        let budget = self.budget();
        let mut inner = self.lock();
        if budget == 0 {
            let ids: Vec<String> = inner
                .objects
                .values()
                .map(|entry| entry.id.clone())
                .collect();
            let evicted = ids.len() as u64;
            inner.objects.clear();
            inner.total = 0;
            drop(inner);
            for id in ids {
                let _ = fs::remove_file(self.object_path(&id));
            }
            if evicted > 0 {
                self.evictions.fetch_add(evicted, Ordering::Relaxed);
            }
            return;
        }
        if inner.total <= budget {
            return;
        }
        let target = budget / 10 * 9;
        let mut order: Vec<(u64, String)> = inner
            .objects
            .values()
            .map(|entry| (entry.last_access, entry.hash.clone()))
            .collect();
        order.sort_unstable();
        let mut doomed = Vec::new();
        for (_, hash) in order {
            if inner.total <= target {
                break;
            }
            if let Some(entry) = inner.objects.remove(&hash) {
                inner.total = inner.total.saturating_sub(entry.size);
                doomed.push(entry.id);
            }
        }
        let evicted = doomed.len() as u64;
        drop(inner);
        for id in doomed {
            let _ = fs::remove_file(self.object_path(&id));
        }
        if evicted > 0 {
            self.evictions.fetch_add(evicted, Ordering::Relaxed);
        }
    }

    fn persist(&self) {
        let inner = self.lock();
        let file = IndexFile {
            objects: inner.objects.values().cloned().collect(),
        };
        drop(inner);
        let Ok(bytes) = serde_json::to_vec(&file) else {
            return;
        };
        let tmp = self.dir.join("index.json.tmp");
        let dest = self.dir.join("index.json");
        if fs::write(&tmp, bytes).is_ok() {
            let _ = fs::rename(&tmp, &dest);
        }
    }
}

fn file_id(hash: &str) -> String {
    hex::encode(Sha256::digest(hash.as_bytes()))
}

fn load_index(dir: &Path) -> HashMap<String, IndexEntry> {
    let path = dir.join("index.json");
    if let Ok(bytes) = fs::read(&path) {
        if let Ok(file) = serde_json::from_slice::<IndexFile>(&bytes) {
            let mut objects = HashMap::new();
            for entry in file.objects {
                let file_path = dir.join("objects").join(&entry.id);
                if file_path.is_file() {
                    objects.insert(entry.hash.clone(), entry);
                }
            }
            return objects;
        }
    }
    let mut objects = HashMap::new();
    let Ok(read) = fs::read_dir(dir.join("objects")) else {
        return objects;
    };
    for item in read.flatten() {
        let Ok(StoredObject {
            hash,
            host,
            path,
            meta,
            body,
        }) = read_object(&item.path())
        else {
            continue;
        };
        let Some(id) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let size = body.len() as u64
            + (meta.0.len() + meta.1.len()) as u64
            + (host.len() + path.len()) as u64
            + OVERHEAD;
        objects.insert(
            hash.clone(),
            IndexEntry {
                hash,
                id,
                host,
                path,
                size,
                last_access: 0,
            },
        );
    }
    objects
}

fn write_object(
    path: &Path,
    hash: &str,
    host: &str,
    path_key: &str,
    meta: &(Vec<u8>, Vec<u8>),
    body: &[u8],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(MAGIC)?;
        write_chunk(&mut file, hash.as_bytes())?;
        write_chunk(&mut file, host.as_bytes())?;
        write_chunk(&mut file, path_key.as_bytes())?;
        write_chunk(&mut file, &meta.0)?;
        write_chunk(&mut file, &meta.1)?;
        file.write_all(body)?;
    }
    fs::rename(&tmp, path)
}

struct StoredObject {
    hash: String,
    host: String,
    path: String,
    meta: (Vec<u8>, Vec<u8>),
    body: Bytes,
}

fn read_object(path: &Path) -> std::io::Result<StoredObject> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; MAGIC.len()];
    file.read_exact(&mut magic)?;
    if magic != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a cache object",
        ));
    }
    let _hash = read_chunk(&mut file)?;
    let _host = read_chunk(&mut file)?;
    let _path = read_chunk(&mut file)?;
    let meta0 = read_chunk(&mut file)?;
    let meta1 = read_chunk(&mut file)?;
    let mut body = Vec::new();
    file.read_to_end(&mut body)?;
    let hash = String::from_utf8(_hash).unwrap_or_default();
    let host = String::from_utf8(_host).unwrap_or_default();
    let key_path = String::from_utf8(_path).unwrap_or_default();
    Ok(StoredObject {
        hash,
        host,
        path: key_path,
        meta: (meta0, meta1),
        body: Bytes::from(body),
    })
}

fn write_chunk(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    file.write_all(&len.to_le_bytes())?;
    file.write_all(&bytes[..len as usize])
}

fn read_chunk(file: &mut File) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    file.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rudder-disk-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn objects_survive_a_restart_and_lru_eviction_drops_the_oldest() {
        let dir = scratch();
        let store = DiskStore::open(&dir, 50_000).unwrap();
        store.insert(
            "a",
            &(b"m".to_vec(), b"n".to_vec()),
            b"one",
            "a.test",
            "/one",
        );
        let one = store.totals().1;
        store.insert(
            "b",
            &(b"m".to_vec(), b"n".to_vec()),
            b"two",
            "a.test",
            "/two",
        );
        drop(store);

        let store = DiskStore::open(&dir, 50_000).unwrap();
        let first = store.get("a").unwrap();
        assert_eq!(first.body.as_ref(), b"one");
        assert_eq!(first.host, "a.test");
        assert_eq!(store.totals().0, 2);

        // Room for the object just read, not for both.
        store.set_budget(one + one / 2);
        assert!(store.evictions() >= 1);
        assert_eq!(store.totals().0, 1);
        assert!(store.get("a").is_some(), "the object just read stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn purge_removes_one_host_and_the_file() {
        let dir = scratch();
        let store = DiskStore::open(&dir, 50_000).unwrap();
        store.insert("a", &(vec![], vec![]), b"one", "a.test", "/one");
        store.insert("b", &(vec![], vec![]), b"two", "b.test", "/two");
        assert_eq!(store.purge_where("a.test", |_| true), 1);
        assert!(store.get("a").is_none());
        assert_eq!(store.get("b").unwrap().body.as_ref(), b"two");
        assert_eq!(store.retain_hosts(|host| host == "b.test"), 0);
        store.set_budget(0);
        assert_eq!(store.totals().0, 0);
        assert!(store.get("b").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
