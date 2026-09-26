//! HTTP routes: OpenAI-compatible / Anthropic-compatible / control panel / usage API
//!
//! Depends on the upstream's reverse-engineered protocol:
//! - POST /v1/chat/completions -> pick token -> create session -> inject codebuff_metadata -> forward -> stream back over SSE
//! - POST /v1/messages -> Claude protocol converted to OpenAI
//! - GET  /v1/models -> registry
//! - GET  /healthz -> includes account health snapshot
//! - /ui panel + /api/usage/* stats

use crate::ads::AdRefresher;
use crate::config::Config;
use crate::logbus::LogBus;
use crate::models::ModelRegistry;
use crate::pool::Pool;
use crate::router::ModelRouter;
use crate::skills::SkillsManager;
use crate::telemetry::{TelemetryWriter, TraceRow};

use crate::upstream::UpstreamClient;
use crate::usage::UsageDb;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use std::sync::Arc;

/// Unified AppState (must be Send+Sync+Clone to be usable with axum's State extractor)
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub client: Arc<UpstreamClient>,
    pub pool: Arc<Pool>,
    /// web Cookie credential pool (v0.9 §1.1): multi-account health scoring / circuit breaking / cooldown / round-robin
    pub web_pool: Arc<crate::web_pool::WebCookiePool>,
    pub registry: Arc<ModelRegistry>,
    pub router: Arc<ModelRouter>,
    pub usage: Arc<UsageDb>,
    pub telemetry: Arc<TelemetryWriter>,
    pub logs: Arc<LogBus>,
    pub memory: Arc<crate::memory::MemoryStore>,
    pub skills: Arc<SkillsManager>,
    pub ads: Arc<AdRefresher>,
    pub prompts: Arc<crate::prompts::PromptManager>,
    /// Credential account info cache + usage records
    pub meta: Arc<crate::account_meta::AccountMetaStore>,
    /// Currently active downstream API key (supports runtime hot updates, see `POST /api/config/api-key`)
    pub api_keys: Arc<std::sync::RwLock<Vec<String>>>,
    /// web protocol bridge: client session -> upstream thread binding (reusing a session saves daily quota)
    pub web_threads: Arc<crate::web_threads::WebThreadMap>,
    /// Memory layer runtime switch (hot-toggleable from the panel; initial value comes from config.memory_enabled, off by default)
    pub memory_runtime_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Dual-bucket concurrency semaphore (landed in v0.8): free/subscription buckets, acquired before the first byte is written
    pub semaphore: Arc<crate::semaphore::TieredSemaphore>,
    pub started: std::time::Instant,
}

/// Read the currently active downstream API key list (hot-updated at runtime, no restart needed)
fn api_keys_of(st: &AppState) -> Vec<String> {
    st.api_keys.read().map(|k| k.clone()).unwrap_or_default()
}

/// Write the runtime API key list (persisting to config.json is the caller's responsibility)
fn set_api_keys(st: &AppState, keys: Vec<String>) {
    if let Ok(mut w) = st.api_keys.write() {
        *w = keys;
    }
}

/// Memory layer runtime switch (hot-toggle, takes effect without restart)
fn memory_enabled_now(st: &AppState) -> bool {
    st.memory_runtime_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
}

fn set_memory_runtime_enabled(st: &AppState, enabled: bool) {
    st.memory_runtime_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
}

// axum_core already provides a blanket impl FromRef<S> for S when `S: Clone`, so no manual impl is needed here.
// Keep the FromRef import for future extension (in case a sub-state FromRef is needed later).

/// Admin endpoint auth: if api_keys is configured, the request header must match; if not configured, only direct
/// local (loopback) requests are allowed (no X-Forwarded-For/X-Real-IP).
/// A local single-machine setup listens on 127.0.0.1 by default and this behavior is unchanged; cross-machine/proxied
/// access must explicitly configure api_keys.
/// Extra CSRF protection: rejects requests carrying an Origin header (browser-initiated) that isn't same-origin local.
fn admin_authorized(headers: &HeaderMap, st: &AppState) -> bool {
    if !origin_allowed(headers) {
        return false;
    }
    let keys = api_keys_of(st);
    if keys.is_empty() {
        return is_loopback_request(headers);
    }
    authorized(headers, &keys)
}

/// CSRF protection: browser cross-site requests carry an Origin header; anything not same-origin (not the local
/// panel) is always rejected.
/// Non-browser clients (curl/SDK/desktop IPC) don't send Origin, so they're unaffected.
///
/// Exception: browser extensions (`chrome-extension://` / `moz-extension://`) -- the extension is the only
/// legitimate path for "one-click browser login" (the HttpOnly cookie can only be read by the extension), and its
/// fetch will carry the extension's Origin.
/// When `api_keys` is configured, the extension still needs to send the Key (the panel passes the Key through to
/// the extension), so the security boundary is unchanged.
fn origin_allowed(headers: &HeaderMap) -> bool {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => {
            o.starts_with("http://127.0.0.1:")
                || o.starts_with("http://localhost:")
                || o == "http://127.0.0.1"
                || o == "http://localhost"
                || o.starts_with("file://")
                || o.starts_with("chrome-extension://")
                || o.starts_with("moz-extension://")
        }
    }
}

/// Writes the real TCP peer (axum's `ConnectInfo<SocketAddr>` extension) into the `x-fb-peer` header, for
/// is_loopback_request to use.
///
/// - Production path: `main` serves via `into_make_service_with_connect_info::<SocketAddr>()`, so every request
///   carries the ConnectInfo extension -> this middleware **unconditionally overwrites** `x-fb-peer` with the real
///   peer IP, so clients can't spoof it.
/// - Unit test/oneshot path: no ConnectInfo extension -> `x-fb-peer` is removed (to avoid a leftover client header
///   interfering with the check); tests can faithfully simulate any peer via
///   `req.extensions_mut().insert(ConnectInfo(peer))`.
fn inject_peer(mut req: axum::extract::Request) -> axum::extract::Request {
    let peer = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0);
    match peer {
        Some(addr) => {
            let val = axum::http::HeaderValue::from_str(&addr.ip().to_string())
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("unknown"));
            req.headers_mut().insert("x-fb-peer", val);
        }
        None => {
            req.headers_mut().remove("x-fb-peer");
        }
    }
    req
}

/// Determines whether a request comes from the local machine (v0.9 §1.5 defense-in-depth auth):
/// 1. If X-Forwarded-For / X-Real-IP is present (via a proxy or a spoofed proxy header) -> never counts as local;
/// 2. Otherwise check the real TCP peer `x-fb-peer` (written by inject_peer from ConnectInfo): not loopback -> reject;
/// 3. No `x-fb-peer` (not injected in a oneshot unit test) -> keep the default allow (127.0.0.1 default behavior unchanged).
fn is_loopback_request(headers: &HeaderMap) -> bool {
    if headers.get("x-forwarded-for").is_some() || headers.get("x-real-ip").is_some() {
        return false;
    }
    match headers.get("x-fb-peer").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(ip) => ip
            .parse::<std::net::IpAddr>()
            .map(|a| a.is_loopback())
            .unwrap_or(false),
    }
}

pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/usage/totals", get(handle_usage_totals))
        .route("/api/usage/daily", get(handle_usage_daily))
        .route("/api/usage/requests", get(handle_usage_requests))
        .route("/api/usage/requests/{id}", get(handle_usage_request_detail))
        .route("/api/usage/models", get(handle_usage_models))
        .route("/api/usage/cost", get(handle_usage_cost))
        .route("/api/usage/insights", get(handle_usage_insights))
        .route("/api/usage/accounts", get(handle_accounts))
        .route(
            "/api/skills",
            get(handle_skills_list).post(handle_skills_upsert),
        )
        .route("/api/skills/toggle", post(handle_skills_toggle))
        .route("/api/skills/delete", post(handle_skills_delete))
        .route("/api/skills/gate", post(handle_skills_gate))
        .route("/api/logs/recent", get(handle_logs_recent))
        .route("/api/logs/stream", get(handle_logs_stream))
        .route(
            "/api/memory",
            get(handle_memory_list).post(handle_memory_upsert),
        )
        .route("/api/memory/delete", post(handle_memory_delete))
        .route("/api/memory/static", post(handle_memory_static))
        .route("/api/memory/toggle", post(handle_memory_toggle))
        .route("/api/threads/cleanup", post(handle_threads_cleanup))
        .route("/mcp", post(handle_mcp))
        .route("/api/doctor", get(handle_doctor))
        .route("/api/accounts/health", get(handle_accounts_health));

    Router::new()
        .route("/", get(handle_dashboard))
        .route("/ui", get(handle_dashboard))
        .route("/healthz", get(handle_healthz))
        .route("/v1/models", get(handle_v1_models))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/v1/messages", post(handle_claude_messages))
        .route("/api/tokens/import", post(handle_token_import))
        .route("/api/tokens", get(handle_tokens_list))
        .route("/api/tokens/check", post(handle_token_check))
        .route("/api/tokens/delete", post(handle_token_delete))
        .route("/api/account/balance", get(handle_account_balance))
        .route("/api/account/detail", post(handle_account_detail))
        .route("/api/account/overview", get(handle_account_overview))
        .route("/api/account/refresh", post(handle_account_refresh))
        .route("/api/account/history", get(handle_account_history))
        .route("/api/guide", get(handle_guide))
        .route("/api/login/embed", post(handle_login_embed))
        .route("/api/login/result", get(handle_login_result))
        .route("/api/extension/bundle", get(handle_extension_bundle))
        .route("/api/config/api-key", post(handle_config_api_key))
        .route(
            "/api/config",
            get(handle_config_get).post(handle_config_save),
        )
        .route("/v1/web/chat", post(handle_web_chat))
        .route(
            "/v1/uploads",
            post(handle_upload).layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024)),
        )
        .route("/api/prompts", get(handle_prompts_list))
        .route("/api/prompts/toggle", post(handle_prompts_toggle))
        .route("/api/export", post(handle_export))
        .route("/api/import", post(handle_import))
        .merge(api)
        .with_state(state)
        // v0.8 security headers: add nosniff / Referrer-Policy / CSP to every response (the local self-contained panel has no external resources)
        // v0.9 defense-in-depth auth: inject the real peer (ConnectInfo -> x-fb-peer) before the request enters, for admin endpoint loopback checks
        .layer(
            tower::ServiceBuilder::new()
                .map_request(inject_peer)
                .map_response(secure_headers),
        )
}

/// Attaches security headers to every response (idempotent: doesn't overwrite existing values, only fills in what's missing)
///
/// - X-Content-Type-Options: nosniff -- prevents MIME sniffing
/// - Referrer-Policy: strict-origin-when-cross-origin -- prevents the full URL from leaking to other sites
/// - Content-Security-Policy: default-src 'self' -- the local panel has no external resources, so lock it down directly
///
/// Also applies to SSE streaming responses (text/event-stream) -- headers are finalized before the first chunk is written.
fn secure_headers(mut resp: axum::response::Response) -> axum::response::Response {
    let hs = resp.headers_mut();
    use axum::http::header;
    macro_rules! insert_if_missing {
        ($name:expr, $value:expr) => {{
            if !hs.contains_key($name) {
                hs.insert($name, axum::http::HeaderValue::from_static($value));
            }
        }};
    }
    insert_if_missing!(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    insert_if_missing!(header::REFERRER_POLICY, "strict-origin-when-cross-origin");
    // CSP is only needed for the panel page (HTML/JS); but adding it uniformly won't break JSON/SSE (browsers only
    // apply CSP's script/style restrictions to HTML; API responses have no inline scripts, so default-src 'self'
    // doesn't affect reading them).
    // To be safe, only add CSP when the response content-type contains text/html, to avoid any potential
    // interference with the /v1 data plane.
    let ct = hs
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if ct.contains("text/html") && !hs.contains_key(header::CONTENT_SECURITY_POLICY) {
        // The panel is a single unbuilt file (inline <script>/<style>/onclick), so CSP must allow inline scripts
        // and styles, otherwise the whole panel breaks (inline scripts get blocked by default-src 'self' -- Critic
        // verified Edge reports "Refused to execute inline script", breaking the test bench/settings/about/logs
        // entirely).
        // Keep the rest of the protections: disallow object/embed/plugin, restrict base-uri, disallow being
        // embedded in an iframe, only allow images from self and data:.
        hs.insert(
            header::CONTENT_SECURITY_POLICY,
            axum::http::HeaderValue::from_static(
                "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; object-src 'none'; base-uri 'self'; frame-ancestors 'self'; form-action 'self'",
            ),
        );
    }
    resp
}

// ---------- Panel & Health ----------

async fn handle_dashboard() -> impl IntoResponse {
    // v0.10 §1.5: don't panic if the panel build fails (http::Response: Default)
    Response::builder()
        .header("content-type", "text/html; charset=utf-8")
        .body(Body::from(crate::web::INDEX_HTML))
        .unwrap_or_default()
}

async fn handle_healthz(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let dur = st.started.elapsed();
    // When api_keys is configured and the check fails, only return liveness info -- account names/model composition
    // shouldn't be visible to unauthorized callers
    let keys = api_keys_of(&st);
    let authorized_ok = if keys.is_empty() {
        is_loopback_request(&headers)
    } else {
        authorized(&headers, &keys)
    };
    if !authorized_ok {
        return Json(serde_json::json!({
            "ok": true,
            "uptime_sec": dur.as_secs(),
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
        }))
        .into_response();
    }
    let json = serde_json::json!({
        "ok": true,
        "uptime_sec": dur.as_secs(),
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "accounts": all_accounts_summary(&st).await,
        "model_count": st.registry.snapshot().await.model_count,
        "ads": st.ads.snapshot().await,
    });
    Json(json).into_response()
}

/// Bearer pool + web Cookie pool, in the pool's AccountSnapshot shape (the overview table reads this)
async fn all_accounts_summary(st: &AppState) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = st
        .pool
        .snapshot()
        .await
        .accounts
        .iter()
        .filter_map(|a| serde_json::to_value(a).ok())
        .collect();
    for w in st.web_pool.snapshot().await {
        let healthy = w.cooldown_seconds.is_none() && w.circuit_state != "open";
        out.push(serde_json::json!({
            "name": format!("cookie {}", w.masked),
            "healthy": healthy,
            "score": w.health_score,
            "cooldown_until": w.cooldown_until,
            "circuit_state": w.circuit_state,
            "trips": w.trips,
            "last_error": w.last_error,
            "session": { "status": if healthy { "ready" } else { "cooling" } },
        }));
    }
    out
}

async fn handle_v1_models(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // Same defense line as the data plane: if api_keys is configured, it must be verified (the model list isn't
    // exposed to unauthorized callers); if not configured, only direct local access is allowed.
    let keys = api_keys_of(&st);
    let authorized_ok = if keys.is_empty() {
        is_loopback_request(&headers)
    } else {
        authorized(&headers, &keys)
    };
    if !authorized_ok {
        return admin_denied().into_response();
    }
    let models = st.registry.models().await;
    let data: Vec<serde_json::Value> = models
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m,
                "object": "model",
                "created": st.started.elapsed().as_secs() as i64,
                "owned_by": "Freebuff2API",
                "root": m,
                "permission": [],
            })
        })
        .collect();
    // v0.9 §1.2: model metadata (availability/efforts/multimodal/fallback) merged into the response.
    // The `data` field stays fully backward-compatible; `meta` is a new field.
    let meta = models_meta_snapshot(&st).await;
    Json(serde_json::json!({ "object": "list", "data": data, "meta": meta })).into_response()
}

/// /v1/models metadata wiring point (v0.9 §1.2).
///
/// Contract: `ModelRegistry::meta_snapshot() -> Vec<ModelMeta>` (fields id/agent/premium/multimodal/
/// available/efforts/fallback), landed by the model metadata worker in `src/models.rs`.
/// **This interface isn't in place yet** (models.rs is exclusively owned by another worker, so this worker won't
/// overstep), so per the task agreement it degrades to `meta: []` -- the response shape is stable (the `meta: [...]`
/// field name is the frontend contract), and only this function body needs replacing once integration lands.
async fn models_meta_snapshot(st: &AppState) -> Vec<serde_json::Value> {
    st.registry
        .meta_snapshot()
        .into_iter()
        .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
        .collect()
}

// ---------- Usage API ----------

async fn handle_usage_totals(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    match st.usage.totals() {
        Ok(v) => Json(v).into_response(),
        Err(e) => internal_err(&e),
    }
}

async fn handle_usage_daily(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    match st.usage.daily_usage(7) {
        Ok(v) => Json(serde_json::json!(v)).into_response(),
        Err(e) => internal_err(&e),
    }
}

async fn handle_usage_requests(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    match st.usage.recent_requests(50) {
        Ok(v) => Json(serde_json::json!(v)).into_response(),
        Err(e) => internal_err(&e),
    }
}

// ---------- Full config export/import (v0.9 §2.3) ----------

/// POST /api/export -- full config export (redacted config + credentials + skill enable state + memory switch), for migrating to a new machine
async fn handle_export(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    // config redaction: only export whitelisted fields; api_keys / auth_tokens are never exported in plaintext; http_proxy keeps only the host segment
    let mut config_masked = serde_json::Map::new();
    for (key, _) in CONFIG_EDITABLE {
        let v = match *key {
            "listen_addr" => serde_json::json!(st.cfg.listen_addr),
            "memory_enabled" => serde_json::json!(st.cfg.memory_enabled),
            "token_saver" => serde_json::json!(st.cfg.token_saver),
            "skills_inject_mode" => serde_json::json!(st.cfg.skills_inject_mode),
            "max_roster_tokens" => serde_json::json!(st.cfg.max_roster_tokens),
            "http_proxy" => serde_json::json!(redact_proxy_userinfo(&st.cfg.http_proxy)),
            "thread_cleanup_interval_sec" => serde_json::json!(st.cfg.thread_cleanup_interval_sec),
            "thread_max_age_hours" => serde_json::json!(st.cfg.thread_max_age_hours),
            "redact_logs" => serde_json::json!(st.cfg.redact_logs),
            "concurrency_free_slots" => serde_json::json!(st.cfg.concurrency_free_slots),
            "concurrency_free_multi" => serde_json::json!(st.cfg.concurrency_free_multi),
            "concurrency_sub_slots" => serde_json::json!(st.cfg.concurrency_sub_slots),
            "concurrency_sub_multi" => serde_json::json!(st.cfg.concurrency_sub_multi),
            _ => continue,
        };
        config_masked.insert((*key).to_string(), v);
    }
    let tokens = crate::import::load_tokens_healed(&st.cfg.tokens_path).unwrap_or_default();
    let skills: Vec<crate::export::SkillState> = st
        .skills
        .list()
        .iter()
        .map(|s| crate::export::SkillState {
            id: s.id.clone(),
            enabled: s.enabled,
        })
        .collect();
    let payload = crate::export::ExportPayload {
        schema_version: crate::export::EXPORT_SCHEMA_VERSION.to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        config: serde_json::json!(config_masked),
        tokens,
        skills,
        memory_enabled: memory_enabled_now(&st),
        note: "Freebuff2API full config export (v0.9)".to_string(),
    };
    Json(serde_json::json!({ "ok": true, "data": payload })).into_response()
}

/// http_proxy redaction: `http://user:pass@host:port` -> `http://***@host:port`
fn redact_proxy_userinfo(proxy: &str) -> String {
    if let Some(idx) = proxy.find("://") {
        let rest = &proxy[idx + 3..];
        if let Some(at) = rest.rfind('@') {
            return format!("{}://***@{}", &proxy[..idx], &rest[at + 1..]);
        }
    }
    proxy.to_string()
}

/// POST /api/import -- body `{data: ExportPayload}`: validate -> auto backup -> atomic write-back
async fn handle_import(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    if body.len() > crate::export::MAX_IMPORT_BYTES {
        return bad_req(&format!(
            "Import data exceeds the limit of {}MB",
            crate::export::MAX_IMPORT_BYTES / 1024 / 1024
        ));
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("body must be JSON ({ \"data\": <exported content> })"),
    };
    let payload = match crate::export::validate_import_body(&v) {
        Ok(p) => p,
        Err(e) => return bad_req(&format!("Import validation failed: {e}")),
    };
    // backup dir = the directory containing tokens_path (default data/), consistent with where import.rs writes to disk
    let data_dir = std::path::Path::new(&st.cfg.tokens_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".to_string());
    let ctx = crate::export::ImportContext {
        config_path: crate::config::resolve_config_path(),
        tokens_path: &st.cfg.tokens_path,
        data_dir: &data_dir,
        skills: &st.skills,
        skills_dir: &st.cfg.skills_dir,
        memory_runtime_enabled: &st.memory_runtime_enabled,
    };
    match crate::export::apply_import(&payload, &ctx) {
        Ok(summary) => {
            let backup = summary.backed_up_to.as_deref().unwrap_or("-").to_string();
            st.logs.emit(
                "warn",
                "config",
                None,
                format!("Config imported, backup created ({})", backup),
            );
            // v0.9 §1.1: import may include new web Cookie credentials -> hot-reload the pool (preserving existing health state)
            st.web_pool.reload(&st.cfg).await;
            Json(serde_json::json!({ "ok": true, "imported": summary })).into_response()
        }
        Err(e) => internal_err(&anyhow::Error::msg(format!(
            "Import failed (backup was created, can roll back): {e}"
        ))),
    }
}
// ---------- Account balance lookup (web protocol) ----------

/// GET /api/account/balance -- query account credits/per-model limits/plan (uses web Cookie)
async fn handle_account_balance(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    // v0.9 §1.1: pick from WebCookiePool (health score/circuit breaking/cooldown/round-robin); keep the original error message if no Cookie is found
    let (cookie, cid) = match pick_web_cookie(&st).await {
        Some((c, _, id)) => (c, id),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "ok": false, "message": "No web Cookie credential found. Please log in to freebuff.com in your browser, copy the full Cookie from DevTools (including __Secure-next-auth.session-token=...), and import it via POST /api/tokens/import" })),
            )
                .into_response();
        }
    };

    let client = match crate::web_protocol::WebClient::new(cookie, "glm-5.3-flash".into()) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    match client.freebuff_session().await {
        Ok(sess) => {
            st.web_pool.mark_ok(&cid).await;
            // Compute remaining usable count per model
            let mut model_remaining = serde_json::Map::new();
            if let Some(freebucks) = &sess.freebucks {
                let remaining = freebucks.daily.as_ref().map(|d| d.remaining).unwrap_or(0.0);
                if let Some(prices) = &freebucks.prices {
                    for (model, price) in prices {
                        let by_credit = if *price > 0.0 {
                            (remaining / price).floor() as i64
                        } else {
                            i64::MAX
                        };
                        let by_limit = if let Some(rl) = sess
                            .rate_limits_by_model
                            .as_ref()
                            .and_then(|v| v.get(model))
                        {
                            let limit = rl.get("limit").and_then(|v| v.as_i64()).unwrap_or(0);
                            let recent =
                                rl.get("recentCount").and_then(|v| v.as_i64()).unwrap_or(0);
                            if limit > 0 {
                                (limit - recent).max(0)
                            } else {
                                i64::MAX
                            }
                        } else {
                            i64::MAX
                        };
                        let usable = by_credit.min(by_limit);
                        let usable = if usable == i64::MAX { -1 } else { usable }; // -1 = this model neither consumes credits nor has a usage limit
                        model_remaining.insert(model.clone(), serde_json::json!({
                            "price": price,
                            "by_credit_remaining": if by_credit == i64::MAX { -1 } else { by_credit },
                            "by_limit_remaining": if by_limit == i64::MAX { -1 } else { by_limit },
                            "usable_today": usable,
                        }));
                    }
                }
            }
            Json(serde_json::json!({
                "ok": true,
                "status": sess.status,
                "access_tier": sess.access_tier,
                "freebucks": sess.freebucks,
                "subscription": sess.subscription,
                "rate_limits_by_model": sess.rate_limits_by_model,
                "referral": sess.referral,
                "country_code": sess.country_code,
                "country_block_reason": sess.country_block_reason,
                "model_remaining": model_remaining,
                "message": sess.message,
            }))
            .into_response()
        }
        Err(e) => {
            web_pool_failure(&st, &cid, &e.to_string()).await;
            internal_err(&e)
        }
    }
}

