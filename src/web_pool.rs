//! web Cookie credential pool (v0.9): multi web-account health scoring + rotation + circuit breaking/cooldown
//!
//! Background: the Bearer account pool (`pool.rs`) only covers the desktop Bearer protocol; before
//! v0.8, web Cookie credentials just took "the first valid credential" (`pick_web_cookie`), with no
//! health score/rotation/cooldown, so one dead account would 401 the entire bridging path.
//!
//! This module **pools** web Cookie credentials: reuses `pool.rs::CircuitBreaker` semantics
//! (Closed/Open/HalfOpen, trip after consecutive failures, exponential cooldown capped at 10 minutes,
//! HalfOpen probe gate), and provides:
//! - `pick()`: picks the highest-health, non-tripped, cooldown-elapsed credential (multi-account rotation)
//! - `mark_ok()` / `mark_failure()` / `mark_cooldown()`: writes back request outcomes
//! - `snapshot()`: panel account health display
//!
//! Security boundary: a web Cookie is a user account's full session credential, kept only in local
//! memory and the local `tokens.json`, never sent out, never logged in plaintext (snapshots/panel
//! display are always redacted).

use crate::config::Config;
use crate::import;
use crate::pool::CircuitBreaker;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Credential source
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebCredSource {
    Config,
    Imported,
}

/// An entry in the pool (internal mutable state)
#[derive(Debug)]
struct WebEntry {
    cookie: String,
    id: String,
    source: WebCredSource,
    added_at: Option<String>,
    score: f64,
    breaker: CircuitBreaker,
    last_ok_at: Option<Instant>,
}

/// Pick result (caller only gets the cookie + display info + stable id, no internal state)
#[derive(Debug, Clone)]
pub struct WebPick {
    pub cookie: String,
    pub id: String,
    pub source: &'static str,
    pub added_at: Option<String>,
    /// Display info (source/added_at/token_masked/id), compatible with the old pick_web_cookie return
    pub cred: serde_json::Value,
}

/// Health snapshot (serializable, for the panel)
#[derive(Debug, Clone, serde::Serialize)]
pub struct WebCookieHealth {
    pub id: String,
    pub kind: &'static str,
    pub source: &'static str,
    pub added_at: Option<String>,
    /// Redacted cookie (first 6, last 4 chars)
    pub masked: String,
    pub health_score: f64,
    pub circuit_state: String,
    pub cooldown_until: Option<String>,
    /// v0.10: cooldown seconds remaining (for panel sorting/estimated recovery)
    pub cooldown_seconds: Option<u64>,
    pub trips: u64,
    pub last_error: Option<String>,
    pub last_ok_at: Option<String>,
}

/// Cookie detection consistent with the old `pick_web_cookie` (only recognizes the next-auth trio signature, to avoid misjudging URL-encoded strings)
pub fn looks_like_cookie(t: &str) -> bool {
    t.contains("session-token") || t.contains(".next-auth") || t.contains("callback-url")
}

fn mask(cookie: &str) -> String {
    if cookie.len() <= 10 {
        return "***".to_string();
    }
    format!("{}...{}", &cookie[..6], &cookie[cookie.len() - 4..])
}

/// Cooldown expiry instant -> ISO8601 UTC (v0.10: panel displays on a timeline; remaining seconds given separately via cooldown_seconds)
fn cooldown_iso(instant: Option<Instant>) -> Option<String> {
    instant.map(|i| {
        let d = i.saturating_duration_since(Instant::now());
        (chrono::Utc::now() + chrono::Duration::from_std(d).unwrap_or_default()).to_rfc3339()
    })
}

/// Cooldown seconds remaining
fn cooldown_secs(instant: Option<Instant>) -> Option<u64> {
    instant.map(|i| i.saturating_duration_since(Instant::now()).as_secs())
}

impl WebEntry {
    fn new(cookie: String, source: WebCredSource, added_at: Option<String>, id: String) -> Self {
        Self {
            cookie,
            id,
            source,
            added_at,
            score: 0.0,
            breaker: CircuitBreaker::new(),
            last_ok_at: None,
        }
    }
}

pub struct WebCookiePool {
    inner: RwLock<Vec<WebEntry>>,
}

impl WebCookiePool {
    /// Build entries (config takes priority, dedup by same cookie)
    fn build_entries(cfg: &Config) -> Vec<WebEntry> {
        let mut map: HashMap<String, WebEntry> = HashMap::new();
        for t in &cfg.auth_tokens {
            if looks_like_cookie(t) {
                let id = import::cred_id(t);
                map.entry(id.clone())
                    .or_insert_with(|| WebEntry::new(t.clone(), WebCredSource::Config, None, id));
            }
        }
        if let Ok(toks) = import::load_tokens_healed(&cfg.tokens_path) {
            for t in toks {
                if looks_like_cookie(&t.token) {
                    let id = import::cred_id(&t.token);
                    map.entry(id.clone()).or_insert_with(|| {
                        WebEntry::new(
                            t.token.clone(),
                            WebCredSource::Imported,
                            t.added_at.clone(),
                            id,
                        )
                    });
                }
            }
        }
        map.into_values().collect()
    }

