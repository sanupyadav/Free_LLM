//! Failure classification and retry backoff strategy
//!
//! - HTTP status code / reqwest error -> [`FailureKind`]
//! - [`backoff_delay`]: exponential backoff `2^attempt * base` capped at `max`;
//!   RateLimit uses a 4x base multiplier plus ±20% jitter; Auth is a fixed 10s for refresh/account switch
//! - [`AttemptOutcome`] / [`AttemptFailure`] mark the "committed" boundary:
//!   once committed (the response has started streaming out), errors are passed through as-is instead of retried with a different account

use std::future::Future;
use std::time::Duration;

/// Fixed wait after an Auth failure (for refreshing the token or switching accounts)
pub const AUTH_REFRESH_DELAY_MS: u64 = 10_000;

/// RateLimit backoff base multiplier (rate-limit recovery is usually slower)
const RATE_LIMIT_MULTIPLIER: u64 = 4;

/// Jitter ratio (±20%)
const JITTER_RATIO: f64 = 0.2;

/// Upstream failure classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 429 rate limited
    RateLimit,
    /// 401 credential expired (needs refresh/account switch, not suitable for an in-place retry)
    Auth,
    /// 403 no permission (needs an account switch, not suitable for an in-place retry)
    Forbidden,
    /// 5xx server error
    Server,
    /// Connection failed/network unreachable
    Network,
    /// Request timed out
    Timeout,
    /// Other (protocol error, parse failure, etc.)
    Other,
}

impl FailureKind {
    /// Stable string identifier (used for logs and telemetry error_kind)
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::RateLimit => "rate_limit",
            FailureKind::Auth => "auth",
            FailureKind::Forbidden => "forbidden",
            FailureKind::Server => "server",
            FailureKind::Network => "network",
            FailureKind::Timeout => "timeout",
            FailureKind::Other => "other",
        }
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Retry policy
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total number of attempts (including the first)
    pub max_attempts: usize,
    /// Backoff base (milliseconds)
    pub base_delay_ms: u64,
    /// Cap on a single backoff (milliseconds)
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 500,
            max_delay_ms: 8_000,
        }
    }
}

/// HTTP status code -> failure classification
pub fn classify_status(status: u16) -> FailureKind {
    match status {
        429 => FailureKind::RateLimit,
        401 => FailureKind::Auth,
        403 => FailureKind::Forbidden,
        s if (500..600).contains(&s) => FailureKind::Server,
        _ => FailureKind::Other,
    }
}

/// reqwest transport-layer error -> failure classification
pub fn classify_reqwest_error(e: &reqwest::Error) -> FailureKind {
    if e.is_timeout() {
        FailureKind::Timeout
    } else if e.is_connect() {
        FailureKind::Network
    } else {
        FailureKind::Other
    }
}

/// Whether this is suitable for an in-place retry (account switching is decided by the caller: Auth/Forbidden cannot be retried in place)
pub fn is_retryable(kind: FailureKind) -> bool {
    matches!(
        kind,
        FailureKind::RateLimit | FailureKind::Server | FailureKind::Network | FailureKind::Timeout
    )
}

/// Compute the backoff duration after the `attempt`-th failure (attempt starts at 0)
///
/// - Normal: `min(2^attempt * base, max)`
/// - RateLimit: 4x base + ±20% jitter, then capped at `max`
/// - Auth: fixed [`AUTH_REFRESH_DELAY_MS`]
pub fn backoff_delay(policy: &RetryPolicy, attempt: usize, kind: FailureKind) -> Duration {
    if kind == FailureKind::Auth {
        return Duration::from_millis(AUTH_REFRESH_DELAY_MS);
    }
    let exp = 2u64.saturating_pow(attempt.min(63) as u32);
    let mut ms = policy.base_delay_ms.saturating_mul(exp);
    if kind == FailureKind::RateLimit {
        ms = apply_jitter(ms.saturating_mul(RATE_LIMIT_MULTIPLIER));
    }
    Duration::from_millis(ms.min(policy.max_delay_ms))
}

/// Apply ±20% jitter
fn apply_jitter(ms: u64) -> u64 {
    use rand::Rng;
    let factor = rand::thread_rng().gen_range((1.0 - JITTER_RATIO)..=(1.0 + JITTER_RATIO));
    (ms as f64 * factor).round().max(0.0) as u64
}

/// Whether to keep retrying: committed / not retryable / attempts exhausted -> `None`
///
/// `attempt` is the index of the attempt just completed (starting at 0).
pub fn retry_delay(
    policy: &RetryPolicy,
    attempt: usize,
    kind: FailureKind,
    committed: bool,
) -> Option<Duration> {
    if committed || !is_retryable(kind) {
        return None;
    }
    if attempt + 1 >= policy.max_attempts {
        return None;
    }
    Some(backoff_delay(policy, attempt, kind))
}