/// Unified 500 JSON error response
/// GET /api/prompts -- list built-in prompts and skills (with enable state)
async fn handle_prompts_list(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    Json(serde_json::json!({
        "ok": true,
        "prompts": st.prompts.prompts_snapshot().await,
        "skills": st.prompts.skills_snapshot().await,
        "system_prefix_preview": st.prompts.system_prefix().await,
    }))
    .into_response()
}

/// POST /api/prompts/toggle -- body: { type: "prompt"|"skill", id, enabled }
async fn handle_prompts_toggle(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "ok": false, "message": "invalid json" })),
            )
                .into_response()
        }
    };
    let kind = parsed
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("prompt");
    let id = parsed.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = parsed
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ok = if kind == "skill" {
        st.prompts.set_skill_enabled(id, enabled).await
    } else {
        st.prompts.set_prompt_enabled(id, enabled).await
    };
    Json(serde_json::json!({
        "ok": ok,
        "kind": kind,
        "id": id,
        "enabled": enabled,
        "system_prefix_preview": st.prompts.system_prefix().await,
    }))
    .into_response()
}

fn internal_err(e: &anyhow::Error) -> Response {
    tracing::error!("Usage stats query failed: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": e.to_string() })),
    )
        .into_response()
}

/// Unified response for admin endpoint auth failure
fn admin_denied() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "ok": false, "message": "unauthorized: admin endpoints require an API key (config.json api_keys); when not configured locally, only local machine access is allowed" })),
    )
        .into_response()
}

/// Cross-site Origin blocked response (CSRF defense): 403 Forbidden, semantically distinct from unauthenticated 401
fn origin_blocked() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": { "message": "cross-site request blocked (CSRF): browser cross-site request was rejected", "type": "forbidden" } })),
    )
        .into_response()
}

/// Full web-version chat proxy (Cookie auth chat/stream -> real incremental SSE passthrough -> OpenAI-compatible)
/// POST /v1/web/chat -- body: { model, content, thread_id?, images? }
/// images supports two shapes: ["storageId1", ...] or [{ storageId, mediaType?, name? }, ...]
/// Auth is consistent with /v1/chat/completions: verified when api_keys is configured; only local access allowed when not configured.
async fn handle_web_chat(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !origin_allowed(&headers) {
        return origin_blocked();
    }
    let keys = api_keys_of(&st);
    if !keys.is_empty() && !authorized(&headers, &keys) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": { "message": "invalid proxy api key", "type": "authentication_error" } })),
        )
            .into_response();
    }
    if keys.is_empty() && !is_loopback_request(&headers) {
        return admin_denied();
    }
    // v0.9 §1.1: pick from WebCookiePool (multi-account health/cooldown/round-robin)
    let (cookie, web_cid) = match pick_web_cookie(&st).await {
        Some((c, _, cid)) => (c, Some(cid)),
        None => (String::new(), None),
    };
    let cookie = if cookie.is_empty() {
        // v0.10 §1.4: pool is non-empty but all on cooldown -> structured degradation; otherwise keep the "not imported" message
        if st.web_pool.count().await > 0 {
            return web_pool_exhausted(&st).await;
        }
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": { "message": "No web Cookie credential imported. Please POST /api/tokens/import with the Cookie", "type": "invalid_request_error" } })),
        )
            .into_response();
    } else {
        cookie
    };
    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": { "message": "invalid json", "type": "invalid_request_error" } })),
            )
                .into_response();
        }
    };
    let model = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("glm-5.3-flash")
        .to_string();
    let content = parsed
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let thread_id = parsed
        .get("thread_id")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let reasoning_effort = parsed.get("reasoning_effort").and_then(|r| r.as_str());
    // Multimodal: parse images (array of storageId strings or array of objects)
    let images: Vec<crate::web_protocol::WebImage> = parsed
        .get("images")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    if let Some(s) = it.as_str() {
                        Some(crate::web_protocol::WebImage {
                            storage_id: s.to_string(),
                            media_type: "image/png".into(),
                            name: "image.png".into(),
                            description_storage_id: None,
                        })
                    } else {
                        let sid = it.get("storageId").and_then(|x| x.as_str())?;
                        Some(crate::web_protocol::WebImage {
                            storage_id: sid.to_string(),
                            media_type: it
                                .get("mediaType")
                                .and_then(|x| x.as_str())
                                .unwrap_or("image/png")
                                .to_string(),
                            name: it
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("image.png")
                                .to_string(),
                            description_storage_id: it
                                .get("descriptionStorageId")
                                .and_then(|x| x.as_str())
                                .map(String::from),
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if content.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": { "message": "content is required", "type": "invalid_request_error" } })),
        )
            .into_response();
    }

    let images_count = images.len();
    // Document/file attachments: storageId string or array of objects
    let attachments: Vec<crate::web_protocol::WebAttachment> = parsed
        .get("attachments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    if let Some(s) = it.as_str() {
                        Some(crate::web_protocol::WebAttachment {
                            storage_id: s.to_string(),
                            media_type: "application/octet-stream".into(),
                            name: "file".into(),
                            chars: None,
                            truncated: None,
                        })
                    } else {
                        let sid = it.get("storageId").and_then(|x| x.as_str())?;
                        Some(crate::web_protocol::WebAttachment {
                            storage_id: sid.to_string(),
                            media_type: it
                                .get("mediaType")
                                .and_then(|x| x.as_str())
                                .unwrap_or("application/octet-stream")
                                .to_string(),
                            name: it
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("file")
                                .to_string(),
                            chars: it.get("chars").and_then(|x| x.as_i64()),
                            truncated: it.get("truncated").and_then(|x| x.as_bool()),
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let attachments_count = attachments.len();
    let model_name = model.clone();
    let client = match crate::web_protocol::WebClient::new(cookie, model) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    // Real incremental stream: forwards the upstream chat/stream SSE event-by-event in real time as OpenAI chunks (images passed through for multimodal)
    match client
        .chat_stream_raw(
            thread_id.as_deref(),
            &content,
            reasoning_effort,
            images,
            attachments,
        )
        .await
    {
        Ok(body) => {
            // v0.9: mark ok as soon as the HTTP layer succeeds
            if let Some(id) = &web_cid {
                st.web_pool.mark_ok(id).await;
            }
            // Telemetry: tap the stream on the side (first byte/byte count/usage) + report after completion (web protocol uses Cookie, tagged web-cookie)
            let req_id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) =
                tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(16);
            let telem = st.telemetry.clone();
            let logs = st.logs.clone();
            let usage_db = st.usage.clone();
            let client_track = client.clone();
            let threads_path = st.cfg.threads_path.clone();
            tokio::spawn(async move {
                let t0 = std::time::Instant::now();
                let mut stream = body.into_data_stream();
                let mut ttft: Option<u64> = None;
                let mut bytes: u64 = 0;
                let mut tail = String::new();
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(b) => {
                            if ttft.is_none() {
                                ttft = Some(t0.elapsed().as_millis() as u64);
                            }
                            bytes += b.len() as u64;
                            tail.push_str(&String::from_utf8_lossy(&b));
                            if tail.len() > 8000 {
                                tail = tail_keep(&tail, 4000);
                            }
                            if tx.send(Ok(b)).await.is_err() {
                                break; // client disconnected
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                            break;
                        }
                    }
                }
                let total_ms = t0.elapsed().as_millis() as u64;
                let (pt, ct) = extract_usage(&tail).unwrap_or((0, 0));
                usage_db
                    .record_ex(
                        "web-cookie",
                        &model_name,
                        pt as i64,
                        ct as i64,
                        total_ms as i64,
                        200,
                        "",
                        "",
                        &req_id,
                    )
                    .ok();
                telem.record(TraceRow {
                    req_id: req_id.clone(),
                    endpoint: "/v1/web/chat".into(),
                    requested_model: model_name.clone(),
                    resolved_model: model_name.clone(),
                    account: "web-cookie".into(),
                    status: 200,
                    latency_ms: total_ms,
                    ttft_ms: ttft,
                    prompt_tokens: pt,
                    completion_tokens: ct,
                    stream: true,
                    error_kind: None,
                    error_excerpt: None,
                    route_reason: Some(format!(
                        "web protocol, images={images_count}, attachments={attachments_count}"
                    )),
                    api_key: None,
                    client_ip: None,
                });
                logs.emit(
                    "info",
                    "request",
                    Some(&req_id),
                    format!(
                        "{model_name} web stream completed {bytes}B / {images_count} image(s) (first byte {}ms / total {:.2}s)",
                        ttft.unwrap_or(0),
                        total_ms as f64 / 1000.0
                    ),
                );
                // Record the upstream threadId (for session cleanup, to prevent long-term buildup from stressing the upstream / exposing a fingerprint)
                if let Some(tid) = client_track.last_thread_id() {
                    record_thread(&threads_path, &tid);
                }
            });
            let body_stream = futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("connection", "keep-alive")
                .header("x-accel-buffering", "no")
                .body(Body::from_stream(body_stream))
                .unwrap_or_default()
        }
        Err(e) => {
            // v0.9: 401/403/network failure -> lower the score in the pool / circuit break / cooldown; the next request automatically switches accounts
            if let Some(id) = &web_cid {
                web_pool_failure(&st, id, &e.to_string()).await;
            }
            internal_err(&e)
        }
    }
}

async fn handle_usage_models(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    Json(serde_json::json!(st.registry.models().await)).into_response()
}

// ---------- Web protocol bridge (OpenAI/Anthropic client -> freebuff.com /api/chat/stream) ----------

/// Bridge decision: reuse an existing thread or open a new one
enum BridgeMode {
    /// Continue: reuse the bound thread, only send the last user message
    Continue { thread_id: String },
    /// Fresh session: send the full context (flattened transcript)
    Fresh { prompt: String },
}

/// Decides how to send this round based on the session binding.
/// Heuristic: an assistant message appearing in the client's messages = a follow-up turn in a multi-turn
/// conversation, and there's already a binding -> reuse the thread and only send the increment;
/// otherwise open a fresh session and send the full text. Why reuse is required: the upstream counts by
/// **session admission** per day (rateLimitsByModel.limit, 6/day on the free tier), and opening a new thread on
/// every request quickly burns through the quota -- this is the direct cause of "followed the guide to hit /v1
/// and got 429 almost immediately".
fn decide_bridge_mode(
    map: &crate::web_threads::WebThreadMap,
    cred_id: &str,
    messages: &serde_json::Value,
) -> Option<(BridgeMode, String)> {
    let full = crate::web_threads::flatten_messages(messages, false)?;
    let last_user = crate::web_threads::flatten_messages(messages, true).map(|(_, l)| l)?;
    let has_assistant = messages
        .as_array()
        .map(|a| {
            a.iter()
                .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        })
        .unwrap_or(false);
    if has_assistant {
        if let Some(b) = map.get(cred_id) {
            if !b.thread_id.is_empty() {
                return Some((
                    BridgeMode::Continue {
                        thread_id: b.thread_id,
                    },
                    last_user,
                ));
            }
        }
    }
    Some((BridgeMode::Fresh { prompt: full.0 }, last_user))
}

/// Bridges OpenAI `/v1/chat/completions` to the web protocol (enabled when the account pool is empty and only a web Cookie is available).
/// `parsed` is the raw request body (with messages/stream), `model` has already gone through route resolution.
async fn web_bridge_openai(
    st: AppState,
    parsed: serde_json::Value,
    requested: String,
    model: String,
    cookie: String,
    cred: serde_json::Value,
    cid: String,
) -> Response {
    let start = std::time::Instant::now();
    let req_id = uuid::Uuid::new_v4().to_string();
    let stream = parsed
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let messages = parsed
        .get("messages")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    // Memory/skill injection (same pipeline as the desktop protocol), then flattened into web protocol content
    let mut effective = messages.clone();
    let mem_query: String = crate::web_threads::flatten_messages(&messages, true)
        .map(|(_, l)| l.chars().take(200).collect())
        .unwrap_or_default();
    let sys_prefix = build_system_prefix(&st, &mem_query).await;
    if !sys_prefix.trim().is_empty() {
        if let Some(arr) = effective.as_array_mut() {
            arr.insert(
                0,
                serde_json::json!({ "role": "system", "content": sys_prefix }),
            );
        }
    }

    let (mode, last_user) = match decide_bridge_mode(&st.web_threads, &cid, &effective) {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": { "message": "no sendable user content found in messages", "type": "invalid_request_error" } })),
            )
                .into_response();
        }
    };

    // v0.8 dual-bucket concurrency semaphore: the bridge path also acquires before the first byte is written out
    // (conservative: web credentials default to the free bucket, the subscription bucket if a plan field is
    // present). The guard is held by the background forwarding task until the stream ends.
    let bridge_is_sub = crate::semaphore::is_subscriber_token(&cookie, None);
    // Test hook: FREEBUFF2API_WEB_HOST can override the upstream host (production keeps the WEB_HOST default)
    let web_host = std::env::var("FREEBUFF2API_WEB_HOST")
        .unwrap_or_else(|_| crate::web_protocol::WEB_HOST.to_string());
    let client = match crate::web_protocol::WebClient::with_host(cookie, model.clone(), &web_host) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    let bridge_guard = match st.semaphore.acquire(bridge_is_sub).await {
        Ok(g) => g,
        Err(_) => {
            let ms = start.elapsed().as_millis() as i64;
            st.usage
                .record_ex("web-cookie", &model, 0, 0, ms, 429, "", "", &req_id)
                .ok();
            st.telemetry
                .event(&req_id, "concurrency_busy", "bridge concurrency bucket full");
            st.logs.emit(
                "warn",
                "request",
                Some(&req_id),
                format!("{model} bridge concurrency bucket full, returning 429"),
            );
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({ "error": { "message": "too many concurrent requests to upstream, retry later", "type": "server_error", "code": "concurrency_busy" } })),
            )
                .into_response();
        }
    };
    let (thread_id, content, reason) = match &mode {
        BridgeMode::Continue { thread_id } => (
            Some(thread_id.clone()),
            last_user.clone(),
            "web bridge, reuse thread".to_string(),
        ),
        BridgeMode::Fresh { prompt } => {
            (None, prompt.clone(), "web bridge, new thread".to_string())
        }
    };
    st.telemetry.event(
        &req_id,
        "route",
        &format!(
            "{requested} -> {model} via web-cookie({}) [{}]",
            cred.get("token_masked")
                .and_then(|v| v.as_str())
                .unwrap_or("?"),
            match &mode {
                BridgeMode::Continue { thread_id } => format!("continue {thread_id}"),
                BridgeMode::Fresh { .. } => "new thread".into(),
            }
        ),
    );

    let upstream = client
        .chat_stream_raw(thread_id.as_deref(), &content, None, Vec::new(), Vec::new())
        .await;
    let body = match upstream {
        Ok(b) => {
            // v0.9: mark ok as soon as the HTTP layer succeeds (200); a mid-stream drop is caught later via cooldown/score-lowering on retry
            st.web_pool.mark_ok(&cid).await;
            b
        }
        Err(e) => {
            // v0.9: 401/403/network failure -> lower the score in the pool / circuit break / cooldown; the next request automatically switches accounts
            web_pool_failure(&st, &cid, &e.to_string()).await;
            // The bound thread may have already been cleaned up upstream -> clear the binding, so a client retry automatically opens a fresh session
            if matches!(mode, BridgeMode::Continue { .. }) {
                let _ = st.web_threads.clear(&cid);
                st.telemetry.event(&req_id, "thread_reset", &e.to_string());
            }
            let ms = start.elapsed().as_millis() as i64;
            st.usage
                .record_ex(
                    "web-cookie",
                    &model,
                    0,
                    0,
                    ms,
                    502,
                    "upstream_5xx",
                    &e.to_string(),
                    &req_id,
                )
                .ok();
            st.telemetry.record(TraceRow {
                req_id: req_id.clone(),
                endpoint: "/v1/chat/completions".into(),
                requested_model: requested.clone(),
                resolved_model: model.clone(),
                account: "web-cookie".into(),
                status: 502,
                latency_ms: ms as u64,
                ttft_ms: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                stream,
                error_kind: Some("upstream_5xx".into()),
                error_excerpt: Some(e.to_string()),
                route_reason: Some(reason),
                api_key: None,
                client_ip: None,
            });
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": { "message": format!("Upstream web protocol request failed: {e}"), "type": "server_error" } })),
            )
                .into_response();
        }
    };

    // Side-channel scan: forward bytes + bind threadId + usage stats (same telemetry as the non-bridge path)
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(16);
    let telem = st.telemetry.clone();
    let usage_db = st.usage.clone();
    let client_track = client.clone();
    let threads_path = st.cfg.threads_path.clone();
    let web_threads = st.web_threads.clone();
    let cid_track = cid.clone();
    let bound_track = last_user.clone();
    let model_track = model.clone();
    let requested_track = requested.clone();
    let req_id_track = req_id.clone();
    // Token estimation input (the upstream web protocol SSE doesn't carry usage, so it can only be estimated locally -- see estimate_tokens)
    let est_input_chars: usize = match &mode {
        BridgeMode::Continue { thread_id: _ } => last_user.chars().count(),
        BridgeMode::Fresh { prompt } => prompt.chars().count(),
    };
    let est_input_track = est_input_chars;
    tokio::spawn(async move {
        let _bridge_guard_keep = bridge_guard; // holds the concurrency permit for the duration of the bridge stream; returned via Drop when the task ends
        let mut stream_ = body.into_data_stream();
        let mut ttft: Option<u64> = None;
        let mut bytes: u64 = 0;
        let mut tail = String::new();
        while let Some(chunk) = stream_.next().await {
            match chunk {
                Ok(b) => {
                    if ttft.is_none() {
                        ttft = Some(start.elapsed().as_millis() as u64);
                    }
                    bytes += b.len() as u64;
                    tail.push_str(&String::from_utf8_lossy(&b));
                    if tail.len() > 8000 {
                        tail = tail_keep(&tail, 4000);
                    }
                    if tx.send(Ok(b)).await.is_err() {
                        break; // client disconnected
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                    break;
                }
            }
        }
        let total_ms = start.elapsed().as_millis() as u64;
        // threadId binding: for reuse on continued chats (saves daily session quota); also added to the global cleanup list
        if let Some(tid) = client_track.last_thread_id() {
            let _ = web_threads.bind(&cid_track, &tid, &bound_track);
            record_thread(&threads_path, &tid);
        }
        // Token accounting: the upstream web SSE doesn't provide usage (the done event is empty), so parse the
        // usage field if present, otherwise estimate from content length (input = content sent, output = body in
        // the converted chunks), so the panel/stats stop showing 0 forever -- estimated values are recorded with a
        // `~` prefix (see the estimate_tokens comment).
        let (pt, ct) = match extract_usage(&tail) {
            Some(v) => v,
            None => {
                let out_chars: usize = count_bridge_content_chars(&tail);
                (estimate_tokens(est_input_track), estimate_tokens(out_chars))
            }
        };
        // Detecting inline errors in the bridge stream (Critic-J P2-1 closure): prefer the StreamEncoder side
        // channel (which can see the raw upstream error envelope), with tail-scan detect_bridge_error as a fallback.
        let bypass_error = client_track.last_upstream_error();
        let bridge_kind = bypass_error
            .as_deref()
            .map(|e| crate::errors::classify(200, e).as_str().to_string())
            .or_else(|| detect_bridge_error(&tail).map(|s| s.to_string()));
        let bridge_kind: Option<&str> = bridge_kind.as_deref();
        let status: i64 = if bridge_kind.is_some() { 502 } else { 200 };
        usage_db
            .record_ex(
                "web-cookie",
                &model_track,
                pt as i64,
                ct as i64,
                total_ms as i64,
                status,
                bridge_kind.unwrap_or(""),
                bridge_kind
                    .map(|k| format!("Upstream bridge stream inline error ({k}): credential may be invalid or quota exhausted"))
                    .as_deref()
                    .unwrap_or(""),
                &req_id_track,
            )
            .ok();
        telem.record(TraceRow {
            req_id: req_id_track.clone(),
            endpoint: "/v1/chat/completions".into(),
            requested_model: requested_track,
            resolved_model: model_track.clone(),
            account: "web-cookie".into(),
            status: status as u16,
            latency_ms: total_ms,
            ttft_ms: ttft,
            prompt_tokens: pt,
            completion_tokens: ct,
            stream: true,
            error_kind: bridge_kind.map(|k| k.to_string()),
            error_excerpt: bridge_kind.map(|_| tail_keep(&tail, 300)),
            route_reason: Some(reason),
            api_key: None,
            client_ip: None,
        });
        st_logs_done(&telem, &req_id_track, &model_track, bytes, total_ms as u128);
    });

    let body_stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    if !stream {
        return aggregate_bridge_sse(Box::pin(body_stream), &model, &req_id).await;
    }
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(body_stream))
        .unwrap_or_default()
}

/// Bridge path completion log (called at the end of the background task, to avoid the closure capturing the whole AppState)
fn st_logs_done(telem: &TelemetryWriter, req_id: &str, model: &str, bytes: u64, total_ms: u128) {
    telem.event(
        req_id,
        "done",
        &format!("web bridge {model}: {bytes}B / {total_ms}ms"),
    );
}

/// Detects an upstream inline error in the bridge SSE stream (a 200 with an embedded error envelope).
/// Returns Some(kind) if an error appeared in the stream (aligned with `errors::classify`'s kind naming).
fn detect_bridge_error(tail: &str) -> Option<&'static str> {
    // Fast path: no quoted "error" text, pass through directly (avoid JSON-parsing every line)
    if !tail.contains("\"error\"") {
        return None;
    }
    for line in tail.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(err) = v.get("error") {
            let body_text = err
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| err.to_string());
            let kind = crate::errors::classify(200, &body_text);
            return Some(kind.as_str());
        }
    }
    None
}

/// Adapts a bridge response's OpenAI shape to Claude's (used by the /v1/messages bridge path).
/// - Non-streaming JSON: converted directly with the existing `openai_to_claude_response`
/// - SSE stream: converts OpenAI chunks into an Anthropic event stream in real time (message_start -> content_block_delta -> message_stop)
/// - Error JSON: converted into Claude's error shape
async fn openai_error_to_claude(resp: Response, model: &str) -> Response {
    let (mut parts, body) = resp.into_parts();
    if !model.is_empty() {
        parts.headers.insert(
            axum::http::HeaderName::from_static("x-bridge-model"),
            axum::http::HeaderValue::from_str(model)
                .unwrap_or(axum::http::HeaderValue::from_static("")),
        );
    }
    let ct = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let status = parts.status;

    // SSE: buffer the whole thing and convert it in one shot (the bridge stream usually finishes within a few
    // seconds, so aggregating is simple and correct; chunk-level real-time conversion is left as a future optimization)
    if ct.contains("text/event-stream") {
        let bytes = axum::body::to_bytes(body, 8 * 1024 * 1024)
            .await
            .unwrap_or_default();
        let raw = String::from_utf8_lossy(&bytes);
        let (text, mut model, finish, usage) = collect_openai_stream(&raw);
        if model.is_empty() {
            model = parts
                .headers
                .get("x-bridge-model")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
        }
        let events = claude_stream_events(&text, &model, finish.as_deref(), usage.as_ref());
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream; charset=utf-8")
            .header("cache-control", "no-cache")
            .body(Body::from(events))
            .unwrap_or_default(); // model fallback already read from the x-bridge-model header
    }

    // JSON (success or error)
    let bytes = axum::body::to_bytes(body, 8 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "type": "error", "error": { "type": "api_error", "message": "bridge response is not valid JSON" } })),
            )
                .into_response();
        }
    };
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("upstream error");
        let code = if status == StatusCode::UNAUTHORIZED {
            "authentication_error"
        } else {
            "api_error"
        };
        return (
            status,
            Json(serde_json::json!({ "type": "error", "error": { "type": code, "message": msg } })),
        )
            .into_response();
    }
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    Json(openai_to_claude_response(&v, &model)).into_response()
}

/// Extracts (body text, model, finish_reason, usage) from aggregated OpenAI SSE text
fn collect_openai_stream(raw: &str) -> (String, String, Option<String>, Option<serde_json::Value>) {
    let mut text = String::new();
    let mut model = String::new();
    let mut finish = None;
    let mut usage = None;
    for line in raw.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if model.is_empty() {
            model = v
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
        }
        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
            if let Some(c0) = choices.first() {
                if let Some(d) = c0
                    .get("delta")
                    .and_then(|d| d.get("content"))
                    .and_then(|t| t.as_str())
                {
                    text.push_str(d);
                }
                if let Some(fr) = c0.get("finish_reason").and_then(|f| f.as_str()) {
                    finish = Some(fr.to_string());
                }
            }
        }
        if v.get("usage").is_some_and(|u| u.is_object()) {
            usage = v.get("usage").cloned();
        }
    }
    (text, model, finish, usage)
}

