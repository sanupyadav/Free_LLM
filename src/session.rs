//! Session manager: queued -> active -> heartbeat keep-alive -> ad-for-quota renewal, then keep-alive again
//!
//! Reverse-engineered from Freebuff-0.0.98's SessionManager:
//! - status=none -> POST create
//! - status=queued -> GET poll (estimatedWaitMs determines the delay)
//! - status=active -> usable, heartbeat + ad refresh before expiry
//! - schedules x-freebuff-heartbeat:1 every 45s
//! - FREEBUFF_SESSION_GRACE_MS=1800000 grace period

use crate::config::Config;
use crate::upstream::{FreeSessionResponse, UpstreamClient};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub const SESSION_POLL_INTERVAL_SEC: u64 = 5;
pub const SESSION_HEARTBEAT_INTERVAL_SEC: u64 = 45;
pub const SESSION_GRACE_MS: i64 = 1_800_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    None,
    Queued,
    Active,
    Disabled,
    Ended,
    Superseded,
}

impl From<&str> for SessionStatus {
    fn from(s: &str) -> Self {
        match s.trim() {
            "none" => Self::None,
            "queued" => Self::Queued,
            "active" => Self::Active,
            "disabled" => Self::Disabled,
            "ended" => Self::Ended,
            "superseded" => Self::Superseded,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub status: String,
    pub instance_id: Option<String>,
    pub model: Option<String>,
    pub expires_at: Option<String>,
    pub position: Option<i64>,
    pub queue_depth: Option<i64>,
    pub last_error: Option<String>,
    pub updated_at: String,
    pub heartbeat_count: u64,
    pub ad_renewals: u64,
}

pub struct SessionManager {
    pub client: Arc<UpstreamClient>,
    pub token: String,
    pub cfg: Config,

    mu: Mutex<SessionInner>,
    heartbeat_count: AtomicU32,
    ad_renewals: AtomicU32,
    running: AtomicBool,
}

struct SessionInner {
    status: SessionStatus,
    instance_id: Option<String>,
    model: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    position: Option<i64>,
    queue_depth: Option<i64>,
    estimated_wait_ms: Option<i64>,
    last_error: Option<String>,
    last_poll_at: Option<DateTime<Utc>>,
    poll_after: Option<DateTime<Utc>>,
}

impl SessionManager {
    pub fn new(client: Arc<UpstreamClient>, token: String, cfg: Config) -> Self {
        Self {
            client,
            token,
            cfg,
            mu: Mutex::new(SessionInner {
                status: SessionStatus::None,
                instance_id: None,
                model: None,
                expires_at: None,
                position: None,
                queue_depth: None,
                estimated_wait_ms: None,
                last_error: None,
                last_poll_at: None,
                poll_after: None,
            }),
            heartbeat_count: AtomicU32::new(0),
            ad_renewals: AtomicU32::new(0),
            running: AtomicBool::new(false),
        }
    }

    /// Ensure the session is active; create/wait if not. Returns instance_id
    pub async fn ensure_session(&self, model: &str) -> Result<String> {
        loop {
            let mut inner = self.mu.lock().await;

            // Already active and not expired
            if inner.status == SessionStatus::Active {
                if let Some(id) = &inner.instance_id {
                    let expires = inner
                        .expires_at
                        .unwrap_or(Utc::now() + Duration::from_secs(3600));
                    if Utc::now() + Duration::from_secs(5) < expires {
                        return Ok(id.clone());
                    }
                }
                // Reset and recreate if expired
                inner.status = SessionStatus::None;
                inner.instance_id = None;
            }

            // In the waiting room: wait until poll_after, then retry
            if inner.status == SessionStatus::Queued {
                if let Some(skip_until) = inner.poll_after {
                    if Utc::now() < skip_until {
                        return Err(anyhow!(
                            "waiting_room_queued: position {}/{}, retrying in {} seconds",
                            inner.position.unwrap_or(0),
                            inner.queue_depth.unwrap_or(0),
                            (skip_until - Utc::now()).num_seconds().max(0)
                        ));
                    }
                }
                drop(inner);
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }

            // None/Active(expired) -> create
            inner.status = SessionStatus::None;
            let existing = inner.instance_id.clone();
            drop(inner);

            match self
                .client
                .create_session(&self.token, model, existing.as_deref(), None)
                .await
            {
                Ok(sess) => {
                    self.absorb(sess, model).await?;
                    if self.is_active().await {
                        let id = self.instance_id().await;
                        if let Some(id) = id {
                            return Ok(id);
                        }
                    }
                    // queued: keep looping to wait
                }
                Err(e) => {
                    self.note_error(&e).await;
                    // Server 5xx/network error -> back off briefly then retry
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    return Err(e);
                }
            }
        }
    }

    /// Absorb the session response, internally handling the queued/active state machine
    async fn absorb(&self, sess: FreeSessionResponse, model: &str) -> Result<()> {
        let mut inner = self.mu.lock().await;
        let status = SessionStatus::from(sess.status.as_str());
        inner.status = status;
        inner.model = Some(sess.model.clone().unwrap_or_else(|| model.to_string()));
        inner.last_error = sess.message.clone().or(sess.error.clone());
        inner.estimated_wait_ms = sess.estimated_wait_ms;
        match status {
            SessionStatus::Active => {
                inner.instance_id = sess.instance_id();
                inner.expires_at = sess
                    .expires_at
                    .as_deref()
                    .and_then(crate::upstream::parse_optional_time);
                inner.position = None;
                inner.queue_depth = None;
            }
            SessionStatus::Queued => {
                inner.instance_id = sess.instance_id();
                inner.position = sess.position;
                inner.queue_depth = sess.queue_depth.or(sess.position);
                let wait_ms = sess
                    .estimated_wait_ms
                    .unwrap_or(5_000)
                    .clamp(1_000, SESSION_POLL_INTERVAL_SEC as i64 * 1000);
                inner.poll_after = Some(Utc::now() + Duration::from_millis(wait_ms as u64));
            }
            SessionStatus::Disabled => {
                // Account has no free-tier eligibility
                return Err(anyhow!("freebuff session disabled: account has no free quota available"));
            }
            SessionStatus::None | SessionStatus::Ended | SessionStatus::Superseded => {
                // Automatic recreation is handled by the ensure_session loop above
            }
        }
        inner.last_poll_at = Some(Utc::now());
        Ok(())
    }

    #[allow(dead_code)]
    async fn note_error(&self, e: &anyhow::Error) {
        let mut inner = self.mu.lock().await;
        inner.last_error = Some(e.to_string());
    }

    /// For the pool's circuit breaker to record an error
    #[allow(dead_code)]
    pub async fn record_error(&self, err: &str) {
        let mut inner = self.mu.lock().await;
        inner.last_error = Some(err.to_string());
    }

    pub async fn is_active(&self) -> bool {
        self.mu.lock().await.status == SessionStatus::Active
    }

    pub async fn instance_id(&self) -> Option<String> {
        self.mu.lock().await.instance_id.clone()
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        let inner = self.mu.lock().await;
        SessionSnapshot {
            status: format!("{:?}", inner.status).to_lowercase(),
            instance_id: inner.instance_id.clone(),
            model: inner.model.clone(),
            expires_at: inner.expires_at.map(|d| d.to_rfc3339()),
            position: inner.position,
            queue_depth: inner.queue_depth,
            last_error: inner.last_error.clone(),
            updated_at: inner
                .last_poll_at
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
            heartbeat_count: self.heartbeat_count.load(Ordering::Relaxed) as u64,
            ad_renewals: self.ad_renewals.load(Ordering::Relaxed) as u64,
        }
    }

    /// Background keep-alive loop: heartbeat while active + ad refresh before expiry
    pub async fn run_keepalive(self: Arc<Self>, ads: Arc<crate::ads::AdRefresher>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let hb = Duration::from_secs(SESSION_HEARTBEAT_INTERVAL_SEC);
        loop {
            tokio::time::sleep(hb).await;
            if self.instance_id().await.is_none() {
                continue;
            }
            if let Some(id) = self.instance_id().await {
                // Heartbeat
                if self.client.heartbeat(&self.token, &id).await.is_ok() {
                    self.heartbeat_count.fetch_add(1, Ordering::Relaxed);
                }
                // If close to expiry, try an ad refresh
                let near_expiry = {
                    let inner = self.mu.lock().await;
                    match inner.expires_at {
                        Some(exp) => {
                            let remain = (exp - Utc::now()).num_seconds();
                            remain < 120 && remain > 0
                        }
                        None => false,
                    }
                };
                if near_expiry && !self.cfg.ad_providers.is_empty() {
                    let _ = ads.refresh(&self.token).await;
                    self.ad_renewals.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}
