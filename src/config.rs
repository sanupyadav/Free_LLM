use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::str::FromStr;
use std::time::Duration;

/// Global config, from JSON file + environment variables (env vars take priority)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Listen address, defaults to 127.0.0.1:47821 (local software defaults to loopback only)
    pub listen_addr: String,
    /// Upstream API address
    pub upstream_base_url: String,
    /// Freebuff auth tokens (multi-account rotation)
    pub auth_tokens: Vec<String>,
    /// This gateway's outward-facing auth key (empty = no auth check)
    pub api_keys: Vec<String>,
    /// Run rotation interval
    pub rotation_interval_sec: u64,
    /// Upstream request timeout
    pub request_timeout_sec: u64,
    /// HTTP proxy (supports http/socks5)
    pub http_proxy: String,
    /// Session keepalive interval (ad refresh / heartbeat)
    pub session_keepalive_sec: u64,
    /// Ad keepalive providers (comma-separated: gravity,zeroclick,carbon)
    pub ad_providers: Vec<String>,
    /// Model routing fallback chain config
    pub fallback_models: Vec<String>,
    /// Whether to enable token saving (compress oversized tool_result)
    pub token_saver: bool,
    /// Usage stats SQLite path (empty = disable stats)
    pub sqlite_path: String,
    /// Imported credential storage path (where curl/HAR/Cookie parsing writes to disk)
    pub tokens_path: String,
    /// Telemetry SQLite path (request details/event chains, separate DB to avoid write-lock contention)
    pub telemetry_path: String,
    /// Memory store SQLite path (user preferences/corrections; separate DB)
    pub memory_path: String,
    /// Upstream session records (web protocol threadId; for auto cleanup)
    pub threads_path: String,
    /// Credential account info cache (nickname/email/plan/today's remaining, used by panel credential list)
    pub cred_meta_path: String,
    /// Account usage history (JSONL, queryable per credential)
    pub account_history_path: String,
    /// Upstream session auto-cleanup interval (seconds); 0 = disable auto cleanup
    pub thread_cleanup_interval_sec: u64,
    /// Upstream session retention (hours); cleaned up once exceeded
    pub thread_max_age_hours: u64,
    /// Session bindings for web protocol bridging (OpenAI/Anthropic client -> upstream thread reuse)
    pub web_threads_path: String,
    /// Memory layer switch (when false, neither auto-records nor injects; privacy-sensitive users can disable)
    pub memory_enabled: bool,
    /// Skills directory (source of truth for skill files)
    pub skills_dir: String,
    /// Skill injection mode: roster (inject name+description only) | full (concatenate everything)
    pub skills_inject_mode: String,
    /// Token budget cap for roster injection
    pub max_roster_tokens: usize,
    /// Built-in panel directory (empty = use embedded resources)
    pub web_dir: String,
    /// Skip upstream connectivity check on startup
    pub skip_upstream_check: bool,
    /// Log/telemetry redaction (on by default): replaces Cookie/Bearer/authorization values with *** before writing to the log bus and telemetry
    pub redact_logs: bool,
    /// Dual-bucket concurrency semaphore: free tier {paid slots, regular}
    pub concurrency_free_slots: usize,
    pub concurrency_free_multi: usize,
    /// Dual-bucket concurrency semaphore: subscription tier {paid slots, regular}
    pub concurrency_sub_slots: usize,
    pub concurrency_sub_multi: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:47821".into(),
            upstream_base_url: "https://www.codebuff.com".into(),
            auth_tokens: vec![],
            api_keys: vec![],
            rotation_interval_sec: 6 * 3600,
            request_timeout_sec: 900,
            http_proxy: String::new(),
            session_keepalive_sec: 45,
            ad_providers: vec!["gravity".into()],
            fallback_models: vec![],
            token_saver: false,
            sqlite_path: "data/freebuff2api.sqlite".into(),
            tokens_path: "data/tokens.json".into(),
            telemetry_path: "data/telemetry.sqlite".into(),
            memory_path: "data/memory.sqlite".into(),
            threads_path: "data/threads.json".into(),
            cred_meta_path: "data/cred_meta.json".into(),
            account_history_path: "data/account_history.jsonl".into(),
            // User note (网页对话.txt:605): the reverse proxy must clean up upstream sessions itself, don't leave the load on upstream and get caught
            thread_cleanup_interval_sec: 3600,
            thread_max_age_hours: 24,
            web_threads_path: "data/web_threads.json".into(),
            // Memory disabled by default (user note 2026-09-11: memory isn't for everyone, needs its own toggle and defaults off)
            memory_enabled: false,
            skills_dir: "data/skills".into(),
            skills_inject_mode: "roster".into(),
            max_roster_tokens: 2000,
            web_dir: String::new(),
            skip_upstream_check: false,
            redact_logs: true,
            concurrency_free_slots: 1,
            concurrency_free_multi: 3,
            concurrency_sub_slots: 3,
            concurrency_sub_multi: 8,
        }
    }
}

