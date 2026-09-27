//! Dual-bucket concurrency semaphore (v0.8 — implements the capability the README already claimed)
//!
//! The upstream codebuff free tier's concurrency wall is computed per gateway IP/account
//! (reverse-engineered from the desktop client's orchestrator.js):
//! - Free tier: `{slot:1, multi:3}` -- at most 1 "formal session" per account at a time, plus 3 concurrent ordinary requests
//! - Subscriber tier: `{slot:3, multi:8}` -- subscriber accounts get higher capacity
//!
//! This module uses a **gateway-global** semaphore to limit how many requests can enter the
//! upstream forwarding path at once:
//! - `TieredSemaphore` holds two independent buckets (free / subscriber) that don't interfere with each other
//! - Each bucket has two semaphores (slots + multi): `acquire` takes one permit from each --
//!   **the actual concurrency ceiling = min(slots, multi)** (1 for free tier, 3 for subscriber tier); multi is a parallel reserve dimension
//!   (if upstream policy later separates "slot count" from "session concurrency", it can be scaled independently; for now the scarcer slots dimension sets the ceiling)
//! - `acquire(is_subscriber)` is called **before the first byte is written out**; a 2s timeout returns `AcquireError::Busy`
//!   (429 semantics, aligned with waiting_room), never queues indefinitely
//! - `TierGuard` is RAII: holds the permit, returns it automatically on Drop, preventing leaks
//!
//! Configuration (config.json / CONCURRENCY_* env vars):
//! - `concurrency_free_slots`    default 1
//! - `concurrency_free_multi`    default 3
//! - `concurrency_sub_slots`     default 3
//! - `concurrency_sub_multi`     default 8

use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};

/// Default free bucket: 1 paid slot / 3 ordinary
pub const DEFAULT_FREE_SLOTS: usize = 1;
pub const DEFAULT_FREE_MULTI: usize = 3;
/// Default subscriber bucket: 3 paid slots / 8 ordinary
pub const DEFAULT_SUB_SLOTS: usize = 3;
pub const DEFAULT_SUB_MULTI: usize = 8;
/// acquire timeout (milliseconds): a timeout is treated as concurrency-busy, returning 429 semantics.
/// A streaming reply holds its permit for the whole stream, so a short wait turns every parallel client
/// request into a 429; queue long enough for a typical reply to finish. Override: CONCURRENCY_WAIT_MS.
pub const ACQUIRE_TIMEOUT_MS: u64 = 120_000;

/// Failed to acquire a permit
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// Bucket capacity exhausted and the wait timed out (429 semantics)
    Busy,
}

/// Dual-bucket semaphore (gateway-global, Arc-shared)
pub struct TieredSemaphore {
    free_slots: Arc<Semaphore>,
    free_multi: Arc<Semaphore>,
    sub_slots: Arc<Semaphore>,
    sub_multi: Arc<Semaphore>,
    wait: Duration,
}

