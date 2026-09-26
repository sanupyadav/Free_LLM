//! Telemetry writer: independent SQLite connection + background writer thread (WAL mode)
//!
//! - `requests_v2`: per-request detail (model, account, status, latency, tokens, errors, etc.)
//! - `events`: request lifecycle events
//! - Uses a separate connection and thread so it doesn't contend for locks with `usage.rs`; `record` uses `try_send`
//!   non-blocking, counted into [`TelemetryWriter::dropped`] when the queue is full
//! - [`TelemetryWriter::flush`] sends a barrier message and waits for writes to complete (used in tests/shutdown)
//! - `Drop` sends a shutdown signal and joins the thread, ensuring the SQLite handle is released on Windows

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Table creation statements (idempotent)
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS requests_v2 (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  req_id TEXT,
  ts TEXT NOT NULL,
  endpoint TEXT,
  requested_model TEXT,
  resolved_model TEXT,
  account TEXT,
  status INTEGER,
  latency_ms INTEGER,
  ttft_ms INTEGER,
  prompt_tokens INTEGER,
  completion_tokens INTEGER,
  stream INTEGER,
  error_kind TEXT,
  error_excerpt TEXT,
  route_reason TEXT,
  api_key TEXT,
  client_ip TEXT
);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  req_id TEXT,
  ts TEXT NOT NULL,
  kind TEXT NOT NULL,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_requests_v2_req ON requests_v2(req_id);
CREATE INDEX IF NOT EXISTS idx_events_req ON events(req_id);
"#;

/// Per-request telemetry detail
#[derive(Debug, Clone, Default)]
pub struct TraceRow {
    pub req_id: String,
    pub endpoint: String,
    pub requested_model: String,
    pub resolved_model: String,
    pub account: String,
    pub status: u16,
    pub latency_ms: u64,
    pub ttft_ms: Option<u64>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub stream: bool,
    pub error_kind: Option<String>,
    pub error_excerpt: Option<String>,
    pub route_reason: Option<String>,
    pub api_key: Option<String>,
    pub client_ip: Option<String>,
}

/// Background writer thread message
enum Msg {
    Row(Box<TraceRow>),
    Event {
        req_id: String,
        kind: String,
        detail: String,
    },
    /// Barrier: replies with ack as soon as received (all prior messages are already persisted)
    Flush(std::sync::mpsc::Sender<()>),
    Shutdown,
}

/// Telemetry writer (clone the shared sender to use it in multiple places)
pub struct TelemetryWriter {
    tx: SyncSender<Msg>,
    dropped: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl TelemetryWriter {
    /// Opens/creates the database and tables, and starts the background writer thread
    ///
    /// Table creation happens on the calling thread, so a successful return guarantees the tables already exist.
    pub fn spawn(db_path: PathBuf, capacity: usize) -> Result<Self> {
        let conn = open_db(&db_path)?;
        let (tx, rx) = sync_channel::<Msg>(capacity.clamp(1, 1 << 20));
        let dropped = Arc::new(AtomicU64::new(0));
        let handle = std::thread::Builder::new()
            .name("telemetry-writer".into())
            .spawn(move || writer_loop(conn, rx))
            .context("failed to start telemetry writer thread")?;
        Ok(Self {
            tx,
            dropped,
            handle: Some(handle),
        })
    }

    /// Records a request detail (non-blocking; dropped and counted if the queue is full)
    pub fn record(&self, row: TraceRow) {
        self.enqueue(Msg::Row(Box::new(row)));
    }

    /// Records a request event
    pub fn event(&self, req_id: &str, kind: &str, detail: &str) {
        self.enqueue(Msg::Event {
            req_id: req_id.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
        });
    }

    /// Number of messages dropped due to a full queue / thread exit
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Waits for the queue to drain (sends a barrier and waits for the writer thread to confirm)
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        if self.tx.send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }

