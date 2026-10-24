//! In-memory object store with a global byte budget and least-recently-used eviction.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use pingora_cache::key::CacheHashKey;
use pingora_cache::storage::{
    HandleHit, HandleMiss, HitHandler, MissFinishType, MissHandler, PurgeOutcome, PurgeTarget,
    PurgeType, Storage,
};
use pingora_cache::trace::SpanHandle;
use pingora_cache::{CacheKey, CacheMeta};
use pingora_core::Result;

use crate::disk::DiskStore;

/// Bytes accounted per object on top of its body and metadata.
const OBJECT_OVERHEAD: u64 = 256;

struct Entry {
    meta: (Vec<u8>, Vec<u8>),
    body: Bytes,
    host: String,
    path: String,
    size: u64,
    last_access: u64,
}

#[derive(Default)]
struct Inner {
    objects: HashMap<String, Entry>,
    total: u64,
    tick: u64,
}

pub struct MemoryStore {
    inner: Arc<Mutex<Inner>>,
    budget: AtomicU64,
    evictions: AtomicU64,
    disk: Option<DiskStore>,
}

/// The primary key is `host \n encoding \n path`.
pub fn primary_key(host: &str, encoding: &str, path: &str) -> String {
    format!("{host}\n{encoding}\n{path}")
}