/// The result of one attempt: `value` + whether it was committed (has produced an irreversible side effect upstream/to the client)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptOutcome<T> {
    pub value: T,
    pub committed: bool,
}

impl<T> AttemptOutcome<T> {
    pub fn new(value: T, committed: bool) -> Self {
        Self { value, committed }
    }

    /// Committed (not retryable)
    pub fn committed(value: T) -> Self {
        Self::new(value, true)
    }

    /// Not committed (safe to retry with a different account)
    pub fn draft(value: T) -> Self {
        Self::new(value, false)
    }

    pub fn is_committed(&self) -> bool {
        self.committed
    }

    /// Map the inner value while preserving the committed flag
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> AttemptOutcome<U> {
        AttemptOutcome {
            value: f(self.value),
            committed: self.committed,
        }
    }
}

/// A single failed attempt: the error itself + its classification + whether it was committed
#[derive(Debug, Clone)]
pub struct AttemptFailure<E> {
    pub error: E,
    pub kind: FailureKind,
    pub committed: bool,
}

impl<E> AttemptFailure<E> {
    pub fn new(error: E, kind: FailureKind, committed: bool) -> Self {
        Self {
            error,
            kind,
            committed,
        }
    }

    /// Uncommitted failure (retryable)
    pub fn uncommitted(error: E, kind: FailureKind) -> Self {
        Self::new(error, kind, false)
    }

    /// Committed failure (passed through as-is)
    pub fn committed(error: E, kind: FailureKind) -> Self {
        Self::new(error, kind, true)
    }
}

