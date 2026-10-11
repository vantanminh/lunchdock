//! Metrics sampling and log following on the node, independent of the panel connection.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rudder_core::{ContainerInfo, LogStreamKind, NodeDriver, CACHE_CONTAINER, TRAEFIK_CONTAINER};
use rudder_proto::agent::{LogRecord, StatsSample};
use tokio::task::JoinHandle;

use crate::telemetry::Buffer;

const LOG_TICK: Duration = Duration::from_secs(5);
/// How far back to read a container seen for the first time.
const FIRST_SEEN_LOOKBACK_MS: i64 = 5 * 60 * 1000;
const MAX_LINE_BYTES: usize = 16 * 1024;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Sample host and container stats every `interval`.
pub async fn stats(driver: Arc<dyn NodeDriver>, buffer: Arc<Buffer>, interval: Duration) {
    loop {
        match driver.host_stats().await {
            Ok(host) => {
                let containers = match driver.container_stats().await {
                    Ok(containers) => containers,
                    Err(error) => {
                        tracing::debug!(%error, "container stats unavailable");
                        Vec::new()
                    }
                };
                let host_json = serde_json::to_vec(&host).unwrap_or_default();
                let containers_json = serde_json::to_vec(&containers).unwrap_or_default();
                buffer.push_stats(StatsSample {
                    sampled_at_unix_ms: now_ms(),
                    host_json,
                    containers_json,
                });
            }
            Err(error) => tracing::warn!(%error, "host stats unavailable"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// Containers whose output belongs in the panel's log store.
pub fn wanted(container: &ContainerInfo) -> bool {
    container.name == TRAEFIK_CONTAINER
        || container.name == CACHE_CONTAINER
        || container.labels.get("rudder.managed").map(String::as_str) == Some("true")
}

pub fn load_cursors(path: &Path) -> HashMap<String, i64> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Follow every wanted container, resuming after the newest acknowledged line.
pub async fn logs(driver: Arc<dyn NodeDriver>, buffer: Arc<Buffer>, cursor_file: PathBuf) {
    let mut followed: HashMap<String, JoinHandle<()>> = HashMap::new();
    // Newest line buffered per container, so a restarted follow does not resend.
    let mut sent: HashMap<String, Arc<AtomicI64>> = HashMap::new();
    loop {
        match driver.list_containers().await {
            Ok(containers) => {
                let live: HashMap<&str, &ContainerInfo> = containers
                    .iter()
                    .filter(|c| wanted(c))
                    .map(|c| (c.id.as_str(), c))
                    .collect();
                followed.retain(|id, handle| {
                    let keep = live.contains_key(id.as_str()) && !handle.is_finished();
                    if !keep {
                        handle.abort();
                    }
                    keep
                });
                sent.retain(|id, _| live.contains_key(id.as_str()));
                for (id, container) in &live {
                    if followed.contains_key(*id) {
                        continue;
                    }
                    let last = sent
                        .entry((*id).to_owned())
                        .or_insert_with(|| Arc::new(AtomicI64::new(i64::MIN)))
                        .clone();
                    let since = buffer
                        .cursor(id)
                        .max(Some(last.load(Ordering::Relaxed)))
                        .filter(|at| *at != i64::MIN)
                        .unwrap_or_else(|| now_ms() - FIRST_SEEN_LOOKBACK_MS);
                    let task = tokio::spawn(follow(
                        driver.clone(),
                        buffer.clone(),
                        (*container).clone(),
                        since,
                        last,
                    ));
                    followed.insert((*id).to_owned(), task);
                }
                if let Some(cursors) = buffer.take_cursors(&|id| live.contains_key(id)) {
                    save_cursors(&cursor_file, &cursors);
                }
            }
            Err(error) => tracing::debug!(%error, "cannot list containers"),
        }
        tokio::time::sleep(LOG_TICK).await;
    }
}

fn save_cursors(path: &Path, cursors: &HashMap<String, i64>) {
    let Ok(bytes) = serde_json::to_vec(cursors) else {
        return;
    };
    if let Err(error) = crate::identity::write_private(path, &bytes) {
        tracing::warn!(error = %format!("{error:#}"), "cannot save log cursors");
    }
}

async fn follow(
    driver: Arc<dyn NodeDriver>,
    buffer: Arc<Buffer>,
    container: ContainerInfo,
    since_ms: i64,
    last: Arc<AtomicI64>,
) {
    let mut stream = match driver.follow_logs(&container.id, since_ms / 1000).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::debug!(%error, container = %container.name, "cannot follow logs");
            return;
        }
    };
    let labels_json = serde_json::to_string(&container.labels).unwrap_or_default();
    while let Some(item) = stream.next().await {
        let Ok(line) = item else { break };
        let at = line
            .timestamp
            .as_deref()
            .and_then(rudder_core::parse_docker_timestamp)
            .map_or_else(now_ms, |at| at.timestamp_millis());
        // stdout and stderr interleave out of timestamp order, so only the
        // starting point filters lines within one follow.
        if at <= since_ms {
            continue;
        }
        last.fetch_max(at, Ordering::Relaxed);
        buffer.push_log(LogRecord {
            logged_at_unix_ms: at,
            container_id: container.id.clone(),
            container_name: container.name.clone(),
            labels_json: labels_json.clone(),
            stderr: line.stream == LogStreamKind::Stderr,
            text: clip(line.text),
            source: String::new(),
        });
    }
}

fn clip(mut text: String) -> String {
    if text.len() > MAX_LINE_BYTES {
        let mut end = MAX_LINE_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

static SHIPPED: Mutex<Option<Arc<Buffer>>> = Mutex::new(None);

/// Send the agent's own log lines to the panel as well as to stdout.
pub fn ship_own_logs(buffer: Arc<Buffer>) {
    if let Ok(mut slot) = SHIPPED.lock() {
        *slot = Some(buffer);
    }
}

/// `tracing_subscriber` writer: stdout plus the shipping buffer once installed.
#[derive(Default)]
pub struct AgentLogWriter {
    line: Vec<u8>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for crate::LogWriterFactory {
    type Writer = AgentLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        AgentLogWriter::default()
    }
}

impl Write for AgentLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.line.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for AgentLogWriter {
    fn drop(&mut self) {
        if self.line.is_empty() {
            return;
        }
        let _ = std::io::stdout().write_all(&self.line);
        let buffer = SHIPPED.lock().ok().and_then(|slot| slot.clone());
        if let Some(buffer) = buffer {
            let text = String::from_utf8_lossy(&self.line).trim_end().to_owned();
            buffer.push_log(LogRecord {
                logged_at_unix_ms: now_ms(),
                text: clip(text),
                source: "agent".into(),
                ..LogRecord::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn container(name: &str, labels: &[(&str, &str)]) -> ContainerInfo {
        ContainerInfo {
            id: format!("id-{name}"),
            name: name.into(),
            image: "img".into(),
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn follows_managed_containers_and_the_edge() {
        assert!(wanted(&container("rudder-traefik", &[])));
        assert!(wanted(&container("rudder-cache", &[])));
        assert!(wanted(&container("app", &[("rudder.managed", "true")])));
        assert!(!wanted(&container("postgres", &[])));
        assert!(!wanted(&container("app", &[("rudder.managed", "false")])));
    }

    #[test]
    fn long_lines_are_cut_on_a_character_boundary() {
        let text = "é".repeat(MAX_LINE_BYTES);
        let cut = clip(text);
        assert!(cut.len() <= MAX_LINE_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
    }
}
