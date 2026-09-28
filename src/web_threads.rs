//! "Credential -> upstream session" mapping used by the Web protocol bridge.
//!
//! Why it's needed: is upstream's free quota given per **session (thread)**? No -- it's given per
//! **admission (session admission)** per model per day (`rateLimitsByModel[m].limit` defaults to 6/day,
//! `recentCount` tracks usage). Continuing a conversation in the same thread doesn't consume a new
//! admission; **creating a new thread per request will burn through the daily quota fast**.
//!
//! So the bridge layer that connects OpenAI/Anthropic clients in must **reuse the same upstream thread
//! as much as possible**: send the full context to open a new session on the first turn, then send only
//! the latest user message and reuse the same thread on subsequent turns.
//!
//! Persisted to: `data/web_threads.json` (`{ "<cred_id>": { "thread_id": "...", "last_user_text": "..." } }`).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The upstream session currently in use for a given credential
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadBinding {
    #[serde(default)]
    pub thread_id: String,
    /// The last user text sent out; used to determine whether the client's current request is "a new turn" or "a retry of the same turn"
    #[serde(default)]
    pub last_user_text: String,
    #[serde(default)]
    pub turns: u64,
    /// Timestamp of the most recent binding (epoch seconds). Used for TTL cleanup and capacity trimming.
    #[serde(default)]
    pub last_seen_sec: i64,
}

/// Max number of entries in the binding table (oldest trimmed first once exceeded)
pub const MAX_BINDINGS: usize = 2000;
/// Max lifetime of a binding entry (seconds); cleared on expiry (the upstream thread also gets reclaimed by global cleanup)
pub const BINDING_TTL_SECS: i64 = 24 * 3600;

pub struct WebThreadMap {
    path: PathBuf,
    cache: Mutex<HashMap<String, ThreadBinding>>,
}

