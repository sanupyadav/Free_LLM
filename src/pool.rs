//! Multi-account pool: session health score -> best-token round robin + 3-state circuit breaker (Closed/Open/HalfOpen)
//!
//! Scoring dimensions (once per token/session cycle):
//! - Session active with enough remaining time  +100
//! - The earlier the queue position  +50~+80
//! - Circuit Open  -999 (removed outright); HalfOpen lets a probe through
//! - Recent errors  -weighted
//! - Heartbeat/ad refresh succeeded  +small bonus
//!
//! Circuit breaker rules (modeled on cc-switch):
//! - Closed: >=4 consecutive failures -> Open (cooldown = min(60s * 2^(trips-1), 600s))
//! - Open: rejects selection during cooldown; expires -> HalfOpen
//! - HalfOpen: lets a probe through; >=2 consecutive successes -> Closed; failure -> Open again

use crate::config::Config;
use crate::session::SessionManager;
use crate::upstream::UpstreamClient;

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

/// Consecutive failure threshold (Closed -> Open)
const FAILURE_THRESHOLD: u32 = 4;
/// Consecutive successes needed in HalfOpen (-> Closed)
const HALF_OPEN_SUCCESS_TO_CLOSE: u32 = 2;
/// Base cooldown
const BASE_COOLDOWN: Duration = Duration::from_secs(60);
/// Cooldown cap
const MAX_COOLDOWN: Duration = Duration::from_secs(600);