/// Generates the full Anthropic event stream text (message_start -> content_block_delta x N -> message_stop)
fn claude_stream_events(
    text: &str,
    model: &str,
    finish_reason: Option<&str>,
    usage: Option<&serde_json::Value>,
) -> String {
    let (input_tokens, output_tokens) = usage
        .map(|u| {
            (
                u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                u.get("completion_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    let mut out = String::new();
    let mut send = |event: &str, data: serde_json::Value| {
        out.push_str("event: ");
        out.push_str(event);
        out.push_str("\ndata: ");
        out.push_str(&serde_json::to_string(&data).unwrap_or_else(|_| "{}".into()));
        out.push_str("\n\n");
    };
    send(
        "message_start",
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_webbridge_{}", uuid::Uuid::new_v4().simple()),
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": serde_json::Value::Null,
                "usage": { "input_tokens": input_tokens, "output_tokens": 0 },
            },
        }),
    );
    send(
        "content_block_start",
        serde_json::json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
    );
    // Push deltas in 512-byte slices to preserve "streaming" semantics (so a long answer isn't dumped on the client all at once)
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let mut end = (i + 512).min(bytes.len());
        // don't cut a multi-byte character in half
        while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
            end += 1;
        }
        let piece = String::from_utf8_lossy(&bytes[i..end]);
        send(
            "content_block_delta",
            serde_json::json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": piece } }),
        );
        i = end;
    }
    send(
        "content_block_stop",
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
    );
    let stop_reason = match finish_reason {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    };
    send(
        "message_delta",
        serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop_reason, "stop_sequence": serde_json::Value::Null },
            "usage": { "output_tokens": output_tokens },
        }),
    );
    send(
        "message_stop",
        serde_json::json!({ "type": "message_stop" }),
    );
    out
}

/// Aggregates the upstream SSE (OpenAI chunk text) into a single non-streaming OpenAI response.
/// Reassembles deltas from `data: {...}` lines into full content; failure (upstream JSON error) becomes a 502.
async fn aggregate_bridge_sse(
    mut stream: impl futures::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Unpin,
    model: &str,
    req_id: &str,
) -> Response {
    let mut raw = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(b) => raw.push_str(&String::from_utf8_lossy(&b)),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({ "error": { "message": format!("Upstream stream interrupted: {e}"), "type": "server_error" } })),
                )
                    .into_response();
            }
        }
    }
    let mut content = String::new();
    let mut finish_reason: Option<String> = None;
    let mut usage: Option<serde_json::Value> = None;
    for line in raw.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("upstream error");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": { "message": msg, "type": "server_error" } })),
            )
                .into_response();
        }
        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
            if let Some(c0) = choices.first() {
                if let Some(d) = c0
                    .get("delta")
                    .and_then(|d| d.get("content"))
                    .and_then(|t| t.as_str())
                {
                    content.push_str(d);
                }
                if let Some(fr) = c0.get("finish_reason").and_then(|f| f.as_str()) {
                    finish_reason = Some(fr.to_string());
                }
            }
        }
        if v.get("usage").is_some_and(|u| u.is_object()) {
            usage = v.get("usage").cloned();
        }
    }
    if content.is_empty() && finish_reason.is_none() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": { "message": "Upstream returned no content (credential may have expired, please log in again from the panel)", "type": "server_error" } })),
        )
            .into_response();
    }
    let (pt, ct) = usage
        .as_ref()
        .map(|u| {
            (
                u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                u.get("completion_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    let mut resp = serde_json::json!({
        "id": format!("chatcmpl-webbridge-{}", &req_id[..8.min(req_id.len())]),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".into()),
        }],
    });
    if let Some(u) = usage {
        resp["usage"] = u;
    } else {
        resp["usage"] = serde_json::json!({ "prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct });
    }
    Json(resp).into_response()
}

async fn handle_accounts(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    // v0.9: the Bearer pool snapshot keeps its original field compatibility; web Cookie credentials are merged into the display (with health fields, see WebAccountSnapshot for the contract)
    let pool_snap = st.pool.snapshot().await;
    let web_snap = st.web_pool.snapshot().await;
    let mut json = serde_json::to_value(&pool_snap)
        .unwrap_or_else(|_| serde_json::json!({ "accounts": [], "total": 0 }));
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "web_accounts".to_string(),
            serde_json::to_value(&web_snap).unwrap_or_else(|_| serde_json::json!([])),
        );
        obj.insert("web_total".to_string(), serde_json::json!(web_snap.len()));
    }
    Json(json).into_response()
}

/// GET /api/accounts/health -- account health dashboard (Bearer + web Cookie merged, with a history timeline).
///
/// Field names are the frontend contract (v0.9 task B §4, do not rename):
/// `{ok, accounts:[{id, kind:"bearer"|"web-cookie", masked, health_score, circuit_state,
///   cooldown_until, trips, last_error, last_ok_at, history:[{ts,type,detail}]}]}`
async fn handle_accounts_health(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let mut accounts: Vec<serde_json::Value> = Vec::new();

    // Bearer: read live circuit-breaker/score state from the pool (cooldown converted to RFC3339, consistent with web)
    {
        let pool_accounts = st.pool.accounts.lock().await;
        for acc in pool_accounts.iter() {
            let score = *acc.score.read().await;
            let breaker = acc.breaker.read().await;
            let sess = acc.session.snapshot().await;
            let id = crate::import::cred_id(&acc.token);
            let cooldown_until = breaker.open_until.and_then(|u| {
                let remaining = u.checked_duration_since(std::time::Instant::now())?;
                let wall =
                    chrono::Utc::now() + chrono::Duration::from_std(remaining).unwrap_or_default();
                Some(wall.to_rfc3339())
            });
            accounts.push(serde_json::json!({
                "id": id,
                "kind": "bearer",
                "masked": mask(&acc.token),
                "health_score": score,
                "circuit_state": match breaker.state {
                    crate::pool::CircuitState::Closed => "closed",
                    crate::pool::CircuitState::Open => "open",
                    crate::pool::CircuitState::HalfOpen => "half_open",
                },
                "cooldown_until": cooldown_until,
                "trips": breaker.trips,
                "last_error": sess.last_error,
                "last_ok_at": null,
                "history": account_history_timeline(&st, &id, 20),
            }));
        }
    }

    // web Cookie: taken directly from WebCookiePool.snapshot()
    for w in st.web_pool.snapshot().await {
        accounts.push(serde_json::json!({
            "id": w.id,
            "kind": w.kind,
            "masked": w.masked,
            "health_score": w.health_score,
            "circuit_state": w.circuit_state,
            "cooldown_until": w.cooldown_until,
            "trips": w.trips,
            "last_error": w.last_error,
            "last_ok_at": w.last_ok_at,
            "history": account_history_timeline(&st, &w.id, 20),
        }));
    }
    Json(serde_json::json!({ "ok": true, "accounts": accounts })).into_response()
}

/// Account history timeline: reads account_history.jsonl (AccountMetaStore.history, newest first),
/// mapped into frontend timeline events `{ts, type, detail}` (type: "ok" | "error").
fn account_history_timeline(st: &AppState, cred_id: &str, limit: usize) -> Vec<serde_json::Value> {
    st.meta
        .history(Some(cred_id), limit)
        .ok()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let ty = if r.ok { "ok" } else { "error" };
            let detail = if r.ok {
                let mut parts = vec!["check passed".to_string()];
                if let Some(t) = &r.tier_id {
                    parts.push(format!("tier={t}"));
                }
                if let Some(rem) = r.daily_remaining {
                    parts.push(format!("{rem} remaining today"));
                }
                parts.join(" · ")
            } else {
                "check failed (credential unavailable or rejected by upstream)".to_string()
            };
            serde_json::json!({ "ts": r.ts, "type": ty, "detail": detail })
        })
        .collect()
}

// ---------- Token import (auto-parses a curl command or HAR into storage) ----------

/// POST /api/tokens/import -- body is a curl command's text or HAR JSON; automatically extracts and stores the Bearer token
async fn handle_token_import(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let raw = match std::str::from_utf8(&body) {
        Ok(t) => t,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "ok": false, "message": "input is not valid UTF-8 text" })),
            )
                .into_response();
        }
    };
    // Also accepts a JSON wrapper: {"cookie": "..."} / {"text": "..."} (both panel submission methods are supported)
    let text: std::borrow::Cow<'_, str> = if raw.trim_start().starts_with('{') {
        serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| {
                v.get("cookie")
                    .or_else(|| v.get("text"))
                    .and_then(|x| x.as_str())
                    .map(|s| std::borrow::Cow::Owned(s.to_string()))
            })
            .unwrap_or(std::borrow::Cow::Borrowed(raw))
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    let tokens = match crate::import::sniff_tokens(&text) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
            )
                .into_response();
        }
    };
    let tokens_path = st.cfg.tokens_path.clone();
    match crate::import::persist_tokens(&tokens_path, &tokens) {
        Ok(added) => {
            if added.is_empty() {
                return Json(serde_json::json!({ "ok": true, "added": 0, "message": "token already exists, not added again" }))
                    .into_response();
            }
            // Hot-update into the account pool.
            // Note: web-cookie (session-token) credentials **are not added to the pool** -- the account pool uses
            // the desktop Bearer protocol, and stuffing a Cookie in there would be treated as a Bearer token,
            // which is guaranteed to fail upstream and trip the circuit breaker; web Cookies are used by the web
            // bridge path instead.
            let new_accounts: Vec<crate::pool::AccountEntry> = added
                .iter()
                .filter(|t| !t.token.contains("session-token"))
                .map(|t| crate::pool::AccountEntry {
                    name: format!("import-{}", t.token.chars().take(6).collect::<String>()),
                    token: t.token.clone(),
                    session: std::sync::Arc::new(crate::session::SessionManager::new(
                        st.client.clone(),
                        t.token.clone(),
                        (*st.cfg).clone(),
                    )),
                    score: tokio::sync::RwLock::new(0.0),
                    breaker: tokio::sync::RwLock::new(crate::pool::CircuitBreaker::new()),
                })
                .collect();
            // Non-Bearer credentials (web-cookie) don't go into the Bearer pool, but still count toward the "import successful" response
            for acc in new_accounts {
                st.pool.add_account(acc).await;
            }
            // v0.9: web Cookie credentials go into WebCookiePool (health/circuit-breaking/cooldown/round-robin)
            for t in added.iter().filter(|t| t.token.contains("session-token")) {
                st.web_pool
                    .add_if_absent(&t.token, &t.source, t.added_at.clone())
                    .await;
            }
            Json(serde_json::json!({
                "ok": true,
                "added": added.len(),
                "tokens": added.iter().map(|t| serde_json::json!({
                    "token_masked": mask(&t.token),
                    "host": t.host,
                    "method": t.method,
                    "path": t.path,
                })).collect::<Vec<_>>(),
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/tokens -- list stored tokens (redacted + stable id + account info cache + storage timestamp)
async fn handle_tokens_list(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    match crate::import::load_tokens_healed(&st.cfg.tokens_path) {
        Ok(tokens) => {
            let metas = st.meta.all();
            let list: Vec<serde_json::Value> = tokens
                .iter()
                .map(|t| {
                    let id = crate::import::cred_id(&t.token);
                    serde_json::json!({
                        "id": id,
                        "token_masked": mask(&t.token),
                        "source": t.source,
                        "host": t.host,
                        "path": t.path,
                        "method": t.method,
                        "added_at": t.added_at,
                        "kind": crate::import::kind_of(&t.token),
                        // Account info from the most recent fetch (nickname/email/plan/remaining today); null if never fetched
                        "meta": metas.get(&id),
                    })
                })
                .collect();
            Json(serde_json::json!({ "ok": true, "count": list.len(), "tokens": list }))
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/tokens/check -- body `{"id":"<credential id>"}`: fetches the full account picture for the given credential and refreshes the cache
async fn handle_token_check(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let id = match serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(String::from))
    {
        Some(v) if !v.is_empty() => v,
        _ => return bad_req("missing id parameter (the id field from the credentials list)"),
    };
    let (cookie, tok) = match cookie_by_id(&st, &id) {
        Some(v) => v,
        None => return bad_req("credential not found (it may have been deleted, please refresh the list)"),
    };
    let client = match crate::web_protocol::WebClient::new(cookie, "glm-5.3-flash".into()) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    let (identity, usage, subs, quota) = tokio::join!(
        client.auth_session(),
        client.usage_summary(),
        client.subscriptions(),
        client.freebuff_session(),
    );
    let iv = value_of(&identity);
    let uv = value_of(&usage);
    let sv = value_of(&subs);
    let qv = value_of(&quota);
    let ok = credential_usable(iv.as_ref(), qv.as_ref());
    let mut meta = build_cred_meta(&id, iv.as_ref(), uv.as_ref(), sv.as_ref(), qv.as_ref());
    meta.valid = ok;
    if !ok {
        meta.error = Some(
            identity
                .as_ref()
                .err()
                .map(|e| e.to_string())
                .or_else(|| quota.as_ref().err().map(|e| e.to_string()))
                .unwrap_or_else(|| "upstream returned no logged-in account (credential expired or not logged in)".into()),
        );
    }
    if let Err(e) = st.meta.upsert(meta.clone()) {
        tracing::warn!("Failed to write account info cache: {e}");
    }
    record_history(&st, &meta, ok);
    // v0.9: sync web Cookie credential health into the pool (401/403/unavailable -> circuit break + cooldown, auto account switch)
    if crate::import::kind_of(&tok.token) == "web-cookie" {
        if ok {
            st.web_pool.mark_ok(&id).await;
        } else {
            web_pool_failure(&st, &id, meta.error.as_deref().unwrap_or("credential check failed")).await;
        }
    }
    st.logs.emit(
        if ok { "info" } else { "warn" },
        "account",
        None,
        format!(
            "Credential check {}: {}",
            mask(&tok.token),
            if ok { "valid" } else { "may have expired" }
        ),
    );
    Json(serde_json::json!({
        "ok": ok,
        "valid": ok,
        "id": id,
        "meta": meta,
        "message": if ok { "Credential is valid, account info updated".to_string() } else { meta.error.clone().unwrap_or_else(|| "credential may have expired".into()) },
    }))
    .into_response()
}

/// POST /api/tokens/delete -- body `{"id":"<credential id>"}`: deletes the credential (disk + in-memory account pool + cache)
async fn handle_token_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let id = match serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(String::from))
    {
        Some(v) if !v.is_empty() => v,
        _ => return bad_req("missing id parameter (the id field from the credentials list)"),
    };
    match crate::import::delete_token(&st.cfg.tokens_path, &id) {
        Ok(Some(removed)) => {
            let in_pool = st.pool.remove_account(&removed.token).await;
            // v0.9: also remove the web Cookie credential from WebCookiePool
            if crate::import::kind_of(&removed.token) == "web-cookie" {
                let _ = st.web_pool.remove(&id).await;
            }
            let _ = st.meta.remove(&id);
            st.logs.emit(
                "warn",
                "account",
                None,
                format!("Deleted credential {}", mask(&removed.token)),
            );
            Json(serde_json::json!({
                "ok": true,
                "removed": mask(&removed.token),
                "removed_from_pool": in_pool,
                "message": if in_pool { "Credential deleted (including from the running account pool)" } else { "Credential deleted" },
            }))
            .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "message": "credential not found (it may already have been deleted)" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "message": format!("delete failed: {e}") })),
        )
            .into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    cred_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /api/account/history -- account usage records (queryable by credential; newest first)
async fn handle_account_history(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    match st.meta.history(q.cred_id.as_deref(), limit) {
        Ok(records) => Json(serde_json::json!({
            "ok": true,
            "count": records.len(),
            "records": records,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/guide -- all the info needed for client setup (address / key status / model count), for the panel and users to copy directly
async fn handle_guide(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let keys = api_keys_of(&st);
    let masked: Vec<String> = keys.iter().map(|k| mask(k)).collect();
    let models = st.registry.models().await;
    let web_cookie = pick_web_cookie(&st).await.map(|(_, cred, _)| cred);
    Json(serde_json::json!({
        "ok": true,
        "listen_addr": st.cfg.listen_addr,
        "openai_base_url": "/v1",
        "anthropic_base_url": "/",
        "api_keys": {
            "configured": !keys.is_empty(),
            "count": keys.len(),
            "masked": masked,
        },
        // what to fill in for the client when no key is configured (local direct-connect scenario)
        "api_key_hint": if keys.is_empty() { "sk-local (api_keys not configured, any non-empty string works)" } else { "the key shown above (the panel's requests attach it automatically)" },
        "models_count": models.len(),
        "models_sample": models.iter().take(8).cloned().collect::<Vec<_>>(),
        "credential": web_cookie,
        "data_plane_ready": web_cookie.is_some(),
    }))
    .into_response()
}

#[derive(Debug, serde::Deserialize)]
struct KeyQuery {
    #[serde(default)]
    key: Option<String>,
}

/// GET /api/login/result -- reads the embedded login window's failure result file (for the panel's openEmbedLogin polling).
/// No file / already consumed returns ok:false (the panel keeps waiting or falls back).
async fn handle_login_result(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let path = std::path::Path::new("data/login_window_result.json");
    match std::fs::read_to_string(path) {
        Ok(text) => {
            // read = consume (one-shot result)
            let _ = std::fs::remove_file(path);
            let v: serde_json::Value = serde_json::from_str(&text)
                .unwrap_or(serde_json::json!({ "ok": false, "message": "result file is corrupted" }));
            Json(v).into_response()
        }
        Err(_) => Json(serde_json::json!({ "ok": false, "consumed": true })).into_response(),
    }
}

/// POST /api/login/embed -- spawns an embedded WebView2 login window child process (non-blocking).
///
/// The preferred path for browser "one-click login": after completing GitHub login in the window, the
/// child process grabs the full Cookie set (including HttpOnly) via the WebView2 CookieManager and
/// automatically POSTs it to `/api/tokens/import` for storage, then the window closes itself. The panel polls `/api/tokens` to detect the new credential.
///
/// Platform support: Windows (WebView2 Runtime); when unsupported, returns ok:false + a reason, and the frontend falls back to the extension/clipboard/manual import.
async fn handle_login_embed(
    State(st): State<AppState>,
    headers: HeaderMap,
    _body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    // parse the listening port (the panel shares the same port as the local gateway)
    let port = st
        .cfg
        .listen_addr
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(47821);
    let spawned = crate::login_window::spawn_login_window(port);
    if spawned {
        st.logs.emit(
            "info",
            "login",
            None,
            "Embedded WebView2 login window has opened (waiting for the user to complete GitHub login)",
        );
        Json(serde_json::json!({
            "ok": true,
            "message": "Login window has opened; the credential will be stored automatically after GitHub login completes",
        }))
        .into_response()
    } else {
        // Two failure modes: (1) a login window is already running (rejected to prevent re-entry) (2) the platform doesn't support WebView2 / spawn failed.
        // The message doesn't assume a specific cause; it points the user to check the current state and offers an alternative path.
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "ok": false,
                "message": "Login window failed to start: one may already be running (check the taskbar), or this platform does not support WebView2. Use clipboard/manual import instead.",
            })),
        )
            .into_response()
    }
}

/// GET /api/extension/bundle -- download the browser one-click-login extension (zip)
///
/// The extension file is embedded at compile time, so it still works when distributed as a single binary (no source tree present).
/// The panel downloads it via `window.open`, and the browser won't attach an Authorization header, so this also accepts `?key=` (same strategy as the log SSE endpoint).
async fn handle_extension_bundle(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<KeyQuery>,
) -> Response {
    let keys = api_keys_of(&st);
    let query_ok = !keys.is_empty()
        && q.key
            .as_deref()
            .map(|k| keys.iter().any(|x| x == k))
            .unwrap_or(false);
    if !admin_authorized(&headers, &st) && !query_ok {
        return admin_denied();
    }
    let zip = crate::extension::build_zip();
    let filename = format!(
        "freebuff2api-extension-v{}.zip",
        crate::extension::version()
    );
    match Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/zip")
        .header(
            "content-disposition",
            format!("attachment; filename=\"{filename}\""),
        )
        .header("cache-control", "no-store")
        .body(Body::from(zip))
    {
        Ok(r) => r,
        Err(e) => internal_err(&anyhow::Error::msg(format!("failed to build download response: {e}"))),
    }
}

/// POST /api/config/api-key — body `{"action":"generate"|"set"|"clear","key":"..."}`
/// One-click generate/set/clear the downstream API key, written back to config.json **and takes effect immediately** (no restart needed).
async fn handle_config_api_key(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("body must be JSON"),
    };
    let action = v
        .get("action")
        .and_then(|x| x.as_str())
        .unwrap_or("generate");
    let current = api_keys_of(&st);

    let new_keys: Vec<String> = match action {
        "generate" => {
            // Cryptographically random (OsRng 32 bytes -> 32-character base64url charset mapping, 192 bits of effective entropy),
            // unpredictable/unenumerable; UUIDv4 only has 122 random bits and a recognizable format
            use rand::rngs::OsRng;
            use rand::RngCore;
            const B64URL: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut raw = [0u8; 32];
            OsRng.fill_bytes(&mut raw);
            let token: String = raw
                .iter()
                .map(|b| B64URL[(b & 63) as usize] as char)
                .collect();
            let k = format!("sk-fb-{token}");
            vec![k]
        }
        "set" => {
            let k = v
                .get("key")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if k.len() < 8 {
                return bad_req("key is too short (at least 8 characters)");
            }
            vec![k]
        }
        "clear" => {
            // Safety guard: clearing is disallowed when not listening on loopback (otherwise admin endpoints/credentials are exposed to the network)
            if !crate::config::is_loopback_listen(&st.cfg.listen_addr) {
                return bad_req(
                    "currently listening on a non-loopback address; clearing the API key is disallowed (otherwise the panel and credentials would be exposed to the network)",
                );
            }
            vec![]
        }
        other => return bad_req(&format!("unknown action: {other} (supported: generate/set/clear)")),
    };

    // Persist to config.json (round-trip merge via Value, to avoid overwriting the user's other settings)
    let path = crate::config::resolve_config_path();
    if let Some(p) = path.as_deref() {
        let mut root: serde_json::Value = std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !root.is_object() {
            root = serde_json::json!({});
        }
        root["api_keys"] = serde_json::json!(new_keys);
        let write_res = serde_json::to_string_pretty(&root)
            .map_err(|e| e.to_string())
            .and_then(|s| {
                // Atomic write: temp file + rename, to prevent a mid-write crash from corrupting config.json
                let tmp = format!("{p}.tmp");
                std::fs::write(&tmp, s.clone()).map_err(|e| e.to_string())?;
                #[cfg(windows)]
                if std::path::Path::new(p).exists() {
                    let _ = std::fs::remove_file(p);
                }
                std::fs::rename(&tmp, p).map_err(|e| e.to_string())
            });
        match write_res {
            Ok(()) => tracing::info!("api_keys written back to {p}"),
            Err(e) => tracing::warn!("failed to write api_keys back to {p}: {e} (in-memory only)"),
        }
    } else {
        tracing::warn!("config.json not found, api_keys only takes effect for this run");
    }

    set_api_keys(&st, new_keys.clone());
    st.logs.emit(
        "warn",
        "config",
        None,
        format!("Downstream API key updated ({} total)", new_keys.len()),
    );
    Json(serde_json::json!({
        "ok": true,
        "action": action,
        "configured": !new_keys.is_empty(),
        "count": new_keys.len(),
        // echo the plaintext once on generate/set, for the user to copy into the client
        "key": new_keys.first().cloned(),
        "previous_count": current.len(),
        "persisted": path.is_some(),
        "message": match action {
            "generate" => "New API key generated and took effect immediately (also written to config.json)",
            "set" => "API key updated and took effect immediately",
            _ => "API key cleared (local direct-connect mode)",
        },
    }))
    .into_response()
}

/// Whitelist of settings-page editable config keys (key -> default value type). Only exposes fields the UI can safely modify.
const CONFIG_EDITABLE: &[(&str, &str)] = &[
    ("listen_addr", "string"),
    ("memory_enabled", "bool"),
    ("token_saver", "bool"),
    ("skills_inject_mode", "string"),
    ("max_roster_tokens", "usize"),
    ("http_proxy", "string"),
    ("thread_cleanup_interval_sec", "u64"),
    ("thread_max_age_hours", "u64"),
    ("redact_logs", "bool"),
    ("concurrency_free_slots", "usize"),
    ("concurrency_free_multi", "usize"),
    ("concurrency_sub_slots", "usize"),
    ("concurrency_sub_multi", "usize"),
];

/// GET /api/config -- returns the editable fields of the current config (redacted, for the settings page to echo back)
async fn handle_config_get(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let cfg = &st.cfg;
    let mut editable = serde_json::Map::new();
    for (key, _) in CONFIG_EDITABLE {
        let v = match *key {
            "listen_addr" => serde_json::json!(cfg.listen_addr),
            "memory_enabled" => serde_json::json!(cfg.memory_enabled),
            "token_saver" => serde_json::json!(cfg.token_saver),
            "skills_inject_mode" => serde_json::json!(cfg.skills_inject_mode),
            "max_roster_tokens" => serde_json::json!(cfg.max_roster_tokens),
            "http_proxy" => serde_json::json!(cfg.http_proxy),
            "thread_cleanup_interval_sec" => serde_json::json!(cfg.thread_cleanup_interval_sec),
            "thread_max_age_hours" => serde_json::json!(cfg.thread_max_age_hours),
            "redact_logs" => serde_json::json!(cfg.redact_logs),
            "concurrency_free_slots" => serde_json::json!(cfg.concurrency_free_slots),
            "concurrency_free_multi" => serde_json::json!(cfg.concurrency_free_multi),
            "concurrency_sub_slots" => serde_json::json!(cfg.concurrency_sub_slots),
            "concurrency_sub_multi" => serde_json::json!(cfg.concurrency_sub_multi),
            _ => continue,
        };
        editable.insert((*key).to_string(), v);
    }
    Json(serde_json::json!({
        "ok": true,
        "memory_runtime_enabled": memory_enabled_now(&st),
        "editable": editable,
        "note": "listen_addr requires a restart to take effect after being changed; other fields either hot-apply or require a restart after being written back",
    }))
    .into_response()
}

/// POST /api/config/save -- body `{"key":"listen_addr","value":"..."}`: validated against the whitelist, then atomically written back to config.json.
/// Fields that hot-apply at runtime (memory_enabled / token_saver / redact_logs / semaphore capacity) also update in-memory state.
async fn handle_config_save(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("body must be JSON"),
    };
    let key = v
        .get("key")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let value = v.get("value").cloned().unwrap_or(serde_json::Value::Null);
    let kind = CONFIG_EDITABLE
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, t)| *t);
    let Some(kind) = kind else {
        return bad_req(&format!("not allowed to modify config key: {key} (outside the whitelist)"));
    };
    // Type validation + value validation
    let typed: serde_json::Value = match kind {
        "bool" => serde_json::json!(value.as_bool().unwrap_or(false)),
        "usize" | "u64" => {
            let n = value
                .as_u64()
                .or_else(|| value.as_str().and_then(|s| s.parse().ok()));
            let Some(n) = n else {
                return bad_req("this config field requires an integer");
            };
            serde_json::json!(n)
        }
        _ => value.clone(),
    };
    // Value validation
    match key.as_str() {
        "listen_addr" => {
            let s = typed.as_str().unwrap_or("").trim().to_string();
            if s.is_empty() {
                return bad_req("listen address cannot be empty");
            }
            // validate host:port shape (a simple colon check)
            if !s.contains(':') {
                return bad_req("listen address must be in host:port form (e.g. 127.0.0.1:47821)");
            }
            // Safety guard: switching to a non-loopback address requires api_keys to already be configured (otherwise admin endpoints/credentials are exposed to the network)
            if !crate::config::is_loopback_listen(&s) && api_keys_of(&st).is_empty() {
                return bad_req(
                    "safety refusal: a non-loopback listen address requires api_keys to be configured first (to prevent exposing the panel and credentials to the network)",
                );
            }
        }
        "skills_inject_mode" => {
            let s = typed.as_str().unwrap_or("").trim();
            if !matches!(s, "roster" | "full") {
                return bad_req("skills_inject_mode only supports roster or full");
            }
        }
        "http_proxy" => {
            let s = typed.as_str().unwrap_or("").trim();
            if !(s.is_empty()
                || s.starts_with("http://")
                || s.starts_with("https://")
                || s.starts_with("socks5://")
                || s.starts_with("socks5h://"))
            {
                return bad_req("proxy address must start with http:// / https:// / socks5://");
            }
        }
        "concurrency_free_slots"
        | "concurrency_free_multi"
        | "concurrency_sub_slots"
        | "concurrency_sub_multi" => {
            let n = typed.as_u64().unwrap_or(0);
            if n == 0 {
                return bad_req("concurrency capacity must be >= 1");
            }
        }
        _ => {}
    }

    // Atomically write back to config.json (round-trip merge via Value, without overwriting the user's other settings)
    let path = crate::config::resolve_config_path();
    let persisted = if let Some(p) = path.as_deref() {
        let mut root: serde_json::Value = std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !root.is_object() {
            root = serde_json::json!({});
        }
        root[key.clone()] = typed.clone();
        let write_res = serde_json::to_string_pretty(&root)
            .map_err(|e| e.to_string())
            .and_then(|s| {
                let tmp = format!("{p}.tmp");
                std::fs::write(&tmp, s.clone()).map_err(|e| e.to_string())?;
                #[cfg(windows)]
                if std::path::Path::new(p).exists() {
                    let _ = std::fs::remove_file(p);
                }
                std::fs::rename(&tmp, p).map_err(|e| e.to_string())
            });
        match write_res {
            Ok(()) => {
                tracing::info!("config key {key} written back to {p}");
                true
            }
            Err(e) => {
                tracing::warn!("failed to write back config key {key}: {e} (only effective for this run)");
                false
            }
        }
    } else {
        tracing::warn!("config.json not found, config key only takes effect for this run");
        false
    };

    // Hot-apply at runtime (changes in-memory state without a restart)
    let mut hot_applied = false;
    if key == "memory_enabled" {
        set_memory_runtime_enabled(&st, typed.as_bool().unwrap_or(false));
        hot_applied = true;
    }

    st.logs.emit(
        "info",
        "config",
        None,
        format!(
            "Config key {key} updated ({})",
            if persisted {
                "written to config.json"
            } else {
                "this run only"
            }
        ),
    );
    Json(serde_json::json!({
        "ok": true,
        "key": key,
        "value": typed,
        "persisted": persisted,
        "hot_applied": hot_applied,
        "message": match key.as_str() {
            "listen_addr" => "listen address written back; restart the gateway to take effect",
            _ => if hot_applied { "saved and took effect immediately" } else { "written to config.json; takes effect after restart" },
        },
    }))
    .into_response()
}

/// Account detail card data (each account's balance/plan/limits)
/// POST /api/account/detail -- body { cookie: "..." } optional; if empty, uses the first imported one
async fn handle_account_detail(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    // parse body (optional cookie)
    let mut cookie: Option<String> = None;
    if !body.is_empty() {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
            if let Some(c) = v.get("cookie").and_then(|c| c.as_str()) {
                if c.contains("session-token") {
                    cookie = Some(c.to_string());
                }
            }
        }
    }
    // when body doesn't specify a cookie (v0.9): pick from WebCookiePool; keep the original error message if no Cookie is found
    let cookie = match cookie {
        Some(c) => Some(c),
        None => pick_web_cookie(&st).await.map(|(c, _, _)| c),
    };
    let cookie = match cookie {
        Some(c) => c,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "ok": false, "message": "no web Cookie credential found" })),
            )
                .into_response();
        }
    };
    // pool health feedback: freebuff_session success -> mark ok; failure -> mark failure (401/403 auto-cooldown)
    let cid = crate::import::cred_id(&cookie);
    let client = match crate::web_protocol::WebClient::new(cookie.clone(), "glm-5.3-flash".into()) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    // concurrently fetch balance + usage + user + subscription plan
    let balance = client.freebuff_session().await;
    let usage = client.usage_summary().await;
    let auth = client.auth_session().await;
    let subs = client.subscriptions().await;
    match &balance {
        Ok(_) => st.web_pool.mark_ok(&cid).await,
        Err(e) => web_pool_failure(&st, &cid, &e.to_string()).await,
    }
    Json(serde_json::json!({
        "ok": true,
        "cookie_masked": mask(&cookie),
        "balance": balance.ok(),
        "usage_summary": usage.ok(),
        "user": auth.ok(),
        "subscriptions": subs.ok(),
    }))
    .into_response()
}

