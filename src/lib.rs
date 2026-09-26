//! Freebuff2API — Rust OpenAI/Anthropic-compatible gateway
//!
//! Architecture layers:
//! - `config`   config loading (JSON + env vars)
//! - `models`   model registry (upstream free-agents.ts + hardcoded base set)
//! - `upstream` upstream Codebuff HTTP client (session/run/chat/ads)
//! - `pool`     multi-account pool: session health scoring + round-robin + cooldown + circuit breaker
//! - `session`  Freebuff session management (queue/active/keepalive/heartbeat)
//! - `routes`   OpenAI / Anthropic / panel / usage stats HTTP routes
//! - `usage`    SQLite usage stats and auditing
//! - `router`   model routing + fallback chain + token savings (9router style)
//! - `ads`      ad-for-token keepalive
//! - `web`      embedded control panel static assets

pub mod account_meta;
pub mod ads;
pub mod api;
pub mod config;
pub mod errors;
pub mod export;
pub mod extension;
pub mod import;
pub mod logbus;
pub mod login_window;
mod login_window_stub;
#[cfg(windows)]
mod login_window_windows;
pub mod mcp;
pub mod memory;
pub mod models;
pub mod pool;
pub mod prompts;
pub mod protocol;
pub mod redact;
pub mod retry;
pub mod router;
pub mod semaphore;
pub mod session;
pub mod skills;
pub mod telemetry;
pub mod upstream;
pub mod usage;
pub mod web;
pub mod web_pool;
pub mod web_protocol;
pub mod web_threads;

pub use config::Config;