fn split_primary(key: &CacheKey) -> (String, String) {
    let primary = String::from_utf8_lossy(key.primary_key());
    let mut parts = primary.splitn(3, '\n');
    let host = parts.next().unwrap_or_default().to_owned();
    let _encoding = parts.next();
    let path = parts.next().unwrap_or_default().to_owned();
    (host, path)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostUsage {
    pub objects: u64,
    pub bytes: u64,
}

impl MemoryStore {
    pub fn new(budget: u64) -> Self {
        Self {
            inner: Arc::default(),
            budget: AtomicU64::new(budget),
            evictions: AtomicU64::new(0),
            disk: None,
        }
    }

    /// Memory store plus a disk directory. The disk budget stays unlimited until
    /// [`Self::set_disk_budget`]; a later budget of zero drops the files.
    pub fn with_disk(budget: u64, dir: &std::path::Path) -> Self {
        let disk = match DiskStore::open(dir, u64::MAX) {
            Ok(disk) => Some(disk),
            Err(error) => {
                tracing::warn!(%error, path = %dir.display(), "disk cache is off");
                None
            }
        };
        Self {
            disk,
            ..Self::new(budget)
        }
    }

    pub fn set_disk_budget(&self, budget: u64) {
        if let Some(disk) = &self.disk {
            disk.set_budget(budget);
        }
    }

    pub fn disk_budget(&self) -> u64 {
        self.disk.as_ref().map(DiskStore::budget).unwrap_or(0)
    }

    pub fn disk_totals(&self) -> (u64, u64) {
        self.disk.as_ref().map(DiskStore::totals).unwrap_or((0, 0))
    }

    pub fn disk_evictions(&self) -> u64 {
        self.disk.as_ref().map(DiskStore::evictions).unwrap_or(0)
    }

    pub fn set_budget(&self, budget: u64) {
        self.budget.store(budget, Ordering::Relaxed);
        let mut inner = self.lock();
        self.evict(&mut inner);
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

    pub fn usage_by_host(&self) -> HashMap<String, HostUsage> {
        let inner = self.lock();
        let mut out: HashMap<String, HostUsage> = HashMap::new();
        for entry in inner.objects.values() {
            let usage = out.entry(entry.host.clone()).or_default();
            usage.objects += 1;
            usage.bytes += entry.size;
        }
        out
    }

    /// Remove every object for `host` whose path satisfies `select`. Returns how many went.
    pub fn purge_where(&self, host: &str, select: impl Fn(&str) -> bool) -> u64 {
        let mut inner = self.lock();
        let doomed: Vec<String> = inner
            .objects
            .iter()
            .filter(|(_, entry)| entry.host == host && select(&entry.path))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &doomed {
            if let Some(entry) = inner.objects.remove(key) {
                inner.total -= entry.size;
            }
        }
        drop(inner);
        if let Some(disk) = &self.disk {
            for key in &doomed {
                disk.remove(key);
            }
        }
        let disk_only = self
            .disk
            .as_ref()
            .map(|disk| disk.purge_where(host, &select))
            .unwrap_or(0);
        doomed.len() as u64 + disk_only
    }

    /// Remove every object whose host fails `keep`.
    pub fn retain_hosts(&self, keep: impl Fn(&str) -> bool) -> u64 {
        let mut inner = self.lock();
        let before = inner.objects.len();
        let mut freed = 0;
        inner.objects.retain(|_, entry| {
            let kept = keep(&entry.host);
            if !kept {
                freed += entry.size;
            }
            kept
        });
        inner.total -= freed;
        let removed = (before - inner.objects.len()) as u64;
        drop(inner);
        if let Some(disk) = &self.disk {
            disk.retain_hosts(&keep);
        }
        removed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Drop the least recently used objects until the store is at 90% of its budget.
    fn evict(&self, inner: &mut Inner) {
        let budget = self.budget();
        if inner.total <= budget {
            return;
        }
        let target = budget / 10 * 9;
        let mut order: Vec<(u64, String)> = inner
            .objects
            .iter()
            .map(|(key, entry)| (entry.last_access, key.clone()))
            .collect();
        order.sort_unstable();
        let mut evicted = 0;
        for (_, key) in order {
            if inner.total <= target {
                break;
            }
            if let Some(entry) = inner.objects.remove(&key) {
                inner.total -= entry.size;
                evicted += 1;
            }
        }
        self.evictions.fetch_add(evicted, Ordering::Relaxed);
    }

    fn insert(&self, hash: String, entry: Entry) {
        let mut inner = self.lock();
        inner.tick += 1;
        let entry = Entry {
            last_access: inner.tick,
            ..entry
        };
        inner.total += entry.size;
        if let Some(old) = inner.objects.insert(hash, entry) {
            inner.total -= old.size;
        }
        self.evict(&mut inner);
    }
}

#[async_trait]
impl Storage for MemoryStore {
    async fn lookup(
        &'static self,
        key: &CacheKey,
        _trace: &SpanHandle,
    ) -> Result<Option<(CacheMeta, HitHandler)>> {
        let hash = key.combined();
        let found = {
            let mut inner = self.lock();
            inner.tick += 1;
            let tick = inner.tick;
            inner.objects.get_mut(&hash).map(|entry| {
                entry.last_access = tick;
                (entry.meta.clone(), entry.body.clone())
            })
        };
        let Some((meta, body)) = found else {
            if let Some(disk) = &self.disk {
                if let Some(object) = disk.get(&hash) {
                    let meta = CacheMeta::deserialize(&object.meta.0, &object.meta.1)?;
                    let size = object.body.len() as u64
                        + (object.meta.0.len() + object.meta.1.len()) as u64
                        + (object.host.len() + object.path.len()) as u64
                        + OBJECT_OVERHEAD;
                    self.insert(
                        hash,
                        Entry {
                            meta: object.meta,
                            body: object.body.clone(),
                            host: object.host,
                            path: object.path,
                            size,
                            last_access: 0,
                        },
                    );
                    let end = object.body.len();
                    return Ok(Some((
                        meta,
                        Box::new(Hit {
                            body: object.body,
                            start: 0,
                            end,
                            done: false,
                        }),
                    )));
                }
            }
            return Ok(None);
        };
        let meta = CacheMeta::deserialize(&meta.0, &meta.1)?;
        let end = body.len();
        Ok(Some((
            meta,
            Box::new(Hit {
                body,
                start: 0,
                end,
                done: false,
            }),
        )))
    }

    async fn get_miss_handler(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
        _trace: &SpanHandle,
    ) -> Result<MissHandler> {
        let (host, path) = split_primary(key);
        Ok(Box::new(Miss {
            store: self,
            hash: key.combined(),
            meta: meta.serialize()?,
            host,
            path,
            body: Vec::new(),
        }))
    }

    async fn purge(
        &'static self,
        target: PurgeTarget<'_>,
        _purge_type: PurgeType,
        _trace: &SpanHandle,
    ) -> Result<PurgeOutcome> {
        let hash = target.key().combined();
        let mut inner = self.lock();
        let removed = match inner.objects.remove(&hash) {
            Some(entry) => {
                inner.total -= entry.size;
                true
            }
            None => false,
        };
        drop(inner);
        let disk_removed = self.disk.as_ref().is_some_and(|disk| disk.remove(&hash));
        if removed || disk_removed {
            Ok(PurgeOutcome::Purged(None))
        } else {
            Ok(PurgeOutcome::NotFound)
        }
    }

    async fn update_meta(
        &'static self,
        key: &CacheKey,
        meta: &CacheMeta,
        _trace: &SpanHandle,
    ) -> Result<bool> {
        let serialized = meta.serialize()?;
        let mut inner = self.lock();
        let Some(entry) = inner.objects.get_mut(&key.combined()) else {
            return Ok(false);
        };
        let old = (entry.meta.0.len() + entry.meta.1.len()) as u64;
        let new = (serialized.0.len() + serialized.1.len()) as u64;
        entry.meta = serialized.clone();
        entry.size = entry.size - old + new;
        inner.total = inner.total - old + new;
        drop(inner);
        if let Some(disk) = &self.disk {
            disk.update_meta(&key.combined(), &serialized);
        }
        Ok(true)
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync + 'static) {
        self
    }
}

struct Hit {
    body: Bytes,
    start: usize,
    end: usize,
    done: bool,
}

#[async_trait]
impl HandleHit for Hit {
    async fn read_body(&mut self) -> Result<Option<Bytes>> {
        if self.done {
            return Ok(None);
        }
        self.done = true;
        Ok(Some(self.body.slice(self.start..self.end)))
    }

    async fn finish(
        self: Box<Self>,
        _storage: &'static (dyn Storage + Sync),
        _key: &CacheKey,
        _trace: &SpanHandle,
    ) -> Result<()> {
        Ok(())
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, start: usize, end: Option<usize>) -> Result<()> {
        let len = self.body.len();
        if start >= len {
            return pingora_core::Error::e_explain(
                pingora_core::ErrorType::InternalError,
                format!("seek start out of range {start} >= {len}"),
            );
        }
        self.start = start;
        self.end = end.map_or(len, |end| (end + 1).min(len));
        self.done = false;
        Ok(())
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }

    fn as_any_mut(&mut self) -> &mut (dyn Any + Send + Sync) {
        self
    }
}

struct Miss {
    store: &'static MemoryStore,
    hash: String,
    meta: (Vec<u8>, Vec<u8>),
    host: String,
    path: String,
    body: Vec<u8>,
}

#[async_trait]
impl HandleMiss for Miss {
    async fn write_body(&mut self, data: Bytes, _eof: bool) -> Result<()> {
        self.body.extend_from_slice(&data);
        Ok(())
    }

    async fn finish(self: Box<Self>) -> Result<MissFinishType> {
        let len = self.body.len();
        let size = len as u64
            + (self.meta.0.len() + self.meta.1.len()) as u64
            + (self.host.len() + self.path.len()) as u64
            + OBJECT_OVERHEAD;
        let store = self.store;
        if let Some(disk) = &store.disk {
            disk.insert(&self.hash, &self.meta, &self.body, &self.host, &self.path);
        }
        let hash = self.hash;
        store.insert(
            hash,
            Entry {
                meta: self.meta,
                body: Bytes::from(self.body),
                host: self.host,
                path: self.path,
                size,
                last_access: 0,
            },
        );
        Ok(MissFinishType::Created(len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingora_cache::trace::Span;
    use pingora_http::ResponseHeader;
    use std::time::{Duration, SystemTime};

    fn meta() -> CacheMeta {
        let now = SystemTime::now();
        CacheMeta::new(
            now + Duration::from_secs(60),
            now,
            0,
            0,
            ResponseHeader::build(200, None).unwrap(),
        )
    }

    async fn put(store: &'static MemoryStore, host: &str, path: &str, body: &[u8]) -> CacheKey {
        let key = CacheKey::new(primary_key(host, "identity", path), "");
        let mut miss = store
            .get_miss_handler(&key, &meta(), &Span::inactive().handle())
            .await
            .unwrap();
        miss.write_body(Bytes::copy_from_slice(body), true)
            .await
            .unwrap();
        miss.finish().await.unwrap();
        key
    }

    #[tokio::test]
    async fn stores_evicts_lru_and_purges_by_host_and_prefix() {
        let store: &'static MemoryStore = Box::leak(Box::new(MemoryStore::new(1_000_000)));
        let first = put(store, "a.test", "/one", &[1; 1000]).await;
        put(store, "a.test", "/two/x", &[2; 1000]).await;
        put(store, "b.test", "/one", &[3; 1000]).await;
        let (meta, mut hit) = store
            .lookup(&first, &Span::inactive().handle())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(meta.response_header().status.as_u16(), 200);
        assert_eq!(hit.read_body().await.unwrap().unwrap().len(), 1000);
        assert!(hit.read_body().await.unwrap().is_none());
        assert_eq!(store.totals().0, 3);
        assert_eq!(store.usage_by_host()["a.test"].objects, 2);

        assert_eq!(
            store.purge_where("a.test", |path| path.starts_with("/two")),
            1
        );
        assert_eq!(store.retain_hosts(|host| host == "a.test"), 1);
        assert_eq!(store.totals().0, 1);

        let (_, one_size) = store.totals();
        store.set_budget(one_size * 5 / 2);
        put(store, "a.test", "/two", &[2; 1000]).await;
        store
            .lookup(&first, &Span::inactive().handle())
            .await
            .unwrap()
            .unwrap();
        put(store, "a.test", "/three", &[3; 1000]).await;
        assert!(store.evictions() >= 1);
        assert!(store.totals().1 <= one_size * 5 / 2);
        assert!(
            store
                .lookup(&first, &Span::inactive().handle())
                .await
                .unwrap()
                .is_some(),
            "recently read object survives eviction"
        );
    }

    #[tokio::test]
    async fn a_restart_reads_the_object_back_from_disk() {
        let dir = std::env::temp_dir().join(format!(
            "rudder-cache-disk-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store: &'static MemoryStore =
            Box::leak(Box::new(MemoryStore::with_disk(1_000_000, &dir)));
        store.set_disk_budget(1_000_000);
        let key = put(store, "a.test", "/kept", b"hello").await;
        assert_eq!(store.disk_totals().0, 1);

        let restarted: &'static MemoryStore =
            Box::leak(Box::new(MemoryStore::with_disk(1_000_000, &dir)));
        restarted.set_disk_budget(1_000_000);
        assert_eq!(restarted.totals().0, 0, "memory starts empty");
        let (_meta, mut hit) = restarted
            .lookup(&key, &Span::inactive().handle())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit.read_body().await.unwrap().unwrap().as_ref(), b"hello");
        assert_eq!(restarted.totals().0, 1, "a disk hit is copied into memory");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