    /// Non-blocking enqueue; counted into dropped on failure
    fn enqueue(&self, msg: Msg) {
        match self.tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for TelemetryWriter {
    fn drop(&mut self) {
        // Blocks and waits when the queue is full, to ensure the shutdown signal gets through; ignores the error if the thread has already exited
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Background thread main loop: consumes messages in order, warns on error without interrupting
/// Sensitive fields are redacted before persisting (Cookie/Bearer/authorization -> ***), see `crate::redact`.
fn writer_loop(conn: Connection, rx: Receiver<Msg>) {
    for msg in rx {
        match msg {
            Msg::Row(mut row) => {
                // v0.8 redaction: error excerpt / route reason / request key may all contain credential fragments
                if let Some(ex) = row.error_excerpt.take() {
                    row.error_excerpt = Some(crate::redact::redact(&ex));
                }
                if let Some(rr) = row.route_reason.take() {
                    row.route_reason = Some(crate::redact::redact(&rr));
                }
                if let Some(k) = row.api_key.take() {
                    row.api_key = Some(crate::redact::redact(&k));
                }
                if let Err(e) = insert_row(&conn, &row) {
                    tracing::warn!(error = %e, "failed to write telemetry detail");
                }
            }
            Msg::Event {
                req_id,
                kind,
                detail,
            } => {
                let detail = crate::redact::redact(&detail);
                if let Err(e) = insert_event(&conn, &req_id, &kind, &detail) {
                    tracing::warn!(error = %e, "failed to write telemetry event");
                }
            }
            Msg::Flush(ack) => {
                let _ = ack.send(());
            }
            Msg::Shutdown => break,
        }
    }
}

/// Opens the database: creates the directory, enables WAL, creates tables
fn open_db(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create telemetry directory: {}", dir.display()))?;
        }
    }
    let conn =
        Connection::open(path).with_context(|| format!("failed to open telemetry database: {}", path.display()))?;
    // Audit L1: sets busy_timeout to avoid SQLITE_BUSY from momentary lock contention with the background writer thread
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    // journal_mode returns a row, so it must be read with query_row
    let _mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .context("failed to enable WAL")?;
    conn.execute_batch(SCHEMA).context("failed to initialize telemetry tables")?;
    Ok(conn)
}

fn insert_row(conn: &Connection, row: &TraceRow) -> Result<()> {
    conn.execute(
        "INSERT INTO requests_v2 (
            req_id, ts, endpoint, requested_model, resolved_model, account, status,
            latency_ms, ttft_ms, prompt_tokens, completion_tokens, stream,
            error_kind, error_excerpt, route_reason, api_key, client_ip
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            row.req_id,
            Utc::now().to_rfc3339(),
            row.endpoint,
            row.requested_model,
            row.resolved_model,
            row.account,
            row.status as i64,
            row.latency_ms as i64,
            row.ttft_ms.map(|v| v as i64),
            row.prompt_tokens as i64,
            row.completion_tokens as i64,
            i64::from(row.stream),
            row.error_kind,
            row.error_excerpt,
            row.route_reason,
            row.api_key,
            row.client_ip
        ],
    )
    .context("failed to write to requests_v2")?;
    Ok(())
}

fn insert_event(conn: &Connection, req_id: &str, kind: &str, detail: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO events (req_id, ts, kind, detail) VALUES (?1,?2,?3,?4)",
        params![req_id, Utc::now().to_rfc3339(), kind, detail],
    )
    .context("failed to write to events")?;
    Ok(())
}

