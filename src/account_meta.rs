//! Credential account info cache + account usage history (local disk, zero new deps)
//!
//! Two things:
//! 1. `AccountMetaStore` -- the latest full account snapshot per credential (nickname/email/plan/today's remaining...),
//!    so the "credential list" can show account details without hitting upstream for each one.
//! 2. **Usage history** (user note: "of course you also need queryable records per account") -- every successful
//!    fetch appends one JSONL history entry, queryable by credential to see trends.
//!
//! On disk: `data/cred_meta.json` (overwrite) + `data/account_history.jsonl` (append).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// A single credential's account info snapshot (all fields nullable -- if any upstream endpoint fails, it degrades without affecting other fields)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredMeta {
    #[serde(default)]
    pub cred_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    /// Login session expiry (upstream auth/session's `expires`)
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default)]
    pub access_tier: Option<String>,
    /// starter / plus / pro; None = free tier
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub daily_limit: Option<i64>,
    #[serde(default)]
    pub daily_spent: Option<i64>,
    #[serde(default)]
    pub daily_remaining: Option<i64>,
    #[serde(default)]
    pub reset_at: Option<String>,
    #[serde(default)]
    pub streak_current: Option<i64>,
    #[serde(default)]
    pub all_time_active_days: Option<i64>,
    #[serde(default)]
    pub tokens_7d: Option<i64>,
    #[serde(default)]
    pub models: Vec<ModelQuota>,
    #[serde(default)]
    pub country_code: Option<String>,
    /// Non-empty means region-restricted (e.g. country_not_allowed)
    #[serde(default)]
    pub country_block_reason: Option<String>,
    /// Whether the most recent fetch succeeded
    #[serde(default)]
    pub valid: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub checked_at: String,
}

/// Today's quota per model
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelQuota {
    pub model: String,
    #[serde(default)]
    pub price: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub used: Option<i64>,
    #[serde(default)]
    pub remaining: Option<i64>,
    #[serde(default)]
    pub reset_at: Option<String>,
    #[serde(default)]
    pub pool_label: Option<String>,
}

/// A single usage record (for querying history per account)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub ts: String,
    pub cred_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub daily_limit: Option<i64>,
    #[serde(default)]
    pub daily_spent: Option<i64>,
    #[serde(default)]
    pub daily_remaining: Option<i64>,
    #[serde(default)]
    pub tokens_7d: Option<i64>,
    #[serde(default)]
    pub streak_current: Option<i64>,
    pub ok: bool,
}

/// Account info / usage history store (in-process cache + file persistence, Mutex-serialized writes)
pub struct AccountMetaStore {
    meta_path: PathBuf,
    history_path: PathBuf,
    cache: Mutex<HashMap<String, CredMeta>>,
}

/// Compact the history file once it exceeds this many bytes, keeping the latest `HISTORY_KEEP` entries
const HISTORY_MAX_BYTES: u64 = 2 * 1024 * 1024;
const HISTORY_KEEP: usize = 2000;