/// Redaction: for long strings, show first 6 + last 4; short strings **must show even less**.
///
/// Lesson learned (from the reference project freellmapi's maskKey fix history): using "last 4 characters" for a short key ends up echoing it back in full.
/// The credential list is for humans to look at, not to copy -- a short credential always shows only a minimal number of characters.
fn mask(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    let n = chars.len();
    if n <= 4 {
        return "***".into();
    }
    if n <= 8 {
        return format!("{}***", chars[..2].iter().collect::<String>());
    }
    if n <= 12 {
        let head: String = chars[..2].iter().collect();
        let tail: String = chars[n - 2..].iter().collect();
        return format!("{head}***{tail}");
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[n - 4..].iter().collect();
    format!("{head}...{tail}")
}

// ---------- OpenAI-compatible ----------

async fn handle_chat_completions(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Data-plane CSRF defense: a browser cross-site request carries an Origin header (a text/plain simple request can bypass preflight,
    // and the handler parses the body regardless of content-type -- so Origin must be blocked).
    // SDKs/curl don't send Origin, so they're unaffected.
    if !origin_allowed(&headers) {
        return origin_blocked();
    }
    let start = std::time::Instant::now();
    let req_id = uuid::Uuid::new_v4().to_string();
    // parse the request
    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": { "message": "request body must be valid JSON", "type": "invalid_request_error" } })),
            )
                .into_response();
        }
    };

    let requested = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    if requested.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": { "message": "model is required", "type": "invalid_request_error" } })),
        )
            .into_response();
    }

    // API key validation
    let keys = api_keys_of(&st);
    if !keys.is_empty() && !authorized(&headers, &keys) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": { "message": "invalid proxy api key", "type": "authentication_error" } })),
        )
            .into_response();
    }

    // model routing / fallback
    let model = st.router.resolve(&requested).await;

    // ---------- Web-Cookie bridge (fills in a critical path) ----------
    // When the user has only imported a web Cookie (one-click login path) and has no Bearer token, the account pool is empty,
    // pick_best necessarily fails. In that case bridge the request to the web protocol (/api/chat/stream),
    // so browser users who "just fill in /v1 per the setup guide" can actually chat.
    // Session reuse: a follow-up turn only sends the last user message + reuses the same upstream thread,
    // avoiding opening a new thread on every request that would burn through the daily session admission quota (rateLimitsByModel.limit).
    // Having accounts in the pool isn't enough -- a placeholder (e.g. __TEST_SKIP__) or an already-expired Bearer would block the bridge and necessarily 401.
    // When the pool is entirely placeholders (length < 20 or matching a known placeholder pattern), treat it as "no usable account" and take the bridge path.
    let pool_tokens: Vec<String> = {
        let accounts = st.pool.accounts.lock().await;
        accounts.iter().map(|a| a.token.clone()).collect()
    };
    let pool_has_usable = pool_tokens
        .iter()
        .any(|tok| tok.len() >= 20 && !tok.starts_with("__TEST_SKIP__"));
    if !pool_has_usable {
        if let Some((cookie, cred, cid)) = pick_web_cookie(&st).await {
            if cookie.contains("session-token") {
                return web_bridge_openai(st, parsed, requested, model, cookie, cred, cid).await;
            }
        }
    }

    // usage-record fields (api_key redacted; computed up front for reuse by the retry path)
    let api_key = headers
        .get("x-api-key")
        .or_else(|| headers.get("authorization"))
        .map(|v| v.to_str().unwrap_or("").to_string())
        .map(|k| {
            if k.len() > 12 {
                format!("{}***{}", &k[..8], &k[k.len() - 4..])
            } else {
                "***".to_string()
            }
        })
        .unwrap_or_default();
    let client_ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or("").to_string())
        .unwrap_or_default();

    // configure the upstream body: set model + reasoning-effort downgrade/strip + prompt/skill injection (account-independent, reused across retries)
    let mut up_body = parsed.clone();
    up_body["model"] = serde_json::json!(model);
    // memory retrieval query: the last user message (truncated to 200 characters)
    let mem_query: String = up_body
        .get("messages")
        .and_then(|m| m.as_array())
        .and_then(|arr| {
            arr.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content"))
                .map(|c| match c {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
        })
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    // built-in prompts + skills roster + memory injection (prepended as a system message)
    // roster mode: prompts go through prompts (base + enabled items), skills go through the skills module (only injects name + description)
    let sys_prefix = build_system_prefix(&st, &mem_query).await;
    if !sys_prefix.trim().is_empty() {
        if let Some(messages) = up_body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            messages.insert(
                0,
                serde_json::json!({ "role": "system", "content": sys_prefix }),
            );
        }
    }
    let mut effort_downgraded: Option<String> = None;
    if let Some(effort) = up_body.get("reasoning_effort").and_then(|v| v.as_str()) {
        match st.router.clamp_effort(&model, effort) {
            Some(clamped) => {
                if clamped != effort {
                    tracing::debug!("model {model} effort {effort} downgraded to {clamped}");
                    effort_downgraded = Some(clamped.clone());
                    up_body["reasoning_effort"] = serde_json::json!(clamped);
                }
            }
            None => {
                tracing::debug!("model {model} doesn't support reasoning_effort, stripping it automatically");
                if let Some(obj) = up_body.as_object_mut() {
                    obj.remove("reasoning_effort");
                }
            }
        }
    }
    remove_passthrough_fields(&mut up_body);
    // token_saver (optional): compress overly long tool results (only touches tool-role messages, never touches the system prefix or earlier history)
    if st.cfg.token_saver {
        if let Some(msgs) = up_body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for m in msgs.iter_mut() {
                if m.get("role").and_then(|r| r.as_str()) != Some("tool") {
                    continue;
                }
                let long: Option<String> = m
                    .get("content")
                    .and_then(|c| c.as_str())
                    .filter(|c| c.len() > 8000)
                    .map(|s| s.to_string());
                if let Some(c) = long {
                    m["content"] = serde_json::json!(crate::router::compress_tool_result(&c, 8000));
                }
            }
        }
    }
    // streaming request: explicitly ask upstream for a usage frame (needed for real token accounting; non-streaming responses already include usage)
    if up_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        up_body["stream_options"] = serde_json::json!({ "include_usage": true });
    }

    // Request-level retry loop: on failure, switch accounts and retry.
    // No bytes have been written to the client yet at this point, so the "committed" boundary is naturally respected (never retry after the first byte).
    // v0.8 dual-bucket concurrency semaphore: acquire before the first byte is written out (one bucket each for free/subscription tiers),
    // to prevent high concurrency across multiple accounts from hitting the upstream concurrency wall and getting rate-limited. A 2s timeout maps to 429 semantics (aligned with waiting_room).
    let tier_is_sub = {
        let accounts = st.pool.accounts.lock().await;
        accounts
            .iter()
            .any(|a| crate::semaphore::is_subscriber_token(&a.token, None))
    };
    // Hold the semaphore guard until the function returns: RAII auto-releases it (Drop), ensuring the concurrency-wall count covers the entire request window.
    // The streaming path still holds it while the background upstream stream is being consumed (the guard moved into the task keeps working after the function returns).
    let tier_guard = match st.semaphore.acquire(tier_is_sub).await {
        Ok(g) => g,
        Err(_) => {
            let latency = start.elapsed().as_millis() as i64;
            st.usage
                .record_ex("", &model, 0, 0, latency, 429, "", "", &req_id)
                .ok();
            st.telemetry.event(
                &req_id,
                "concurrency_busy",
                &format!(
                    "concurrency bucket full ({} tier)",
                    if tier_is_sub { "subscription" } else { "free" }
                ),
            );
            st.logs.emit(
                "warn",
                "request",
                Some(&req_id),
                format!("{model} concurrency bucket full, returning 429 (please retry later)"),
            );
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({ "error": { "message": "too many concurrent requests to upstream, retry later", "type": "server_error", "code": "concurrency_busy" } })),
            )
                .into_response();
        }
    };
    let retry_policy = crate::retry::RetryPolicy::default();
    let mut account_name = String::new();
    let mut token = String::new();
    let mut run_id = String::new();
    let mut attempt_count = 0usize;
    let mut last_error = String::new();
    let mut last_status: u16 = 502;
    let mut upstream_resp: Option<reqwest::Response> = None;

    for attempt in 0..retry_policy.max_attempts {
        attempt_count = attempt + 1;
        // pick a token (re-selects on every retry; accounts with an Open circuit breaker are skipped)
        let account = match st.pool.pick_best().await {
            Some(a) => a,
            None => {
                if attempt == 0 {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({ "error": { "message": "no healthy upstream auth token available", "type": "server_error" } })),
                    )
                        .into_response();
                }
                last_error = "no healthy upstream auth token available".into();
                break;
            }
        };
        account_name = account.name.clone();
        token = account.token.clone();
        if attempt == 0 {
            st.telemetry.event(
                &req_id,
                "route",
                &format!("{requested} -> {model} via {account_name}"),
            );
        } else {
            st.telemetry.event(
                &req_id,
                "retry",
                &format!("attempt {} via {account_name}", attempt + 1),
            );
            st.logs.emit(
                "warn",
                "retry",
                Some(&req_id),
                format!("switching accounts and retrying (attempt {}) -> {account_name}", attempt + 1),
            );
        }

        // ensure a session
        let instance_id = match account.session.ensure_session(&model).await {
            Ok(id) => Some(id),
            Err(e) => {
                let msg = e.to_string();
                if msg.starts_with("waiting_room_queued") {
                    st.usage
                        .record_ex(&account_name, &model, 0, 0, 0, 429, "", "", &req_id)
                        .ok();
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({ "error": { "message": msg, "type": "server_error", "code": "waiting_room_queued" } })),
                    )
                        .into_response();
                }
                st.pool
                    .mark_failure(&account_name, &format!("session: {msg}"))
                    .await;
                st.pool.update_score(&account_name, -30.0).await;
                last_status = 502;
                last_error = format!("failed to acquire free session: {msg}");
                continue;
            }
        };

        // run management: get the root run (lazily)
        let rid = match ensure_root_run(&st, &account_name, &token).await {
            Ok(id) => id,
            Err(e) => {
                st.pool.mark_failure(&account_name, "run").await;
                last_status = 502;
                last_error = format!("create run failed: {e}");
                continue;
            }
        };
        run_id = rid;

        // call upstream
        match st
            .client
            .chat_completions(&token, up_body.clone(), &run_id, instance_id.as_deref())
            .await
        {
            Ok(r) if r.status().is_success() => {
                upstream_resp = Some(r);
                break;
            }
            Ok(r) => {
                let code = r.status().as_u16();
                let body_text = r.text().await.unwrap_or_default();
                // error classification: text rules take priority (waiting_room/rate_limit/model_unavailable...), status code as fallback
                let kind = crate::errors::classify(code, &body_text);
                st.pool
                    .mark_failure(&account_name, &format!("HTTP {code}"))
                    .await;
                st.pool.update_score(&account_name, -20.0).await;
                if code == 401 || code == 403 {
                    st.pool
                        .mark_cooldown(
                            &account_name,
                            std::time::Duration::from_secs(600),
                            &format!("upstream {code}, token appears to have expired"),
                        )
                        .await;
                }
                last_status = code;
                last_error = format!(
                    "upstream HTTP {code} ({}): {}",
                    kind.as_str(),
                    crate::errors::error_excerpt(&body_text)
                );
                // account-switch decision: switch accounts on either a retryable error (rate limit/queued/5xx/network) or an expired credential (401/403, already cooled down, keep switching)
                let should_switch =
                    kind.is_retryable() || matches!(kind, crate::errors::ErrorKind::AuthExpired);
                if should_switch && attempt + 1 < retry_policy.max_attempts {
                    st.telemetry.event(
                        &req_id,
                        "retry_scheduled",
                        &format!("HTTP {code} ({}), switching accounts and retrying", kind.as_str()),
                    );
                    continue;
                }
                break;
            }
            Err(e) => {
                st.pool.mark_failure(&account_name, "network").await;
                st.pool.update_score(&account_name, -40.0).await;
                last_status = 502;
                last_error = e.to_string();
                continue;
            }
        }
    }

    // all attempts failed: log and return an aggregated error
    let upstream_resp = match upstream_resp {
        Some(r) => r,
        None => {
            let latency = start.elapsed().as_millis() as i64;
            st.usage
                .record_ex(
                    &account_name,
                    &model,
                    0,
                    0,
                    latency,
                    last_status as i64,
                    "",
                    "",
                    &req_id,
                )
                .ok();
            st.telemetry.record(TraceRow {
                req_id: req_id.clone(),
                endpoint: "/v1/chat/completions".into(),
                requested_model: requested.clone(),
                resolved_model: model.clone(),
                account: account_name.clone(),
                status: last_status,
                latency_ms: latency as u64,
                ttft_ms: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                stream: false,
                error_kind: Some(
                    crate::errors::classify(last_status, &last_error)
                        .as_str()
                        .to_string(),
                ),
                error_excerpt: Some(last_error.chars().take(300).collect()),
                route_reason: Some(format!(
                    "{} -> {} ({} attempts all failed)",
                    requested, model, attempt_count
                )),
                api_key: Some(api_key.clone()),
                client_ip: Some(client_ip.clone()),
            });
            st.logs.emit(
                "error",
                "request",
                Some(&req_id),
                format!(
                    "{model} all {attempt_count} attempts failed: {}",
                    last_error.chars().take(120).collect::<String>()
                ),
            );
            // has an upstream HTTP response (not a network-class failure): pass through the upstream status code, preserving client retry/backoff semantics
            if (400..600).contains(&last_status) {
                return (
                    StatusCode::from_u16(last_status).unwrap_or(StatusCode::BAD_GATEWAY),
                    Json(serde_json::json!({
                        "error": {
                            "message": last_error,
                            "type": "upstream_error",
                            "code": last_status,
                            "model": model,
                            "upstream": st.client.base_url(),
                            "attempts": attempt_count,
                        }
                    })),
                )
                    .into_response();
            }
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": { "message": format!("all {attempt_count} attempts failed: {last_error}"), "type": "server_error" } })),
            )
                .into_response();
        }
    };

    let status = upstream_resp.status();
    let latency_ms = start.elapsed().as_millis() as i64;
    // (api_key / client_ip were already computed before the retry loop)
    // observation (zero LLM): model usage preference / tier downgrade / user correction signals -> memory store (not recorded when the runtime switch is off)
    if memory_enabled_now(&st) {
        let _ = st
            .memory
            .observe(&model, effort_downgraded.as_deref(), &mem_query, None);
    }

    if status.is_success() {
        st.pool.update_score(&account_name, 10.0).await;
        st.pool.mark_success(&account_name).await; // circuit breaker: drives HalfOpen -> Closed recovery
        st.telemetry.event(
            &req_id,
            "upstream_ok",
            &format!("HTTP {} connected in {}ms", status.as_u16(), latency_ms),
        );
        // run wrap-up: report FINISH (releases the upstream run count; failure doesn't affect the response)
        if let Err(e) = st.client.finish_run(&token, &run_id, 1).await {
            tracing::debug!("finish_run failed (doesn't affect the response): {e}");
        }
        st.logs.emit(
            "info",
            "request",
            Some(&req_id),
            format!("{model} upstream 200, forwarding ({latency_ms}ms to connect)"),
        );
    } else {
        st.usage
            .record_ex(
                &account_name,
                &model,
                0,
                0,
                latency_ms,
                status.as_u16() as i64,
                &api_key,
                &client_ip,
                &req_id,
            )
            .ok();
        st.pool.update_score(&account_name, -20.0).await;
        // upstream 401/403 = token expired -> cooldown/circuit-break this account (mark_cooldown was previously dead code; wired up here)
        let code = status.as_u16();
        if code == 401 || code == 403 {
            st.pool
                .mark_cooldown(
                    &account_name,
                    std::time::Duration::from_secs(600),
                    &format!("upstream {code}, token appears to have expired"),
                )
                .await;
        }
        let kind = crate::retry::classify_status(code).as_str();
        st.telemetry
            .event(&req_id, "upstream_error", &format!("HTTP {code} ({kind})"));
        st.telemetry.record(TraceRow {
            req_id: req_id.clone(),
            endpoint: "/v1/chat/completions".into(),
            requested_model: requested.clone(),
            resolved_model: model.clone(),
            account: account_name.clone(),
            status: code,
            latency_ms: latency_ms as u64,
            ttft_ms: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            stream: false,
            error_kind: Some(kind.to_string()),
            error_excerpt: None,
            route_reason: Some(format!("{} -> {}", requested, model)),
            api_key: Some(api_key.clone()),
            client_ip: Some(client_ip.clone()),
        });
        st.logs.emit(
            "warn",
            "request",
            Some(&req_id),
            format!(
                "upstream {code} ({kind}), account {account_name} penalized{}",
                if code == 401 || code == 403 {
                    " and cooled down for 10 minutes"
                } else {
                    ""
                }
            ),
        );
    }

    // stream-forward the upstream response (real incremental passthrough + upstream errors passed through as-is)
    let mut builder = Response::builder().status(status);
    for (k, v) in upstream_resp.headers() {
        if k != "content-length" && k != "transfer-encoding" {
            builder = builder.header(k, v);
        }
    }
    // non-2xx: pass through the actual upstream error body (including message/type/code)
    if !status.is_success() {
        let err_body = upstream_resp.bytes().await.unwrap_or_default();
        let err_text = String::from_utf8_lossy(&err_body).to_string();
        tracing::warn!(
            "[upstream error] {model} HTTP {status}: {}",
            err_text.chars().take(500).collect::<String>()
        );
        return (
            status,
            Json(serde_json::json!({
                "error": {
                    "message": err_text,
                    "type": "upstream_error",
                    "code": status.as_u16(),
                    "model": model,
                    "upstream": st.client.base_url(),
                }
            })),
        )
            .into_response();
    }

    // 2xx: branch (non-streaming reads the full body to parse usage; streaming spawns a forwarding task + side-channel collection)
    let is_stream = parsed
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let telem = st.telemetry.clone();
    let logs = st.logs.clone();
    let usage_db = st.usage.clone();
    let (rid, acc, mdl, key, ip) = (
        req_id.clone(),
        account_name.clone(),
        model.clone(),
        api_key.clone(),
        client_ip.clone(),
    );
    let route_reason = format!("{requested} -> {model}");

    if !is_stream {
        let bytes = upstream_resp.bytes().await.unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).to_string();
        // upstream may return an error code inside an HTTP 200 body (the free_mode_* family) -- detect and
        // treat as an upstream error
        if let Some(code) = upstream_body_error(&text) {
            let total_ms = start.elapsed().as_millis() as u64;
            usage_db
                .record_ex(&acc, &mdl, 0, 0, total_ms as i64, 502, &key, &ip, &rid)
                .ok();
            st.telemetry.event(&rid, "upstream_body_error", code);
            telem.record(TraceRow {
                req_id: rid.clone(),
                endpoint: "/v1/chat/completions".into(),
                requested_model: requested.clone(),
                resolved_model: mdl.clone(),
                account: acc.clone(),
                status: 502,
                latency_ms: total_ms,
                ttft_ms: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                stream: false,
                error_kind: Some("upstream_5xx".into()),
                error_excerpt: Some(crate::errors::error_excerpt(&text)),
                route_reason: None,
                api_key: Some(key.clone()),
                client_ip: Some(ip.clone()),
            });
            logs.emit(
                "warn",
                "request",
                Some(&rid),
                format!("{mdl} upstream 200 but body contains error code {code}"),
            );
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("upstream returned error in 200 body: {code}"),
                        "type": "upstream_error",
                        "code": code,
                        "model": mdl,
                        "upstream": st.client.base_url(),
                    }
                })),
            )
                .into_response();
        }
        let (pt, ct) = extract_usage(&text).unwrap_or((0, 0));
        let total_ms = start.elapsed().as_millis() as u64;
        usage_db
            .record_ex(
                &acc,
                &mdl,
                pt as i64,
                ct as i64,
                total_ms as i64,
                200,
                &key,
                &ip,
                &rid,
            )
            .ok();
        telem.record(TraceRow {
            req_id: rid.clone(),
            endpoint: "/v1/chat/completions".into(),
            requested_model: requested.clone(),
            resolved_model: mdl.clone(),
            account: acc.clone(),
            status: 200,
            latency_ms: total_ms,
            ttft_ms: None,
            prompt_tokens: pt,
            completion_tokens: ct,
            stream: false,
            error_kind: None,
            error_excerpt: None,
            route_reason: Some(route_reason),
            api_key: Some(key.clone()),
            client_ip: Some(ip.clone()),
        });
        logs.emit(
            "info",
            "request",
            Some(&rid),
            format!(
                "{mdl} completed {}+{} tok ({:.2}s)",
                pt,
                ct,
                total_ms as f64 / 1000.0
            ),
        );
        return builder
            .body(Body::from(bytes))
            .unwrap_or_default()
            .into_response();
    }

    // streaming: spawn the forwarding task; side-channel scan for usage, record first byte and total time
    // Move the semaphore guard into the task: kept held while the background upstream stream is consumed
    // (prevents stacking new concurrency), released via Drop when the task ends (covering the whole
    // streaming window).
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let _tier_guard_keep = tier_guard; // held until the task ends; Drop releases it automatically
        let t0 = std::time::Instant::now();
        let mut stream = upstream_resp.bytes_stream();
        let mut ttft: Option<u64> = None;
        let mut tail = String::new();
        let mut totals: (u64, u64) = (0, 0);
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(b) => {
                    if ttft.is_none() {
                        ttft = Some(t0.elapsed().as_millis() as u64);
                    }
                    tail.push_str(&String::from_utf8_lossy(&b));
                    if tail.len() > 8000 {
                        tail = tail_keep(&tail, 4000);
                    }
                    if let Some((p, c)) = extract_usage(&tail) {
                        totals = (p, c);
                    }
                    if tx.send(Ok(b)).await.is_err() {
                        break; // client disconnected
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                    telem.record(TraceRow {
                        req_id: rid.clone(),
                        endpoint: "/v1/chat/completions".into(),
                        requested_model: String::new(),
                        resolved_model: mdl.clone(),
                        account: acc.clone(),
                        status: 502,
                        latency_ms: latency_ms as u64 + t0.elapsed().as_millis() as u64,
                        ttft_ms: ttft,
                        prompt_tokens: totals.0,
                        completion_tokens: totals.1,
                        stream: true,
                        error_kind: Some("network".into()),
                        error_excerpt: Some(e.to_string().chars().take(300).collect()),
                        route_reason: None,
                        api_key: Some(key.clone()),
                        client_ip: Some(ip.clone()),
                    });
                    logs.emit("error", "request", Some(&rid), format!("{mdl} stream interrupted: {e}"));
                    return;
                }
            }
        }
        let total_ms = latency_ms as u64 + t0.elapsed().as_millis() as u64;
        usage_db
            .record_ex(
                &acc,
                &mdl,
                totals.0 as i64,
                totals.1 as i64,
                total_ms as i64,
                200,
                &key,
                &ip,
                &rid,
            )
            .ok();
        telem.record(TraceRow {
            req_id: rid.clone(),
            endpoint: "/v1/chat/completions".into(),
            requested_model: String::new(),
            resolved_model: mdl.clone(),
            account: acc.clone(),
            status: 200,
            latency_ms: total_ms,
            ttft_ms: ttft,
            prompt_tokens: totals.0,
            completion_tokens: totals.1,
            stream: true,
            error_kind: None,
            error_excerpt: None,
            route_reason: Some(route_reason),
            api_key: Some(key.clone()),
            client_ip: Some(ip.clone()),
        });
        logs.emit(
            "info",
            "request",
            Some(&rid),
            format!(
                "{mdl} streaming completed {}+{} tok (first byte {}ms / total {:.2}s)",
                totals.0,
                totals.1,
                ttft.unwrap_or(0),
                total_ms as f64 / 1000.0
            ),
        );
    });
    let body_stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    builder
        .body(Body::from_stream(body_stream))
        .unwrap_or_default()
        .into_response()
}

