//! Web protocol client (freebuff.com)
//!
//! Reverse-engineered from the Freebuff web app's network traffic:
//! - POST /api/chat/stream  -- Cookie auth, SSE streaming (11 event types)
//! - POST /api/chat/upload   -- multipart upload -> convex storageId -> images
//! - GET  /api/web/freebuff-session -- freebucks / plan / per-model daily limits
//! - GET  /api/web/usage-summary -- usage summary (streak/tokens/sessionsByModel)
//! - GET  /api/auth/session   -- user info
//! - GET  /api/web/subscriptions -- plan tiers
//! - GET  /api/web/convex-token -- token renewal (short-lived JWT)
//!
//! Auth relies on Cookie (__Secure-next-auth.session-token etc.), no Bearer.

use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const WEB_HOST: &str = "https://freebuff.com";

/// FNV-1a 64-bit (zero dependency, stable across versions; same algorithm as `import::cred_id`, so the fingerprint is reproducible)
fn fnv1a64(s: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Fingerprint seed: takes the session-token value (falls back to the whole Cookie if absent).
/// The seed only participates in local hashing; the raw Cookie is never sent out.
fn session_seed(cookie: &str) -> String {
    cookie
        .split(';')
        .map(str::trim)
        .find(|p| p.starts_with("__Secure-next-auth.session-token="))
        .map(|p| {
            p.trim_start_matches("__Secure-next-auth.session-token=")
                .to_string()
        })
        .unwrap_or_else(|| cookie.to_string())
}

/// Upstream packet captures show the client sends `x-freebuff-instance-id` (UUID-shaped).
/// The gateway previously never sent this header at all -- present in captures but missing on
/// our side is one of the differences most likely to be flagged by risk control.
/// Derived here from the Cookie: the same account always gets the same value on every startup,
/// different accounts get different values.
pub fn instance_id_for_cookie(cookie: &str) -> String {
    let seed = session_seed(cookie);
    let a = fnv1a64(&seed);
    let b = fnv1a64(&format!("{seed}#instance"));
    let hex = format!("{a:016x}{b:016x}"); // 32 ASCII hex characters
    format!(
        "{}-{}-4{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[derive(Debug, Clone)]
pub struct WebClient {
    http: reqwest::Client,
    pub cookie: String,
    pub model: String,
    /// Upstream host (defaults to WEB_HOST; tests can inject a local mock address)
    base_host: String,
    /// Instance id derived from the Cookie (upstream `x-freebuff-instance-id`; stable per account, different across accounts)
    instance_id: String,
    /// The threadId from the most recent upstream meta/title event in this stream (for multi-turn continuation).
    /// When concurrent streams share the same WebClient, the last writer wins.
    last_thread_id: Arc<Mutex<Option<String>>>,
    /// Upstream embedded-error bypass for a 200 response (the most recently detected error envelope on this stream).
    /// Detected by encode_block before deserialization -- the bridge layer's tail only contains converted chunks, so this is the only visible point.
    last_upstream_error: Arc<Mutex<Option<String>>>,
}

/// chat/stream SSE events (11 types in the web app)
///
/// Note: upstream JSON field names are camelCase (`threadId`/`toolCallId`/`accessTier`),
/// and `rename_all` only affects variant names, so `rename_all_fields` must be used to map
/// field names too, otherwise fields with underscores silently fall back to None.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ChatEvent {
    Meta {
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        access_tier: Option<String>,
    },
    Title {
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
    },
    ReasoningDelta {
        text: String,
    },
    Delta {
        text: String,
    },
    Suggestions {
        #[serde(default)]
        tool_call_id: Option<String>,
        #[serde(default)]
        followups: Vec<Followup>,
    },
    AgentStart {
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        agent_type: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
    },
    AgentTool {
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        tool_call_id: Option<String>,
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default)]
        label: Option<String>,
    },
    AgentToolDone {
        #[serde(default)]
        tool_call_id: Option<String>,
    },
    AgentDelta {
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },
    AgentFinish {
        #[serde(default)]
        agent_id: Option<String>,
    },
    Button,
    Done,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Followup {
    pub prompt: String,
    pub label: String,
}

/// Accumulated streaming result
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamResult {
    pub thread_id: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub access_tier: Option<String>,
    pub reasoning: String,
    pub text: String,
    pub suggestions: Vec<Followup>,
    pub tools: Vec<String>,
    pub done: bool,
    /// Intermediate state for converting to OpenAI tool_calls (agent_tool -> agent_tool_done pairs)
    #[serde(default)]
    pub tool_calls: Vec<ToolCallState>,
}

/// Upstream agent_tool event -> OpenAI tool-call state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallState {
    pub id: String,
    pub name: String,
    pub label: String,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityContext {
    pub user_data: GravityUserData,
    #[serde(rename = "event_source_url")]
    pub event_source_url: String,
    #[serde(rename = "client_context")]
    pub client_context: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityUserData {
    #[serde(rename = "visitor_id")]
    pub visitor_id: String,
    #[serde(rename = "session_id")]
    pub session_id: String,
    #[serde(rename = "client_user_agent")]
    pub client_user_agent: String,
}

impl GravityContext {
    /// Derives a deterministic fingerprint from the Cookie (stable per account, different across accounts).
    /// Previously hardcoded the visitor_id/session_id from a packet capture, so all users shared the same
    /// fingerprint -- easily flagged by upstream risk control as the same client.
    /// Uses the session-token value from the Cookie as the seed (falls back to the whole string), computed
    /// locally only, the raw Cookie is never sent out.
    pub fn for_cookie(cookie: &str) -> Self {
        let seed = session_seed(cookie);
        let v = fnv1a64(&seed);
        let v2 = fnv1a64(&format!("{seed}#client-ctx"));
        let mut ctx = Self::default();
        ctx.user_data.visitor_id = format!("gruid_{v:016x}{:08x}", v.rotate_left(21));
        ctx.user_data.session_id = format!(
            "gr_sess_{:016x}{:08x}",
            v.rotate_right(13),
            v ^ 0x9E37_79B9_7F4A_7C15
        );
        // The client environment is also derived per account: all accounts sharing the same screen/viewport/hardware would also be an identifiable trait
        let pick = |salt: u64, lo: u64, hi: u64| -> u64 {
            lo + (fnv1a64(&format!("{seed}#{salt}")) % (hi - lo + 1))
        };
        let screen_w = pick(1, 1366, 2560);
        let screen_h = pick(2, 768, 1440);
        let viewport_w = pick(3, 1024, 1600);
        let viewport_h = pick(4, 720, 1000);
        let dpr_choices = [1.0_f64, 1.25, 1.5, 2.0];
        let mem_choices = [8_u64, 16, 16, 32, 32];
        let hw_choices = [4_u64, 8, 12, 16, 24];
        let dpr = dpr_choices[(v2 % 4) as usize];
        let device_memory = mem_choices[(v % 5) as usize];
        let hardware_concurrency = hw_choices[(v2 % 5) as usize];
        ctx.client_context = serde_json::json!({
            "timezone": "Asia/Shanghai",
            "screen": {"width": screen_w, "height": screen_h, "color_depth": 24, "pixel_depth": 24},
            "viewport": {"width": viewport_w, "height": viewport_h},
            "device_pixel_ratio": dpr,
            "platform": "Windows",
            "device_memory": device_memory,
            "hardware_concurrency": hardware_concurrency,
            "max_touch_points": null,
            "connection": {"effective_type": "4g", "downlink": 8.3, "rtt": 250, "save_data": false},
            "font": "Arial",
            "webgl": null,
            "fonts": ["Arial", "Segoe UI", "Consolas"],
            "audio_fingerprint": "0",
            "navigator_ext": {"languages": ["zh-CN", "zh"], "webdriver": false, "pdf_viewer": true, "cookies_enabled": true},
            "math_fingerprint": "0"
        });
        ctx
    }
}

impl Default for GravityContext {
    fn default() -> Self {
        Self {
            user_data: GravityUserData {
                visitor_id: "gruid_20uznrh75l83tnm3".into(),
                session_id: "gr_sess_5o0e95gndmgtdd5l".into(),
                client_user_agent: crate::upstream::DESKTOP_UA.into(),
            },
            event_source_url: "https://freebuff.com/chat".into(),
            client_context: serde_json::json!({
                "timezone": "Asia/Shanghai",
                "screen": {"width": 1707, "height": 1067, "color_depth": 24, "pixel_depth": 24},
                "viewport": {"width": 1036, "height": 906},
                "device_pixel_ratio": 1.5,
                "platform": "Windows",
                "device_memory": 32,
                "hardware_concurrency": 24,
                "max_touch_points": null,
                "connection": {"effective_type": "4g", "downlink": 8.3, "rtt": 250, "save_data": false},
                "font": "Arial",
                "webgl": null,
                "fonts": ["Arial", "Segoe UI", "Consolas"],
                "audio_fingerprint": "0",
                "navigator_ext": {"languages": ["zh-CN", "zh"], "webdriver": false, "pdf_viewer": true, "cookies_enabled": true},
                "math_fingerprint": "0"
            }),
        }
    }
}

impl WebClient {
    pub fn new(cookie: String, model: String) -> Result<Self> {
        Self::with_host(cookie, model, WEB_HOST)
    }

    /// Construct with a given upstream host (defaults to `WEB_HOST`; tests override with a local mock address)
    pub fn with_host(cookie: String, model: String, base_host: &str) -> Result<Self> {
        // Streaming responses are not affected by the overall timeout: reqwest's .timeout() is a
        // "whole request" timeout and would cut off a stream mid-flight
        // (fixed in v0.7.3: the previous 300s total timeout would hard-kill a long stream that was
        //  still producing increments, which the client saw as ERR_INCOMPLETE_CHUNKED_ENCODING).
        // Same approach as upstream.rs: read_timeout = per-chunk read timeout -- keeps reading as
        // long as increments keep arriving, only cuts off after 5 minutes of total silence.
        let http = reqwest::Client::builder()
            .read_timeout(Duration::from_secs(300))
            .user_agent(crate::upstream::DESKTOP_UA)
            .connect_timeout(Duration::from_secs(15))
            .cookie_store(true)
            .build()?;
        let instance_id = instance_id_for_cookie(&cookie);
        Ok(Self {
            http,
            cookie,
            model,
            base_host: base_host.trim_end_matches('/').to_string(),
            instance_id,
            last_thread_id: Arc::new(Mutex::new(None)),
            last_upstream_error: Arc::new(Mutex::new(None)),
        })
    }

    /// The threadId from the most recent upstream meta/title event in a web stream.
    /// Can be used for the next request's `thread_id` to continue the conversation; returns None if never received.
    pub fn last_thread_id(&self) -> Option<String> {
        self.last_thread_id.lock().ok().and_then(|g| g.clone())
    }

    /// Internal: threadId slot (clones the Arc for the stream closure to hold, avoids borrowing self)
    fn thread_slot(&self) -> Arc<Mutex<Option<String>>> {
        self.last_thread_id.clone()
    }

    /// Internal: upstream embedded-error bypass slot (same pattern as thread_slot)
    fn error_slot(&self) -> Arc<Mutex<Option<String>>> {
        self.last_upstream_error.clone()
    }

    /// The upstream embedded error detected in this WebClient's most recent stream (200 OK + error envelope).
    /// Read by the bridge layer at stream end, used to accurately record the failure reason in telemetry/logs.
    pub fn last_upstream_error(&self) -> Option<String> {
        self.last_upstream_error.lock().ok().and_then(|g| g.clone())
    }

    fn headers(&self, json: bool) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&self.cookie) {
            h.insert("cookie", v);
        }
        h.insert("origin", HeaderValue::from_static("https://freebuff.com"));
        h.insert(
            "referer",
            HeaderValue::from_static("https://freebuff.com/chat"),
        );
        h.insert("accept", HeaderValue::from_static("*/*"));
        // The upstream web app sends this header on every request (advanced-features.txt:610/804); its absence is one of the differences most likely to be flagged by risk control
        if let Ok(v) = HeaderValue::from_str(&self.instance_id) {
            h.insert("x-freebuff-instance-id", v);
        }
        if json {
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        h
    }

    /// Web chat stream (SSE, real incremental passthrough). Returns a per-event body stream.
    /// Each upstream data: event is converted to an OpenAI chunk in real time, no aggregation or buffering.
    /// The threadId carried by upstream meta/title events is exposed via [`WebClient::last_thread_id`]
    /// (readable after the stream ends / after a meta event, for multi-turn continuation).
    pub async fn chat_stream_raw(
        &self,
        thread_id: Option<&str>,
        content: &str,
        reasoning_effort: Option<&str>,
        images: Vec<WebImage>,
        attachments: Vec<WebAttachment>,
    ) -> Result<axum::body::Body> {
        use futures::StreamExt;
        let content = fit_web_limit(content);
        let body = serde_json::json!({
            "threadId": thread_id,
            "content": content,
            "model": self.model,
            "reasoningEffort": reasoning_effort,
            "gravity": GravityContext::for_cookie(&self.cookie),
            "images": images,
            "attachments": attachments,
        });
        let url = format!("{}/api/chat/stream", self.base_host);
        let resp = self
            .http
            .post(&url)
            .headers(self.headers(true))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "web chat HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        let byte_stream = resp.bytes_stream();
        let sse_buf: Vec<u8> = Vec::new();
        // Per-stream converter: tool_calls index allocation + threadId recording + upstream embedded-error bypass
        let error_slot = self.error_slot();
        let encoder = StreamEncoder::new(self.thread_slot(), error_slot);
        let stream = futures::stream::unfold(
            (byte_stream, sse_buf, false, encoder),
            |(mut stream, mut buf, finished, mut enc)| async move {
                if finished {
                    return None;
                }
                loop {
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            buf.extend_from_slice(&chunk);
                            // Slice out complete event blocks (blank-line delimited), convert each to an OpenAI chunk (real increments)
                            let mut out_line = String::new();
                            let mut got_done = false;
                            while let Some(pos) = find_double_newline(&buf) {
                                let event_bytes: Vec<u8> = buf.drain(..pos).collect();
                                let block = String::from_utf8_lossy(&event_bytes);
                                let (text, done) = enc.encode_block(&block);
                                out_line.push_str(&text);
                                if done {
                                    got_done = true;
                                    break;
                                }
                            }
                            if got_done {
                                // encode_block already emitted the finish_reason chunk + [DONE]
                                return Some((
                                    Ok::<_, std::io::Error>(axum::body::Bytes::from(out_line)),
                                    (stream, buf, true, enc),
                                ));
                            }
                            if !out_line.is_empty() {
                                return Some((
                                    Ok::<_, std::io::Error>(axum::body::Bytes::from(out_line)),
                                    (stream, buf, false, enc),
                                ));
                            }
                            // No complete event left in buf, keep receiving the next chunk
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err::<_, std::io::Error>(std::io::Error::other(e.to_string())),
                                (stream, buf, false, enc),
                            ));
                        }
                        None => {
                            // Upstream ended without a done event -> emit the finish_reason chunk + [DONE] ourselves
                            let out = format!("{}data: [DONE]\n\n", enc.finish_chunk());
                            return Some((
                                Ok::<_, std::io::Error>(axum::body::Bytes::from(out)),
                                (stream, buf, true, enc),
                            ));
                        }
                    }
                }
            },
        );
        Ok(axum::body::Body::from_stream(stream))
    }

    /// Web chat stream (SSE aggregated version, returns the full text at once)
    pub async fn chat_stream(
        &self,
        thread_id: Option<&str>,
        content: &str,
        reasoning_effort: Option<&str>,
        images: Vec<WebImage>,
        attachments: Vec<WebAttachment>,
    ) -> Result<StreamResult> {
        let content = fit_web_limit(content);
        let body = serde_json::json!({
            "threadId": thread_id,
            "content": content,
            "model": self.model,
            "reasoningEffort": reasoning_effort,
            "gravity": GravityContext::for_cookie(&self.cookie),
            "images": images,
            "attachments": attachments,
        });
        let url = format!("{}/api/chat/stream", self.base_host);
        let resp = self
            .http
            .post(&url)
            .headers(self.headers(true))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "web chat HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let mut result = StreamResult::default();
        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        // Parse SSE line by line (data: {json} lines, blank line delimits events)
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buf.extend_from_slice(&chunk);
            // Split events on \n\n
            while let Some(pos) = find_double_newline(&buf) {
                let event_line = buf.drain(..pos).collect::<Vec<u8>>();
                let line_str = String::from_utf8_lossy(&event_line);
                for line in line_str.lines() {
                    let line = line.trim();
                    if let Some(json) = line.strip_prefix("data:") {
                        let json = json.trim();
                        if json.is_empty() || json == "[DONE]" {
                            continue;
                        }
                        if let Ok(event) = serde_json::from_str::<ChatEvent>(json) {
                            apply_event(&mut result, event);
                        }
                    }
                }
            }
        }
        // The aggregated path also records threadId (for later continuation)
        record_thread_id(&self.last_thread_id, result.thread_id.as_deref());
        Ok(result)
    }

    /// Upload a file (multipart) -> storageId; the model field is specified by the caller
    /// Response: `{kind:"image"|"document", storageId, url?(image), mediaType, name,
    ///        descriptionStorageId?(image), chars?(document), truncated?(document)}`
    pub async fn upload_with_model(
        &self,
        file_bytes: Vec<u8>,
        filename: &str,
        mime: &str,
        model: &str,
    ) -> Result<WebUploadResult> {
        let url = format!("{}/api/chat/upload", self.base_host);
        let form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(file_bytes)
                    .file_name(filename.to_string())
                    .mime_str(mime)?,
            )
            .text("model", model.to_string());
        let resp = self
            .http
            .post(&url)
            .headers(self.headers(false))
            .multipart(form)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "upload HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(resp.json().await?)
    }

    /// Upload a file (multipart) -> storageId (uses the client's configured default model)
    pub async fn upload(
        &self,
        file_bytes: Vec<u8>,
        filename: &str,
        mime: &str,
    ) -> Result<WebUploadResult> {
        self.upload_with_model(file_bytes, filename, mime, &self.model)
            .await
    }

    /// Delete an upstream conversation (per the user's note: the reverse proxy must not let these pile up and put load on upstream).
    ///
    /// The endpoint has been **verified in practice** (2026-09-11, real-credential probing):
    /// - `DELETE /api/chat/threads/{id}` -> a nonexistent route returns an HTML 404 page, while this
    ///   path returns JSON `{"error":"Not found"}`, meaning **the route exists**, it just didn't find
    ///   that thread; a successful delete returns 2xx.
    /// - `POST /api/chat/threads/delete` -> `405 Method Not Allowed` (swallowed by the dynamic route
    ///   `[id]`, with id="delete"), it is **not** a separate endpoint, so it's no longer used as a fallback.
    /// - Thread list: `GET /api/chat/threads` -> `{"threads":[{id,title,model,updated_at}],...}` (also verified).
    ///
    /// Returns `Ok(true)` if upstream confirms the delete; `Ok(false)` if upstream explicitly says not found; other failures return `Err`.
    pub async fn delete_thread(&self, thread_id: &str) -> Result<bool> {
        let url = format!("{}/api/chat/threads/{thread_id}", self.base_host);
        let resp = self
            .http
            .delete(&url)
            .headers(self.headers(false))
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(true);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            // The route exists but the thread is no longer upstream -- treat as "nothing left to clean up"
            return Ok(false);
        }
        Err(anyhow!("Failed to delete upstream conversation: DELETE {url} -> HTTP {status}"))
    }

    /// Query account freebucks / per-model limits (the web app's core balance endpoint)
    pub async fn freebuff_session(&self) -> Result<WebFreebuffSession> {
        let url = format!("{}/api/web/freebuff-session", self.base_host);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers(false))
            .send()
            .await?;
        parse_json(resp).await
    }

    /// Usage summary (streak/tokens/sessionsByModel)
    pub async fn usage_summary(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/web/usage-summary", self.base_host);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers(false))
            .send()
            .await?;
        parse_json(resp).await
    }

    /// User info
    pub async fn auth_session(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/auth/session", self.base_host);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers(false))
            .send()
            .await?;
        parse_json(resp).await
    }

    /// Subscription plans
    pub async fn subscriptions(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/web/subscriptions", self.base_host);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers(false))
            .send()
            .await?;
        parse_json(resp).await
    }

    /// Conversation list
    pub async fn threads(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/chat/threads", self.base_host);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers(false))
            .send()
            .await?;
        parse_json(resp).await
    }

    /// Short-lived JWT (GET /api/web/convex-token; includes email/name/access_tier/country_code, valid for ~5 minutes)
    /// Purpose: verify credential validity / keep-alive.
    pub async fn convex_token(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/web/convex-token", self.base_host);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers(false))
            .send()
            .await?;
        parse_json(resp).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebImage {
    #[serde(rename = "storageId")]
    pub storage_id: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub name: String,
    #[serde(rename = "descriptionStorageId", default)]
    pub description_storage_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAttachment {
    #[serde(rename = "storageId")]
    pub storage_id: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub name: String,
    #[serde(default)]
    pub chars: Option<i64>,
    #[serde(default)]
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebUploadResult {
    pub kind: String,
    #[serde(rename = "storageId")]
    pub storage_id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub name: String,
    #[serde(rename = "descriptionStorageId", default)]
    pub description_storage_id: Option<String>,
    #[serde(default)]
    pub chars: Option<i64>,
    #[serde(default)]
    pub truncated: Option<bool>,
}

/// Full account balance snapshot (parsed from the web app's session response)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebFreebuffSession {
    pub status: Option<String>,
    #[serde(rename = "accessTier")]
    pub access_tier: Option<String>,
    #[serde(default)]
    pub freebucks: Option<Freebucks>,
    #[serde(default)]
    pub subscription: Option<serde_json::Value>,
    #[serde(rename = "rateLimitsByModel", default)]
    pub rate_limits_by_model: Option<serde_json::Value>,
    #[serde(default)]
    pub referral: Option<serde_json::Value>,
    #[serde(rename = "countryCode", default)]
    pub country_code: Option<String>,
    #[serde(rename = "countryBlockReason", default)]
    pub country_block_reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Freebucks {
    #[serde(default)]
    pub balance: f64,
    #[serde(default)]
    pub daily: Option<FreebucksDaily>,
    #[serde(default)]
    pub wallet: Option<serde_json::Value>,
    #[serde(rename = "planId", default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub prices: Option<HashMap<String, f64>>,
    #[serde(rename = "priceNotices", default)]
    pub price_notices: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FreebucksDaily {
    #[serde(default)]
    pub limit: f64,
    #[serde(default)]
    pub spent: f64,
    #[serde(default)]
    pub remaining: f64,
    #[serde(rename = "resetAt")]
    pub reset_at: Option<String>,
}

/// Converts a single stream's upstream SSE event blocks -> OpenAI chunks.
///
/// Holds this stream's state:
/// - `tool_calls` index allocation (the same toolCallId reuses the same index, parallel tools don't overwrite each other)
/// - whether a tool call has occurred (determines the finish_reason of the terminal chunk)
/// - threadId recording slot (meta/title events)
struct StreamEncoder {
    tool_index: HashMap<String, usize>,
    next_tool_index: usize,
    has_tool_calls: bool,
    thread_id: Arc<Mutex<Option<String>>>,
    /// Upstream embedded-error bypass: if a line that fails to deserialize / is an unknown event
    /// contains an error envelope, it's recorded here (Critic-J P2-1: moved the detection point
    /// earlier, to where the raw upstream event is still visible)
    upstream_error: Option<String>,
    /// Error bypass slot (shared with WebClient, readable by the bridge layer after the stream ends)
    _error_slot: Arc<Mutex<Option<String>>>,
}

impl StreamEncoder {
    fn new(thread_id: Arc<Mutex<Option<String>>>, error_slot: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            tool_index: HashMap::new(),
            next_tool_index: 0,
            has_tool_calls: false,
            thread_id,
            upstream_error: None,
            _error_slot: error_slot,
        }
    }

    /// Read the upstream embedded error (if any). Not cleared after reading -- reading the same error again yields the same result.
    #[cfg(test)]
    pub fn take_upstream_error(&self) -> Option<String> {
        self.upstream_error.clone()
    }

    /// Allocate (or reuse) the OpenAI tool_calls index for a toolCallId.
    /// Returns (index, a non-empty OpenAI-side id): when upstream has no id, generates a deterministic id with `anon_{n}`.
    fn tool_slot(&mut self, raw_id: Option<&str>) -> (usize, String) {
        let key = match raw_id {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => format!("anon_{}", self.next_tool_index),
        };
        let index = match self.tool_index.get(&key) {
            Some(&i) => i,
            None => {
                let i = self.next_tool_index;
                self.tool_index.insert(key.clone(), i);
                self.next_tool_index += 1;
                i
            }
        };
        (index, format!("call_{key}"))
    }

    /// Terminal chunk: content is empty, finish_reason is tool_calls/stop depending on whether a tool call occurred.
    fn finish_chunk(&self) -> String {
        let reason = if self.has_tool_calls {
            "tool_calls"
        } else {
            "stop"
        };
        format!(
            "data: {}\n\n",
            serde_json::json!({ "object": "chat.completion.chunk", "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }] })
        )
    }

    /// One SSE event block (may contain multiple data: lines) -> OpenAI chunk text.
    /// Returns (output text, whether done was received); when done is received the output already includes the finish_reason chunk and the [DONE] sentinel.
    fn encode_block(&mut self, block: &str) -> (String, bool) {
        let mut text_parts: Vec<String> = Vec::new();
        let mut reasoning_parts: Vec<String> = Vec::new();
        let mut tool_parts: Vec<serde_json::Value> = Vec::new();
        let mut done = false;
        for line in block.lines() {
            let line = line.trim();
            let Some(json) = line.strip_prefix("data:") else {
                continue;
            };
            let json = json.trim();
            if json.is_empty() || json == "[DONE]" {
                continue;
            }
            // Detection point moved earlier (Critic-J P2-1): probe for an error envelope first
            // (this covers unknown events that would otherwise fail to deserialize) -- the bridge
            // layer's tail only contains converted chunks and can't see this, so this is the only
            // visible point for an upstream embedded error
            if self.upstream_error.is_none() {
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(json) {
                    if let Some(err) = raw.get("error") {
                        let text = err
                            .as_str()
                            .map(String::from)
                            .unwrap_or_else(|| err.to_string());
                        self.upstream_error = Some(text.clone());
                        if let Ok(mut slot) = self._error_slot.lock() {
                            *slot = Some(text); // bypass slot: readable by the bridge layer after the stream ends
                        }
                    }
                }
            }
            let Ok(event) = serde_json::from_str::<ChatEvent>(json) else {
                continue;
            };
            match event {
                ChatEvent::Delta { text } => text_parts.push(text),
                // Evidence from packet captures: text produced by a tool comes through agent_delta and must be passed through as content
                ChatEvent::AgentDelta {
                    text: Some(text), ..
                } => text_parts.push(text),
                ChatEvent::ReasoningDelta { text } => reasoning_parts.push(text),
                ChatEvent::AgentTool {
                    tool_name: Some(name),
                    tool_call_id,
                    ..
                } => {
                    let (index, id) = self.tool_slot(tool_call_id.as_deref());
                    self.has_tool_calls = true;
                    tool_parts.push(serde_json::json!({
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": "{}" }
                    }));
                }
                ChatEvent::Meta { thread_id, .. } | ChatEvent::Title { thread_id, .. } => {
                    record_thread_id(&self.thread_id, thread_id.as_deref());
                }
                ChatEvent::Done => done = true,
                _ => {}
            }
        }
        let mut out = String::new();
        for r in reasoning_parts {
            out += &format!(
                "data: {}\n\n",
                serde_json::json!({ "object": "chat.completion.chunk", "choices": [{ "index": 0, "delta": { "reasoning_content": r }, "finish_reason": null }] })
            );
        }
        for t in text_parts {
            out += &format!(
                "data: {}\n\n",
                serde_json::json!({ "object": "chat.completion.chunk", "choices": [{ "index": 0, "delta": { "content": t }, "finish_reason": null }] })
            );
        }
        for tc in &tool_parts {
            out += &format!(
                "data: {}\n\n",
                serde_json::json!({ "object": "chat.completion.chunk", "choices": [{ "index": 0, "delta": { "tool_calls": [tc] }, "finish_reason": null }] })
            );
        }
        if done {
            out += &self.finish_chunk();
            out += "data: [DONE]\n\n";
        }
        (out, done)
    }
}

/// Records the threadId given by upstream (empty string ignored; last write wins, since upstream meta/title repeatedly carry the same id)
fn record_thread_id(slot: &Mutex<Option<String>>, id: Option<&str>) {
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        if let Ok(mut g) = slot.lock() {
            *g = Some(id.to_string());
        }
    }
}

/// Event merging for the aggregation path (`chat_stream`).
fn apply_event(result: &mut StreamResult, event: ChatEvent) {
    match event {
        ChatEvent::Meta {
            thread_id,
            title,
            model,
            access_tier,
        } => {
            if let Some(t) = thread_id {
                result.thread_id = Some(t);
            }
            // Second title update: mid-stream, the original user text gets overwritten by the model's summary; last write wins
            if let Some(t) = title {
                result.title = Some(t);
            }
            if result.model.is_none() {
                result.model = model;
            }
            if result.access_tier.is_none() {
                result.access_tier = access_tier;
            }
        }
        ChatEvent::Title { thread_id, title } => {
            if let Some(t) = thread_id {
                result.thread_id = Some(t);
            }
            if let Some(t) = title {
                result.title = Some(t);
            }
        }
        ChatEvent::ReasoningDelta { text } => result.reasoning.push_str(&text),
        ChatEvent::Delta { text } => result.text.push_str(&text),
        // Tool-produced body text (agent_delta) shares the same channel as normal delta and must not be dropped
        ChatEvent::AgentDelta {
            text: Some(text), ..
        } => result.text.push_str(&text),
        ChatEvent::Suggestions { followups, .. } => result.suggestions = followups,
        ChatEvent::AgentTool {
            tool_name: Some(name),
            tool_call_id,
            label,
            ..
        } => {
            let raw = match tool_call_id.filter(|s| !s.is_empty()) {
                Some(id) => id,
                None => format!("anon_{}", result.tool_calls.len()),
            };
            let label = label.unwrap_or_default();
            result.tools.push(format!("{name}: {label}"));
            result.tool_calls.push(ToolCallState {
                id: format!("call_{raw}"),
                name,
                label,
                done: false,
            });
        }
        ChatEvent::AgentToolDone {
            tool_call_id: Some(id),
        } => {
            let id = format!("call_{id}");
            if let Some(tc) = result.tool_calls.iter_mut().find(|t| t.id == id) {
                tc.done = true;
            }
        }
        ChatEvent::AgentStart { agent_type, .. } => result
            .tools
            .push(format!("agent_start: {}", agent_type.unwrap_or_default())),
        ChatEvent::Done => result.done = true,
        _ => {}
    }
}

/// Locates the end of an event block (including the separating blank line). Handles both LF and CRLF SSE encodings.
fn find_double_newline(buf: &[u8]) -> Option<usize> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

async fn parse_json<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    serde_json::from_str(&text).map_err(|e| anyhow!("failed to parse response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_slot() -> Arc<Mutex<Option<String>>> {
        Arc::new(Mutex::new(None))
    }

    /// Chunks and encodes the way a real stream would: split on \n\n (or \r\n\r\n), feeding the encoder block by block
    fn drive(enc: &mut StreamEncoder, raw: &[u8]) -> String {
        let mut buf = raw.to_vec();
        let mut out = String::new();
        while let Some(pos) = find_double_newline(&buf) {
            let block: Vec<u8> = buf.drain(..pos).collect();
            let (text, done) = enc.encode_block(&String::from_utf8_lossy(&block));
            out.push_str(&text);
            if done {
                break;
            }
        }
        out
    }

    /// Extracts OpenAI chunks (skipping the [DONE] sentinel)
    fn chunks(out: &str) -> Vec<serde_json::Value> {
        out.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn contains(out: &str, needle: &str) -> bool {
        out.contains(needle)
    }

    // ---------- agent_delta → content ----------

    #[test]
    fn agent_delta_maps_to_content_delta() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let (out, done) = enc.encode_block(
            "data: {\"type\":\"agent_delta\",\"agentId\":\"a1\",\"text\":\"tool result\"}\n\n",
        );
        assert!(!done);
        let cs = chunks(&out);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0]["choices"][0]["delta"]["content"], "tool result");
        assert!(cs[0]["choices"][0]["delta"]
            .get("reasoning_content")
            .is_none());
    }

    #[test]
    fn agent_delta_interleaves_with_delta_in_order() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let raw = concat!(
            "data: {\"type\":\"delta\",\"text\":\"A\"}\n\n",
            "data: {\"type\":\"agent_delta\",\"agentId\":\"a\",\"text\":\"B\"}\n\n",
            "data: {\"type\":\"delta\",\"text\":\"C\"}\n\n",
        );
        let cs = chunks(&drive(&mut enc, raw.as_bytes()));
        let text: String = cs
            .iter()
            .map(|c| c["choices"][0]["delta"]["content"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(text, "ABC");
    }

    // ---------- tool_calls index / id ----------

    #[test]
    fn parallel_agent_tools_get_incrementing_indexes() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let mut raw = String::new();
        for i in 0..5 {
            raw += &format!("data: {{\"type\":\"agent_tool\",\"agentId\":\"a\",\"toolCallId\":\"t{i}\",\"toolName\":\"web_search\",\"label\":\"l{i}\"}}\n\n");
        }
        let cs = chunks(&drive(&mut enc, raw.as_bytes()));
        assert_eq!(cs.len(), 5);
        for (i, c) in cs.iter().enumerate() {
            let tc = &c["choices"][0]["delta"]["tool_calls"][0];
            assert_eq!(tc["index"], i as i64, "parallel tool indexes must increment");
            assert_eq!(tc["id"], format!("call_t{i}"));
            assert_eq!(tc["function"]["name"], "web_search");
            assert_eq!(tc["type"], "function");
        }
    }

    #[test]
    fn repeated_tool_call_id_reuses_index() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let raw = concat!(
            "data: {\"type\":\"agent_tool\",\"toolCallId\":\"x\",\"toolName\":\"web_search\"}\n\n",
            "data: {\"type\":\"agent_tool_done\",\"toolCallId\":\"x\"}\n\n",
            "data: {\"type\":\"agent_tool\",\"toolCallId\":\"y\",\"toolName\":\"read_url\"}\n\n",
            "data: {\"type\":\"agent_tool\",\"toolCallId\":\"x\",\"toolName\":\"web_search\"}\n\n",
        );
        let cs = chunks(&drive(&mut enc, raw.as_bytes()));
        assert_eq!(cs.len(), 3);
        let idx: Vec<i64> = cs
            .iter()
            .map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["index"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(idx, vec![0, 1, 0], "the same toolCallId must reuse its index");
    }

    #[test]
    fn missing_tool_call_id_yields_non_empty_unique_id() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let raw = concat!(
            "data: {\"type\":\"agent_tool\",\"toolName\":\"web_search\"}\n\n",
            "data: {\"type\":\"agent_tool\",\"toolName\":\"read_url\"}\n\n",
        );
        let cs = chunks(&drive(&mut enc, raw.as_bytes()));
        let ids: Vec<String> = cs
            .iter()
            .map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            ids,
            vec!["call_anon_0".to_string(), "call_anon_1".to_string()]
        );
        let idx: Vec<i64> = cs
            .iter()
            .map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["index"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(idx, vec![0, 1]);
    }

    // ---------- finish_reason + [DONE] ----------

    #[test]
    fn done_after_tool_call_finishes_with_tool_calls_reason() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let raw = concat!(
            "data: {\"type\":\"agent_tool\",\"toolCallId\":\"t1\",\"toolName\":\"web_search\"}\n\n",
            "data: {\"type\":\"done\"}\n\n",
        );
        let out = drive(&mut enc, raw.as_bytes());
        assert!(
            out.ends_with("data: [DONE]\n\n"),
            "the [DONE] sentinel must follow done"
        );
        let cs = chunks(&out);
        let last = cs.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            last["choices"][0]["delta"],
            serde_json::json!({}),
            "the terminal chunk's delta must be empty"
        );
    }

    #[test]
    fn done_without_tools_finishes_with_stop_reason() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let raw = concat!(
            "data: {\"type\":\"delta\",\"text\":\"hi\"}\n\n",
            "data: {\"type\":\"done\"}\n\n",
        );
        let out = drive(&mut enc, raw.as_bytes());
        let cs = chunks(&out);
        let last = cs.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "stop");
        assert_eq!(cs.len(), 2);
    }

    #[test]
    fn upstream_eof_without_done_still_emits_terminal_chunk() {
        // Upstream EOF without a done: caller uses finish_chunk() to backfill (the None branch of chat_stream_raw)
        let enc = StreamEncoder::new(new_slot(), new_slot());
        let out = format!("{}data: [DONE]\n\n", enc.finish_chunk());
        assert!(out.ends_with("data: [DONE]\n\n"));
        assert_eq!(chunks(&out)[0]["choices"][0]["finish_reason"], "stop");
    }

    // ---------- threadId ----------

    #[test]
    fn meta_and_title_record_thread_id() {
        let slot = new_slot();
        let mut enc = StreamEncoder::new(slot.clone(), new_slot());
        enc.encode_block("data: {\"type\":\"meta\",\"threadId\":\"d8557501\",\"title\":\"用户原文\",\"model\":\"deepseek-v4-flash\",\"accessTier\":\"limited\"}\n\n");
        assert_eq!(slot.lock().unwrap().clone(), Some("d8557501".to_string()));
        // Second title update (same threadId)
        enc.encode_block("data: {\"type\":\"title\",\"threadId\":\"d8557501\",\"title\":\"请求搜索GitHub用户仓库\"}\n\n");
        assert_eq!(slot.lock().unwrap().clone(), Some("d8557501".to_string()));
        // An empty threadId must not overwrite the existing value
        enc.encode_block("data: {\"type\":\"title\",\"threadId\":\"\",\"title\":\"x\"}\n\n");
        assert_eq!(slot.lock().unwrap().clone(), Some("d8557501".to_string()));
    }

    #[test]
    fn web_client_exposes_last_thread_id() {
        let client = WebClient::new(
            "__Secure-next-auth.session-token=x".into(),
            "glm-5.3-flash".into(),
        )
        .unwrap();
        assert_eq!(client.last_thread_id(), None);
        let mut enc = StreamEncoder::new(client.thread_slot(), new_slot());
        enc.encode_block("data: {\"type\":\"meta\",\"threadId\":\"th-1\",\"title\":\"t\"}\n\n");
        assert_eq!(client.last_thread_id().as_deref(), Some("th-1"));
    }

    // ---------- Aggregation path ----------

    #[test]
    fn aggregate_title_late_update_wins() {
        let mut r = StreamResult::default();
        apply_event(
            &mut r,
            ChatEvent::Meta {
                thread_id: Some("t".into()),
                title: Some("用户原文".into()),
                model: None,
                access_tier: None,
            },
        );
        apply_event(
            &mut r,
            ChatEvent::Title {
                thread_id: Some("t".into()),
                title: Some("模型摘要".into()),
            },
        );
        assert_eq!(r.title.as_deref(), Some("模型摘要"));
        // A None new value must not clear the old value
        apply_event(
            &mut r,
            ChatEvent::Title {
                thread_id: None,
                title: None,
            },
        );
        assert_eq!(r.title.as_deref(), Some("模型摘要"));
    }

    #[test]
    fn aggregate_agent_delta_appends_content() {
        let mut r = StreamResult::default();
        apply_event(&mut r, ChatEvent::Delta { text: "A".into() });
        apply_event(
            &mut r,
            ChatEvent::AgentDelta {
                agent_id: Some("a".into()),
                text: Some("B".into()),
            },
        );
        apply_event(&mut r, ChatEvent::ReasoningDelta { text: "R".into() });
        assert_eq!(r.text, "AB");
        assert_eq!(r.reasoning, "R");
    }

    #[test]
    fn aggregate_tool_call_id_prefixed_and_done_marked() {
        let mut r = StreamResult::default();
        apply_event(
            &mut r,
            ChatEvent::AgentTool {
                agent_id: None,
                tool_call_id: Some("t1".into()),
                tool_name: Some("web_search".into()),
                label: Some("github".into()),
            },
        );
        apply_event(
            &mut r,
            ChatEvent::AgentToolDone {
                tool_call_id: Some("t1".into()),
            },
        );
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "call_t1");
        assert!(r.tool_calls[0].done);
        assert_eq!(r.tools[0], "web_search: github");
    }

    // ---------- Full captured sequence ----------

    #[test]
    fn full_captured_sequence_preserves_agent_output() {
        let raw = concat!(
            "data: {\"type\":\"meta\",\"threadId\":\"d8557501\",\"title\":\"请你帮我调用工具一下联网搜索\",\"model\":\"deepseek-v4-flash\",\"accessTier\":\"limited\"}\n\n",
            "data: {\"type\":\"reasoning_delta\",\"text\":\"The\"}\n\n",
            "data: {\"type\":\"delta\",\"text\":\"我来\"}\n\n",
            "data: {\"type\":\"agent_start\",\"agentId\":\"h2\",\"parentAgentId\":\"main-agent\",\"name\":\"Web Researcher\",\"agentType\":\"researcher-web\",\"prompt\":\"Search\"}\n\n",
            "data: {\"type\":\"agent_tool\",\"agentId\":\"h2\",\"toolCallId\":\"c1\",\"toolName\":\"web_search\",\"label\":\"github lza6\"}\n\n",
            "data: {\"type\":\"agent_tool_done\",\"toolCallId\":\"c1\"}\n\n",
            "data: {\"type\":\"agent_delta\",\"agentId\":\"h2\",\"text\":\"Based on\"}\n\n",
            "data: {\"type\":\"agent_delta\",\"agentId\":\"h2\",\"text\":\" research\"}\n\n",
            "data: {\"type\":\"agent_finish\",\"agentId\":\"h2\"}\n\n",
            "data: {\"type\":\"title\",\"threadId\":\"d8557501\",\"title\":\"请求搜索GitHub用户仓库\"}\n\n",
            "data: {\"type\":\"delta\",\"text\":\"。\"}\n\n",
            "data: {\"type\":\"suggestions\",\"toolCallId\":\"h2v\",\"followups\":[{\"prompt\":\"p\",\"label\":\"l\"}]}\n\n",
            "data: {\"type\":\"done\"}\n\n",
        );
        let slot = new_slot();
        let mut enc = StreamEncoder::new(slot.clone(), new_slot());
        let out = drive(&mut enc, raw.as_bytes());

        assert!(out.ends_with("data: [DONE]\n\n"));
        assert_eq!(slot.lock().unwrap().clone(), Some("d8557501".to_string()));
        let cs = chunks(&out);
        // Body = delta + agent_delta (tool output must not be lost)
        let text: String = cs
            .iter()
            .map(|c| c["choices"][0]["delta"]["content"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(text, "我来Based on research。");
        // Reasoning goes through reasoning_content separately
        let reasoning: String = cs
            .iter()
            .map(|c| {
                c["choices"][0]["delta"]["reasoning_content"]
                    .as_str()
                    .unwrap_or("")
            })
            .collect();
        assert_eq!(reasoning, "The");
        // The tool call exists and its index is correct
        let tc = cs
            .iter()
            .find_map(|c| c["choices"][0]["delta"]["tool_calls"].as_array().cloned())
            .unwrap();
        assert_eq!(tc[0]["index"], 0);
        assert_eq!(tc[0]["id"], "call_c1");
        // Terminal chunk
        let last = cs.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert!(contains(&out, "\"object\":\"chat.completion.chunk\""));
    }

    // ---------- SSE chunking ----------

    #[test]
    fn find_double_newline_handles_lf_and_crlf() {
        assert_eq!(find_double_newline(b"a\n\nb"), Some(3));
        assert_eq!(find_double_newline(b"a\r\n\r\nb"), Some(5));
        assert_eq!(find_double_newline(b"a\n\r\nb"), None);
        assert_eq!(find_double_newline(b"no separator"), None);
    }

    #[test]
    fn crlf_stream_is_split_correctly() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let raw =
            "data: {\"type\":\"delta\",\"text\":\"hi\"}\r\n\r\ndata: {\"type\":\"done\"}\r\n\r\n";
        let out = drive(&mut enc, raw.as_bytes());
        assert_eq!(chunks(&out).len(), 2);
        assert!(out.ends_with("data: [DONE]\n\n"));
    }

    // ---------- WebUploadResult parsing ----------

    #[test]
    fn parses_image_upload_response() {
        let v: WebUploadResult = serde_json::from_str(
            r#"{"kind":"image","storageId":"kg278","url":"https://harmless-tapir-303.convex.cloud/api/storage/c45e","mediaType":"image/png","name":"logo.png","descriptionStorageId":"kg2ay"}"#,
        )
        .unwrap();
        assert_eq!(v.kind, "image");
        assert_eq!(v.storage_id, "kg278");
        assert_eq!(
            v.url.as_deref(),
            Some("https://harmless-tapir-303.convex.cloud/api/storage/c45e")
        );
        assert_eq!(v.description_storage_id.as_deref(), Some("kg2ay"));
        assert!(v.chars.is_none() && v.truncated.is_none());
    }

    #[test]
    fn parses_document_upload_response() {
        let v: WebUploadResult = serde_json::from_str(
            r#"{"kind":"document","storageId":"kg2d4d","mediaType":"text/plain","name":"a.txt","chars":2460,"truncated":false}"#,
        )
        .unwrap();
        assert_eq!(v.kind, "document");
        assert_eq!(v.chars, Some(2460));
        assert_eq!(v.truncated, Some(false));
        assert!(v.url.is_none() && v.description_storage_id.is_none());
    }

    /// Upstream 200-with-embedded-error bypass (Critic-J P2-1 closure): the error envelope is captured inside encode_block
    #[test]
    fn upstream_error_bypass_captured() {
        let client = WebClient::new(
            "__Secure-next-auth.session-token=x".into(),
            "glm-5.3-flash".into(),
        )
        .unwrap();
        let mut enc = StreamEncoder::new(client.thread_slot(), client.error_slot());
        // A plain error envelope that fails to deserialize (no type field)
        let (out, _) = enc.encode_block(
            "data: {\"error\":\"Unauthorized\"}

",
        );
        assert!(out.is_empty(), "an error event should produce no output");
        assert_eq!(
            client.last_upstream_error().as_deref(),
            Some("Unauthorized"),
            "the bypass slot must be readable (the bridge layer's only visible point)"
        );
        // A known event variant carrying an extra error field
        let mut enc2 = StreamEncoder::new(new_slot(), new_slot());
        enc2.encode_block(
            "data: {\"type\":\"meta\",\"error\":\"rate limited\"}

",
        );
        assert_eq!(client_cases(enc2), Some("rate limited".to_string()));
    }

    fn client_cases(enc: StreamEncoder) -> Option<String> {
        enc.take_upstream_error()
    }

    /// Unmapped events (button/unknown) must produce no output and never error
    #[test]
    fn unknown_events_are_ignored() {
        let mut enc = StreamEncoder::new(new_slot(), new_slot());
        let (out, done) = enc.encode_block(
            "data: {\"type\":\"button\"}\n\ndata: {\"type\":\"brand_new_event\",\"x\":1}\n\n",
        );
        assert!(out.is_empty());
        assert!(!done);
    }

    #[test]
    fn instance_id_is_stable_uuid_shaped_and_per_account() {
        let c1 = "__Secure-next-auth.session-token=aaa-bbb; other=1";
        let c2 = "__Secure-next-auth.session-token=ccc-ddd; other=1";
        let a = instance_id_for_cookie(c1);
        assert_eq!(
            a,
            instance_id_for_cookie(c1),
            "the same account must be stable (otherwise every restart looks like a new device)"
        );
        assert_ne!(a, instance_id_for_cookie(c2), "different accounts must have different instance ids");
        // UUID shape: 8-4-4-4-12, with the third segment starting with 4 (version bit)
        let parts: Vec<&str> = a.split('-').collect();
        assert_eq!(parts.len(), 5, "must be UUID-shaped: {a}");
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "UUID segment lengths are wrong: {a}"
        );
        assert!(parts[2].starts_with('4'), "version bit should be 4: {a}");
        assert!(
            a.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-'),
            "must contain only hex digits and hyphens: {a}"
        );
    }

    #[test]
    fn instance_id_falls_back_to_whole_cookie() {
        // Without the standard session-token naming, the whole Cookie string is used as the seed; still must be stable and non-empty
        let a = instance_id_for_cookie("foo=1; bar=2");
        assert!(!a.is_empty());
        assert_eq!(a, instance_id_for_cookie("foo=1; bar=2"));
    }

    #[test]
    fn gravity_fingerprint_differs_between_accounts() {
        let a = GravityContext::for_cookie("__Secure-next-auth.session-token=acc-a");
        let b = GravityContext::for_cookie("__Secure-next-auth.session-token=acc-b");
        assert_ne!(a.user_data.visitor_id, b.user_data.visitor_id);
        assert_ne!(a.user_data.session_id, b.user_data.session_id);
        // Two calls for the same account must be consistent (otherwise every request looks like a device change)
        let a2 = GravityContext::for_cookie("__Secure-next-auth.session-token=acc-a");
        assert_eq!(a.user_data.visitor_id, a2.user_data.visitor_id);
        assert_eq!(a.client_context, a2.client_context);
    }
}

