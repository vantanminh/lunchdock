//! Bounded in-memory buffer of logs and stats waiting for the panel.
//!
//! Collectors keep filling it while the panel is unreachable. When it is full the
//! oldest items are dropped and counted, and the count travels with the next
//! batch. A batch stays in flight until the panel acknowledges its sequence
//! number; after a reconnect unacknowledged batches are sent again, so delivery
//! is at least once.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;

use rudder_proto::agent::{LogRecord, StatsSample, Telemetry};

const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_LOGS: usize = 20_000;
/// One hour of 10 second samples.
const MAX_STATS: usize = 360;
const MAX_IN_FLIGHT: usize = 4;
const BATCH_LOGS: usize = 500;
const BATCH_STATS: usize = 60;

pub struct Buffer {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    logs: VecDeque<LogRecord>,
    log_bytes: usize,
    stats: VecDeque<StatsSample>,
    dropped: u64,
    next_seq: u64,
    in_flight: BTreeMap<u64, Telemetry>,
    /// Newest acknowledged line per container, in Unix milliseconds.
    cursors: HashMap<String, i64>,
    cursors_dirty: bool,
}

fn record_size(record: &LogRecord) -> usize {
    record.text.len() + record.labels_json.len() + record.container_name.len() + 96
}

impl Inner {
    fn trim(&mut self) {
        while self.logs.len() > MAX_LOGS || self.log_bytes > MAX_LOG_BYTES {
            let Some(old) = self.logs.pop_front() else {
                break;
            };
            self.log_bytes = self.log_bytes.saturating_sub(record_size(&old));
            self.dropped += 1;
        }
        while self.stats.len() > MAX_STATS {
            self.stats.pop_front();
            self.dropped += 1;
        }
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::with_cursors(HashMap::new())
    }
}