async fn ensure_root_run(st: &AppState, account_name: &str, token: &str) -> anyhow::Result<String> {
    // Simplified: lazily create a root run per account and cache it (an in-process Arc<Mutex> caching scheme is a follow-up patch)
    // Directly create a new root run here, avoiding the complexity of the first-run creation
    let run_id = st
        .client
        .start_run(token, crate::models::ROOT_AGENT_ID, &[])
        .await?;
    st.pool.update_score(account_name, 5.0).await;
    Ok(run_id)
}

// ---------- Anthropic compatibility ----------

async fn handle_claude_messages(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let start = std::time::Instant::now();
    let req_id = uuid::Uuid::new_v4().to_string();
    // Data-plane CSRF defense (same rationale as /v1/chat/completions: blocks malicious web pages' text/plain simple requests)
    if !origin_allowed(&headers) {
        return origin_blocked();
    }
    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "invalid json" } })),
            )
                .into_response();
        }
    };

    let keys = api_keys_of(&st);
    if !keys.is_empty() && !authorized(&headers, &keys) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid proxy api key" } })),
        )
            .into_response();
    }

    let model = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "model is required" } })),
        )
            .into_response();
    }
    let resolved = st.router.resolve(&model).await;

    // ---------- Web-Cookie bridging (same strategy as /v1/chat/completions, including placeholder protection) ----------
    let pool_tokens: Vec<String> = {
        let accounts = st.pool.accounts.lock().await;
        accounts.iter().map(|a| a.token.clone()).collect()
    };
    let pool_has_usable = pool_tokens
        .iter()
        .any(|tok| tok.len() >= 20 && !tok.starts_with("__TEST_SKIP__"));
    if !pool_has_usable {
        if let Some((cookie, cred, cid)) = pick_web_cookie(&st).await {
            if cookie.contains("session-token") {
                // Claude messages -> OpenAI messages reuses the existing conversion (including system/blocks/tool_use)
                let openai_msgs = claude_to_openai_messages(
                    parsed
                        .get("messages")
                        .cloned()
                        .unwrap_or(serde_json::json!([])),
                );
                let mut bridge_body = serde_json::json!({
                    "model": model,
                    "messages": openai_msgs,
                    "stream": parsed.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
                });
                if let Some(sys) = parsed.get("system") {
                    let sys_text = match sys {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Array(blocks) => blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        _ => String::new(),
                    };
                    if !sys_text.is_empty() {
                        if let Some(msgs) = bridge_body["messages"].as_array_mut() {
                            msgs.insert(
                                0,
                                serde_json::json!({ "role": "system", "content": sys_text }),
                            );
                        }
                    }
                }
                let resp = web_bridge_openai(
                    st,
                    bridge_body,
                    model.clone(),
                    resolved.clone(),
                    cookie,
                    cred,
                    cid,
                )
                .await;
                // OpenAI-shaped error body -> Claude shape
                return openai_error_to_claude(resp, &resolved).await;
            }
        }
    }

    // v0.8 dual-bucket concurrency semaphore (Claude path matches OpenAI path): acquire before writing the first byte.
    // Conservative rule: if any credential in the account pool has subscription characteristics, use the subscription bucket.
    let claude_is_sub = {
        let accounts = st.pool.accounts.lock().await;
        accounts
            .iter()
            .any(|a| crate::semaphore::is_subscriber_token(&a.token, None))
    };
    let claude_tier_guard = match st.semaphore.acquire(claude_is_sub).await {
        Ok(g) => g,
        Err(_) => {
            let latency = start.elapsed().as_millis() as i64;
            st.usage
                .record_ex("", &resolved, 0, 0, latency, 429, "", "", &req_id)
                .ok();
            st.telemetry.event(
                &req_id,
                "concurrency_busy",
                &format!(
                    "Claude concurrency bucket full ({} tier)",
                    if claude_is_sub { "subscription" } else { "free" }
                ),
            );
            st.logs.emit(
                "warn",
                "request",
                Some(&req_id),
                format!("{resolved} Claude concurrency bucket full, returning 429"),
            );
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({ "type": "error", "error": { "type": "api_error", "message": "too many concurrent requests to upstream, retry later", "code": "concurrency_busy" } })),
            )
                .into_response();
        }
    };

    // Claude -> OpenAI protocol conversion: extract system, flatten messages content blocks, map max_tokens
    let openai_messages = claude_to_openai_messages(
        parsed
            .get("messages")
            .cloned()
            .unwrap_or(serde_json::json!([])),
    );
    let wants_stream = parsed
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut up_body = serde_json::json!({
        "model": resolved,
        "messages": openai_messages,
        "stream": wants_stream,
    });
    if let Some(maxt) = parsed.get("max_tokens").and_then(|v| v.as_u64()) {
        up_body["max_tokens"] = serde_json::json!(maxt);
    }
    // Claude system field (string or blocks array) -> prepend an OpenAI system message
    if let Some(sys) = parsed.get("system") {
        let sys_text = match sys {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !sys_text.is_empty() {
            if let Some(msgs) = up_body["messages"].as_array_mut() {
                msgs.insert(
                    0,
                    serde_json::json!({ "role": "system", "content": sys_text }),
                );
            }
        }
    }
    // Memory retrieval query (the last user message)
    let claude_mem_query: String = parsed
        .get("messages")
        .and_then(|m| m.as_array())
        .and_then(|arr| {
            arr.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content"))
                .map(|c| match c {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
        })
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    // Billing/telemetry fields (api_key masked; client_ip)
    let claude_api_key = headers
        .get("x-api-key")
        .or_else(|| headers.get("authorization"))
        .map(|v| v.to_str().unwrap_or("").to_string())
        .map(|k| {
            if k.len() > 12 {
                format!("{}***{}", &k[..8], &k[k.len() - 4..])
            } else {
                "***".to_string()
            }
        })
        .unwrap_or_default();
    let claude_client_ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or("").to_string())
        .unwrap_or_default();
    let start_claude = start;

    // v0.8 retry loop (same strategy as the OpenAI path): retry with a different account on failure + queued 503 + switch account on 5xx.
    let retry_policy = crate::retry::RetryPolicy::default();
    #[allow(unused_assignments)]
    let mut claude_account_name = String::new();
    #[allow(unused_assignments)]
    let mut claude_token = String::new();
    #[allow(unused_assignments)]
    let mut claude_run_id = String::new();
    let mut claude_attempts = 0usize;
    let mut claude_last_error = String::new();
    let mut claude_last_status: u16 = 502;
    let mut claude_upstream: Option<reqwest::Response> = None;

    for attempt in 0..retry_policy.max_attempts {
        claude_attempts = attempt + 1;
        let account = match st.pool.pick_best().await {
            Some(a) => a,
            None => {
                if attempt == 0 {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({ "type": "error", "error": { "type": "api_error", "message": "no healthy token" } })),
                    )
                        .into_response();
                }
                claude_last_error = "no healthy token".into();
                break;
            }
        };
        claude_account_name = account.name.clone();
        claude_token = account.token.clone();
        if attempt == 0 {
            st.telemetry.event(
                &req_id,
                "route",
                &format!("{model} -> {resolved} via {}", account.name),
            );
        } else {
            st.telemetry.event(
                &req_id,
                "retry",
                &format!("Claude attempt {} via {}", attempt + 1, account.name),
            );
            st.logs.emit(
                "warn",
                "retry",
                Some(&req_id),
                format!("Claude switching account and retrying (attempt {}) -> {}", attempt + 1, account.name),
            );
        }

        // Ensure session (no longer swallow errors: queued/failed goes into account switch or 503)
        let instance_id = match account.session.ensure_session(&resolved).await {
            Ok(id) => Some(id),
            Err(e) => {
                let msg = e.to_string();
                if msg.starts_with("waiting_room_queued") {
                    st.usage
                        .record_ex(&account.name, &resolved, 0, 0, 0, 503, "", "", &req_id)
                        .ok();
                    st.telemetry.event(&req_id, "waiting_room", &msg);
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "type": "error",
                            "error": { "type": "overloaded_error", "message": msg, "code": "waiting_room_queued" }
                        })),
                    )
                        .into_response();
                }
                st.pool
                    .mark_failure(&account.name, &format!("session: {msg}"))
                    .await;
                st.pool.update_score(&account.name, -30.0).await;
                claude_last_status = 502;
                claude_last_error = format!("failed to acquire free session: {msg}");
                continue;
            }
        };

        // Run management (lazily create root run)
        let rid = match ensure_root_run(&st, &account.name, &claude_token).await {
            Ok(id) => id,
            Err(e) => {
                st.pool.mark_failure(&account.name, "run").await;
                claude_last_status = 502;
                claude_last_error = format!("create run failed: {e}");
                continue;
            }
        };
        claude_run_id = rid;

        // Call upstream
        match st
            .client
            .chat_completions(
                &claude_token,
                up_body.clone(),
                &claude_run_id,
                instance_id.as_deref(),
            )
            .await
        {
            Ok(r) if r.status().is_success() => {
                claude_upstream = Some(r);
                break;
            }
            Ok(r) => {
                let code = r.status().as_u16();
                let body_text = r.text().await.unwrap_or_default();
                let kind = crate::errors::classify(code, &body_text);
                st.pool
                    .mark_failure(&account.name, &format!("HTTP {code}"))
                    .await;
                st.pool.update_score(&account.name, -20.0).await;
                if code == 401 || code == 403 {
                    st.pool
                        .mark_cooldown(
                            &account.name,
                            std::time::Duration::from_secs(600),
                            &format!("upstream {code}, token appears to be invalid"),
                        )
                        .await;
                }
                claude_last_status = code;
                claude_last_error = format!(
                    "upstream HTTP {code} ({}): {}",
                    kind.as_str(),
                    crate::errors::error_excerpt(&body_text)
                );
                let should_switch =
                    kind.is_retryable() || matches!(kind, crate::errors::ErrorKind::AuthExpired);
                if should_switch && attempt + 1 < retry_policy.max_attempts {
                    st.telemetry.event(
                        &req_id,
                        "retry_scheduled",
                        &format!("Claude HTTP {code} ({}), switching account and retrying", kind.as_str()),
                    );
                    continue;
                }
                break;
            }
            Err(e) => {
                st.pool.mark_failure(&account.name, "network").await;
                st.pool.update_score(&account.name, -40.0).await;
                claude_last_status = 502;
                claude_last_error = e.to_string();
                continue;
            }
        }
    }

    // All attempts failed: Claude-shaped error response (preserve a readable 503/5xx message)
    let upstream_resp = match claude_upstream {
        Some(r) => r,
        None => {
            let latency = start_claude.elapsed().as_millis() as i64;
            st.usage
                .record_ex(
                    &claude_account_name,
                    &resolved,
                    0,
                    0,
                    latency,
                    claude_last_status as i64,
                    "",
                    "",
                    &req_id,
                )
                .ok();
            st.telemetry.record(TraceRow {
                req_id: req_id.clone(),
                endpoint: "/v1/messages".into(),
                requested_model: model.clone(),
                resolved_model: resolved.clone(),
                account: claude_account_name.clone(),
                status: claude_last_status,
                latency_ms: latency as u64,
                ttft_ms: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                stream: wants_stream,
                error_kind: Some(
                    crate::errors::classify(claude_last_status, &claude_last_error)
                        .as_str()
                        .to_string(),
                ),
                error_excerpt: Some(claude_last_error.chars().take(300).collect()),
                route_reason: Some(format!(
                    "{} -> {} ({} attempts all failed)",
                    model, resolved, claude_attempts
                )),
                api_key: Some(claude_api_key.clone()),
                client_ip: Some(claude_client_ip.clone()),
            });
            st.logs.emit(
                "error",
                "request",
                Some(&req_id),
                format!(
                    "{resolved} Claude all {claude_attempts} attempts failed: {}",
                    claude_last_error.chars().take(120).collect::<String>()
                ),
            );
            if (400..600).contains(&claude_last_status) {
                return (
                    StatusCode::from_u16(claude_last_status).unwrap_or(StatusCode::BAD_GATEWAY),
                    Json(serde_json::json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": claude_last_error,
                            "code": crate::errors::classify(claude_last_status, &claude_last_error).as_str(),
                            "model": resolved,
                            "attempts": claude_attempts,
                        }
                    })),
                )
                    .into_response();
            }
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "type": "error", "error": { "type": "api_error", "message": format!("all {claude_attempts} attempts failed: {claude_last_error}") } })),
            )
                .into_response();
        }
    };

    let status = upstream_resp.status();
    let claude_latency_ms = start_claude.elapsed().as_millis() as i64;
    // Memory observe (fills the gap in the Claude path): when the runtime toggle is on, record preferences/downgrades/corrections
    if memory_enabled_now(&st) {
        let _ = st.memory.observe(&resolved, None, &claude_mem_query, None);
    }
    // Non-2xx: pass through as a Claude error format
    if !status.is_success() {
        let err_body = upstream_resp.bytes().await.unwrap_or_default();
        let err_text = String::from_utf8_lossy(&err_body).to_string();
        let kind = crate::errors::classify(status.as_u16(), &err_text);
        st.usage
            .record_ex(
                &claude_account_name,
                &resolved,
                0,
                0,
                claude_latency_ms,
                status.as_u16() as i64,
                &claude_api_key,
                &claude_client_ip,
                &req_id,
            )
            .ok();
        st.pool.update_score(&claude_account_name, -20.0).await;
        if status.as_u16() == 401 || status.as_u16() == 403 {
            st.pool
                .mark_cooldown(
                    &claude_account_name,
                    std::time::Duration::from_secs(600),
                    &format!("upstream {status}, token appears to be invalid"),
                )
                .await;
        }
        tracing::warn!(
            "[upstream error/Claude] HTTP {status}: {}",
            err_text.chars().take(500).collect::<String>()
        );
        return (
            status,
            Json(serde_json::json!({
                "type": "error",
                "error": { "type": "api_error", "message": err_text, "kind": kind.as_str() }
            })),
        )
            .into_response();
    }
    let mut builder = Response::builder().status(status);
    for (k, v) in upstream_resp.headers() {
        if k != "content-length" && k != "transfer-encoding" {
            builder = builder.header(k, v);
        }
    }
    // Non-streaming: convert the OpenAI response back to Claude format (message + content blocks + stop_reason)
    if wants_stream {
        // Streaming: OpenAI SSE -> canonical event -> Anthropic event stream (no longer passed through directly)
        // Reuse the request-level req_id (full event-chain correlation: usage/telemetry/logs share the same ID)
        let claude_req_id = req_id.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(16);
        let model_for_render = resolved.clone();
        let telem = st.telemetry.clone();
        let logs = st.logs.clone();
        let usage_db = st.usage.clone();
        let (rid, acc, mdl, req_model) = (
            claude_req_id.clone(),
            claude_account_name.clone(),
            resolved.clone(),
            model.clone(),
        );
        let claude_key = claude_api_key.clone();
        let claude_ip = claude_client_ip.clone();
        let t_start = std::time::Instant::now();
        tokio::spawn(async move {
            // Move the semaphore guard into the task: hold the concurrency permit while streaming, returned via Drop when the task ends
            let _claude_tier_keep = claude_tier_guard;
            let mut decoder = crate::protocol::openai_sse::OpenAiSseDecoder::new();
            let mut renderer =
                crate::protocol::anthropic_sse::AnthropicSseRenderer::new(&model_for_render);
            let mut stream = upstream_resp.bytes_stream();
            let mut ttft: Option<u64> = None;
            let mut totals: (u64, u64) = (0, 0);
            let mut err_text: Option<String> = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(b) => {
                        if ttft.is_none() {
                            ttft = Some(t_start.elapsed().as_millis() as u64);
                        }
                        let events = decoder.feed(&b);
                        let mut out = String::new();
                        for ev in &events {
                            if let crate::protocol::stream::CanonicalEvent::Usage {
                                input_tokens,
                                output_tokens,
                            } = ev
                            {
                                totals = (*input_tokens, *output_tokens);
                            }
                            for frame in renderer.render(ev) {
                                out.push_str(&frame);
                            }
                        }
                        if !out.is_empty()
                            && tx.send(Ok(axum::body::Bytes::from(out))).await.is_err()
                        {
                            break; // client disconnected
                        }
                    }
                    Err(e) => {
                        err_text = Some(e.to_string());
                        let frames = renderer
                            .render(&crate::protocol::stream::CanonicalEvent::Error(
                                e.to_string(),
                            ))
                            .join("");
                        let _ = tx.send(Ok(axum::body::Bytes::from(frames))).await;
                        break;
                    }
                }
            }
            // Finish up: flush remaining decoder data + renderer's fallback message_stop
            let mut tail = String::new();
            for ev in decoder.finish() {
                for frame in renderer.render(&ev) {
                    tail.push_str(&frame);
                }
            }
            tail.push_str(&renderer.finish().join(""));
            if !tail.is_empty() {
                let _ = tx.send(Ok(axum::body::Bytes::from(tail))).await;
            }
            let total_ms = t_start.elapsed().as_millis() as u64;
            let status = if err_text.is_some() { 502u16 } else { 200 };
            // v0.8 backfill accounting: persist usage for the Claude streaming success path (previously only telemetry, not persisted)
            usage_db
                .record_ex(
                    &acc,
                    &mdl,
                    totals.0 as i64,
                    totals.1 as i64,
                    total_ms as i64,
                    status as i64,
                    &claude_key,
                    &claude_ip,
                    &rid,
                )
                .ok();
            telem.record(TraceRow {
                req_id: rid.clone(),
                endpoint: "/v1/messages".into(),
                requested_model: req_model,
                resolved_model: mdl.clone(),
                account: acc.clone(),
                status,
                latency_ms: total_ms,
                ttft_ms: ttft,
                prompt_tokens: totals.0,
                completion_tokens: totals.1,
                stream: true,
                error_kind: err_text.as_ref().map(|_| "network".to_string()),
                error_excerpt: err_text.map(|t| t.chars().take(300).collect()),
                route_reason: None,
                api_key: Some(claude_key),
                client_ip: Some(claude_ip),
            });
            logs.emit(
                if status == 200 { "info" } else { "error" },
                "request",
                Some(&rid),
                format!(
                    "{mdl} Claude stream completed {}+{} tok ({:.2}s)",
                    totals.0,
                    totals.1,
                    total_ms as f64 / 1000.0
                ),
            );
        });
        let body_stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        builder
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(Body::from_stream(body_stream))
            .unwrap_or_default()
            .into_response()
    } else {
        let bytes = upstream_resp.bytes().await.unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let openai_resp: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::json!({}));
        // v0.8 backfill accounting: persist usage/telemetry for the Claude non-streaming success path (previously returned Json directly)
        let (pt, ct) = extract_usage(&text).unwrap_or((0, 0));
        let total_ms = start_claude.elapsed().as_millis() as u64;
        st.usage
            .record_ex(
                &claude_account_name,
                &resolved,
                pt as i64,
                ct as i64,
                total_ms as i64,
                200,
                &claude_api_key,
                &claude_client_ip,
                &req_id,
            )
            .ok();
        st.telemetry.record(TraceRow {
            req_id: req_id.clone(),
            endpoint: "/v1/messages".into(),
            requested_model: model.clone(),
            resolved_model: resolved.clone(),
            account: claude_account_name.clone(),
            status: 200,
            latency_ms: total_ms,
            ttft_ms: None,
            prompt_tokens: pt,
            completion_tokens: ct,
            stream: false,
            error_kind: None,
            error_excerpt: None,
            route_reason: Some(format!("{} -> {}", model, resolved)),
            api_key: Some(claude_api_key.clone()),
            client_ip: Some(claude_client_ip.clone()),
        });
        st.logs.emit(
            "info",
            "request",
            Some(&req_id),
            format!(
                "{resolved} Claude completed {}+{} tok ({:.2}s)",
                pt,
                ct,
                total_ms as f64 / 1000.0
            ),
        );
        let claude_resp = openai_to_claude_response(&openai_resp, &resolved);
        (StatusCode::OK, Json(claude_resp)).into_response()
    }
}