    /// Build from config + tokens.json (config takes priority, dedup by same cookie)
    pub fn load(cfg: &Config) -> Self {
        Self {
            inner: RwLock::new(Self::build_entries(cfg)),
        }
    }

    /// Rebuild the pool after importing/deleting credentials (preserves existing health state and circuit-breaker cooldowns)
    pub async fn reload(&self, cfg: &Config) {
        let fresh = Self::build_entries(cfg);
        let mut cur = self.inner.write().await;
        let mut merged: Vec<WebEntry> = Vec::with_capacity(fresh.len());
        for mut e in fresh {
            if let Some(i) = cur.iter().position(|o| o.id == e.id) {
                let prev = cur.remove(i);
                e.score = prev.score;
                e.breaker = prev.breaker;
                e.last_ok_at = prev.last_ok_at;
            }
            merged.push(e);
        }
        *cur = merged;
    }

    /// Alias: consistent with Config's construction entry point (`load` is the canonical name)
    pub fn new(cfg: &Config) -> Self {
        Self::load(cfg)
    }

    /// Hot-append (called after importing a new Cookie; dedup by id), returns whether it was actually added
    pub async fn add_if_absent(
        &self,
        cookie: &str,
        source: &str,
        added_at: Option<String>,
    ) -> bool {
        let mut entries = self.inner.write().await;
        let id = import::cred_id(cookie);
        if entries.iter().any(|e| e.id == id) {
            return false;
        }
        let src = if source == "config" {
            WebCredSource::Config
        } else {
            WebCredSource::Imported
        };
        entries.push(WebEntry::new(cookie.to_string(), src, added_at, id));
        true
    }

    /// Hot-remove (called after credential deletion), returns whether something was actually removed
    pub async fn remove(&self, id: &str) -> bool {
        let mut entries = self.inner.write().await;
        let before = entries.len();
        entries.retain(|e| e.id != id);
        entries.len() != before
    }

    /// Is the pool empty?
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    pub async fn count(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Pick the highest-health credential allowed by the circuit breaker; None if all unavailable
    pub async fn pick(&self) -> Option<WebPick> {
        let mut entries = self.inner.write().await;
        let mut best: Option<usize> = None;
        // Index-based access (avoid iter_mut + closure double-borrow conflict; breaker.allow's mutable semantics unchanged)
        for i in 0..entries.len() {
            if !entries[i].breaker.allow() {
                continue;
            }
            let score = entries[i].score;
            if best.map(|b| score > entries[b].score).unwrap_or(true) {
                best = Some(i);
            }
        }
        best.map(|i| {
            let e = &entries[i];
            let source_label: &'static str = match e.source {
                WebCredSource::Config => "config",
                WebCredSource::Imported => "imported",
            };
            WebPick {
                cookie: e.cookie.clone(),
                id: e.id.clone(),
                source: source_label,
                added_at: e.added_at.clone(),
                cred: serde_json::json!({
                    "source": source_label,
                    "added_at": e.added_at,
                    "token_masked": mask(&e.cookie),
                    "id": e.id,
                }),
            }
        })
    }