/// Serialization lock for the history JSONL: shared by append and compact, closing the window where a row could be lost between "compact reads" and "compact overwrites".
static HISTORY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Atomic replace write (temp file + rename), preventing a truncated file if the write crashes mid-way
fn atomic_write(path: &PathBuf, data: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    #[cfg(windows)]
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl AccountMetaStore {
    pub fn new(meta_path: impl Into<PathBuf>, history_path: impl Into<PathBuf>) -> Self {
        let meta_path = meta_path.into();
        let history_path = history_path.into();
        let cache = Self::load_meta(&meta_path);
        Self {
            meta_path,
            history_path,
            cache: Mutex::new(cache),
        }
    }

    fn load_meta(path: &PathBuf) -> HashMap<String, CredMeta> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, CredMeta>>(&t).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, cred_id: &str) -> Option<CredMeta> {
        self.cache.lock().ok().and_then(|c| c.get(cred_id).cloned())
    }

    pub fn all(&self) -> HashMap<String, CredMeta> {
        self.cache.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Insert/update a snapshot and persist it (on failure, just logs, without affecting the main flow)
    pub fn upsert(&self, meta: CredMeta) -> Result<()> {
        {
            let mut c = self
                .cache
                .lock()
                .map_err(|_| anyhow::anyhow!("meta cache lock poisoned"))?;
            c.insert(meta.cred_id.clone(), meta);
        }
        self.flush()
    }

    /// Remove a credential's snapshot (called when the credential is removed)
    pub fn remove(&self, cred_id: &str) -> Result<()> {
        {
            let mut c = self
                .cache
                .lock()
                .map_err(|_| anyhow::anyhow!("meta cache lock poisoned"))?;
            c.remove(cred_id);
        }
        self.flush()
    }

    /// Clone the snapshot while still holding the lock (in the same critical section as the update); write to disk via atomic replace --
    /// closing the window where releasing and re-acquiring the lock could clone a stale snapshot before writing.
    fn flush(&self) -> Result<()> {
        let json = {
            let c = self
                .cache
                .lock()
                .map_err(|_| anyhow::anyhow!("meta cache lock poisoned"))?;
            serde_json::to_string_pretty(&*c)?
        };
        atomic_write(&self.meta_path, &json)?;
        Ok(())
    }

    /// Append a usage record (JSONL); auto-compacts to keep only the latest N entries once the file grows too large
    pub fn append_history(&self, rec: &HistoryRecord) -> Result<()> {
        use std::io::Write;
        let line = serde_json::to_string(rec)?;
        let need_compact = {
            let _g = HISTORY_LOCK
                .lock()
                .map_err(|_| anyhow::anyhow!("history lock poisoned"))?;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.history_path)?;
            writeln!(f, "{line}")?;
            drop(f);
            std::fs::metadata(&self.history_path)
                .map(|m| m.len())
                .unwrap_or(0)
                > HISTORY_MAX_BYTES
        };
        if need_compact {
            self.compact_history();
        }
        Ok(())
    }

    /// Compact history (holds HISTORY_LOCK + temp file rename; an appended row won't be lost by an overwrite after being read)
    fn compact_history(&self) {
        let _g = match HISTORY_LOCK.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Ok(text) = std::fs::read_to_string(&self.history_path) else {
            return;
        };
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let keep: Vec<&str> = lines
            .iter()
            .rev()
            .take(HISTORY_KEEP)
            .rev()
            .copied()
            .collect();
        let mut out = keep.join("\n");
        out.push('\n');
        if atomic_write(&self.history_path, &out).is_ok() {
            tracing::info!("Usage history compacted to the latest {HISTORY_KEEP} entries");
        }
    }

    /// Read usage history (reverse chronological = newest first); returns all accounts if `cred_id` is empty
    pub fn history(&self, cred_id: Option<&str>, limit: usize) -> Result<Vec<HistoryRecord>> {
        let limit = limit.clamp(1, 1000);
        let text = match std::fs::read_to_string(&self.history_path) {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out: Vec<HistoryRecord> = text
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<HistoryRecord>(l).ok())
            .filter(|r| cred_id.map(|id| r.cred_id == id).unwrap_or(true))
            .take(limit)
            .collect();
        // Already reverse-ordered; keeps "newest first"
        out.shrink_to_fit();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (AccountMetaStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountMetaStore::new(
            dir.path().join("cred_meta.json"),
            dir.path().join("account_history.jsonl"),
        );
        (store, dir)
    }

    #[test]
    fn upsert_and_reload() {
        let (store, dir) = tmp_store();
        let meta = CredMeta {
            cred_id: "abc".into(),
            email: Some("a@b.c".into()),
            valid: true,
            checked_at: "t".into(),
            ..Default::default()
        };
        store.upsert(meta).unwrap();
        let reopened = AccountMetaStore::new(
            dir.path().join("cred_meta.json"),
            dir.path().join("h.jsonl"),
        );
        assert_eq!(reopened.get("abc").unwrap().email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn remove_meta() {
        let (store, _d) = tmp_store();
        store
            .upsert(CredMeta {
                cred_id: "x".into(),
                ..Default::default()
            })
            .unwrap();
        store.remove("x").unwrap();
        assert!(store.get("x").is_none());
    }

    #[test]
    fn history_is_newest_first_and_filterable() {
        let (store, _d) = tmp_store();
        for (i, id) in [("1", "a"), ("2", "b"), ("3", "a")] {
            store
                .append_history(&HistoryRecord {
                    ts: i.into(),
                    cred_id: id.into(),
                    name: None,
                    email: None,
                    tier_id: None,
                    daily_limit: None,
                    daily_spent: None,
                    daily_remaining: None,
                    tokens_7d: None,
                    streak_current: None,
                    ok: true,
                })
                .unwrap();
        }
        let all = store.history(None, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].ts, "3", "the newest record must come first");
        let only_a = store.history(Some("a"), 10).unwrap();
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|r| r.cred_id == "a"));
    }

    #[test]
    fn history_limit_is_clamped() {
        let (store, _d) = tmp_store();
        store
            .append_history(&HistoryRecord {
                ts: "1".into(),
                cred_id: "a".into(),
                name: None,
                email: None,
                tier_id: None,
                daily_limit: None,
                daily_spent: None,
                daily_remaining: None,
                tokens_7d: None,
                streak_current: None,
                ok: true,
            })
            .unwrap();
        assert_eq!(
            store.history(None, 0).unwrap().len(),
            1,
            "limit 0 should be clamped to 1"
        );
    }
}