impl Buffer {
    pub fn with_cursors(cursors: HashMap<String, i64>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                cursors,
                next_seq: 1,
                ..Inner::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn push_log(&self, record: LogRecord) {
        let mut inner = self.lock();
        inner.log_bytes += record_size(&record);
        inner.logs.push_back(record);
        inner.trim();
    }

    pub fn push_stats(&self, sample: StatsSample) {
        let mut inner = self.lock();
        inner.stats.push_back(sample);
        inner.trim();
    }

    /// Items waiting to be sent, not counting batches in flight.
    pub fn pending(&self) -> usize {
        let inner = self.lock();
        inner.logs.len() + inner.stats.len()
    }

    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    /// The next batch to send, or `None` when there is nothing to send or too
    /// many batches await acknowledgement.
    pub fn next_batch(&self) -> Option<Telemetry> {
        let mut inner = self.lock();
        if inner.in_flight.len() >= MAX_IN_FLIGHT
            || (inner.logs.is_empty() && inner.stats.is_empty() && inner.dropped == 0)
        {
            return None;
        }
        let count = inner.logs.len().min(BATCH_LOGS);
        let logs: Vec<LogRecord> = inner.logs.drain(..count).collect();
        let freed: usize = logs.iter().map(record_size).sum();
        inner.log_bytes = inner.log_bytes.saturating_sub(freed);
        let count = inner.stats.len().min(BATCH_STATS);
        let stats: Vec<StatsSample> = inner.stats.drain(..count).collect();
        let seq = inner.next_seq;
        inner.next_seq += 1;
        let batch = Telemetry {
            seq,
            logs,
            stats,
            dropped: std::mem::take(&mut inner.dropped),
        };
        inner.in_flight.insert(seq, batch.clone());
        Some(batch)
    }

    /// The panel stored batch `seq`.
    pub fn ack(&self, seq: u64) {
        let mut inner = self.lock();
        let Some(batch) = inner.in_flight.remove(&seq) else {
            return;
        };
        for record in &batch.logs {
            if record.container_id.is_empty() {
                continue;
            }
            let cursor = inner
                .cursors
                .entry(record.container_id.clone())
                .or_insert(i64::MIN);
            if record.logged_at_unix_ms > *cursor {
                *cursor = record.logged_at_unix_ms;
                inner.cursors_dirty = true;
            }
        }
    }

    /// The session ended: everything in flight goes back to the front of the queue.
    pub fn requeue(&self) {
        let mut inner = self.lock();
        let batches = std::mem::take(&mut inner.in_flight);
        for (_, batch) in batches.into_iter().rev() {
            for record in batch.logs.into_iter().rev() {
                inner.log_bytes += record_size(&record);
                inner.logs.push_front(record);
            }
            for sample in batch.stats.into_iter().rev() {
                inner.stats.push_front(sample);
            }
            inner.dropped += batch.dropped;
        }
        inner.trim();
    }

    pub fn cursor(&self, container_id: &str) -> Option<i64> {
        self.lock().cursors.get(container_id).copied()
    }

    /// Acknowledged cursors, when they changed since the last call.
    pub fn take_cursors(&self, live: &dyn Fn(&str) -> bool) -> Option<HashMap<String, i64>> {
        let mut inner = self.lock();
        let before = inner.cursors.len();
        inner.cursors.retain(|id, _| live(id));
        if !inner.cursors_dirty && inner.cursors.len() == before {
            return None;
        }
        inner.cursors_dirty = false;
        Some(inner.cursors.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(container: &str, at: i64, text: &str) -> LogRecord {
        LogRecord {
            logged_at_unix_ms: at,
            container_id: container.into(),
            text: text.into(),
            ..LogRecord::default()
        }
    }

    #[test]
    fn unacknowledged_batches_are_sent_again_in_order() {
        let buffer = Buffer::default();
        for i in 0..3 {
            buffer.push_log(log("c1", i, &format!("line {i}")));
        }
        let first = buffer.next_batch().unwrap();
        assert_eq!(first.logs.len(), 3);
        buffer.push_log(log("c1", 3, "line 3"));
        buffer.requeue();
        let again = buffer.next_batch().unwrap();
        let texts: Vec<&str> = again.logs.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["line 0", "line 1", "line 2", "line 3"]);
        assert!(again.seq > first.seq);
        buffer.ack(first.seq);
        assert_eq!(buffer.cursor("c1"), None, "a stale ack changes nothing");
        buffer.ack(again.seq);
        assert_eq!(buffer.cursor("c1"), Some(3));
        assert!(buffer.next_batch().is_none());
    }

    #[test]
    fn overflow_drops_the_oldest_and_reports_the_count() {
        let buffer = Buffer::default();
        for i in 0..(MAX_LOGS + 10) {
            buffer.push_log(log("c1", i as i64, "x"));
        }
        assert_eq!(buffer.pending(), MAX_LOGS);
        let batch = buffer.next_batch().unwrap();
        assert_eq!(batch.dropped, 10);
        assert_eq!(batch.logs[0].logged_at_unix_ms, 10);
        assert_eq!(buffer.dropped(), 0);
        buffer.requeue();
        assert_eq!(buffer.dropped(), 10, "a resent batch keeps its drop count");
    }

    #[test]
    fn in_flight_batches_are_bounded() {
        let buffer = Buffer::default();
        for i in 0..(BATCH_LOGS * (MAX_IN_FLIGHT + 1)) {
            buffer.push_log(log("c1", i as i64, "x"));
        }
        for _ in 0..MAX_IN_FLIGHT {
            assert!(buffer.next_batch().is_some());
        }
        assert!(buffer.next_batch().is_none());
        buffer.ack(1);
        assert!(buffer.next_batch().is_some());
    }

    #[test]
    fn cursors_forget_removed_containers() {
        let buffer = Buffer::default();
        buffer.push_log(log("c1", 5, "a"));
        buffer.push_log(log("c2", 7, "b"));
        let batch = buffer.next_batch().unwrap();
        buffer.ack(batch.seq);
        let cursors = buffer.take_cursors(&|_| true).unwrap();
        assert_eq!(cursors.len(), 2);
        assert!(buffer.take_cursors(&|_| true).is_none());
        let cursors = buffer.take_cursors(&|id| id == "c2").unwrap();
        assert_eq!(cursors, HashMap::from([("c2".to_owned(), 7)]));
    }
}