    /// Request succeeded (HalfOpen probe success -> gradual recovery)
    pub async fn mark_ok(&self, id: &str) {
        let mut entries = self.inner.write().await;
        if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
            e.breaker.record_success();
            e.score = (e.score + 1.0).min(100.0);
            e.last_ok_at = Some(Instant::now());
        }
    }

    /// Request failed (only trips the breaker once consecutive failures exceed the threshold)
    pub async fn mark_failure(&self, id: &str, reason: &str) {
        let mut entries = self.inner.write().await;
        if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
            e.breaker.record_failure(reason);
            e.score = (e.score - 2.0).max(-10.0);
            tracing::warn!("web Cookie credential {} failed: {reason}", mask(&e.cookie));
        }
    }

    /// Deterministic failure (401/403 etc.) -> trip and cool down immediately, independent of consecutive failure count
    pub async fn mark_cooldown(&self, id: &str, duration: Duration, reason: &str) {
        let mut entries = self.inner.write().await;
        if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
            e.breaker.trip_for(duration, reason);
            e.score = (e.score - 4.0).max(-10.0);
            tracing::warn!(
                "web Cookie credential {} cooling down {duration:?}: {reason}",
                mask(&e.cookie)
            );
        }
    }

    /// Health snapshot (for /api/accounts/health and the account list)
    pub async fn snapshot(&self) -> Vec<WebCookieHealth> {
        let entries = self.inner.read().await;
        entries
            .iter()
            .map(|e| WebCookieHealth {
                id: e.id.clone(),
                kind: "web-cookie",
                source: match e.source {
                    WebCredSource::Config => "config",
                    WebCredSource::Imported => "imported",
                },
                added_at: e.added_at.clone(),
                masked: mask(&e.cookie),
                health_score: e.score,
                circuit_state: match e.breaker.state {
                    crate::pool::CircuitState::Closed => "closed".into(),
                    crate::pool::CircuitState::Open => "open".into(),
                    crate::pool::CircuitState::HalfOpen => "half_open".into(),
                },
                cooldown_until: cooldown_iso(e.breaker.open_until),
                cooldown_seconds: cooldown_secs(e.breaker.open_until),
                trips: e.breaker.trips,
                last_error: e.breaker.last_reason.clone(),
                last_ok_at: e.last_ok_at.map(|i| format!("{i:?}")),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg_with(tokens: Vec<&str>, tokens_path: &str) -> Config {
        Config {
            auth_tokens: tokens.iter().map(|s| s.to_string()).collect(),
            tokens_path: tokens_path.to_string(),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn load_collects_config_cookies_only() {
        let c = cfg_with(
            vec!["__Secure-next-auth.session-token=abc", "bearer-plain"],
            "no-such-tokens.json",
        );
        let pool = WebCookiePool::load(&c);
        assert_eq!(pool.count().await, 1, "only cookie values enter the pool");
    }

    #[tokio::test]
    async fn pick_prefers_healthy_and_higher_score() {
        let c = cfg_with(
            vec![
                "__Secure-next-auth.session-token=good-one",
                "__Secure-next-auth.session-token=bad-one",
            ],
            "no-such-tokens.json",
        );
        let pool = WebCookiePool::load(&c);
        assert_eq!(pool.count().await, 2);
        // Directly manipulate internal scoring: bad gets a low score, good a high score -> pick should choose good
        {
            let mut n = pool.inner.write().await;
            for e in n.iter_mut() {
                e.score = if e.cookie.contains("bad-one") {
                    -5.0
                } else {
                    30.0
                };
            }
        }
        let p = pool.pick().await.unwrap();
        assert!(p.cookie.contains("good-one"), "should pick the high-score credential");
        assert!(p.cred.get("token_masked").is_some());
        assert!(p.cred.get("id").is_some());
    }

    #[tokio::test]
    async fn cooldown_skips_until_expiry() {
        let c = cfg_with(
            vec!["__Secure-next-auth.session-token=only"],
            "no-such-tokens.json",
        );
        let pool = WebCookiePool::load(&c);
        let p0 = pool.pick().await.unwrap();
        pool.mark_cooldown(&p0.id, Duration::from_secs(3600), "401 expired")
            .await;
        // During cooldown: none available -> None
        assert!(pool.pick().await.is_none());
        // Snapshot reflects the cooldown
        let snap = pool.snapshot().await;
        assert_eq!(snap[0].circuit_state, "open");
        assert!(snap[0].cooldown_until.is_some());
        assert_eq!(snap[0].last_error.as_deref(), Some("401 expired"));
    }

    #[tokio::test]
    async fn mark_ok_recovers_half_open() {
        let c = cfg_with(
            vec!["__Secure-next-auth.session-token=a"],
            "no-such-tokens.json",
        );
        let pool = WebCookiePool::load(&c);
        let p0 = pool.pick().await.unwrap();
        pool.mark_cooldown(&p0.id, Duration::from_millis(1), "boom")
            .await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        // After expiry, allow() enters HalfOpen and lets one probe through (no further probes while one is in flight, so we take this result directly)
        let p1 = pool.pick().await.expect("cooldown expiry should allow a single probe through");
        pool.mark_ok(&p1.id).await;
        let snap = pool.snapshot().await;
        assert_eq!(
            snap[0].circuit_state, "half_open",
            "HalfOpen needs a second success to confirm"
        );
        // Second success -> recovers to Closed
        pool.mark_ok(&p1.id).await;
        let snap2 = pool.snapshot().await;
        assert_eq!(snap2[0].circuit_state, "closed");
        assert!(snap2[0].last_ok_at.is_some());
    }

    #[tokio::test]
    async fn mark_failure_accumulates_until_trip() {
        let c = cfg_with(
            vec!["__Secure-next-auth.session-token=a"],
            "no-such-tokens.json",
        );
        let pool = WebCookiePool::load(&c);
        let p0 = pool.pick().await.unwrap();
        // 4 consecutive failures -> Closed->Open
        for _ in 0..4 {
            pool.mark_failure(&p0.id, "upstream 5xx").await;
        }
        let snap = pool.snapshot().await;
        assert_eq!(snap[0].circuit_state, "open");
        assert!(pool.pick().await.is_none());
    }

    #[tokio::test]
    async fn snapshot_masks_cookie() {
        let c = cfg_with(
            vec!["__Secure-next-auth.session-token=0123456789abcdef"],
            "no-such-tokens.json",
        );
        let pool = WebCookiePool::load(&c);
        let snap = pool.snapshot().await;
        assert!(snap[0].masked.contains("..."));
        assert!(!snap[0].masked.contains("0123456789abcdef"), "must not be plaintext");
        assert_eq!(snap[0].kind, "web-cookie");
    }

    #[tokio::test]
    async fn empty_pool_picks_none() {
        let c = cfg_with(vec![], "no-such-tokens.json");
        let pool = WebCookiePool::load(&c);
        assert!(pool.is_empty().await);
        assert!(pool.pick().await.is_none());
        assert!(pool.snapshot().await.is_empty());
    }
}