impl WebThreadMap {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let cache = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, ThreadBinding>>(&t).ok())
            .unwrap_or_default();
        let this = Self {
            path,
            cache: Mutex::new(cache),
        };
        // Trim once on load (old data may already be over the limit)
        let _ = this.prune(None);
        this
    }

    pub fn get(&self, cred_id: &str) -> Option<ThreadBinding> {
        self.cache.lock().ok().and_then(|c| c.get(cred_id).cloned())
    }

    /// Record a session binding (first turn gets a threadId, or subsequent turns refresh last_user_text)
    /// Record a session binding (first turn gets a threadId, or subsequent turns refresh last_user_text).
    /// The snapshot clone and the update happen in **the same critical section**, eliminating the window
    /// where a snapshot cloned outside the lock could overwrite a newer binding on write.
    pub fn bind(&self, cred_id: &str, thread_id: &str, last_user_text: &str) -> Result<()> {
        let json = {
            let mut c = self
                .cache
                .lock()
                .map_err(|_| anyhow::anyhow!("web_threads lock poisoned"))?;
            // turns counts "number of text changes - 1" (the first bind is the initial turn, not a continuation)
            let was_new = c.get(cred_id).is_none();
            let e = c.entry(cred_id.to_string()).or_default();
            e.thread_id = thread_id.to_string();
            if !was_new && e.last_user_text != last_user_text {
                e.turns = e.turns.saturating_add(1);
            }
            e.last_user_text = last_user_text.to_string();
            e.last_seen_sec = unix_now_sec();
            self.prune_locked(&mut c, None);
            serde_json::to_string_pretty(&*c)?
        };
        self.atomic_write(&json)
    }

    /// Clear the binding when the session is invalid (deleted upstream / 404 / error), so a new one is opened next time
    pub fn clear(&self, cred_id: &str) -> Result<()> {
        let json = {
            let mut c = self
                .cache
                .lock()
                .map_err(|_| anyhow::anyhow!("web_threads lock poisoned"))?;
            c.remove(cred_id);
            serde_json::to_string_pretty(&*c)?
        };
        self.atomic_write(&json)
    }

    /// Trigger a cleanup pass: remove TTL-expired entries + trim the oldest entries by capacity (if len exceeds limit).
    /// Returns the number of credential ids removed. `limit=None` uses the default `MAX_BINDINGS`.
    pub fn prune(&self, limit: Option<usize>) -> Result<usize> {
        let (json, removed) = {
            let mut c = self
                .cache
                .lock()
                .map_err(|_| anyhow::anyhow!("web_threads lock poisoned"))?;
            let removed = self.prune_locked(&mut c, limit);
            if removed > 0 {
                (serde_json::to_string_pretty(&*c)?, removed)
            } else {
                (String::new(), 0)
            }
        };
        if json.is_empty() {
            Ok(0)
        } else {
            self.atomic_write(&json)?;
            Ok(removed)
        }
    }

    /// Internal trim (caller must already hold the lock): truncate by `limit` + remove TTL-expired entries, returns count removed
    fn prune_locked(&self, c: &mut HashMap<String, ThreadBinding>, limit: Option<usize>) -> usize {
        let now = unix_now_sec();
        let limit = limit.unwrap_or(MAX_BINDINGS);
        let mut removed = 0usize;
        // 1) Remove TTL-expired entries (runs regardless of whether the limit is exceeded)
        let before_ttl = c.len();
        c.retain(|_, e| now.saturating_sub(e.last_seen_sec) < BINDING_TTL_SECS);
        removed += before_ttl - c.len();
        // 2) Capacity trim: if still over the limit, remove the oldest (ascending by last_seen_sec)
        if c.len() > limit {
            let over = c.len() - limit;
            let mut entries: Vec<(String, i64)> = c
                .iter()
                .map(|(k, v)| (k.clone(), v.last_seen_sec))
                .collect();
            entries.sort_by_key(|(_, t)| *t);
            for (k, _) in entries.into_iter().take(over) {
                c.remove(&k);
            }
            removed += over;
        }
        removed
    }

    /// Atomic replace write (temp file + rename), prevents a truncated file being left behind by a mid-write crash
    fn atomic_write(&self, json: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        #[cfg(windows)]
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Current unix seconds
fn unix_now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Flatten OpenAI-style `messages` into the single `content` string the upstream web protocol expects.
///
/// Returns `(prompt, last user message)`: the latter is used to determine whether the upstream thread
/// can be reused by sending only the increment.
/// `only_last_user=true` takes only the last user message (used for continuation turns that reuse a session).
pub fn flatten_messages(
    messages: &serde_json::Value,
    only_last_user: bool,
) -> Option<(String, String)> {
    let arr = messages.as_array()?;
    let text_of = |m: &serde_json::Value| -> Option<String> {
        let t = match m.get("content")? {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(|it| {
                    // Compatible with Anthropic-style content blocks (text field) and OpenAI multimodal (type=text)
                    it.get("text").and_then(|x| x.as_str()).map(String::from)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => return None,
        };
        let t = t.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    };

    let last_user = arr
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(&text_of)?;

    if only_last_user {
        return Some((last_user.clone(), last_user));
    }

    let mut parts: Vec<String> = Vec::new();
    let mut systems: Vec<String> = Vec::new();
    let non_system: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|m| {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            if role == "system" || role == "developer" {
                if let Some(t) = text_of(m) {
                    systems.push(t);
                }
                false
            } else {
                true
            }
        })
        .collect();

    if let Some(sys) = systems.first() {
        parts.push(format!("[System instructions]\n{sys}"));
    }
    // Only one non-system message (most common case: the client only sends the current message) -> no heading, sent verbatim
    let rest: Vec<String> = non_system.iter().filter_map(|m| text_of(m)).collect();
    if rest.len() == 1 {
        parts.push(rest[0].clone());
    } else {
        for m in &non_system {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let Some(t) = text_of(m) else { continue };
            let label = match role {
                "assistant" => "[Assistant]",
                "tool" => "[Tool result]",
                _ => "[User]",
            };
            parts.push(format!("{label}\n{t}"));
        }
    }
    // Too long for upstream's per-message limit: drop whole oldest turns (keep system + latest) rather
    // than cutting mid-message; fit_web_limit still guards a single oversized message.
    let first_turn = usize::from(!systems.is_empty());
    let size = |ps: &[String]| ps.iter().map(|p| p.chars().count() + 2).sum::<usize>();
    let mut dropped = 0;
    while parts.len() > first_turn + 1 && size(&parts) > crate::web_protocol::WEB_CONTENT_MAX_CHARS {
        parts.remove(first_turn);
        dropped += 1;
    }
    if dropped > 0 {
        parts.insert(first_turn, format!("[{dropped} earlier messages omitted]"));
    }
    let prompt = parts.join("\n\n");
    if prompt.trim().is_empty() {
        None
    } else {
        Some((prompt, last_user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_chat_drops_oldest_turns_keeps_system_and_latest() {
        let big = "x".repeat(12_000);
        let msgs = serde_json::json!([
            {"role":"system","content":"SYS"},
            {"role":"user","content":format!("old1 {big}")},
            {"role":"assistant","content":format!("old2 {big}")},
            {"role":"user","content":format!("mid {big}")},
            {"role":"user","content":"LATEST"},
        ]);
        let (p, _) = flatten_messages(&msgs, false).unwrap();
        assert!(p.chars().count() <= crate::web_protocol::WEB_CONTENT_MAX_CHARS);
        assert!(p.contains("SYS") && p.ends_with("LATEST"));
        assert!(!p.contains("old1") && p.contains("earlier messages omitted"));
    }

    fn msgs(v: serde_json::Value) -> serde_json::Value {
        v
    }

    #[test]
    fn single_user_message_is_passed_verbatim() {
        let m = msgs(serde_json::json!([{ "role": "user", "content": "hello" }]));
        let (p, last) = flatten_messages(&m, false).unwrap();
        assert_eq!(p, "hello");
        assert_eq!(last, "hello");
    }

    #[test]
    fn system_prompt_and_single_user_keeps_system_prefix() {
        let m = msgs(serde_json::json!([
            { "role": "system", "content": "you are an assistant" },
            { "role": "user", "content": "write a poem" }
        ]));
        let (p, _) = flatten_messages(&m, false).unwrap();
        assert!(p.starts_with("[System instructions]\nyou are an assistant"));
        assert!(p.ends_with("write a poem"));
    }

    #[test]
    fn multi_turn_is_labelled_transcript() {
        let m = msgs(serde_json::json!([
            { "role": "user", "content": "1+1" },
            { "role": "assistant", "content": "2" },
            { "role": "user", "content": "add one more" }
        ]));
        let (p, last) = flatten_messages(&m, false).unwrap();
        assert!(p.contains("[User]\n1+1"));
        assert!(p.contains("[Assistant]\n2"));
        assert!(p.ends_with("[User]\nadd one more"));
        assert_eq!(
            last, "add one more",
            "the last user message must be extractable on its own (continuation only sends it)"
        );
    }

    #[test]
    fn only_last_user_returns_just_that() {
        let m = msgs(serde_json::json!([
            { "role": "user", "content": "old question" },
            { "role": "assistant", "content": "old answer" },
            { "role": "user", "content": "new question" }
        ]));
        let (p, last) = flatten_messages(&m, true).unwrap();
        assert_eq!(p, "new question");
        assert_eq!(last, "new question");
    }

    #[test]
    fn anthropic_style_blocks_are_supported() {
        let m = msgs(serde_json::json!([
            { "role": "user", "content": [{ "type": "text", "text": "block content" }] }
        ]));
        assert_eq!(flatten_messages(&m, false).unwrap().0, "block content");
    }

    #[test]
    fn empty_or_missing_content_yields_none() {
        assert!(flatten_messages(&serde_json::json!([]), false).is_none());
        assert!(flatten_messages(
            &serde_json::json!([{ "role": "assistant", "content": "assistant only" }]),
            false
        )
        .is_none());
        assert!(flatten_messages(
            &serde_json::json!([{ "role": "user", "content": "   " }]),
            false
        )
        .is_none());
    }

    #[test]
    fn map_persists_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web_threads.json");
        let map = WebThreadMap::new(&path);
        map.bind("cred1", "t-1", "hello").unwrap();
        assert_eq!(map.get("cred1").unwrap().thread_id, "t-1");
        // Continuing the same turn doesn't count a repeat turn (same thread + same text)
        map.bind("cred1", "t-1", "hello").unwrap();
        assert_eq!(map.get("cred1").unwrap().turns, 0);
        // New text (even with the same thread) counts as a new turn
        map.bind("cred1", "t-1", "next").unwrap();
        assert_eq!(
            map.get("cred1").unwrap().turns,
            1,
            "a text change must count as a new turn"
        );

        let reopened = WebThreadMap::new(&path);
        assert_eq!(reopened.get("cred1").unwrap().last_user_text, "next");

        reopened.clear("cred1").unwrap();
        assert!(reopened.get("cred1").is_none());
    }

    #[test]
    fn bind_records_last_seen_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web_threads.json");
        let map = WebThreadMap::new(&path);
        map.bind("cred1", "t-1", "hello").unwrap();
        let b = map.get("cred1").unwrap();
        assert!(b.last_seen_sec > 0, "a binding must record a timestamp");
        assert!(b.last_seen_sec <= unix_now_sec());
    }

    #[test]
    fn prune_caps_at_max_bindings_and_evicts_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web_threads.json");
        let map = WebThreadMap::new(&path);
        // Use a small capacity limit to trigger trimming (avoid actually writing 2000 entries -- construct an equivalent condition)
        let small_limit = 10usize;
        for i in 0..(small_limit + 5) {
            map.bind(&format!("cred-{i}"), &format!("t-{i}"), "x")
                .unwrap();
        }
        // Manually check the large-capacity semantics: injecting 2001 entries evicts the oldest (use a small limit to simulate the 2000 cap's behavior)
        let removed = {
            let mut c = map.cache.lock().unwrap();
            // Inject entries beyond the limit
            for i in 0..25 {
                c.insert(
                    format!("extra-{i}"),
                    ThreadBinding {
                        thread_id: format!("te-{i}"),
                        last_user_text: String::new(),
                        turns: 0,
                        last_seen_sec: unix_now_sec() - (25 - i) as i64, // smaller index = older
                    },
                );
            }
            map.prune_locked(&mut c, Some(small_limit))
        };
        assert!(removed >= 20, "entries over capacity should be trimmed, actually removed {removed}");
        let size = map.cache.lock().unwrap().len();
        assert!(size <= small_limit, "should not exceed the limit after trimming, actual {size}");
    }

    #[test]
    fn prune_removes_expired_ttl_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web_threads.json");
        let map = WebThreadMap::new(&path);
        map.bind("fresh", "t-1", "x").unwrap();
        {
            let mut c = map.cache.lock().unwrap();
            // Fake an already-expired entry (25h ago)
            c.insert(
                "stale".into(),
                ThreadBinding {
                    thread_id: "t-old".into(),
                    last_user_text: String::new(),
                    turns: 0,
                    last_seen_sec: unix_now_sec() - BINDING_TTL_SECS - 3600,
                },
            );
        }
        let removed = map.prune(None).unwrap();
        assert!(removed >= 1, "expired entries should be cleared by TTL, actually removed {removed}");
        assert!(map.get("stale").is_none());
        assert!(map.get("fresh").is_some(), "non-expired entries should be kept");
    }
}