/// "Top 3" telemetry aggregation: slowest accounts Top3, most-used models Top5, highest-error-rate hours Top3.
///
/// - window_hours: when >0, only counts the last N hours; otherwise unbounded
/// - All aggregation is done locally in SQLite; an empty database returns empty arrays, no error
/// - account empty/NULL falls back to "unknown"; an error is defined as status >= 400 (covers 5xx)
/// - Slow accounts are sorted descending by mean total latency (latency_ms), ties broken descending by mean TTFT (ttft_ms),
///   and the mean TTFT is also returned (per guideline 2.3 "mean TTFT" definition)
pub fn insights(db_path: &str, hours: i64) -> Result<serde_json::Value> {
    let conn = open_db(Path::new(db_path))?;
    let generated_at = Utc::now();
    let cutoff = if hours > 0 {
        (generated_at - chrono::Duration::hours(hours)).to_rfc3339()
    } else {
        "1970-01-01T00:00:00Z".to_string()
    };

    // 1) Slowest accounts Top3
    let mut slowest_stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(TRIM(account), ''), 'unknown') AS acct,
                COUNT(*), AVG(latency_ms), AVG(ttft_ms)
         FROM requests_v2 WHERE ts >= ?1
         GROUP BY COALESCE(NULLIF(TRIM(account), ''), 'unknown')",
    )?;
    let mut slowest: Vec<(String, i64, f64, Option<f64>)> = slowest_stmt
        .query_map(params![cutoff], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, Option<f64>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    slowest.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.3.unwrap_or(0.0)
                    .partial_cmp(&a.3.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    slowest.truncate(3);
    let slowest_accounts: Vec<serde_json::Value> = slowest
        .into_iter()
        .map(|(acct, n, total, ttft)| {
            serde_json::json!({
                "account": acct,
                "requests": n,
                "avg_total_ms": (total * 10.0).round() / 10.0,
                "avg_first_byte_ms": (ttft.unwrap_or(0.0) * 10.0).round() / 10.0,
            })
        })
        .collect();
    // 2) Most-used models Top5 (descending request count + error rate)
    let mut model_stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(TRIM(resolved_model), ''), NULLIF(TRIM(requested_model), ''), 'unknown') AS model,
                COUNT(*), SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END)
         FROM requests_v2 WHERE ts >= ?1
         GROUP BY COALESCE(NULLIF(TRIM(resolved_model), ''), NULLIF(TRIM(requested_model), ''), 'unknown')",
    )?;
    let mut models: Vec<(String, i64, i64)> = model_stmt
        .query_map(params![cutoff], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    models.sort_by_key(|x| std::cmp::Reverse(x.1));
    models.truncate(5);
    let top_models: Vec<serde_json::Value> = models
        .into_iter()
        .map(|(model, n, errs)| {
            serde_json::json!({
                "model": model,
                "requests": n,
                "error_rate": if n > 0 { round_rate(errs as f64 / n as f64) } else { 0.0 },
            })
        })
        .collect();

    // 3) Highest-error-rate hours Top3 (by UTC hour)
    let mut hour_stmt = conn.prepare(
        "SELECT substr(ts, 1, 13) AS hour_utc, COUNT(*),
                SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END)
         FROM requests_v2 WHERE ts >= ?1
         GROUP BY substr(ts, 1, 13)",
    )?;
    let mut hour_rows: Vec<(String, i64, i64)> = hour_stmt
        .query_map(params![cutoff], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    hour_rows.sort_by(|a, b| {
        let ra = if a.1 > 0 {
            a.2 as f64 / a.1 as f64
        } else {
            0.0
        };
        let rb = if b.1 > 0 {
            b.2 as f64 / b.1 as f64
        } else {
            0.0
        };
        rb.partial_cmp(&ra)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.cmp(&a.1))
    });
    hour_rows.truncate(3);
    let worst_hours: Vec<serde_json::Value> = hour_rows
        .into_iter()
        .map(|(hour, n, errs)| {
            serde_json::json!({
                "hour_utc": hour,
                "requests": n,
                "error_rate": if n > 0 { round_rate(errs as f64 / n as f64) } else { 0.0 },
            })
        })
        .collect();

    Ok(serde_json::json!({
        "window_hours": hours,
        "slowest_accounts": slowest_accounts,
        "top_models": top_models,
        "worst_hours": worst_hours,
        "generated_at": generated_at.to_rfc3339(),
    }))
}

