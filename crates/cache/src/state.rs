//! Rules, counters, and the store shared by the proxy, admin, and reload services.

use std::collections::{BTreeMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rudder_core::{CacheDocument, CacheHostStats, CachePurge, CacheStats, DEFAULT_MEMORY_BYTES};

use crate::storage::MemoryStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Hit,
    Miss,
    Stale,
    Expired,
    Revalidated,
    Bypass,
}

impl Outcome {
    pub fn header(self) -> &'static str {
        match self {
            Self::Hit => "HIT",
            Self::Miss => "MISS",
            Self::Stale => "STALE",
            Self::Expired => "EXPIRED",
            Self::Revalidated => "REVALIDATED",
            Self::Bypass => "BYPASS",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    hits: u64,
    misses: u64,
    stale: u64,
    bypass: u64,
    bytes_served: u64,
}

pub struct Shared {
    rules: RwLock<Arc<CacheDocument>>,
    rules_hash: Mutex<Option<u64>>,
    pub rules_path: PathBuf,
    pub memory_override: Option<u64>,
    pub store: &'static MemoryStore,
    pub token: String,
    counters: Mutex<BTreeMap<String, Counters>>,
    reloads: AtomicU64,
    /// Unix second and how many purge or reload calls landed in it.
    admin_hits: Mutex<(u64, u32)>,
}

impl Shared {
    pub fn new(rules_path: PathBuf, memory_override: Option<u64>, token: String) -> &'static Self {
        let store: &'static MemoryStore = Box::leak(Box::new(MemoryStore::new(
            memory_override.unwrap_or(DEFAULT_MEMORY_BYTES),
        )));
        Box::leak(Box::new(Self {
            rules: RwLock::new(Arc::new(CacheDocument::default())),
            rules_hash: Mutex::new(None),
            rules_path,
            memory_override,
            store,
            token,
            counters: Mutex::default(),
            reloads: AtomicU64::new(0),
            admin_hits: Mutex::new((0, 0)),
        }))
    }

    /// Purge and reload share a small per-second budget so a stolen admin token
    /// cannot hammer the process.
    pub fn allow_admin_mutation(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let mut slot = self
            .admin_hits
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if slot.0 != now {
            *slot = (now, 1);
            return true;
        }
        if slot.1 >= 30 {
            return false;
        }
        slot.1 += 1;
        true
    }

    pub fn rules(&self) -> Arc<CacheDocument> {
        self.rules
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Load the rules file when it changed. A missing file means no sites; a bad file keeps
    /// the current rules.
    pub fn reload(&self) -> Result<bool, String> {
        let text = match std::fs::read_to_string(&self.rules_path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("read {}: {error}", self.rules_path.display())),
        };
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        let mut current = self
            .rules_hash
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *current == Some(hash) {
            return Ok(false);
        }
        let document = if text.trim().is_empty() {
            CacheDocument::default()
        } else {
            CacheDocument::parse(&text).map_err(|error| error.to_string())?
        };
        *current = Some(hash);
        let hosts: HashSet<String> = document
            .sites
            .iter()
            .map(|site| site.host.clone())
            .collect();
        self.store.set_budget(
            self.memory_override
                .or(document.memory_max_bytes)
                .unwrap_or(DEFAULT_MEMORY_BYTES),
        );
        self.store.retain_hosts(|host| hosts.contains(host));
        *self
            .rules
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Arc::new(document);
        self.reloads.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    pub fn record(&self, host: &str, outcome: Outcome, bytes: u64) {
        let mut counters = self
            .counters
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = counters.entry(host.to_owned()).or_default();
        match outcome {
            Outcome::Hit | Outcome::Revalidated => entry.hits += 1,
            Outcome::Miss | Outcome::Expired => entry.misses += 1,
            Outcome::Stale => entry.stale += 1,
            Outcome::Bypass => entry.bypass += 1,
        }
        entry.bytes_served += bytes;
    }

    pub fn purge(&self, request: &CachePurge) -> u64 {
        if request.all {
            return self.store.purge_where(&request.host, |_| true);
        }
        if let Some(prefix) = &request.prefix {
            return self
                .store
                .purge_where(&request.host, |path| path.starts_with(prefix.as_str()));
        }
        let paths: HashSet<&str> = request.paths.iter().map(String::as_str).collect();
        self.store
            .purge_where(&request.host, |path| paths.contains(path))
    }

    pub fn stats(&self) -> CacheStats {
        let usage = self.store.usage_by_host();
        let counters = self
            .counters
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let mut hosts: BTreeMap<String, CacheHostStats> = BTreeMap::new();
        for site in &self.rules().sites {
            hosts
                .entry(site.host.clone())
                .or_insert_with(|| CacheHostStats {
                    host: site.host.clone(),
                    ..CacheHostStats::default()
                });
        }
        for (host, counter) in counters {
            let entry = hosts.entry(host.clone()).or_insert_with(|| CacheHostStats {
                host,
                ..CacheHostStats::default()
            });
            entry.hits = counter.hits;
            entry.misses = counter.misses;
            entry.stale = counter.stale;
            entry.bypass = counter.bypass;
            entry.bytes_served = counter.bytes_served;
        }
        for (host, used) in usage {
            let entry = hosts.entry(host.clone()).or_insert_with(|| CacheHostStats {
                host,
                ..CacheHostStats::default()
            });
            entry.objects = used.objects;
            entry.stored_bytes = used.bytes;
        }
        let (objects, stored_bytes) = self.store.totals();
        CacheStats {
            version: rudder_core::VERSION.into(),
            rules_version: self.rules().version,
            objects,
            stored_bytes,
            memory_max_bytes: self.store.budget(),
            evictions: self.store.evictions(),
            reloads: self.reloads.load(Ordering::Relaxed),
            hosts: hosts.into_values().collect(),
        }
    }

    pub fn metrics(&self) -> String {
        let stats = self.stats();
        let mut out = String::new();
        out.push_str(
            "# HELP rudder_cache_requests_total Requests by host and cache outcome.\n\
             # TYPE rudder_cache_requests_total counter\n",
        );
        let mut lines = String::new();
        for host in &stats.hosts {
            let label = escape_label(&host.host);
            for (outcome, value) in [
                ("hit", host.hits),
                ("miss", host.misses),
                ("stale", host.stale),
                ("bypass", host.bypass),
            ] {
                lines.push_str(&format!(
                    "rudder_cache_requests_total{{host=\"{label}\",outcome=\"{outcome}\"}} {value}\n"
                ));
            }
        }
        out.push_str(&lines);
        let per_host = [
            (
                "rudder_cache_served_bytes_total",
                "counter",
                "Response body bytes sent to clients.",
            ),
            (
                "rudder_cache_host_objects",
                "gauge",
                "Objects stored per host.",
            ),
            (
                "rudder_cache_host_stored_bytes",
                "gauge",
                "Bytes stored per host.",
            ),
        ];
        for (index, (name, kind, help)) in per_host.iter().enumerate() {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
            for host in &stats.hosts {
                let value = match index {
                    0 => host.bytes_served,
                    1 => host.objects,
                    _ => host.stored_bytes,
                };
                out.push_str(&format!(
                    "{name}{{host=\"{}\"}} {value}\n",
                    escape_label(&host.host)
                ));
            }
        }
        for (name, kind, help, value) in [
            (
                "rudder_cache_objects",
                "gauge",
                "Objects in the memory store.",
                stats.objects,
            ),
            (
                "rudder_cache_stored_bytes",
                "gauge",
                "Bytes held by the memory store.",
                stats.stored_bytes,
            ),
            (
                "rudder_cache_memory_max_bytes",
                "gauge",
                "Memory store budget.",
                stats.memory_max_bytes,
            ),
            (
                "rudder_cache_evictions_total",
                "counter",
                "Objects evicted to stay within the budget.",
                stats.evictions,
            ),
            (
                "rudder_cache_reloads_total",
                "counter",
                "Rule reloads applied.",
                stats.reloads,
            ),
        ] {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n"
            ));
        }
        out
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