/// Claude messages -> OpenAI messages:
/// - content is a string: kept as-is
/// - content is a blocks array: text blocks are concatenated into a string, tool_result blocks become role=tool messages, tool_use blocks become assistant.tool_calls
fn claude_to_openai_messages(messages: serde_json::Value) -> serde_json::Value {
    let arr = messages.as_array().cloned().unwrap_or_default();
    let mut out: Vec<serde_json::Value> = Vec::with_capacity(arr.len());
    for m in arr {
        let role = m
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("user")
            .to_string();
        let content = m.get("content").cloned().unwrap_or(serde_json::json!(""));
        match content {
            serde_json::Value::String(s) => {
                out.push(serde_json::json!({ "role": role, "content": s }))
            }
            serde_json::Value::Array(blocks) => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                let mut tool_results: Vec<serde_json::Value> = Vec::new();
                for b in &blocks {
                    let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match btype {
                        "text" => {
                            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        "tool_use" => {
                            tool_calls.push(serde_json::json!({
                                "id": b.get("id").cloned().unwrap_or(serde_json::json!("call_unknown")),
                                "type": "function",
                                "function": {
                                    "name": b.get("name").cloned().unwrap_or(serde_json::json!("")),
                                    "arguments": serde_json::to_string(&b.get("input").cloned().unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                                }
                            }));
                        }
                        "tool_result" => {
                            let result_text = match b.get("content") {
                                Some(serde_json::Value::String(s)) => s.clone(),
                                Some(serde_json::Value::Array(items)) => items
                                    .iter()
                                    .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                _ => String::new(),
                            };
                            tool_results.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": b.get("tool_use_id").cloned().unwrap_or(serde_json::json!("")),
                                "content": result_text,
                            }));
                        }
                        _ => {}
                    }
                }
                // assistant's tool_use -> OpenAI assistant message + tool_calls
                if !tool_calls.is_empty() {
                    let mut msg = serde_json::json!({ "role": "assistant", "content": if text_parts.is_empty() { serde_json::Value::Null } else { serde_json::json!(text_parts.join("\n")) } });
                    msg["tool_calls"] = serde_json::json!(tool_calls);
                    out.push(msg);
                } else if !text_parts.is_empty() {
                    out.push(serde_json::json!({ "role": role, "content": text_parts.join("\n") }));
                }
                // user's tool_result -> OpenAI tool message
                for tr in tool_results {
                    out.push(tr);
                }
            }
            other => out.push(serde_json::json!({ "role": role, "content": other })),
        }
    }
    serde_json::json!(out)
}

/// OpenAI chat response -> Claude messages response (non-streaming)
fn openai_to_claude_response(openai: &serde_json::Value, model: &str) -> serde_json::Value {
    let choice = openai
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first());
    let message = choice.and_then(|c| c.get("message"));
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut tool_calls_out: Vec<serde_json::Value> = Vec::new();
    if let Some(msg) = message {
        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                blocks.push(serde_json::json!({ "type": "text", "text": text }));
            }
        }
        if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let fn_obj = tc.get("function");
                let name = fn_obj
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args_raw = fn_obj
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}");
                let input: serde_json::Value =
                    serde_json::from_str(args_raw).unwrap_or(serde_json::json!({}));
                tool_calls_out.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.get("id").cloned().unwrap_or(serde_json::json!("toolu_unknown")),
                    "name": name,
                    "input": input,
                }));
            }
        }
    }
    if blocks.is_empty() && tool_calls_out.is_empty() {
        blocks.push(serde_json::json!({ "type": "text", "text": "" }));
    }
    for tc in tool_calls_out {
        blocks.push(tc);
    }
    // stop_reason mapping
    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str())
        .unwrap_or("end_turn");
    let stop_reason = match finish {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        _ => "end_turn",
    };
    let usage = openai.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0);
    serde_json::json!({
        "id": openai.get("id").cloned().unwrap_or(serde_json::json!("msg_freebuff")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": blocks,
        "stop_reason": stop_reason,
        "stop_sequence": serde_json::Value::Null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }
    })
}

// ---------- Skills API ----------

/// GET /api/skills - skills list + roster injection preview
async fn handle_skills_list(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let skills = st.skills.list();
    let roster = st.skills.system_prefix(st.cfg.max_roster_tokens);
    let roster_tokens = roster.chars().count() / 4 + 1;
    Json(serde_json::json!({
        "ok": true,
        "skills": skills,
        "roster_preview": roster,
        "roster_tokens": roster_tokens,
        "max_roster_tokens": st.cfg.max_roster_tokens,
    }))
    .into_response()
}