/// Execution driver with backoff retries: `attempt_fn` receives the attempt index (starting at 0)
///
/// An uncommitted, retryable failure is retried after a policy-determined backoff; a committed,
/// non-retryable, or attempts-exhausted error is returned as-is.
pub async fn run_with_retry<T, E, F, Fut>(policy: &RetryPolicy, mut attempt_fn: F) -> Result<T, E>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<AttemptOutcome<T>, AttemptFailure<E>>>,
{
    let mut attempt = 0usize;
    loop {
        match attempt_fn(attempt).await {
            Ok(outcome) => return Ok(outcome.value),
            Err(failure) => match retry_delay(policy, attempt, failure.kind, failure.committed) {
                Some(delay) => {
                    tracing::warn!(
                        attempt,
                        kind = failure.kind.as_str(),
                        delay_ms = delay.as_millis() as u64,
                        "upstream failure, retrying after backoff"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                None => return Err(failure.error),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn classify_status_covers_all_branches() {
        assert_eq!(classify_status(429), FailureKind::RateLimit);
        assert_eq!(classify_status(401), FailureKind::Auth);
        assert_eq!(classify_status(403), FailureKind::Forbidden);
        assert_eq!(classify_status(500), FailureKind::Server);
        assert_eq!(classify_status(502), FailureKind::Server);
        assert_eq!(classify_status(599), FailureKind::Server);
        assert_eq!(classify_status(404), FailureKind::Other);
        assert_eq!(classify_status(200), FailureKind::Other);
    }

    #[test]
    fn is_retryable_matrix() {
        assert!(is_retryable(FailureKind::RateLimit));
        assert!(is_retryable(FailureKind::Server));
        assert!(is_retryable(FailureKind::Network));
        assert!(is_retryable(FailureKind::Timeout));
        assert!(!is_retryable(FailureKind::Auth));
        assert!(!is_retryable(FailureKind::Forbidden));
        assert!(!is_retryable(FailureKind::Other));
    }

    #[test]
    fn backoff_is_monotonic_and_capped() {
        let policy = RetryPolicy::default();
        let mut prev = Duration::ZERO;
        for attempt in 0..8 {
            let d = backoff_delay(&policy, attempt, FailureKind::Server);
            assert!(d >= prev, "backoff #{attempt} should not be smaller than the previous one");
            assert!(d <= Duration::from_millis(policy.max_delay_ms));
            prev = d;
        }
        assert_eq!(
            backoff_delay(&policy, 0, FailureKind::Server),
            Duration::from_millis(500)
        );
        assert_eq!(
            backoff_delay(&policy, 1, FailureKind::Server),
            Duration::from_millis(1000)
        );
        assert_eq!(
            backoff_delay(&policy, 2, FailureKind::Server),
            Duration::from_millis(2000)
        );
        // Capped at the max
        assert_eq!(
            backoff_delay(&policy, 20, FailureKind::Server),
            Duration::from_millis(8000)
        );
    }

    #[test]
    fn rate_limit_backoff_has_jitter_within_bounds_and_caps() {
        let policy = RetryPolicy::default();
        // attempt=0 -> 500 * 4 = 2000ms, jitter range [1600, 2400]
        for _ in 0..200 {
            let d = backoff_delay(&policy, 0, FailureKind::RateLimit).as_millis() as u64;
            assert!((1600..=2400).contains(&d), "jitter out of bounds: {d}ms");
        }
        // At high attempt counts, still capped at max after jitter
        for _ in 0..50 {
            assert_eq!(
                backoff_delay(&policy, 12, FailureKind::RateLimit),
                Duration::from_millis(8000)
            );
        }
        // Jitter really does vary (not a constant value)
        let samples: std::collections::HashSet<u128> = (0..50)
            .map(|_| backoff_delay(&policy, 0, FailureKind::RateLimit).as_millis())
            .collect();
        assert!(samples.len() > 1, "RateLimit backoff should include random jitter");
    }

    #[test]
    fn auth_backoff_is_fixed_refresh_delay() {
        let policy = RetryPolicy::default();
        for attempt in [0usize, 1, 5, 20] {
            assert_eq!(
                backoff_delay(&policy, attempt, FailureKind::Auth),
                Duration::from_millis(AUTH_REFRESH_DELAY_MS)
            );
        }
    }

    #[test]
    fn retry_delay_stops_when_committed_or_exhausted_or_not_retryable() {
        let policy = RetryPolicy::default();
        // Uncommitted and retryable: allowed
        assert!(retry_delay(&policy, 0, FailureKind::Server, false).is_some());
        // Committed: give up immediately
        assert!(retry_delay(&policy, 0, FailureKind::Server, true).is_none());
        // Not retryable
        assert!(retry_delay(&policy, 0, FailureKind::Auth, false).is_none());
        assert!(retry_delay(&policy, 0, FailureKind::Forbidden, false).is_none());
        // Attempts exhausted (max_attempts=3 -> attempt 0/1 retryable, attempt 2 not)
        assert!(retry_delay(&policy, 1, FailureKind::Network, false).is_some());
        assert!(retry_delay(&policy, 2, FailureKind::Network, false).is_none());
    }

    #[tokio::test]
    async fn run_with_retry_retries_uncommitted_until_success() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay_ms: 1,
            max_delay_ms: 2,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let result: Result<&str, &'static str> = run_with_retry(&policy, move |attempt| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err(AttemptFailure::uncommitted("boom", FailureKind::Network))
                } else {
                    Ok(AttemptOutcome::draft("ok"))
                }
            }
        })
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn run_with_retry_stops_immediately_on_committed_or_exhausted() {
        // Committed error: only one attempt, error passed through
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay_ms: 1,
            max_delay_ms: 2,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let result: Result<(), &'static str> = run_with_retry(&policy, move |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(AttemptFailure::committed(
                    "stream broken",
                    FailureKind::Network,
                ))
            }
        })
        .await;
        assert_eq!(result.err(), Some("stream broken"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Attempts exhausted: exactly max_attempts attempts
        let calls2 = Arc::new(AtomicUsize::new(0));
        let c2 = calls2.clone();
        let result2: Result<(), &'static str> = run_with_retry(&policy, move |_| {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(AttemptFailure::uncommitted(
                    "always down",
                    FailureKind::Server,
                ))
            }
        })
        .await;
        assert_eq!(result2.err(), Some("always down"));
        assert_eq!(calls2.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn classify_reqwest_timeout_error_as_timeout() {
        use tokio::io::AsyncReadExt;
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(_) => return, // Skip if the environment doesn't support local listening
        };
        let addr = match listener.local_addr() {
            Ok(a) => a,
            Err(_) => return,
        };
        // Accept the connection but never respond, to induce a read timeout
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_millis(80))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let err = match client.get(format!("http://{addr}/")).send().await {
            Ok(_) => return, // Skip if an environment quirk made the request succeed
            Err(e) => e,
        };
        assert!(err.is_timeout(), "expected a timeout error, got: {err}");
        assert_eq!(classify_reqwest_error(&err), FailureKind::Timeout);
    }

    #[tokio::test]
    async fn classify_reqwest_connect_error_as_network() {
        // Bind then release immediately, to get a local port that will almost certainly refuse connections
        let addr = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => match l.local_addr() {
                Ok(a) => a,
                Err(_) => return,
            },
            Err(_) => return,
        };
        let err = match reqwest::Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
        {
            Ok(_) => return,
            Err(e) => e,
        };
        assert_eq!(classify_reqwest_error(&err), FailureKind::Network);
    }

    #[tokio::test]
    async fn classify_reqwest_builder_error_as_other() {
        let err = match reqwest::Client::new().get("http://[").send().await {
            Ok(_) => return,
            Err(e) => e,
        };
        assert_eq!(classify_reqwest_error(&err), FailureKind::Other);
    }

    #[test]
    fn attempt_outcome_map_preserves_committed_flag() {
        let draft = AttemptOutcome::draft(2).map(|v| v * 3);
        assert_eq!(draft.value, 6);
        assert!(!draft.is_committed());
        let committed = AttemptOutcome::committed(1).map(|v| v + 1);
        assert_eq!(committed.value, 2);
        assert!(committed.is_committed());
    }
}