/// Upstream rejects `content` over 32,000 characters (`message_too_long`); counted in JS UTF-16 units,
/// so budget below that in chars. Keeps the head (system instructions) and the tail (latest turns).
pub const WEB_CONTENT_MAX_CHARS: usize = 30_000;
const WEB_CONTENT_HEAD_CHARS: usize = 6_000;

// Last-resort cut for a single oversized message; flatten_messages already drops whole old turns first
pub fn fit_web_limit(content: &str) -> std::borrow::Cow<'_, str> {
    let total = content.chars().count();
    if total <= WEB_CONTENT_MAX_CHARS {
        return std::borrow::Cow::Borrowed(content);
    }
    let marker = format!("\n\n[... {} characters of earlier context omitted ...]\n\n", total - WEB_CONTENT_MAX_CHARS);
    let tail_len = WEB_CONTENT_MAX_CHARS - WEB_CONTENT_HEAD_CHARS - marker.chars().count();
    let head: String = content.chars().take(WEB_CONTENT_HEAD_CHARS).collect();
    let tail: String = content.chars().skip(total - tail_len).collect();
    std::borrow::Cow::Owned(format!("{head}{marker}{tail}"))
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn fit_web_limit_caps_long_content() {
        assert_eq!(fit_web_limit("short"), "short");
        let long = format!("HEAD{}TAIL", "x".repeat(50_000));
        let out = fit_web_limit(&long);
        assert!(out.chars().count() <= WEB_CONTENT_MAX_CHARS);
        assert!(out.starts_with("HEAD") && out.ends_with("TAIL"));
        assert!(out.contains("omitted"));
    }
}