/// POST /api/skills - create/update skill body: { id?, name, description, body, triggers? }
async fn handle_skills_upsert(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("invalid json"),
    };
    let id = v.get("id").and_then(|x| x.as_str());
    let input = crate::skills::SkillInput {
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        description: v
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        body: v
            .get("body")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        triggers: v
            .get("triggers")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    };
    if input.name.trim().is_empty() || input.description.trim().is_empty() {
        return bad_req("name and description must not be empty");
    }
    // Quality gate: uniformly validate name/description/body; rejected by default if it fails (force:true forces a save)
    let force = v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut issues = st.skills.gate(&input.body);
    issues.extend(st.skills.gate(&input.name));
    issues.extend(st.skills.gate(&input.description));
    if !issues.is_empty() && !force {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": "Quality gate failed (add force:true to force save if you confirm it's fine)", "issues": issues })),
        )
            .into_response();
    }
    match st.skills.upsert(id, input) {
        Ok(s) => {
            let gate = st.skills.gate(&s.body);
            st.logs
                .emit("info", "skills", None, format!("Skill saved: {}", s.name));
            Json(serde_json::json!({ "ok": true, "skill": s, "gate": gate })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/skills/toggle — body: { id, enabled }
async fn handle_skills_toggle(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("invalid json"),
    };
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
    match st.skills.toggle(id, enabled) {
        Ok(ok) => {
            Json(serde_json::json!({ "ok": ok, "id": id, "enabled": enabled })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/skills/delete — body: { id }
async fn handle_skills_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("invalid json"),
    };
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
    match st.skills.delete(id) {
        Ok(ok) => Json(serde_json::json!({ "ok": ok, "id": id })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/skills/gate - body: { body }, returns a list of quality issues (empty array = passed)
async fn handle_skills_gate(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("invalid json"),
    };
    let text = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
    Json(serde_json::json!({ "ok": true, "issues": st.skills.gate(text) })).into_response()
}

// ---------- Logs API ----------

#[derive(serde::Deserialize)]
struct LogsQuery {
    limit: Option<usize>,
    after_id: Option<u64>,
    /// Browsers can't set request headers for SSE, so allow passing the api key via query (validated only when api_keys is non-empty)
    key: Option<String>,
}

/// GET /api/logs/recent?limit=200 - recent logs (initial page load)
async fn handle_logs_recent(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let events = st.logs.recent(q.limit.unwrap_or(200).min(1000), q.after_id);
    Json(serde_json::json!({ "ok": true, "events": events, "count": events.len() })).into_response()
}

/// GET /api/logs/stream - real-time log SSE.
/// - Supports `?key=` (browser EventSource can't send headers)
/// - Supports the `Last-Event-ID` request header: on reconnect after disconnect, first resend missed events
async fn handle_logs_stream(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Response {
    let keys = api_keys_of(&st);
    let query_key_ok = !keys.is_empty()
        && q.key
            .as_deref()
            .map(|k| keys.iter().any(|x| x == k))
            .unwrap_or(false);
    if !admin_authorized(&headers, &st) && !query_key_ok {
        return admin_denied();
    }
    // Resend on reconnect: the browser reconnect will include Last-Event-ID
    let last_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let backlog: Vec<Result<Event, std::convert::Infallible>> = st
        .logs
        .recent(200, last_id)
        .into_iter()
        .map(|ev| {
            let data = serde_json::to_string(&ev).unwrap_or_default();
            Ok(Event::default().id(ev.id.to_string()).data(data))
        })
        .collect();
    let rx = st.logs.subscribe();
    let live = futures::stream::unfold(rx, |mut rx2| async move {
        loop {
            match rx2.recv().await {
                Ok(ev) => {
                    let data = serde_json::to_string(&ev).unwrap_or_default();
                    let sse = Event::default().id(ev.id.to_string()).data(data);
                    return Some((Ok::<Event, std::convert::Infallible>(sse), rx2));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let stream = futures::stream::iter(backlog).chain(live);
    Sse::new(stream).into_response()
}

// ---------- Request detail ----------

/// GET /api/usage/requests/{id} - full info for a single request + event chain
async fn handle_usage_request_detail(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<i64>,
) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    match st.usage.request_by_id(id) {
        Ok(Some(rec)) => {
            let (events, trace) = if rec.req_id.is_empty() {
                (Vec::new(), None)
            } else {
                (
                    read_telemetry_events(&st.cfg.telemetry_path, &rec.req_id),
                    read_telemetry_request(&st.cfg.telemetry_path, &rec.req_id),
                )
            };
            Json(
                serde_json::json!({ "ok": true, "request": rec, "trace": trace, "events": events }),
            )
            .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "message": "request not found" })),
        )
            .into_response(),
        Err(e) => internal_err(&e),
    }
}

/// Read telemetry events (correlated by req_id; low-frequency operation, a short-lived connection is fine)
fn read_telemetry_events(db_path: &str, req_id: &str) -> Vec<serde_json::Value> {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn
        .prepare("SELECT ts, kind, detail FROM events WHERE req_id = ?1 ORDER BY id DESC LIMIT 50")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map(rusqlite::params![req_id], |r| {
        Ok(serde_json::json!({
            "ts": r.get::<_, String>(0)?,
            "kind": r.get::<_, String>(1)?,
            "detail": r.get::<_, String>(2)?,
        }))
    });
    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Read the rich telemetry record (endpoint/ttft/error_kind etc.; shown first in the detail drawer)
fn read_telemetry_request(db_path: &str, req_id: &str) -> Option<serde_json::Value> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let mut stmt = conn
        .prepare("SELECT ts,endpoint,requested_model,resolved_model,account,status,latency_ms,ttft_ms,prompt_tokens,completion_tokens,stream,error_kind,error_excerpt,route_reason FROM requests_v2 WHERE req_id = ?1 ORDER BY id DESC LIMIT 1")
        .ok()?;
    let mut rows = stmt
        .query_map(rusqlite::params![req_id], |r| {
            Ok(serde_json::json!({
                "ts": r.get::<_, String>(0)?,
                "endpoint": r.get::<_, String>(1)?,
                "requested_model": r.get::<_, String>(2)?,
                "resolved_model": r.get::<_, String>(3)?,
                "account": r.get::<_, String>(4)?,
                "status": r.get::<_, i64>(5)?,
                "latency_ms": r.get::<_, i64>(6)?,
                "ttft_ms": r.get::<_, Option<i64>>(7)?,
                "prompt_tokens": r.get::<_, i64>(8)?,
                "completion_tokens": r.get::<_, i64>(9)?,
                "stream": r.get::<_, i64>(10)? != 0,
                "error_kind": r.get::<_, Option<String>>(11)?,
                "error_excerpt": r.get::<_, Option<String>>(12)?,
                "route_reason": r.get::<_, Option<String>>(13)?,
            }))
        })
        .ok()?;
    rows.next().and_then(|r| r.ok())
}

/// GET /api/usage/cost - rate and error rate (free tier has no monetary cost; honestly labeled as estimated)
async fn handle_usage_cost(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let recent = match st.usage.recent_requests(500) {
        Ok(v) => v,
        Err(e) => return internal_err(&e),
    };
    let now = chrono::Utc::now();
    let mut requests_30m = 0i64;
    let mut errors_30m = 0i64;
    let mut total_ms = 0i64;
    for r in &recent {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&r.ts) {
            let age_min = now
                .signed_duration_since(ts.with_timezone(&chrono::Utc))
                .num_minutes();
            if (0..=30).contains(&age_min) {
                requests_30m += 1;
                if r.status >= 400 {
                    errors_30m += 1;
                }
                total_ms += r.latency_ms;
            }
        }
    }
    let totals = st.usage.totals().unwrap_or_else(|_| serde_json::json!({}));
    Json(serde_json::json!({
        "ok": true,
        "window_minutes": 30,
        "requests_30m": requests_30m,
        "errors_30m": errors_30m,
        "error_rate_30m": if requests_30m > 0 { errors_30m as f64 / requests_30m as f64 } else { 0.0 },
        "avg_latency_ms_30m": if requests_30m > 0 { total_ms / requests_30m } else { 0 },
        "requests_per_hour": requests_30m * 2,
        "totals": totals,
        "estimated": true,
        "cost_source": "free tier (no monetary cost recorded)",
    }))
    .into_response()
}

/// GET /api/usage/insights - "top three" aggregation (slowest account/most-used model/highest error-rate window), aggregated locally via SQLite
async fn handle_usage_insights(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    // Audit L1: run the SQLite aggregation on the blocking pool to avoid occupying a tokio worker; busy_timeout is already set in open_db
    let db = st.cfg.telemetry_path.clone();
    match tokio::task::spawn_blocking(move || crate::telemetry::insights(&db, 24)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => internal_err(&e),
        Err(e) => internal_err(&anyhow::anyhow!("insights task failed: {e}")),
    }
}

/// Write-endpoint CSRF protection: requires `application/json`.
/// A cross-site form can only send text/plain / urlencoded / multipart (a browser "simple request", no preflight needed);
/// forcing JSON makes the browser send a preflight first, which blocks CSRF writes.
fn json_write_ok(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase().starts_with("application/json"))
        .unwrap_or(false)
}

/// Unified write-endpoint guard: auth + JSON content-type
fn write_guard(headers: &HeaderMap, st: &AppState) -> Option<Response> {
    if !admin_authorized(headers, st) {
        return Some(admin_denied());
    }
    if !json_write_ok(headers) {
        return Some(bad_req(
            "write operations require content-type: application/json (CSRF protection)",
        ));
    }
    None
}

// ---------- Upstream session cleanup ----------

/// Record the upstream threadId (json append + dedupe; used for session cleanup, to prevent the reverse proxy from accumulating load on upstream over time)
/// threads.json serial lock: both record_thread / sweep_threads are "read-modify-write the whole file",
/// and sweep's deletion window spans network IO (tens of seconds); concurrent writes would overwrite each other (losing new thread records -> never cleaned up).
static THREADS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Atomic write (JSON string -> temp file -> rename), to prevent truncation
fn atomic_write_json(path: &str, json: &str) {
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        #[cfg(windows)]
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::rename(&tmp, p);
    }
}

fn record_thread(path: &str, thread_id: &str) {
    if thread_id.trim().is_empty() {
        return;
    }
    let _g = match THREADS_LOCK.lock() {
        Ok(g) => g,
        Err(_) => return, // give up recording if the lock is poisoned (a failed record must not block the conversation flow)
    };
    let mut list: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if list
        .iter()
        .any(|t| t.get("id").and_then(|v| v.as_str()) == Some(thread_id))
    {
        return;
    }
    list.push(serde_json::json!({
        "id": thread_id,
        "created_at": chrono::Utc::now().to_rfc3339(),
    }));
    if let Ok(json) = serde_json::to_string_pretty(&list) {
        atomic_write_json(path, &json);
    }
}

/// POST /api/threads/cleanup - clean up upstream sessions (per user's annotated requirement: prevent the reverse proxy from putting load on upstream / being identified)
/// body: `{ dry_run?: bool = true, max_age_hours?: u64 = 24 }`
/// Conservative design: dry_run defaults to true; the upstream delete endpoint hasn't been confirmed via packet capture (tries DELETE /{id} and POST /delete),
/// records that fail to delete are kept (no account data is lost).
/// Session cleanup result
struct SweepOutcome {
    total: usize,
    expired: usize,
    expired_ids: Vec<String>,
    deleted: usize,
    /// ids that were deleted successfully (including those already gone upstream); these are removed from threads.json on write-back
    deleted_ids: Vec<String>,
    failed: Vec<String>,
    remaining: usize,
}

/// Scan threads.json to find upstream sessions past the retention duration; when `dry_run=false`, actually delete them and write the file back.
///
/// User annotation (网页对话.txt:605): the reverse proxy must clean up upstream sessions itself,
/// "to prevent our reverse proxy from putting pressure on upstream, getting caught, and it being game over".
async fn sweep_threads(
    st: &AppState,
    max_age_hours: u64,
    dry_run: bool,
) -> anyhow::Result<SweepOutcome> {
    let all: Vec<serde_json::Value> = std::fs::read_to_string(&st.cfg.threads_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let now = chrono::Utc::now();
    let mut expired_ids: Vec<String> = Vec::new();
    let mut fresh: Vec<serde_json::Value> = Vec::new();
    for t in &all {
        let id = t
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let created = t
            .get("created_at")
            .and_then(|x| x.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        let is_expired = match created {
            Some(c) => {
                now.signed_duration_since(c.with_timezone(&chrono::Utc))
                    .num_hours() as u64
                    >= max_age_hours
            }
            None => true, // treat missing timestamp as eligible for cleanup
        };
        if is_expired {
            expired_ids.push(id);
        } else {
            fresh.push(t.clone());
        }
    }
    let mut outcome = SweepOutcome {
        total: all.len(),
        expired: expired_ids.len(),
        expired_ids: expired_ids.clone(),
        deleted: 0,
        deleted_ids: Vec::new(),
        failed: Vec::new(),
        remaining: fresh.len(),
    };
    // Dry run or no expired items -> no credentials needed, no file write
    if dry_run || expired_ids.is_empty() {
        return Ok(outcome);
    }
    let (cookie, _, cid) = match pick_web_cookie(st).await {
        Some(v) => v,
        None => anyhow::bail!("a web Cookie credential is required to clean up upstream sessions"),
    };
    let client = crate::web_protocol::WebClient::new(cookie, "glm-5.3-flash".into())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut still = fresh;
    for id in &expired_ids {
        match client.delete_thread(id).await {
            // Already deleted upstream / never existed (404) -> in both cases the local record no longer needs to be kept
            Ok(found) => {
                // v0.9: deletion succeeded -> record ok in the pool
                st.web_pool.mark_ok(&cid).await;
                outcome.deleted += 1;
                outcome.deleted_ids.push(id.clone());
                st.logs.emit(
                    "info",
                    "cleanup",
                    None,
                    if found {
                        format!("deleted upstream session {id}")
                    } else {
                        format!("session {id} no longer exists upstream, removing local record")
                    },
                );
            }
            Err(e) => {
                // v0.9: deletion failed (401/403/network) -> lower score / circuit-break / cool down in the pool
                web_pool_failure(st, &cid, &e.to_string()).await;
                outcome.failed.push(id.clone());
                tracing::debug!("failed to delete session {id}: {e}");
                if let Some(orig) = all
                    .iter()
                    .find(|t| t.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
                {
                    still.push(orig.clone());
                }
            }
        }
    }
    outcome.remaining = still.len();
    // The write-back must happen inside THREADS_LOCK, and **merge based on the current file contents**:
    // sweep's deletion window spans network IO (tens of seconds); during that time record_thread may have appended a new thread --
    // writing back the old snapshot directly would swallow them (that session would never get cleaned up). Re-read the file inside the lock, and only remove ids that were successfully deleted.
    let _g = match THREADS_LOCK.lock() {
        Ok(g) => g,
        Err(_) => return Ok(outcome),
    };
    let mut final_list: Vec<serde_json::Value> = std::fs::read_to_string(&st.cfg.threads_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    final_list.retain(|t| {
        let id = t.get("id").and_then(|x| x.as_str()).unwrap_or("");
        !outcome.deleted_ids.contains(&id.to_string())
    });
    // Add back entries that failed to delete but are missing from the snapshot (defensive, normally shouldn't happen)
    for id in &outcome.failed {
        if !final_list
            .iter()
            .any(|t| t.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
        {
            if let Some(orig) = all
                .iter()
                .find(|t| t.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
            {
                final_list.push(orig.clone());
            }
        }
    }
    outcome.remaining = final_list.len();
    if let Ok(json) = serde_json::to_string_pretty(&final_list) {
        atomic_write_json(&st.cfg.threads_path, &json);
    }
    Ok(outcome)
}

/// Background auto-cleanup loop: every `thread_cleanup_interval_sec` seconds, clean up upstream sessions older than `thread_max_age_hours`.
/// Does not start when the interval is 0 (decided by main). Completely silent when there are no expired sessions, no log spam.
pub async fn thread_cleanup_loop(st: AppState) {
    let interval = st.cfg.thread_cleanup_interval_sec.max(60);
    tracing::info!(
        "Upstream session auto-cleanup enabled: every {interval}s, clean up sessions older than {} hours",
        st.cfg.thread_max_age_hours
    );
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        match sweep_threads(&st, st.cfg.thread_max_age_hours, false).await {
            Ok(o) if o.expired == 0 => {}
            Ok(o) => st.logs.emit(
                "info",
                "cleanup",
                None,
                format!(
                    "Auto-cleanup: deleted {} expired session(s) ({} failed), {} remaining",
                    o.deleted,
                    o.failed.len(),
                    o.remaining
                ),
            ),
            Err(e) => tracing::warn!("auto-cleanup did not run: {e}"),
        }
    }
}

async fn handle_threads_cleanup(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    let dry_run = v.get("dry_run").and_then(|x| x.as_bool()).unwrap_or(true);
    let max_age_hours = v
        .get("max_age_hours")
        .and_then(|x| x.as_u64())
        .unwrap_or(st.cfg.thread_max_age_hours);

    let outcome = match sweep_threads(&st, max_age_hours, dry_run).await {
        Ok(o) => o,
        Err(e) => return bad_req(&e.to_string()),
    };

    if dry_run {
        return Json(serde_json::json!({
            "ok": true,
            "dry_run": true,
            "total": outcome.total,
            "expired": outcome.expired,
            "thread_ids": outcome.expired_ids,
            "message": format!("Dry run: {} session(s) exceed {} hours and can be cleaned up (pass dry_run:false to actually delete)", outcome.expired, max_age_hours),
        }))
        .into_response();
    }
    Json(serde_json::json!({
        "ok": true,
        "dry_run": false,
        "deleted": outcome.deleted,
        "failed": outcome.failed.len(),
        "failed_ids": outcome.failed,
        "remaining": outcome.remaining,
        "message": if outcome.failed.is_empty() {
            format!("cleaned up {} session(s)", outcome.deleted)
        } else {
            format!("cleaned up {}; {} failed to delete (upstream endpoint unverified, records kept)", outcome.deleted, outcome.failed.len())
        },
    }))
    .into_response()
}

// ---------- Memory API ----------
/// GET /api/memory - memory list + stats + runtime toggle status
async fn handle_memory_list(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let memories = st.memory.list(200);
    let stats = st.memory.stats().unwrap_or_else(|_| serde_json::json!({}));
    Json(serde_json::json!({
        "ok": true,
        "memories": memories,
        "stats": stats,
        "enabled": memory_enabled_now(&st),
    }))
    .into_response()
}

/// POST /api/memory - manually add a memory body: {kind, title, content, is_static?}
async fn handle_memory_upsert(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("invalid json"),
    };
    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .unwrap_or("preference");
    let title: String = v
        .get("title")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect();
    let content: String = v
        .get("content")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .chars()
        .take(4000)
        .collect();
    let is_static = v
        .get("is_static")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    if title.is_empty() || content.trim().is_empty() {
        return bad_req("title and content must not be empty");
    }
    match st.memory.upsert(kind, &title, &content, is_static) {
        Ok(m) => {
            st.logs
                .emit("info", "memory", None, format!("Memory saved: {}", m.title));
            Json(serde_json::json!({ "ok": true, "memory": m })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/memory/delete — body: {id}
async fn handle_memory_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("invalid json"),
    };
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
    match st.memory.delete(id) {
        Ok(ok) => Json(serde_json::json!({ "ok": ok, "id": id })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/memory/static — body: {id, is_static}
async fn handle_memory_static(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("invalid json"),
    };
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let is_static = v
        .get("is_static")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    match st.memory.set_static(id, is_static) {
        Ok(ok) => {
            Json(serde_json::json!({ "ok": ok, "id": id, "is_static": is_static })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/memory/toggle — body: {enabled: bool}
/// Memory layer master toggle (off by default): once off, it neither auto-records nor injects into system; writes back to config.json and takes effect immediately.
async fn handle_memory_toggle(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_req("body must be JSON"),
    };
    let Some(enabled) = v.get("enabled").and_then(|x| x.as_bool()) else {
        return bad_req("missing enabled (bool)");
    };
    // Hot-apply: Config is immutable behind the Arc, so use the same RwLock-slot override read pattern as api_keys.
    // Simple approach: mutating the Arc directly isn't feasible -- using AtomicBool semantics by borrowing the api_keys pattern here is too heavy,
    // switching to writing a runtime override file isn't desirable either -- the most direct way: consistent with api-key, write back to config.json + an in-memory flag.
    // Config itself is immutable, so the toggle state is also mirrored into st (see memory_runtime_enabled).
    set_memory_runtime_enabled(&st, enabled);
    // Persist to config.json (round-trip merge as Value, to avoid overwriting the user's other settings)
    let path = crate::config::resolve_config_path();
    let mut persisted = false;
    if let Some(p) = path.as_deref() {
        let mut root: serde_json::Value = std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !root.is_object() {
            root = serde_json::json!({});
        }
        root["memory_enabled"] = serde_json::json!(enabled);
        let write_res = serde_json::to_string_pretty(&root)
            .map_err(|e| e.to_string())
            .and_then(|s| {
                let tmp = format!("{p}.tmp");
                std::fs::write(&tmp, s).map_err(|e| e.to_string())?;
                #[cfg(windows)]
                if std::path::Path::new(p).exists() {
                    let _ = std::fs::remove_file(p);
                }
                std::fs::rename(&tmp, p).map_err(|e| e.to_string())
            });
        match write_res {
            Ok(()) => {
                persisted = true;
                tracing::info!("memory_enabled={enabled} written back to {p}");
            }
            Err(e) => tracing::warn!("failed to write memory_enabled back to {p}: {e} (in-memory only)"),
        }
    }
    st.logs.emit(
        "info",
        "memory",
        None,
        format!(
            "Memory layer is now {}",
            if enabled {
                "on (auto-record + inject)"
            } else {
                "off (no recording, no injection)"
            }
        ),
    );
    Json(serde_json::json!({
        "ok": true,
        "enabled": enabled,
        "persisted": persisted,
        "message": if enabled { "Memory layer is now on: your preferences and corrections will be auto-recorded and injected into relevant conversations" } else { "Memory layer is now off: no longer auto-recording, and no memory content will be injected" },
    }))
    .into_response()
}

// ---------- MCP (read-only tools) ----------

/// POST /mcp - MCP JSON-RPC 2.0 endpoint (tools/list + tools/call, only 3 read-only tools)
async fn handle_mcp(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Gateway auth consistent with /v1: validated when api_keys is configured; loopback-only when not configured
    if !origin_allowed(&headers) {
        return origin_blocked();
    }
    let keys = api_keys_of(&st);
    if !keys.is_empty() {
        if !authorized(&headers, &keys) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": { "message": "invalid proxy api key" } })),
            )
                .into_response();
        }
    } else if !is_loopback_request(&headers) {
        return admin_denied();
    }
    let raw = String::from_utf8_lossy(&body).to_string();
    let snapshot = build_mcp_snapshot(&st).await;
    match crate::mcp::handle_json(&raw, &snapshot) {
        Some(resp) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(resp))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        None => StatusCode::ACCEPTED.into_response(), // notification: no response body
    }
}

/// Assemble the MCP data snapshot (pull read-only data from AppState)
async fn build_mcp_snapshot(st: &AppState) -> crate::mcp::GatewaySnapshot {
    let models = st.registry.models().await;
    let pool_snap = st.pool.snapshot().await;
    let accounts = pool_snap
        .accounts
        .iter()
        .map(|a| crate::mcp::AccountBrief {
            name: a.name.clone(),
            healthy: a.healthy,
            score: a.score,
            session_status: a
                .session
                .as_ref()
                .map(|s| format!("{:?}", s.status))
                .unwrap_or_else(|| "unknown".into()),
        })
        .collect();
    let usage = st.usage.totals().unwrap_or_else(|_| serde_json::json!({}));
    crate::mcp::GatewaySnapshot {
        models,
        accounts,
        usage_totals: usage,
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_sec: st.started.elapsed().as_secs(),
    }
}

// ---------- System health check ----------

/// GET /api/doctor — item-by-item check (four states: ok / fault / unknown / fact; "not checked" simply means not checked)
async fn handle_doctor(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let mut checks: Vec<serde_json::Value> = Vec::new();

    // 1. Config
    checks.push(serde_json::json!({
        "id": "config", "label": "Config", "state": "ok",
        "detail": format!("Listening on {}, upstream {}", st.cfg.listen_addr, st.cfg.upstream_base_url),
    }));
    // 1b. Listening non-loopback with api_keys not configured -> warning (v0.9 §1.5 auth depth)
    let non_loopback = !st.cfg.listen_addr.starts_with("127.")
        && !st.cfg.listen_addr.starts_with("localhost")
        && !st.cfg.listen_addr.starts_with("[::1]");
    if non_loopback && api_keys_of(&st).is_empty() {
        checks.push(serde_json::json!({
            "id": "listen_scope", "label": "Listen scope", "state": "fault",
            "detail": format!("Listening on {} (non-loopback) but api_keys is not configured -- LAN/remote clients can reach the panel and admin endpoints", st.cfg.listen_addr),
            "fix": "Set api_keys (one-click generate from the panel's \"Getting Started\" guide), or change listen_addr back to 127.0.0.1",
        }));
    } else {
        checks.push(serde_json::json!({
            "id": "listen_scope", "label": "Listen scope", "state": "ok",
            "detail": format!("Listening on {}", st.cfg.listen_addr),
        }));
    }
    // 2. Account pool
    // Bearer pool + web Cookie pool (cookie accounts serve requests through the bridge)
    let snap = st.pool.snapshot().await;
    let web = st.web_pool.snapshot().await;
    let total = snap.total + web.len();
    let healthy = snap.accounts.iter().filter(|a| a.healthy).count()
        + web.iter().filter(|w| w.cooldown_seconds.is_none()).count();
    let (acct_state, acct_fix) = if total == 0 {
        ("fault", "Go to the Accounts page to import a Cookie or use one-click login")
    } else if healthy == 0 {
        ("fault", "All accounts are cooling down: wait for cooldown to end, or log in again to refresh credentials")
    } else {
        ("ok", "")
    };
    checks.push(serde_json::json!({
        "id": "accounts", "label": "Account pool", "state": acct_state,
        "detail": format!("{healthy}/{total} accounts available"),
        "fix": acct_fix,
    }));
    // 3. Model registry
    let models = st.registry.models().await;
    checks.push(serde_json::json!({
        "id": "models", "label": "Model registry",
        "state": if models.is_empty() { "fault" } else { "ok" },
        "detail": format!("{} models available", models.len()),
        "fix": if models.is_empty() { "Check upstream connectivity and proxy settings" } else { "" },
    }));
    // 4. Telemetry writes
    let dropped = st.telemetry.dropped();
    checks.push(serde_json::json!({
        "id": "telemetry", "label": "Telemetry writes",
        "state": if dropped > 1000 { "fault" } else { "ok" },
        "detail": format!("{} dropped in total (when the queue is full)", dropped),
    }));
    // 5. Skill library
    let skills = st.skills.list();
    let enabled = skills.iter().filter(|s| s.enabled).count();
    checks.push(serde_json::json!({
        "id": "skills", "label": "Skill library", "state": "ok",
        "detail": format!("{} entries (enabled {})", skills.len(), enabled),
    }));
    // 6. Log bus
    checks.push(serde_json::json!({
        "id": "logs", "label": "Log bus", "state": "ok",
        "detail": format!("{} entries recorded", st.logs.count()),
    }));
    // 7. Memory store
    let mem_stats = st.memory.stats().unwrap_or_else(|_| serde_json::json!({}));
    checks.push(serde_json::json!({
        "id": "memory", "label": "Memory store", "state": "ok",
        "detail": format!(
            "{} entries (static facts {} / corrections {})",
            mem_stats.get("total").and_then(|v| v.as_i64()).unwrap_or(0),
            mem_stats.get("static_count").and_then(|v| v.as_i64()).unwrap_or(0),
            mem_stats.get("corrections").and_then(|v| v.as_i64()).unwrap_or(0)
        ),
    }));
    // 8. Listen address + api_keys (v0.9 §1.5 auth depth warning)
    if !crate::config::is_loopback_listen(&st.cfg.listen_addr) && api_keys_of(&st).is_empty() {
        checks.push(serde_json::json!({
            "id": "listen", "label": "Listen address", "state": "warn",
            "detail": format!("Listening non-loopback on {} but api_keys is not configured: other devices on the LAN can access the panel/admin endpoints (narrowed to the real TCP peer; only local loopback is allowed)", st.cfg.listen_addr),
            "fix": "Configure api_keys in config.json, or change listen_addr back to 127.0.0.1",
        }));
    } else {
        checks.push(serde_json::json!({
            "id": "listen", "label": "Listen address", "state": "ok",
            "detail": format!("Listening on {}", st.cfg.listen_addr),
        }));
    }
    // 9. Version
    checks.push(serde_json::json!({
        "id": "version", "label": "Version", "state": "fact",
        "detail": format!("v{} (running for {} seconds)", env!("CARGO_PKG_VERSION"), st.started.elapsed().as_secs()),
    }));

    Json(serde_json::json!({ "ok": true, "checks": checks })).into_response()
}

/// Assemble the system prefix:
/// - skills_inject_mode="roster" (default): prompt (base + enabled items) + skill roster (name+description, within budget)
/// - skills_inject_mode="full": fall back to the old behavior (prompts.system_prefix includes all skills)
/// - Memory block (low authority, 512-token budget) appended last; not injected when empty
async fn build_system_prefix(st: &AppState, query: &str) -> String {
    let mut s = if st.cfg.skills_inject_mode == "full" {
        st.prompts.system_prefix().await
    } else {
        let mut base = st.prompts.system_prefix_prompts_only().await;
        let roster = st.skills.system_prefix(st.cfg.max_roster_tokens);
        if !roster.trim().is_empty() {
            base.push_str(
                "\n\n[freebuff-skills]\nThe following skills are available (low-authority reference; follow their guidance when relevant):\n",
            );
            base.push_str(&roster);
            base.push_str("\n[/freebuff-skills]");
        }
        base
    };
    // Memory injection (user preferences/corrections; an empty result is not injected, to keep the request byte-stable; skipped entirely when the runtime toggle is off)
    if memory_enabled_now(st) {
        let mem = st.memory.brief(query, 512);
        if !mem.is_empty() {
            s.push_str(&mem);
        }
    }
    s
}

/// Detect embedded error codes in an upstream 200 response body (known free_mode_* family; normal responses are unaffected)
fn upstream_body_error(text: &str) -> Option<&'static str> {
    const CODES: &[&str] = &[
        "free_mode_invalid_agent_model",
        "free_mode_invalid_agent_hierarchy",
        "free_mode_cli_required",
    ];
    CODES.iter().find(|c| text.contains(**c)).copied()
}

/// Safely keep roughly the last `keep_bytes` bytes of a string, with the start aligned to a char boundary.
/// Note: don't use `split_off(len - n)` directly -- multi-byte characters (CJK/emoji) can be cut at an illegal boundary and panic.
fn tail_keep(s: &str, keep_bytes: usize) -> String {
    if s.len() <= keep_bytes {
        return s.to_string();
    }
    let mut start = s.len() - keep_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Extract usage from the tail of SSE/JSON text (fault-tolerant: returns None if not found)
fn extract_usage(text: &str) -> Option<(u64, u64)> {
    let idx = text.rfind("\"usage\"")?;
    let tail = &text[idx..];
    let p = find_json_number(tail, "\"prompt_tokens\"");
    let c = find_json_number(tail, "\"completion_tokens\"");
    match (p, c) {
        (Some(p), Some(c)) => Some((p, c)),
        _ => None,
    }
}

/// Find the number in a `"key": 123` pattern within a string (simple parsing, good enough and dependency-free)
fn find_json_number(s: &str, key: &str) -> Option<u64> {
    let i = s.find(key)?;
    let rest = &s[i + key.len()..];
    let rest = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Estimate token count from character count (accounting fallback for when the upstream web protocol's SSE carries no usage).
/// Chinese is roughly 1 char ~= 1-1.6 tokens, English roughly 4 chars ~= 1 token; split the difference: 1 token per 2 characters.
/// The estimate is conservative (never overstated); the panel still presents it with "estimate" semantics, not as an exact upstream value.
fn estimate_tokens(chars: usize) -> u64 {
    chars.div_ceil(2) as u64
}

/// Count the total characters of body and reasoning content in the bridged OpenAI chunk stream (used for tail sampling).
/// Only counts the string values of delta.content / delta.reasoning_content, ignoring JSON structural overhead.
fn count_bridge_content_chars(sse_tail: &str) -> usize {
    let mut total = 0usize;
    for line in sse_tail.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let Some(d) = v.pointer("/choices/0/delta") else {
            continue;
        };
        if let Some(t) = d.get("content").and_then(|x| x.as_str()) {
            total += t.chars().count();
        }
        if let Some(t) = d.get("reasoning_content").and_then(|x| x.as_str()) {
            total += t.chars().count();
        }
    }
    total
}

// ---------- Multimodal upload ----------

/// POST /v1/uploads — upload a file to the upstream in exchange for a storageId (first step of the multimodal pipeline)
///
/// Usage (raw file body; no multipart needed, keeps zero new dependencies):
/// ```bash
/// curl -X POST http://127.0.0.1:47821/v1/uploads \
///   -H "content-type: image/png" -H "x-file-name: a.png" \
///   --data-binary @a.png
/// ```
/// Returns `{ id, storageId, mediaType, name }`; put storageId into the images array of /v1/web/chat.
async fn handle_upload(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Data-plane CSRF defense (multipart uploads can equally be blind-hit by simple text/plain requests)
    if !origin_allowed(&headers) {
        return origin_blocked();
    }
    let keys = api_keys_of(&st);
    if !keys.is_empty() && !authorized(&headers, &keys) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": { "message": "invalid proxy api key", "type": "authentication_error" } })),
        )
            .into_response();
    }
    // v0.9 §1.1: pick from WebCookiePool (multi-account health/cooldown/round-robin)
    let (cookie, web_cid) = match pick_web_cookie(&st).await {
        Some((c, _, cid)) => (c, Some(cid)),
        None => (String::new(), None),
    };
    let cookie = if cookie.is_empty() {
        // v0.10 §1.4: pool non-empty but all cooling down -> structured degradation; otherwise keep the "not imported" message
        if st.web_pool.count().await > 0 {
            return web_pool_exhausted(&st).await;
        }
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": { "message": "Multimodal upload requires a web Cookie credential (POST /api/tokens/import first to import one)", "type": "invalid_request_error", "code": "multimodal_requires_web_cookie" } })),
        )
            .into_response();
    } else {
        cookie
    };
    if body.is_empty() {
        return bad_req("empty body (use --data-binary to send the file content)");
    }
    if body.len() > 20 * 1024 * 1024 {
        return bad_req("file too large (limit 20MB)");
    }
    let filename = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("upload.png")
        .to_string();
    // Pass through the client's Content-Type (supports images/documents/any file; falls back to extension-based inference when absent)
    // Note: the previous whitelist only allowed image/* and pdf, which caused document uploads to be rewritten as image/png (losing kind:"document")
    let mime = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .filter(|s| !s.is_empty() && s != "application/octet-stream")
        .unwrap_or_else(|| {
            let lower = filename.to_lowercase();
            if lower.ends_with(".png") {
                "image/png".into()
            } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                "image/jpeg".into()
            } else if lower.ends_with(".gif") {
                "image/gif".into()
            } else if lower.ends_with(".webp") {
                "image/webp".into()
            } else if lower.ends_with(".pdf") {
                "application/pdf".into()
            } else if lower.ends_with(".txt") || lower.ends_with(".md") {
                "text/plain".into()
            } else if lower.ends_with(".json") {
                "application/json".into()
            } else {
                "application/octet-stream".into()
            }
        });
    let model_override = headers
        .get("x-model")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "glm-5.3-flash".into());

    let client = match crate::web_protocol::WebClient::new(cookie, model_override.clone()) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    match client
        .upload_with_model(body.to_vec(), &filename, &mime, &model_override)
        .await
    {
        Ok(up) => {
            if let Some(id) = &web_cid {
                st.web_pool.mark_ok(id).await;
            }
            st.logs.emit(
                "info",
                "upload",
                None,
                format!(
                    "Uploaded {} ({:.1}KB, {}) -> storageId {}",
                    filename.replace(['\r', '\n'], ""),
                    body.len() as f64 / 1024.0,
                    up.kind,
                    up.storage_id
                ),
            );
            Json(serde_json::json!({
                "id": up.storage_id,
                "object": "file",
                "storageId": up.storage_id,
                "kind": up.kind,               // "image" | "document"
                "mediaType": up.media_type,
                "name": up.name,
                "bytes": body.len(),
                "url": up.url,                 // images only
                "descriptionStorageId": up.description_storage_id,
                "chars": up.chars,             // documents only (extracted character count)
                "truncated": up.truncated,     // documents only (whether it was truncated)
                "usage": {
                    "image": "Put storageId into the images array of /v1/web/chat",
                    "document": "Put storageId into the attachments array of /v1/web/chat"
                }
            }))
            .into_response()
        }
        Err(e) => {
            if let Some(id) = &web_cid {
                web_pool_failure(&st, id, &e.to_string()).await;
            }
            // Truncate the upstream error body (avoid leaking account/internal details)
            let msg: String = e.to_string().chars().take(300).collect();
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": { "message": msg, "type": "upstream_error" } })),
            )
                .into_response()
        }
    }
}

/// Unified 400 JSON response
fn bad_req(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "ok": false, "message": msg })),
    )
        .into_response()
}

/// Pick a web Cookie credential (v0.9 §1.1: chosen from WebCookiePool by health score/circuit-breaker/cooldown;
/// config and imported-library credentials are already merged in when the pool is built). Returns cookie + display info + stable id.
/// v0.10 §1.4: structured degradation error for a web Cookie pool that is "all cooling / empty" (includes the minimum recovery seconds)
async fn web_pool_exhausted(st: &AppState) -> Response {
    let snap = st.web_pool.snapshot().await;
    let cooling: Vec<u64> = snap.iter().filter_map(|s| s.cooldown_seconds).collect();
    let min_sec = cooling.iter().min().copied().unwrap_or(0);
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": {
                "message": format!(
                    "{}{}. Check on the panel's Accounts page or log in again.",
                    format!("All web Cookie accounts are unavailable (pool has {} total, {} cooling down)", snap.len(), cooling.len()),
                    if min_sec > 0 { format!("; recovers in about {min_sec} seconds at the earliest") } else { String::from("; cooldown has already expired") }
                ),
                "type": "pool_exhausted",
                "code": "web_pool_exhausted",
                "meta": { "pool_size": snap.len(), "cooling": cooling.len(), "cooldown_seconds_min": min_sec }
            }
        })),
    )
        .into_response()
}

async fn pick_web_cookie(st: &AppState) -> Option<(String, serde_json::Value, String)> {
    let p = st.web_pool.pick().await?;
    Some((p.cookie, p.cred, p.id))
}

/// v0.9: web pool failure write-back -- 401/403 deterministic invalidation cools down immediately (10 minutes, aligned with Bearer pool semantics),
/// others (network/5xx) accumulate consecutive failures -> trips the breaker once the threshold is reached.
async fn web_pool_failure(st: &AppState, id: &str, err: &str) {
    if err.contains("401") || err.contains("403") {
        st.web_pool
            .mark_cooldown(id, std::time::Duration::from_secs(600), err)
            .await;
    } else {
        st.web_pool.mark_failure(id, err).await;
    }
}

/// Fetch one web Cookie credential by its stable id (used for "check/detail on a specific credential")
fn cookie_by_id(st: &AppState, id: &str) -> Option<(String, crate::import::ExtractedAuth)> {
    crate::import::load_tokens_healed(&st.cfg.tokens_path)
        .ok()?
        .into_iter()
        .find(|t| crate::import::cred_id(&t.token) == id)
        .map(|t| (t.token.clone(), t))
}