/// Error rate normalized to 4 decimal places, to avoid floating-point tail differences
fn round_rate(rate: f64) -> f64 {
    (rate * 10_000.0).round() / 10_000.0
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel as test_channel;

    fn sample_row(req_id: &str) -> TraceRow {
        TraceRow {
            req_id: req_id.to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            requested_model: "gpt-4o".to_string(),
            resolved_model: "claude-sonnet-5".to_string(),
            account: "token-1".to_string(),
            status: 200,
            latency_ms: 1234,
            ttft_ms: Some(321),
            prompt_tokens: 100,
            completion_tokens: 200,
            stream: true,
            error_kind: Some("timeout".to_string()),
            error_excerpt: Some("upstream timed out".to_string()),
            route_reason: Some("fallback".to_string()),
            api_key: Some("sk-test".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
        }
    }

    #[test]
    fn record_then_flush_persists_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let writer = TelemetryWriter::spawn(path.clone(), 64).unwrap();
        writer.record(sample_row("req-1"));
        writer.record(TraceRow {
            req_id: "req-2".to_string(),
            status: 500,
            ttft_ms: None,
            stream: false,
            ..Default::default()
        });
        writer.flush();

        {
            let conn = Connection::open(&path).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM requests_v2", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 2);
            struct Persisted {
                status: i64,
                latency: i64,
                ttft: Option<i64>,
                stream: i64,
                err_kind: Option<String>,
                ip: Option<String>,
                resolved: Option<String>,
            }
            let p: Persisted = conn
                .query_row(
                    "SELECT status,latency_ms,ttft_ms,stream,error_kind,client_ip,resolved_model
                     FROM requests_v2 WHERE req_id='req-1'",
                    [],
                    |r| {
                        Ok(Persisted {
                            status: r.get(0)?,
                            latency: r.get(1)?,
                            ttft: r.get(2)?,
                            stream: r.get(3)?,
                            err_kind: r.get(4)?,
                            ip: r.get(5)?,
                            resolved: r.get(6)?,
                        })
                    },
                )
                .unwrap();
            assert_eq!(p.status, 200);
            assert_eq!(p.latency, 1234);
            assert_eq!(p.ttft, Some(321));
            assert_eq!(p.stream, 1);
            assert_eq!(p.err_kind.as_deref(), Some("timeout"));
            assert_eq!(p.ip.as_deref(), Some("127.0.0.1"));
            assert_eq!(p.resolved.as_deref(), Some("claude-sonnet-5"));
        }
        drop(writer);
    }

    #[test]
    fn event_then_flush_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let writer = TelemetryWriter::spawn(path.clone(), 16).unwrap();
        writer.event("req-7", "retry", "2nd attempt");
        writer.flush();
        {
            let conn = Connection::open(&path).unwrap();
            let (req_id, kind, detail): (String, String, String) = conn
                .query_row("SELECT req_id,kind,detail FROM events", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap();
            assert_eq!(req_id, "req-7");
            assert_eq!(kind, "retry");
            assert_eq!(detail, "2nd attempt");
        }
        drop(writer);
    }

    #[test]
    fn enqueue_counts_dropped_when_full() {
        // Internal enqueue logic: fill a capacity-1 channel first, so the next enqueue must count into dropped
        let (tx, rx) = test_channel::<Msg>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        tx.try_send(Msg::Shutdown).unwrap();
        let writer = TelemetryWriter {
            tx,
            dropped: dropped.clone(),
            handle: None,
        };
        writer.record(sample_row("a"));
        writer.record(sample_row("b"));
        assert_eq!(writer.dropped(), 2);
        // Disconnect the receiver first, to avoid the blocking send of Shutdown in Drop waiting forever
        drop(rx);
        drop(writer);
    }

    #[test]
    fn dropped_and_persisted_conserve_total_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        // Tiny capacity + instant flood, triggering drops; total is conserved: persisted + dropped == total
        let writer = TelemetryWriter::spawn(path.clone(), 1).unwrap();
        let total = 300u64;
        for i in 0..total {
            writer.record(TraceRow {
                req_id: format!("req-{i}"),
                ..Default::default()
            });
        }
        writer.flush();
        let stored: i64 = {
            let conn = Connection::open(&path).unwrap();
            conn.query_row("SELECT COUNT(*) FROM requests_v2", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(stored as u64 + writer.dropped(), total);
        drop(writer);
    }

    #[test]
    fn spawn_twice_same_path_reuses_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        {
            let writer = TelemetryWriter::spawn(path.clone(), 8).unwrap();
            writer.record(sample_row("first"));
            writer.flush();
        }
        // Second spawn: tables already exist, should be idempotently reused
        let writer = TelemetryWriter::spawn(path.clone(), 8).unwrap();
        writer.record(sample_row("second"));
        writer.flush();
        {
            let conn = Connection::open(&path).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM requests_v2", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 2);
        }
        drop(writer);
    }

    #[test]
    fn drop_releases_sqlite_files_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let writer = TelemetryWriter::spawn(path.clone(), 8).unwrap();
        writer.record(sample_row("r"));
        drop(writer);
        // Handle already released: on Windows the whole directory should be directly removable
        let removed = std::fs::remove_dir_all(dir.path());
        assert!(removed.is_ok(), "SQLite handle not released: {removed:?}");
    }

    /// Directly inserts a request with a known timestamp (bypassing the writer thread, for building window/hour samples)
    #[allow(clippy::too_many_arguments)] // test constructor: many fields, clarity over brevity
    fn insert_direct(
        conn: &Connection,
        req_id: &str,
        ts: &str,
        account: &str,
        model: &str,
        status: i64,
        latency_ms: i64,
        ttft_ms: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO requests_v2 (req_id, ts, requested_model, resolved_model, account, status, latency_ms, ttft_ms)
             VALUES (?1,?2,?3,?3,?4,?5,?6,?7)",
            params![req_id, ts, model, account, status, latency_ms, ttft_ms],
        )
        .unwrap();
    }

    #[test]
    fn insights_slowest_accounts_ranking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let now = Utc::now().to_rfc3339();
        insert_direct(
            &conn,
            "r1",
            &now,
            "acct-slow",
            "gpt-4o",
            200,
            1000,
            Some(400),
        );
        insert_direct(
            &conn,
            "r2",
            &now,
            "acct-slow",
            "gpt-4o",
            200,
            800,
            Some(300),
        );
        insert_direct(&conn, "r3", &now, "acct-mid", "gpt-4o", 200, 500, Some(200));
        insert_direct(&conn, "r4", &now, "acct-fast", "claude", 200, 100, Some(50));
        insert_direct(&conn, "r5", &now, "acct-fast", "claude", 200, 100, Some(60));
        insert_direct(&conn, "r6", &now, "acct-fast", "claude", 200, 100, Some(70));

        let v = insights(path.to_str().unwrap(), 24).unwrap();
        let arr = v["slowest_accounts"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        let accounts: Vec<String> = arr
            .iter()
            .map(|x| x["account"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(accounts, vec!["acct-slow", "acct-mid", "acct-fast"]);
        assert_eq!(arr[0]["requests"], 2);
        assert_eq!(arr[0]["avg_total_ms"], 900.0);
        assert_eq!(arr[0]["avg_first_byte_ms"], 350.0);
        assert_eq!(arr[1]["avg_total_ms"], 500.0);
        assert_eq!(arr[2]["avg_total_ms"], 100.0);
    }

    #[test]
    fn insights_top_models_counts_and_error_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let now = Utc::now().to_rfc3339();
        insert_direct(&conn, "a", &now, "acct", "gpt-4o", 200, 100, Some(10));
        insert_direct(&conn, "b", &now, "acct", "gpt-4o", 400, 100, Some(10));
        insert_direct(&conn, "c", &now, "acct", "gpt-4o", 500, 100, Some(10));
        insert_direct(&conn, "d", &now, "acct", "gpt-4o", 200, 100, Some(10));
        insert_direct(&conn, "e", &now, "acct", "claude", 200, 100, Some(10));
        insert_direct(&conn, "f", &now, "acct", "claude", 200, 100, Some(10));

        let v = insights(path.to_str().unwrap(), 24).unwrap();
        let arr = v["top_models"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["model"], "gpt-4o");
        assert_eq!(arr[0]["requests"], 4);
        assert_eq!(arr[0]["error_rate"], 0.5);
        assert_eq!(arr[1]["model"], "claude");
        assert_eq!(arr[1]["requests"], 2);
        assert_eq!(arr[1]["error_rate"], 0.0);
    }
    #[test]
    fn insights_worst_hours_ranking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let h1 = "2026-09-19T08:00:00.000Z";
        let h2 = "2026-09-19T09:00:00.000Z";
        insert_direct(&conn, "a", h1, "acct", "gpt-4o", 500, 100, Some(10));
        insert_direct(&conn, "b", h1, "acct", "gpt-4o", 200, 100, Some(10));
        insert_direct(&conn, "c", h1, "acct", "gpt-4o", 200, 100, Some(10));
        insert_direct(&conn, "d", h1, "acct", "gpt-4o", 200, 100, Some(10));
        insert_direct(&conn, "e", h2, "acct", "claude", 200, 100, Some(10));
        insert_direct(&conn, "f", h2, "acct", "claude", 200, 100, Some(10));

        // window 0 = unbounded: the fixed timestamps above would age out of a 24h window
        let v = insights(path.to_str().unwrap(), 0).unwrap();
        let arr = v["worst_hours"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["hour_utc"], "2026-09-19T08");
        assert_eq!(arr[0]["requests"], 4);
        assert_eq!(arr[0]["error_rate"], 0.25);
        assert_eq!(arr[1]["hour_utc"], "2026-09-19T09");
        assert_eq!(arr[1]["error_rate"], 0.0);
    }

    #[test]
    fn insights_empty_db_returns_empty_arrays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry-empty.db");
        let v = insights(path.to_str().unwrap(), 24).unwrap();
        assert!(v["slowest_accounts"].as_array().unwrap().is_empty());
        assert!(v["top_models"].as_array().unwrap().is_empty());
        assert!(v["worst_hours"].as_array().unwrap().is_empty());
        assert_eq!(v["window_hours"], 24);
    }

    #[test]
    fn insights_window_filters_and_unknown_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let old = (Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        let now = Utc::now().to_rfc3339();
        insert_direct(&conn, "a", &old, "acct-old", "gpt-4o", 200, 100, Some(10));
        insert_direct(&conn, "b", &now, "", "claude", 500, 200, Some(20));

        let v = insights(path.to_str().unwrap(), 24).unwrap();
        assert_eq!(v["slowest_accounts"].as_array().unwrap().len(), 1);
        assert_eq!(v["slowest_accounts"][0]["account"], "unknown");
        let models = v["top_models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["model"], "claude");
        assert_eq!(models[0]["error_rate"], 1.0);
    }
}