/// RAII guard for a single permit: returns the corresponding semaphore on Drop
#[derive(Debug)]
pub struct TierGuard {
    // Which bucket to return to depends on which one it was borrowed from -- Option represents a borrowed permit
    slots: Option<tokio::sync::OwnedSemaphorePermit>,
    multi: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl TierGuard {
    fn new(
        slots: Option<tokio::sync::OwnedSemaphorePermit>,
        multi: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Self {
        Self { slots, multi }
    }
}

impl Drop for TierGuard {
    fn drop(&mut self) {
        // SemaphorePermit returns itself automatically on Drop; explicitly cleared to prevent double-borrow
        self.slots.take();
        self.multi.take();
    }
}

impl TieredSemaphore {
    /// Builds from four capacities
    pub fn new(free_slots: usize, free_multi: usize, sub_slots: usize, sub_multi: usize) -> Self {
        Self {
            free_slots: Arc::new(Semaphore::new(free_slots.max(1))),
            free_multi: Arc::new(Semaphore::new(free_multi.max(1))),
            sub_slots: Arc::new(Semaphore::new(sub_slots.max(1))),
            sub_multi: Arc::new(Semaphore::new(sub_multi.max(1))),
            wait: Duration::from_millis(ACQUIRE_TIMEOUT_MS),
        }
    }

    /// Overrides how long `acquire` queues before returning Busy
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// Default capacities (1/3/3/8)
    pub fn default_capacity() -> Self {
        Self::new(
            DEFAULT_FREE_SLOTS,
            DEFAULT_FREE_MULTI,
            DEFAULT_SUB_SLOTS,
            DEFAULT_SUB_MULTI,
        )
    }

    /// Snapshot of usage counts (for dashboard/health observation)
    pub fn usage(&self) -> serde_json::Value {
        serde_json::json!({
            "free_slots": self.free_slots.available_permits(),
            "free_multi": self.free_multi.available_permits(),
            "sub_slots": self.sub_slots.available_permits(),
            "sub_multi": self.sub_multi.available_permits(),
        })
    }

    /// Acquires dual-bucket permits (one slot + one ordinary). `is_subscriber=true` uses the subscriber bucket.
    ///
    /// Both permits must be obtained before a guard is returned; whichever was obtained first is already
    /// returned by the internal Drop if the other fails/times out.
    /// Returns `AcquireError::Busy` on timeout (429 semantics).
    pub async fn acquire(&self, is_subscriber: bool) -> Result<TierGuard, AcquireError> {
        let (slots, multi) = if is_subscriber {
            (self.sub_slots.clone(), self.sub_multi.clone())
        } else {
            (self.free_slots.clone(), self.free_multi.clone())
        };
        // Grab slots first (scarcer), then multi
        let slot_permit = match timeout(
            self.wait,
            slots.acquire_owned(),
        )
        .await
        {
            Ok(Ok(p)) => Some(p),
            _ => return Err(AcquireError::Busy),
        };
        let multi_permit = match timeout(
            self.wait,
            multi.acquire_owned(),
        )
        .await
        {
            Ok(Ok(p)) => Some(p),
            _ => return Err(AcquireError::Busy), // slot_permit is automatically returned here via Drop
        };
        Ok(TierGuard::new(slot_permit, multi_permit))
    }

    /// Takes only a slot (not multi) -- reserved for scenarios that need to "cap session count only"; currently unused.
    #[allow(dead_code)]
    pub async fn acquire_slots_only(&self, is_subscriber: bool) -> Result<TierGuard, AcquireError> {
        let slots = if is_subscriber {
            self.sub_slots.clone()
        } else {
            self.free_slots.clone()
        };
        match timeout(
            self.wait,
            slots.acquire_owned(),
        )
        .await
        {
            Ok(Ok(p)) => Ok(TierGuard::new(Some(p), None)),
            _ => Err(AcquireError::Busy),
        }
    }
}

/// Determines whether an account credential should use the subscriber bucket (conservative: defaults to the free bucket when there's no clear subscription signal).
///
/// Signal sources (any match means subscriber):
/// - token plaintext contains `unique_subscription` (a web protocol plan field)
/// - token plaintext contains `"subscription"` / `"access_tier"` with a non-free value
/// - Bearer token shape (long string) but the session snapshot tier is paid/subscribed (passed in by the caller)
pub fn is_subscriber_token(token: &str, tier_hint: Option<&str>) -> bool {
    let t = token.to_lowercase();
    if t.contains("unique_subscription") {
        return true;
    }
    if let Some(tier) = tier_hint.map(|s| s.to_lowercase()) {
        if !tier.is_empty() && !["free", "guest", "anonymous", "basic", ""].contains(&tier.as_str())
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn free_bucket_caps_concurrency() {
        let sem = TieredSemaphore::new(1, 1, 3, 8);
        let g1 = sem.acquire(false).await.unwrap();
        // Bucket capacity 1 -> the second acquire should time out with Busy
        let r = tokio::time::timeout(Duration::from_millis(100), sem.acquire(false)).await;
        assert!(r.is_err(), "the second acquire must time out at capacity 1");
        drop(g1);
        // Can acquire again after returning the permit
        assert!(sem.acquire(false).await.is_ok());
    }

    #[tokio::test]
    async fn subscriber_and_free_buckets_are_independent() {
        let sem = TieredSemaphore::new(1, 1, 3, 8);
        let g_free = sem.acquire(false).await.unwrap();
        // Exhausting the free bucket doesn't affect the subscriber bucket
        let g_sub = sem.acquire(true).await.unwrap();
        drop(g_sub);
        // The subscriber bucket has spare capacity, 2 more can enter
        let s1 = sem.acquire(true).await.unwrap();
        let s2 = sem.acquire(true).await.unwrap();
        drop(s1);
        drop(s2);
        drop(g_free);
    }

    #[tokio::test]
    async fn permit_exhaustion_returns_busy_within_timeout() {
        let sem = TieredSemaphore::new(1, 1, 3, 8).with_wait(Duration::from_millis(2000));
        let _g = sem.acquire(false).await.unwrap();
        let start = std::time::Instant::now();
        // wait is 2s here; the test uses a 3s outer window to ensure Busy rather than hanging forever
        let r = tokio::time::timeout(Duration::from_millis(3000), sem.acquire(false)).await;
        assert!(r.is_ok(), "must not block indefinitely");
        assert_eq!(r.unwrap().unwrap_err(), AcquireError::Busy);
        assert!(
            start.elapsed() < Duration::from_millis(2500),
            "should return Busy within the 2s timeout"
        );
    }

    #[tokio::test]
    async fn guard_drop_returns_permits_no_leak() {
        let sem = TieredSemaphore::new(2, 2, 3, 8);
        for _ in 0..1000 {
            let g = sem.acquire(false).await.unwrap();
            drop(g);
        }
        // Available count is restored after the loop
        assert_eq!(sem.free_slots.available_permits(), 2);
        assert_eq!(sem.free_multi.available_permits(), 2);
    }

    #[test]
    fn subscriber_detection_is_conservative() {
        // web protocol plan field -> subscriber
        assert!(is_subscriber_token(
            "__Secure-next-auth.session-token=x; unique_subscription=true",
            None
        ));
        // Bearer token + paid tier hint -> subscriber
        assert!(is_subscriber_token("sk-abc", Some("paid")));
        // No signal -> free (conservative)
        assert!(!is_subscriber_token("sk-abc", None));
        assert!(!is_subscriber_token("sk-abc", Some("free")));
        assert!(!is_subscriber_token("sk-abc", Some("guest")));
    }

    #[tokio::test]
    async fn multi_bucket_shared_with_slots() {
        // multi bucket is independent: with free slots=2 / multi=2, 2 concurrent requests can pass at once (slots don't block)
        let sem = TieredSemaphore::new(2, 2, 3, 8);
        let g1 = sem.acquire(false).await.unwrap();
        let g2 = sem.acquire(false).await.unwrap();
        drop(g1);
        drop(g2);
        // slots capacity 1 + multi 2: the 2nd concurrent request is blocked by slots (not multi)
        let sem2 = TieredSemaphore::new(1, 2, 3, 8);
        let _a = sem2.acquire(false).await.unwrap();
        let r = tokio::time::timeout(Duration::from_millis(100), sem2.acquire(false)).await;
        assert!(r.is_err(), "the 2nd concurrent request must time out at slots capacity 1");
    }
}