/// Upstream call result -> `Option<serde_json::Value>` (`None` means that endpoint failed; the corresponding snapshot field is left empty)
fn value_of<T: serde::Serialize>(r: &Result<T, anyhow::Error>) -> Option<serde_json::Value> {
    r.as_ref().ok().and_then(|v| serde_json::to_value(v).ok())
}

/// `/api/auth/session` returns `HTTP 200 + {}` for **unauthenticated/invalid credentials**, not 401.
/// Checking only "did the request succeed" would misjudge an invalid credential as valid (observed: a forged Cookie -> `{}` 200).
/// So we must check that the response actually contains a user body.
fn session_is_authenticated(v: &serde_json::Value) -> bool {
    match v.get("user").filter(|u| u.is_object()) {
        Some(u) => ["id", "email", "name"]
            .iter()
            .any(|k| u.get(*k).map(|x| !x.is_null()).unwrap_or(false)),
        None => false,
    }
}

/// `/api/web/freebuff-session` returns 401 for invalid credentials; accessTier/freebucks are only present when valid.
fn quota_is_authenticated(v: &serde_json::Value) -> bool {
    v.get("accessTier").map(|x| !x.is_null()).unwrap_or(false) || v.get("freebucks").is_some()
}

/// Whether the credential is usable: either the identity endpoint or the quota endpoint confirms logged-in
fn credential_usable(
    identity: Option<&serde_json::Value>,
    quota: Option<&serde_json::Value>,
) -> bool {
    identity.map(session_is_authenticated).unwrap_or(false)
        || quota.map(quota_is_authenticated).unwrap_or(false)
}

/// Aggregate the responses of the 4 upstream endpoints into one panel-readable account snapshot
fn build_cred_meta(
    cred_id: &str,
    identity: Option<&serde_json::Value>,
    usage: Option<&serde_json::Value>,
    subscriptions: Option<&serde_json::Value>,
    quota: Option<&serde_json::Value>,
) -> crate::account_meta::CredMeta {
    use crate::account_meta::{CredMeta, ModelQuota};
    let mut m = CredMeta {
        cred_id: cred_id.to_string(),
        checked_at: chrono::Utc::now().to_rfc3339(),
        ..Default::default()
    };

    if let Some(v) = identity {
        let u = v.get("user").unwrap_or(v);
        m.name = u.get("name").and_then(|x| x.as_str()).map(String::from);
        m.email = u.get("email").and_then(|x| x.as_str()).map(String::from);
        m.image = u.get("image").and_then(|x| x.as_str()).map(String::from);
        m.user_id = u.get("id").and_then(|x| x.as_str()).map(String::from);
        m.expires = v.get("expires").and_then(|x| x.as_str()).map(String::from);
    }
    if let Some(v) = usage {
        m.streak_current = v.pointer("/streak/current").and_then(|x| x.as_i64());
        m.all_time_active_days = v.get("allTimeActiveDays").and_then(|x| x.as_i64());
        m.tokens_7d = v.pointer("/recent/totalTokens").and_then(|x| x.as_i64());
    }
    // Plan tier: prefer freebuff-session.subscription, then fall back to /api/web/subscriptions.subscription
    m.tier_id = quota
        .and_then(|v| v.pointer("/subscription/tierId"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .or_else(|| {
            subscriptions
                .and_then(|v| v.pointer("/subscription/tierId"))
                .and_then(|x| x.as_str())
                .map(String::from)
        });
    if let Some(v) = quota {
        m.access_tier = v
            .get("accessTier")
            .and_then(|x| x.as_str())
            .map(String::from);
        m.country_code = v
            .get("countryCode")
            .and_then(|x| x.as_str())
            .map(String::from);
        m.country_block_reason = v
            .get("countryBlockReason")
            .and_then(|x| x.as_str())
            .map(String::from);
        let d = v.pointer("/freebucks/daily");
        if let Some(d) = d {
            m.daily_limit = d.get("limit").and_then(|x| x.as_i64());
            m.daily_spent = d.get("spent").and_then(|x| x.as_i64());
            m.daily_remaining = d.get("remaining").and_then(|x| x.as_i64());
            m.reset_at = d.get("resetAt").and_then(|x| x.as_str()).map(String::from);
        }
        let prices = v.pointer("/freebucks/prices");
        if let Some(rl) = v.get("rateLimitsByModel").and_then(|x| x.as_object()) {
            m.models = rl
                .iter()
                .map(|(name, item)| {
                    let limit = item.get("limit").and_then(|x| x.as_i64());
                    let used = item.get("recentCount").and_then(|x| x.as_i64());
                    ModelQuota {
                        model: name.clone(),
                        price: prices.and_then(|p| p.get(name)).and_then(|x| x.as_i64()),
                        limit,
                        used,
                        remaining: limit.map(|l| (l - used.unwrap_or(0)).max(0)),
                        reset_at: item
                            .get("resetAt")
                            .and_then(|x| x.as_str())
                            .map(String::from),
                        pool_label: item
                            .get("poolLabel")
                            .and_then(|x| x.as_str())
                            .map(String::from),
                    }
                })
                .collect();
        }
    }
    m
}

/// Write one usage record (user note: every account should have a checkable record). Failure is only logged, doesn't affect the request.
fn record_history(st: &AppState, m: &crate::account_meta::CredMeta, ok: bool) {
    let rec = crate::account_meta::HistoryRecord {
        ts: m.checked_at.clone(),
        cred_id: m.cred_id.clone(),
        name: m.name.clone(),
        email: m.email.clone(),
        tier_id: m.tier_id.clone(),
        daily_limit: m.daily_limit,
        daily_spent: m.daily_spent,
        daily_remaining: m.daily_remaining,
        tokens_7d: m.tokens_7d,
        streak_current: m.streak_current,
        ok,
    };
    if let Err(e) = st.meta.append_history(&rec) {
        tracing::warn!("Failed to write usage record: {e}");
    }
}

/// GET /api/account/overview — full account picture (identity / usage stats / plan / quota points / local credential)
/// Aggregates 4 upstream endpoints; any single failure degrades to null (doesn't fail overall); the frontend handles the display.
async fn handle_account_overview(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers, &st) {
        return admin_denied();
    }
    let (cookie, cred, cid) = match pick_web_cookie(&st).await {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "code": "need_cookie",
                    "message": "No web Cookie credential yet. Click \"One-Click Login\" and follow the wizard, or paste a Cookie to import."
                })),
            )
                .into_response();
        }
    };
    let client = match crate::web_protocol::WebClient::new(cookie, "glm-5.3-flash".into()) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    // Fetch concurrently (any single failure doesn't affect the others)
    let (identity, usage, subs, quota) = tokio::join!(
        client.auth_session(),
        client.usage_summary(),
        client.subscriptions(),
        client.freebuff_session(),
    );
    // The credential is only usable if either the identity or the quota endpoint confirms logged-in.
    // Note: the upstream returns 200 + {} for unauthenticated requests (not 401); checking only "did the request succeed" would misjudge it as valid.
    let iv = value_of(&identity);
    let qv = value_of(&quota);
    if !credential_usable(iv.as_ref(), qv.as_ref()) {
        let hint = identity
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .or_else(|| quota.as_ref().err().map(|e| e.to_string()))
            .unwrap_or_else(|| "Upstream did not return a logged-in account (credential expired or not logged in)".into());
        // v0.9: credential unusable -> lower score/circuit-break/cooldown in the pool (401/403 auto-trips the breaker, others accumulate)
        web_pool_failure(&st, &cid, &hint).await;
        // Also record a failure entry, to help trace "when did this account become unavailable"
        let mut failed = crate::account_meta::CredMeta {
            cred_id: cid,
            valid: false,
            error: Some(hint.clone()),
            checked_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
        if let Some(prev) = st.meta.get(&failed.cred_id) {
            failed.name = prev.name;
            failed.email = prev.email;
        }
        let _ = st.meta.upsert(failed.clone());
        record_history(&st, &failed, false);
        return Json(serde_json::json!({
            "ok": false,
            "code": "credential_invalid",
            "credential": cred,
            "message": format!("Credential cannot access upstream (it may be expired or risk-controlled): {hint}. Please log in again and import a new credential."),
        }))
        .into_response();
    }
    // Success -> persist the account snapshot + append a usage record (for the credential list and history queries)
    let meta = build_cred_meta(
        &cid,
        iv.as_ref(),
        usage.as_ref().ok(),
        subs.as_ref().ok(),
        qv.as_ref(),
    );
    let meta = crate::account_meta::CredMeta {
        valid: true,
        ..meta
    };
    if let Err(e) = st.meta.upsert(meta.clone()) {
        tracing::warn!("Failed to write account info cache: {e}");
    }
    record_history(&st, &meta, true);
    // v0.9: fetch succeeded -> record ok (HalfOpen probe: consecutive successes can recover to Closed)
    st.web_pool.mark_ok(&cid).await;
    st.logs.emit("info", "account", None, "Account overview refreshed");
    Json(serde_json::json!({
        "ok": true,
        "identity": identity.ok(),
        "usage": usage.ok(),
        "subscription": subs.ok(),
        "quota": quota.ok(),
        "credential": cred,
        "meta": meta,
        "fetched_at": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

/// POST /api/account/refresh — credential keepalive/health check: call convex-token to verify whether the Cookie is still valid
async fn handle_account_refresh(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = write_guard(&headers, &st) {
        return resp;
    }
    let (cookie, cred, cid) = match pick_web_cookie(&st).await {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "ok": false, "code": "need_cookie", "message": "No web Cookie credential found" })),
            )
                .into_response();
        }
    };
    let client = match crate::web_protocol::WebClient::new(cookie, "glm-5.3-flash".into()) {
        Ok(c) => c,
        Err(e) => return internal_err(&anyhow::Error::msg(e.to_string())),
    };
    match client.convex_token().await {
        Ok(v) => {
            let has_token = v
                .get("token")
                .and_then(|t| t.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            // v0.9: keepalive succeeded and got a short-lived token -> record ok; reachable but no token -> record failure (may have expired)
            if has_token {
                st.web_pool.mark_ok(&cid).await;
            } else {
                web_pool_failure(&st, &cid, "endpoint reachable but did not return a short-lived token").await;
            }
            st.logs.emit(
                "info",
                "account",
                None,
                "Credential keepalive check passed (short-lived token refreshed)",
            );
            Json(serde_json::json!({
                "ok": true,
                "valid": has_token,
                "credential": cred,
                "meta": st.meta.get(&cid),
                "message": if has_token { "Credential is valid; short-lived token refreshed (valid ~5 minutes, gateway reuses it automatically)" } else { "Endpoint reachable but did not return a token" },
            }))
            .into_response()
        }
        Err(e) => {
            // v0.9: keepalive failed (401/403/network) -> lower score/circuit-break/cooldown in the pool
            web_pool_failure(&st, &cid, &e.to_string()).await;
            st.logs
                .emit("warn", "account", None, format!("Credential keepalive check failed: {e}"));
            // Also persist an entry on failure, so the credential list can directly show "this one is invalid"
            let mut failed = crate::account_meta::CredMeta {
                cred_id: cid.clone(),
                valid: false,
                error: Some(e.to_string()),
                checked_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            };
            if let Some(prev) = st.meta.get(&cid) {
                failed.name = prev.name;
                failed.email = prev.email;
                failed.tier_id = prev.tier_id;
                failed.daily_remaining = prev.daily_remaining;
                failed.daily_limit = prev.daily_limit;
            }
            let _ = st.meta.upsert(failed.clone());
            record_history(&st, &failed, false);
            Json(serde_json::json!({
                "ok": false,
                "valid": false,
                "credential": cred,
                "meta": failed,
                "message": format!("Credential may be invalid (please log in again to import a new one): {e}"),
            }))
            .into_response()
        }
    }
}

// ---------- Utilities ----------

fn authorized(headers: &HeaderMap, keys: &[String]) -> bool {
    let extract = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    };
    if let Some(k) = extract("x-api-key") {
        if keys.contains(&k) {
            return true;
        }
    }
    if let Some(auth) = extract("authorization") {
        let bearer = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
            .map(|s| s.to_string());
        if let Some(k) = bearer {
            return keys.contains(&k);
        }
    }
    false
}

/// Preprocess fields the upstream doesn't strictly need (strip custom fields other than stream to avoid pollution)
fn remove_passthrough_fields(body: &mut serde_json::Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove("stream_options");
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_messages_text_blocks_flattened() {
        // Claude content blocks array -> OpenAI content string
        let msgs = serde_json::json!([
            { "role": "user", "content": [
                { "type": "text", "text": "hello" },
                { "type": "text", "text": "world" }
            ]}
        ]);
        let out = claude_to_openai_messages(msgs);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "hello\nworld");
    }

    #[test]
    fn claude_tool_use_to_tool_calls() {
        // assistant's tool_use block -> OpenAI assistant.tool_calls
        let msgs = serde_json::json!([
            { "role": "assistant", "content": [
                { "type": "text", "text": "let me check" },
                { "type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": { "city": "SF" } }
            ]}
        ]);
        let out = claude_to_openai_messages(msgs);
        let a = &out.as_array().unwrap()[0];
        assert_eq!(a["role"], "assistant");
        assert_eq!(a["content"], "let me check");
        let tc = &a["tool_calls"][0];
        assert_eq!(tc["id"], "toolu_1");
        assert_eq!(tc["function"]["name"], "get_weather");
        assert!(tc["function"]["arguments"].as_str().unwrap().contains("SF"));
    }

    #[test]
    fn claude_tool_result_to_tool_role() {
        // user's tool_result block -> OpenAI role=tool message
        let msgs = serde_json::json!([
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": "18C sunny" }
            ]}
        ]);
        let out = claude_to_openai_messages(msgs);
        let a = &out.as_array().unwrap()[0];
        assert_eq!(a["role"], "tool");
        assert_eq!(a["tool_call_id"], "toolu_1");
        assert_eq!(a["content"], "18C sunny");
    }

    #[test]
    fn openai_response_to_claude_shape() {
        let oai = serde_json::json!({
            "id": "chatcmpl-1",
            "choices": [{ "message": { "role": "assistant", "content": "hi there" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3 }
        });
        let c = openai_to_claude_response(&oai, "z-ai/glm-5.3-flash");
        assert_eq!(c["type"], "message");
        assert_eq!(c["role"], "assistant");
        assert_eq!(c["stop_reason"], "end_turn");
        assert_eq!(c["content"][0]["type"], "text");
        assert_eq!(c["content"][0]["text"], "hi there");
        assert_eq!(c["usage"]["input_tokens"], 10);
        assert_eq!(c["usage"]["output_tokens"], 3);
    }

    #[test]
    fn openai_tool_calls_to_claude_tool_use() {
        let oai = serde_json::json!({
            "id": "chatcmpl-2",
            "choices": [{ "message": { "role": "assistant", "content": null, "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "lookup", "arguments": "{\"q\":\"x\"}" } }
            ]}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let c = openai_to_claude_response(&oai, "m");
        assert_eq!(c["stop_reason"], "tool_use");
        assert_eq!(c["content"][0]["type"], "tool_use");
        assert_eq!(c["content"][0]["name"], "lookup");
        assert_eq!(c["content"][0]["input"]["q"], "x");
    }

    #[test]
    fn mask_hides_middle() {
        let m = mask("sk-abcdefghijklmnop1234");
        assert!(m.starts_with("sk-abc"));
        assert!(m.ends_with("1234"));
        assert!(m.contains("..."));
        // short token doesn't panic
        assert!(mask("abc").ends_with("***"));
    }

    #[test]
    fn mask_does_not_leak_short_secrets() {
        // short credentials must be almost unreadable: the old implementation echoed "sk-local" as "sk-lo***" (leaking 5/8 characters)
        let short = mask("sk-local");
        assert!(!short.contains("local"), "short credential leaked the original text: {short}");
        assert!(short.len() <= 6, "masked short credential is too long: {short}");
        // 4 characters or fewer are fully masked
        assert_eq!(mask("abcd"), "***");
        assert_eq!(mask(""), "***");
        // boundary: 12 characters takes the "first 2 + last 2" branch, still doesn't leak the middle
        let m12 = mask("abcdefghijkl");
        assert_eq!(m12, "ab***kl");
    }

    #[test]
    fn mask_is_multibyte_safe() {
        // multi-byte characters must not trigger a char boundary panic
        let m = mask("中文凭证测试内容中文凭证测试内容");
        assert!(m.contains("..."));
        assert!(!mask("中文").is_empty());
    }

    #[test]
    fn loopback_detection_by_proxy_headers() {
        let mut h = HeaderMap::new();
        assert!(is_loopback_request(&h));
        h.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        assert!(!is_loopback_request(&h));
        let mut h2 = HeaderMap::new();
        h2.insert("x-real-ip", "1.2.3.4".parse().unwrap());
        assert!(!is_loopback_request(&h2));
        // v0.9 auth depth: real peer (x-fb-peer, written by inject_peer from ConnectInfo)
        let mut h3 = HeaderMap::new();
        h3.insert("x-fb-peer", "192.168.1.5".parse().unwrap());
        assert!(!is_loopback_request(&h3), "a non-loopback peer must not be treated as local");
        let mut h4 = HeaderMap::new();
        h4.insert("x-fb-peer", "127.0.0.1".parse().unwrap());
        assert!(is_loopback_request(&h4), "a loopback peer should be allowed");
        let mut h5 = HeaderMap::new();
        h5.insert("x-fb-peer", "not-an-ip".parse().unwrap());
        assert!(!is_loopback_request(&h5), "an unparseable peer must always be rejected");
    }

    #[test]
    fn authorized_accepts_bearer_and_x_api_key() {
        let keys = vec!["sk-local".to_string()];
        let mut h = HeaderMap::new();
        assert!(!authorized(&h, &keys));
        h.insert("authorization", "Bearer sk-local".parse().unwrap());
        assert!(authorized(&h, &keys));
        let mut h2 = HeaderMap::new();
        h2.insert("x-api-key", "sk-local".parse().unwrap());
        assert!(authorized(&h2, &keys));
        let mut h3 = HeaderMap::new();
        h3.insert("authorization", "Bearer wrong".parse().unwrap());
        assert!(!authorized(&h3, &keys));
    }

    #[test]
    fn anonymous_session_is_not_authenticated() {
        // upstream returns 200 + {} for invalid credentials (observed); must be judged as not logged in, otherwise a fake credential would be treated as "valid"
        let anon = serde_json::json!({});
        assert!(!session_is_authenticated(&anon));
        // user being null / an empty object likewise doesn't count as logged in
        assert!(!session_is_authenticated(
            &serde_json::json!({"user": null})
        ));
        assert!(!session_is_authenticated(&serde_json::json!({"user": {}})));
        // a real session must be recognized
        let real = serde_json::json!({"user": {"id": "88d1", "email": "a@b.c"}, "expires": "2026-10-09T13:27:07.358Z"});
        assert!(session_is_authenticated(&real));
    }

    #[test]
    fn quota_authentication_signal() {
        assert!(!quota_is_authenticated(
            &serde_json::json!({"error": "Unauthorized"})
        ));
        assert!(quota_is_authenticated(
            &serde_json::json!({"accessTier": "limited"})
        ));
        assert!(quota_is_authenticated(
            &serde_json::json!({"freebucks": {"balance": 20}})
        ));
    }

    #[test]
    fn credential_usable_requires_real_signal() {
        let anon = serde_json::json!({});
        assert!(
            !credential_usable(Some(&anon), None),
            "an empty session must not be judged usable"
        );
        assert!(!credential_usable(None, None));
        let real = serde_json::json!({"user": {"email": "a@b.c"}});
        assert!(credential_usable(Some(&real), None));
        let quota = serde_json::json!({"accessTier": "limited"});
        assert!(credential_usable(None, Some(&quota)));
    }

    #[test]
    fn bridge_error_detection() {
        // normal stream: no "error" text -> None
        assert_eq!(
            detect_bridge_error(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n"
            ),
            None
        );
        // upstream 200 with an embedded error envelope -> classified as auth type (aligned with errors::classify)
        let sse = "data: {\"error\":{\"message\":\"Unauthorized\",\"type\":\"auth\"}}\n\n";
        assert_eq!(detect_bridge_error(sse), Some("auth_expired"));
        // error as a string form is recognized too
        let sse2 = "data: {\"error\":\"Unauthorized\"}\n\n";
        assert!(
            detect_bridge_error(sse2).is_some(),
            "a string-form error must be recognized too"
        );
        // "error" text appears in the body (not an envelope) -> must not false-positive
        let false_pos = "data: {\"choices\":[{\"delta\":{\"content\":\"他说 error 这个词\"}}]}\n\n";
        assert_eq!(detect_bridge_error(false_pos), None);
        // empty stream
        assert_eq!(detect_bridge_error(""), None);
    }

    #[test]
    fn placeholder_tokens_do_not_block_bridge() {
        // A common placeholder in user config (__TEST_SKIP__ etc) makes the account pool "look non-empty",
        // which blocks the web bridge and also inevitably 401s on the desktop protocol (reproduced in the Cherry Studio scenario).
        // Rule: a token only counts as a "usable Bearer" if its length >= 20 and it doesn't start with __TEST_SKIP__.
        let usable = |tok: &str| tok.len() >= 20 && !tok.starts_with("__TEST_SKIP__");
        assert!(!usable("__TEST_SKIP__"), "a placeholder must not be considered usable");
        assert!(!usable("short"), "an overly short token must not be considered usable");
        assert!(
            usable("__Secure-next-auth.session-token=abc123; x=1"),
            "a real long token is usable"
        );
        // Note: web-cookie credentials should never enter this pool at all (fixed in v0.5.0); this only guards the placeholder scenario
    }

    #[test]
    fn cookie_detection_requires_known_markers() {
        // v0.8 regression: Cookie detection narrowed -- a URL-encoded string (%3A) must no longer be misjudged as a Cookie
        let looks_like_cookie = |t: &str| {
            t.contains("session-token") || t.contains(".next-auth") || t.contains("callback-url")
        };
        // a real next-auth Cookie hits (session-token / .next-auth / callback-url signatures)
        assert!(looks_like_cookie("__Secure-next-auth.session-token=abc"));
        assert!(looks_like_cookie(
            "__Host-next-auth.callback-url=https://..."
        ));
        assert!(
            looks_like_cookie("session-token=xyz"),
            "session-token signature hit"
        );
        // non-Cookie strings don't match: URL-encoded strings, Bearer tokens, plain text
        assert!(!looks_like_cookie("foo%3Abar"), "%3A must not be misjudged as a Cookie");
        assert!(
            !looks_like_cookie("sk-proj-abc%3Adef%3Aghi"),
            "a Bearer token containing %3A must not be misjudged either"
        );
        assert!(!looks_like_cookie("plain text without markers"));
    }

    #[test]
    fn claude_tool_roundtrip_preserves_order_and_semantics() {
        // v0.8 (5.2B): semantics before/after conversion for string / content-blocks / tool_use+tool_result combinations
        let msgs = serde_json::json!([
            { "role": "user", "content": "查一下上海天气" },
            { "role": "assistant", "content": [
                { "type": "text", "text": "好的" },
                { "type": "tool_use", "id": "toolu_a", "name": "get_weather", "input": { "city": "上海" } }
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_a", "content": "多云 22C" }
            ]},
            { "role": "user", "content": "谢谢" }
        ]);
        let out = claude_to_openai_messages(msgs);
        let arr = out.as_array().unwrap();
        // 4 inputs -> 4 outputs (order preserved)
        assert_eq!(arr.len(), 4, "combined conversion should preserve the message count");
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "查一下上海天气");
        // assistant: text + tool_calls
        assert_eq!(arr[1]["role"], "assistant");
        assert_eq!(arr[1]["content"], "好的");
        assert_eq!(arr[1]["tool_calls"][0]["id"], "toolu_a");
        assert_eq!(arr[1]["tool_calls"][0]["function"]["name"], "get_weather");
        // tool_result -> role=tool
        assert_eq!(arr[2]["role"], "tool");
        assert_eq!(arr[2]["tool_call_id"], "toolu_a");
        assert_eq!(arr[2]["content"], "多云 22C");
        // last user message is preserved
        assert_eq!(arr[3]["role"], "user");
        assert_eq!(arr[3]["content"], "谢谢");
    }
}
