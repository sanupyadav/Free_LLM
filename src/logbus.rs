//! Runtime log bus: broadcast subscription + ring buffer
//!
//! - [`LogBus::emit`] writes to both the broadcast channel (SSE real-time push) and the ring buffer (history replay)
//! - [`LogBus::recent`] supports `after_id` replay (SSE Last-Event-ID reconnect)
//! - The ring buffer drops the oldest event when full; broadcasting never blocks when there are no subscribers

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// A single runtime log event
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEvent {
    /// Globally increasing id (starts at 1, usable as the SSE event id)
    pub id: u64,
    /// RFC3339 timestamp
    pub ts: String,
    /// Level: debug/info/warn/error
    pub level: String,
    /// Event category: request/retry/account/system/...
    pub kind: String,
    /// Associated request id
    pub req_id: Option<String>,
    pub message: String,
}

/// Log bus: broadcast sender + ring buffer
pub struct LogBus {
    sender: broadcast::Sender<LogEvent>,
    ring: Arc<Mutex<VecDeque<LogEvent>>>,
    capacity: usize,
    next_id: AtomicU64,
    /// Redaction toggle (v0.8): replaces Cookie/Bearer/authorization values with *** before writing
    redact: bool,
}

impl LogBus {
    /// `capacity` is used for both the ring buffer and the broadcast buffer capacity (at least 1)
    pub fn new(capacity: usize) -> Self {
        Self::new_with_redact(capacity, true)
    }

    /// Constructor with a redaction toggle (redaction is on by default; set redact_logs=false to disable)
    pub fn new_with_redact(capacity: usize, redact: bool) -> Self {
        let cap = capacity.clamp(1, 1 << 20);
        let (sender, _) = broadcast::channel(cap);
        Self {
            sender,
            ring: Arc::new(Mutex::new(VecDeque::with_capacity(cap.min(4096)))),
            capacity: cap,
            next_id: AtomicU64::new(0),
            redact,
        }
    }

    /// Broadcasts and writes to the ring buffer (silently drops the broadcast when there are no subscribers)
    pub fn emit(&self, level: &str, kind: &str, req_id: Option<&str>, message: impl Into<String>) {
        let raw: String = message.into();
        let message = if self.redact {
            crate::redact::redact(&raw)
        } else {
            raw
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let event = LogEvent {
            id,
            ts: chrono::Utc::now().to_rfc3339(),
            level: level.to_string(),
            kind: kind.to_string(),
            req_id: req_id.map(str::to_string),
            message,
        };
        self.push_ring(event.clone());
        // send returns Err when there are no subscribers; safe to ignore
        let _ = self.sender.send(event);
    }

    /// Subscribes to the real-time event stream
    pub fn subscribe(&self) -> broadcast::Receiver<LogEvent> {
        self.sender.subscribe()
    }

    /// Reads historical events (returned in chronological order)
    ///
    /// - `after_id = None`: returns the latest `limit` entries
    /// - `after_id = Some(x)`: replays up to `limit` entries starting from the earliest unread event after `x`
    pub fn recent(&self, limit: usize, after_id: Option<u64>) -> Vec<LogEvent> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(after) = after_id {
            return ring
                .iter()
                .filter(|e| e.id > after)
                .take(limit)
                .cloned()
                .collect();
        }
        let start = ring.len().saturating_sub(limit);
        ring.iter().skip(start).cloned().collect()
    }

    /// Total number of events produced so far (including evicted history)
    pub fn count(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed)
    }

    /// Writes to the ring buffer, dropping the oldest entry when over capacity
    fn push_ring(&self, event: LogEvent) {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if ring.len() >= self.capacity {
            ring.pop_front();
        }
        ring.push_back(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_then_recent_returns_chronological_newest() {
        let bus = LogBus::new(16);
        for i in 0..5 {
            bus.emit("info", "request", Some("r1"), format!("message {i}"));
        }
        let recent = bus.recent(3, None);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message, "message 2");
        assert_eq!(recent[2].message, "message 4");
        assert_eq!(recent[2].req_id.as_deref(), Some("r1"));
        assert_eq!(recent[2].kind, "request");
        assert_eq!(recent[2].level, "info");
        assert!(chrono::DateTime::parse_from_rfc3339(&recent[2].ts).is_ok());
    }

    #[test]
    fn capacity_evicts_oldest_events() {
        let bus = LogBus::new(3);
        for i in 0..6 {
            bus.emit("info", "tick", None, format!("e{i}"));
        }
        let all = bus.recent(100, None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].message, "e3");
        assert_eq!(all[2].message, "e5");
        assert_eq!(bus.count(), 6);
    }

    #[test]
    fn recent_after_id_filters_for_replay() {
        let bus = LogBus::new(32);
        for i in 0..6 {
            bus.emit("info", "tick", None, format!("e{i}"));
        }
        let replay = bus.recent(100, Some(3));
        assert_eq!(replay.len(), 3);
        assert_eq!(replay[0].id, 4);
        assert_eq!(replay[2].id, 6);
        // limit truncation: starts from the earliest unread event after 3
        let limited = bus.recent(2, Some(3));
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].id, 4);
        assert_eq!(limited[1].id, 5);
        // after_id beyond the latest id: nothing to replay
        assert!(bus.recent(10, Some(999)).is_empty());
    }

    #[test]
    fn ids_increase_monotonically() {
        let bus = LogBus::new(8);
        assert_eq!(bus.count(), 0);
        bus.emit("warn", "retry", None, "a");
        bus.emit("error", "account", Some("req-9"), "b");
        assert_eq!(bus.count(), 2);
        let all = bus.recent(10, None);
        assert_eq!(all[0].id, 1);
        assert_eq!(all[1].id, 2);
        assert_eq!(all[1].req_id.as_deref(), Some("req-9"));
    }

    #[tokio::test]
    async fn subscribe_receives_broadcast_events() {
        let bus = LogBus::new(8);
        let mut rx = bus.subscribe();
        bus.emit("info", "system", None, "online");
        let ev = match rx.recv().await {
            Ok(e) => e,
            Err(e) => panic!("should have received a broadcast event: {e}"),
        };
        assert_eq!(ev.message, "online");
        assert_eq!(ev.id, 1);
    }
}