impl Config {
    /// Load by merging defaults + JSON file + environment variables
    pub fn load(path: Option<&str>) -> Result<Self> {
        let mut cfg = Self::default();

        if let Some(p) = path {
            if std::path::Path::new(p).exists() {
                let data = std::fs::read_to_string(p)
                    .map_err(|e| anyhow!("Failed to read config file {p}: {e}"))?;
                let file_cfg: Config = serde_json::from_str(&data)
                    .map_err(|e| anyhow!("Failed to parse config file {p}: {e}"))?;
                cfg = file_cfg;
            } else {
                return Err(anyhow!("Config file does not exist: {p}"));
            }
        }
        // Auto-detect default config.json
        else if std::path::Path::new("config.json").exists() {
            let data = std::fs::read_to_string("config.json")?;
            cfg = serde_json::from_str(&data)?;
        }

        cfg.apply_env();
        cfg.validate()?;
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        if let Ok(v) = env::var("LISTEN_ADDR") {
            self.listen_addr = v;
        }
        if let Ok(v) = env::var("UPSTREAM_BASE_URL") {
            self.upstream_base_url = v;
        }
        if let Ok(v) = env::var("AUTH_TOKENS") {
            self.auth_tokens = v
                .split([',', '\n'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = env::var("API_KEYS") {
            self.api_keys = v
                .split([',', '\n'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = env::var("ROTATION_INTERVAL") {
            self.rotation_interval_sec =
                parse_duration_sec(&v).unwrap_or(self.rotation_interval_sec);
        }
        if let Ok(v) = env::var("REQUEST_TIMEOUT") {
            self.request_timeout_sec = parse_duration_sec(&v).unwrap_or(self.request_timeout_sec);
        }
        if let Ok(v) = env::var("HTTP_PROXY") {
            self.http_proxy = v;
        }
        if let Ok(v) = env::var("AD_PROVIDERS") {
            self.ad_providers = v
                .split([',', '\n'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = env::var("SQLITE_PATH") {
            self.sqlite_path = v;
        }
        if let Ok(v) = env::var("TOKENS_PATH") {
            self.tokens_path = v;
        }
        if let Ok(v) = env::var("TELEMETRY_PATH") {
            self.telemetry_path = v;
        }
        if let Ok(v) = env::var("MEMORY_PATH") {
            self.memory_path = v;
        }
        if let Ok(v) = env::var("CRED_META_PATH") {
            self.cred_meta_path = v;
        }
        if let Ok(v) = env::var("ACCOUNT_HISTORY_PATH") {
            self.account_history_path = v;
        }
        if let Ok(v) = env::var("THREAD_CLEANUP_INTERVAL") {
            self.thread_cleanup_interval_sec =
                parse_duration_sec(&v).unwrap_or(self.thread_cleanup_interval_sec);
        }
        if let Ok(v) = env::var("THREAD_MAX_AGE_HOURS") {
            if let Ok(n) = v.parse() {
                self.thread_max_age_hours = n;
            }
        }
        if let Ok(v) = env::var("WEB_THREADS_PATH") {
            self.web_threads_path = v;
        }
        if let Ok(v) = env::var("MEMORY_ENABLED") {
            self.memory_enabled = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = env::var("SKILLS_DIR") {
            self.skills_dir = v;
        }
        if let Ok(v) = env::var("SKILLS_INJECT_MODE") {
            self.skills_inject_mode = v;
        }
        if let Ok(v) = env::var("MAX_ROSTER_TOKENS") {
            if let Ok(n) = v.parse() {
                self.max_roster_tokens = n;
            }
        }
        if let Ok(v) = env::var("WEB_DIR") {
            self.web_dir = v;
        }
        if let Ok(v) = env::var("REDACT_LOGS") {
            self.redact_logs = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = env::var("CONCURRENCY_FREE_SLOTS") {
            if let Ok(n) = v.parse() {
                self.concurrency_free_slots = n;
            }
        }
        if let Ok(v) = env::var("CONCURRENCY_FREE_MULTI") {
            if let Ok(n) = v.parse() {
                self.concurrency_free_multi = n;
            }
        }
        if let Ok(v) = env::var("CONCURRENCY_SUB_SLOTS") {
            if let Ok(n) = v.parse() {
                self.concurrency_sub_slots = n;
            }
        }
        if let Ok(v) = env::var("CONCURRENCY_SUB_MULTI") {
            if let Ok(n) = v.parse() {
                self.concurrency_sub_multi = n;
            }
        }
        if let Ok(v) = env::var("TOKEN_SAVER") {
            self.token_saver = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = env::var("FALLBACK_MODELS") {
            self.fallback_models = v
                .split([',', '\n'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    fn validate(&self) -> Result<()> {
        if self.listen_addr.trim().is_empty() {
            return Err(anyhow!("LISTEN_ADDR must not be empty"));
        }
        // Safety guard: api_keys must be configured when listening on a non-local address (otherwise admin endpoints/memory/credentials are exposed to the network unprotected)
        if !is_loopback_listen(&self.listen_addr) && self.api_keys.is_empty() {
            return Err(anyhow!(
                "Safety refusal: listen_addr={} is not a local address but api_keys is not configured. \
                 Please configure api_keys (recommended) or switch back to 127.0.0.1",
                self.listen_addr
            ));
        }
        if self.upstream_base_url.trim().is_empty() {
            return Err(anyhow!("UPSTREAM_BASE_URL must not be empty"));
        }
        if self.auth_tokens.is_empty() && !self.skip_upstream_check {
            return Err(anyhow!("At least one AUTH_TOKENS entry is required"));
        }
        let unique: HashSet<&String> = self.auth_tokens.iter().collect();
        if unique.len() != self.auth_tokens.len() {
            return Err(anyhow!("AUTH_TOKENS contains duplicate tokens"));
        }
        Ok(())
    }
}

/// Determine whether the listen address is local (host part matched exactly, to prevent prefix bypass tricks like `localhost.evil.com`).
pub fn is_loopback_listen(listen_addr: &str) -> bool {
    // host[:port] → host；[::1]:port → ::1
    let host = if let Some(rest) = listen_addr.strip_prefix('[') {
        rest.split(']').next().unwrap_or("").to_string()
    } else {
        listen_addr
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(listen_addr)
            .to_string()
    };
    matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]")
        || std::net::IpAddr::from_str(&host)
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Resolve config file path: `--config x.json` > first positional arg > config.json in cwd > None
///
/// Kept consistent with the choices `Config::load` makes at startup, reused by "runtime config write-back" (e.g. the panel's one-click API Key generation).
pub fn resolve_config_path() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--config") {
        if let Some(p) = args.get(i + 1) {
            return Some(p.clone());
        }
    }
    if let Some(a) = args.get(1) {
        if !a.starts_with("--") {
            return Some(a.clone());
        }
    }
    if std::path::Path::new("config.json").exists() {
        return Some("config.json".into());
    }
    None
}

/// Parse "6h" / "900s" / "15m" or a plain number of seconds
pub fn parse_duration_sec(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    let (num, unit) = raw.split_at(raw.len().saturating_sub(1));
    let n: u64 = num.trim().parse().ok()?;
    Some(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return None,
    })
}

pub fn default_request_timeout() -> Duration {
    Duration::from_secs(900)
}