/// Circuit breaker's three states
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Per-account circuit breaker
#[derive(Debug)]
pub struct CircuitBreaker {
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub half_open_successes: u32,
    pub open_until: Option<Instant>,
    /// HalfOpen probe gate: only one in-flight probe is let through at a time
    pub probing: bool,
    /// Cumulative trip count (observable)
    pub trips: u64,
    pub last_reason: Option<String>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            half_open_successes: 0,
            open_until: None,
            probing: false,
            trips: 0,
            last_reason: None,
        }
    }

    /// Whether this request is allowed (Open not yet expired -> false; on expiry, auto-transitions to HalfOpen and lets a single probe through)
    pub fn allow(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => {
                if self.probing {
                    false // a probe is already in flight, don't let another through
                } else {
                    self.probing = true;
                    true
                }
            }
            CircuitState::Open => {
                if let Some(until) = self.open_until {
                    if Instant::now() >= until {
                        self.state = CircuitState::HalfOpen;
                        self.half_open_successes = 0;
                        self.probing = true;
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Business success (accumulates probe successes while HalfOpen)
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.probing = false;
        if self.state == CircuitState::HalfOpen {
            self.half_open_successes += 1;
            if self.half_open_successes >= HALF_OPEN_SUCCESS_TO_CLOSE {
                self.state = CircuitState::Closed;
                self.open_until = None;
                self.half_open_successes = 0;
            }
        }
    }

    /// Business failure (Closed with consecutive failures over threshold / any HalfOpen failure -> trips)
    pub fn record_failure(&mut self, reason: &str) {
        self.consecutive_failures += 1;
        self.last_reason = Some(reason.to_string());
        self.probing = false;
        match self.state {
            CircuitState::HalfOpen => self.trip(reason),
            CircuitState::Closed if self.consecutive_failures >= FAILURE_THRESHOLD => {
                self.trip(reason)
            }
            _ => {}
        }
    }

    /// Trips immediately (deterministic failures like 401/403; cooldown grows exponentially with trip count, capped at 10 minutes)
    pub fn trip(&mut self, reason: &str) {
        self.trips += 1;
        self.state = CircuitState::Open;
        self.consecutive_failures = FAILURE_THRESHOLD;
        self.half_open_successes = 0;
        self.probing = false;
        // trips=1..-> 1,2,4,8,16x (16x gets truncated to 10 minutes by MAX_COOLDOWN)
        let factor = 1u32 << self.trips.min(5).saturating_sub(1);
        let cd = BASE_COOLDOWN.saturating_mul(factor).min(MAX_COOLDOWN);
        self.open_until = Some(Instant::now() + cd);
        self.last_reason = Some(reason.to_string());
    }

    /// Trips with a custom cooldown duration (e.g. upstream Retry-After)
    pub fn trip_for(&mut self, duration: Duration, reason: &str) {
        self.trips += 1;
        self.state = CircuitState::Open;
        self.consecutive_failures = FAILURE_THRESHOLD;
        self.half_open_successes = 0;
        self.probing = false;
        self.open_until = Some(Instant::now() + duration.min(MAX_COOLDOWN));
        self.last_reason = Some(reason.to_string());
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PoolSnapshot {
    pub accounts: Vec<AccountSnapshot>,
    pub total: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccountSnapshot {
    pub name: String,
    pub healthy: bool,
    pub score: f64,
    pub cooldown_until: Option<String>,
    /// Circuit breaker's three states (closed/open/half_open)
    pub circuit_state: String,
    /// Cumulative trip count
    pub trips: u64,
    pub last_error: Option<String>,
    pub session: Option<crate::session::SessionSnapshot>,
}

pub struct AccountEntry {
    pub name: String,
    pub token: String,
    pub session: Arc<SessionManager>,
    pub score: RwLock<f64>,
    /// Circuit breaker (replaces a bare cooldown_until)
    pub breaker: RwLock<CircuitBreaker>,
}

impl Clone for AccountEntry {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            token: self.token.clone(),
            session: self.session.clone(),
            score: RwLock::new(0.0),
            breaker: RwLock::new(CircuitBreaker::new()),
        }
    }
}

/// Load balancing state
pub struct Pool {
    pub accounts: Mutex<Vec<AccountEntry>>,
    pub next: std::sync::atomic::AtomicUsize,
}

impl Pool {
    /// Builds a multi-account pool from configuration
    pub fn new(cfg: &Config, client: Arc<UpstreamClient>) -> Self {
        let accounts = cfg
            .auth_tokens
            .iter()
            .enumerate()
            .map(|(i, token)| AccountEntry {
                name: format!("token-{}", i + 1),
                token: token.clone(),
                session: Arc::new(SessionManager::new(
                    client.clone(),
                    token.clone(),
                    cfg.clone(),
                )),
                score: RwLock::new(0.0),
                breaker: RwLock::new(CircuitBreaker::new()),
            })
            .collect();
        Self {
            accounts: Mutex::new(accounts),
            next: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Picks the healthiest token (skips circuit-Open, lets HalfOpen probes through), returns a clone
    pub async fn pick_best(&self) -> Option<AccountEntry> {
        let accounts = self.accounts.lock().await;
        if accounts.is_empty() {
            return None;
        }
        let mut best: Option<&AccountEntry> = None;
        for acc in accounts.iter() {
            let allowed = acc.breaker.write().await.allow();
            if !allowed {
                continue;
            }
            if best.is_none() {
                best = Some(acc);
            } else {
                let s = *acc.score.read().await;
                let bs = *best.unwrap().score.read().await;
                if s > bs {
                    best = Some(acc);
                }
            }
        }
        best.cloned()
    }

    /// Records business success (drives HalfOpen -> Closed)
    pub async fn mark_success(&self, name: &str) {
        let accounts = self.accounts.lock().await;
        if let Some(acc) = accounts.iter().find(|a| a.name == name) {
            acc.breaker.write().await.record_success();
        }
    }

    /// Records business failure (drives Closed -> Open / HalfOpen -> Open)
    pub async fn mark_failure(&self, name: &str, reason: &str) {
        let accounts = self.accounts.lock().await;
        if let Some(acc) = accounts.iter().find(|a| a.name == name) {
            acc.breaker.write().await.record_failure(reason);
        }
    }

    /// Marks a cooldown (trips the breaker) with a custom duration (e.g. upstream Retry-After / 401 cooldown)
    pub async fn mark_cooldown(&self, name: &str, duration: std::time::Duration, reason: &str) {
        let accounts = self.accounts.lock().await;
        if let Some(acc) = accounts.iter().find(|a| a.name == name) {
            acc.breaker.write().await.trip_for(duration, reason);
            acc.session.record_error(reason).await;
            tracing::warn!("account {name} tripped for {duration:?}: {reason}");
        }
    }

    /// Updates an account's score
    pub async fn update_score(&self, name: &str, delta: f64) {
        let accounts = self.accounts.lock().await;
        if let Some(acc) = accounts.iter().find(|a| a.name == name) {
            let mut score = acc.score.write().await;
            *score += delta;
            *score = score.clamp(-1000.0, 1000.0);
        }
    }

    /// Health snapshot
    pub async fn snapshot(&self) -> PoolSnapshot {
        let accounts = self.accounts.lock().await;
        let mut snapshot_accounts = Vec::with_capacity(accounts.len());
        for acc in accounts.iter() {
            let score = *acc.score.read().await;
            let breaker = acc.breaker.read().await;
            let sess = acc.session.snapshot().await;
            let healthy = breaker.state == CircuitState::Closed
                || (breaker.state == CircuitState::Open
                    && breaker
                        .open_until
                        .map(|u| Instant::now() >= u)
                        .unwrap_or(true));
            snapshot_accounts.push(AccountSnapshot {
                name: acc.name.clone(),
                healthy,
                score,
                cooldown_until: breaker.open_until.map(|i| format!("{:?}", i)),
                circuit_state: match breaker.state {
                    CircuitState::Closed => "closed".into(),
                    CircuitState::Open => "open".into(),
                    CircuitState::HalfOpen => "half_open".into(),
                },
                trips: breaker.trips,
                last_error: sess.last_error.clone(),
                session: Some(sess),
            });
        }
        PoolSnapshot {
            accounts: snapshot_accounts,
            total: accounts.len(),
        }
    }

    /// Hot-adds an account (called after a token import), returns whether it was actually added
    pub async fn add_account(&self, entry: AccountEntry) -> bool {
        let mut accounts = self.accounts.lock().await;
        if accounts.iter().any(|a| a.token == entry.token) {
            return false;
        }
        accounts.push(entry);
        true
    }

    /// Hot-removes an account (called after a credential is deleted), returns whether it was actually removed
    pub async fn remove_account(&self, token: &str) -> bool {
        let mut accounts = self.accounts.lock().await;
        let before = accounts.len();
        accounts.retain(|a| a.token != token);
        accounts.len() != before
    }
}

/// Converts a token list into a Pool (returns an empty pool if there are none)
pub fn build_pool(cfg: &Config, client: Arc<UpstreamClient>) -> Arc<Pool> {
    Arc::new(Pool::new(cfg, client))
}

#[allow(dead_code)]
fn round_robin_next(pool: &Pool) -> usize {
    pool.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_opens_after_consecutive_failures() {
        let mut b = CircuitBreaker::new();
        assert!(b.allow());
        for _ in 0..FAILURE_THRESHOLD - 1 {
            b.record_failure("x");
        }
        assert_eq!(b.state, CircuitState::Closed, "should not trip before reaching the threshold");
        b.record_failure("x");
        assert_eq!(b.state, CircuitState::Open);
        assert!(!b.allow(), "should reject during the Open cooldown period");
    }

    #[test]
    fn breaker_half_open_closes_after_successes() {
        let mut b = CircuitBreaker::new();
        b.trip("test");
        // Manually move the expiry time earlier, to simulate the cooldown ending
        b.open_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(b.allow(), "should let a probe through after expiry");
        assert_eq!(b.state, CircuitState::HalfOpen);
        b.record_success();
        assert_eq!(b.state, CircuitState::HalfOpen, "one success is not enough");
        b.record_success();
        assert_eq!(b.state, CircuitState::Closed, "consecutive successes should close it");
    }

    #[test]
    fn breaker_half_open_failure_reopens() {
        let mut b = CircuitBreaker::new();
        b.trip("test");
        b.open_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(b.allow());
        b.record_failure("again");
        assert_eq!(b.state, CircuitState::Open);
        assert!(b.trips >= 2, "tripping again should accumulate trips");
    }

    #[test]
    fn breaker_cooldown_grows_and_caps() {
        let mut b = CircuitBreaker::new();
        b.trip("1");
        let first = b.open_until.unwrap() - Instant::now();
        b.trip("2");
        let second = b.open_until.unwrap() - Instant::now();
        assert!(second > first, "cooldown should grow");
        for _ in 0..8 {
            b.trip("n");
        }
        let capped = b.open_until.unwrap() - Instant::now();
        assert!(capped <= MAX_COOLDOWN, "cooldown should be capped");
    }

    #[test]
    fn cooldown_reaches_cap_after_five_trips() {
        let mut b = CircuitBreaker::new();
        for _ in 0..5 {
            b.trip("x");
        }
        let cd = b.open_until.unwrap() - Instant::now();
        // 5th trip: 1<<4 = 16x -> 960s gets truncated by MAX_COOLDOWN (600s)
        assert!(
            cd > Duration::from_secs(550),
            "the 5th trip should be close to the 10-minute cap: {cd:?}"
        );
        assert!(cd <= MAX_COOLDOWN);
    }

    #[test]
    fn half_open_only_one_probe_at_a_time() {
        let mut b = CircuitBreaker::new();
        b.trip("x");
        b.open_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(b.allow(), "should let the first probe through once the cooldown expires");
        assert!(!b.allow(), "should not let another through while a probe is in flight (prevents a concurrent probe storm)");
        b.record_success();
        assert!(b.allow(), "should let the next one through after the probe completes");
        assert!(!b.allow());
    }
}
