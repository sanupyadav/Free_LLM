use freebuff2api::ads::AdRefresher;
use freebuff2api::api::{build_router, AppState};
use freebuff2api::config::Config;
use freebuff2api::logbus::LogBus;
use freebuff2api::models::ModelRegistry;
use freebuff2api::pool::Pool;
use freebuff2api::router::{ModelRouter, RouterConfig};
use freebuff2api::skills::SkillsManager;
use freebuff2api::telemetry::TelemetryWriter;
use freebuff2api::upstream::UpstreamClient;
use freebuff2api::usage::UsageDb;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Login window child process mode: skip initializing heavy tokio infrastructure, go straight into the WebView2 event loop (result reported via exit code)
    if std::env::args().any(|a| a == "--login-window") {
        let port = std::env::var("GATEWAY_PORT")
            .ok()
            .and_then(|p| p.parse().ok());
        let code = freebuff2api::login_window::run_login_window(port);
        std::process::exit(code);
    }

    // Logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,freebuff2api=debug")),
        )
        .with_target(false)
        .init();

    let config_path = freebuff2api::config::resolve_config_path();
    let cfg = Config::load(config_path.as_deref())?;
    let listen_addr = cfg.listen_addr.clone();
    tracing::info!("Freebuff2API v{} starting", env!("CARGO_PKG_VERSION"));
    tracing::info!("Listening on {}", listen_addr);
    tracing::info!("Account count: {}", cfg.auth_tokens.len());

    // Client
    let proxy = if cfg.http_proxy.is_empty() {
        None
    } else {
        Some(cfg.http_proxy.clone())
    };
    let client = Arc::new(UpstreamClient::new(
        cfg.upstream_base_url.clone(),
        proxy,
        Duration::from_secs(cfg.request_timeout_sec),
    )?);

    // Model registry
    let registry = Arc::new(ModelRegistry::new());
    registry.init().await;
    if let Ok((added, removed)) = registry.refresh_from_upstream(&client_http()).await {
        tracing::info!("Model registry synced: {added} added, {removed} removed");
    }
    // v0.10 H1 fix: the production path loads the vendored upstream model snapshot -> policy overrides
    // (availability time windows/efforts/fallback).
    // Fails silently degrade to the static base with a warn, never blocking startup (consistent with plan §1.1).
    if let Some(snap) = freebuff2api::models::load_local_snapshot() {
        match registry.refresh_strategy_from_snapshot(&snap) {
            Ok((added, updated)) => {
                tracing::info!("Model policy snapshot synced: {added} policies added, {updated} updated");
            }
            Err(e) => {
                tracing::warn!("Model policy snapshot sync failed, using the static base: {e}");
            }
        }
    }
    let router = Arc::new(ModelRouter::new(
        registry.clone(),
        RouterConfig::from_app_config(&cfg),
    ));

    // Multi-account pool
    let pool = Arc::new(Pool::new(&cfg, client.clone()));

    // Usage stats
    let usage = Arc::new(UsageDb::open(&cfg.sqlite_path)?);
    tracing::info!("Usage stats SQLite: {}", cfg.sqlite_path);

    // Telemetry (per-request detail/event chain; independent DB + independent writer thread, doesn't block the request path)
    let telemetry = Arc::new(TelemetryWriter::spawn(
        PathBuf::from(&cfg.telemetry_path),
        4096,
    )?);
    tracing::info!("Telemetry SQLite: {}", cfg.telemetry_path);

    // Live log bus (SSE broadcast + ring buffer); the redaction switch follows config.redact_logs (on by default)
    let logs = Arc::new(LogBus::new_with_redact(500, cfg.redact_logs));
    if cfg.redact_logs {
        tracing::info!("Log redaction is enabled (config.redact_logs=true)");
    }

    // Memory layer (user preferences/corrections; zero-LLM rule-based observe)
    let memory = Arc::new(freebuff2api::memory::MemoryStore::open(PathBuf::from(
        &cfg.memory_path,
    ))?);
    tracing::info!("Memory store SQLite: {}", cfg.memory_path);
    // Memory layer runtime switch (off by default -- per user note: memory isn't for everyone; the panel can toggle it live)
    let memory_runtime_enabled = Arc::new(std::sync::atomic::AtomicBool::new(cfg.memory_enabled));
    if cfg.memory_enabled {
        tracing::info!("Memory layer is enabled (config memory_enabled=true)");
    } else {
        tracing::info!("Memory layer is disabled (default; can be enabled on the panel's \"Memory\" page)");
    }

    // Skills system (files are the source of truth + SQLite index; legacy prompts kept for compatibility)
    // Note: doesn't use with_extension (it truncates when the directory name contains '.', e.g. data/my.skills -> data/my.sqlite)
    let skills_db = PathBuf::from(format!(
        "{}.sqlite",
        cfg.skills_dir.trim_end_matches(['/', '\\'])
    ));
    let skills = Arc::new(SkillsManager::open(
        PathBuf::from(&cfg.skills_dir),
        skills_db,
    )?);
    tracing::info!(
        "Skills directory: {} ({} loaded)",
        cfg.skills_dir,
        skills.list().len()
    );

    // Ad keepalive
    let ads = Arc::new(AdRefresher::new(client.clone(), cfg.clone()));

    // Built-in prompts/skills
    let prompts = Arc::new(freebuff2api::prompts::PromptManager::new());

    // Credential account info cache + account usage history (panel credential list and history lookups)
    let meta = Arc::new(freebuff2api::account_meta::AccountMetaStore::new(
        PathBuf::from(&cfg.cred_meta_path),
        PathBuf::from(&cfg.account_history_path),
    ));
    tracing::info!(
        "Credential info cache: {} - Usage history: {}",
        cfg.cred_meta_path,
        cfg.account_history_path
    );

    // Runtime API key (the panel can generate one with a click and it takes effect live, no restart needed)
    let api_keys = Arc::new(std::sync::RwLock::new(cfg.api_keys.clone()));
    if cfg.api_keys.is_empty() {
        tracing::info!("No api_keys configured: only accessible from localhost (the panel's \"Setup Guide\" can generate one with a click)");
    }

    // Web protocol bridge (OpenAI/Anthropic clients -> reuses upstream threads, saving daily session quota)
    let web_threads = Arc::new(freebuff2api::web_threads::WebThreadMap::new(PathBuf::from(
        &cfg.web_threads_path,
    )));
    tracing::info!("Web bridge session bindings: {}", cfg.web_threads_path);

    // Start background keepalive for each account
    {
        let accounts = pool.accounts.lock().await;
        for acc in accounts.iter() {
            let sess = acc.session.clone();
            let ads = ads.clone();
            tokio::spawn(async move { sess.run_keepalive(ads).await });
        }
    }

    // Dual-bucket concurrency semaphore capacities (read before cfg moves into the Arc)
    let (conc_free_slots, conc_free_multi, conc_sub_slots, conc_sub_multi) = (
        cfg.concurrency_free_slots,
        cfg.concurrency_free_multi,
        cfg.concurrency_sub_slots,
        cfg.concurrency_sub_multi,
    );

    // v0.9 §1.1: web Cookie credential pool (config Cookie entries + imported DB kind=web-cookie)
    let web_pool = Arc::new(freebuff2api::web_pool::WebCookiePool::new(&cfg));

    let state = AppState {
        cfg: Arc::new(cfg),
        client,
        pool,
        web_pool,
        registry,
        router,
        usage,
        telemetry,
        logs,
        memory,
        skills,
        ads,
        prompts,
        meta,
        api_keys,
        web_threads,
        memory_runtime_enabled,
        semaphore: Arc::new(freebuff2api::semaphore::TieredSemaphore::new(
            conc_free_slots,
            conc_free_multi,
            conc_sub_slots,
            conc_sub_multi,
        )
        .with_wait(std::time::Duration::from_millis(
            std::env::var("CONCURRENCY_WAIT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(freebuff2api::semaphore::ACQUIRE_TIMEOUT_MS),
        ))),
        started: std::time::Instant::now(),
    };

    // Upstream session auto-cleanup (per user note: a reverse proxy should clean up after itself, don't leave load on upstream that could get it flagged)
    if state.cfg.thread_cleanup_interval_sec > 0 {
        tokio::spawn(freebuff2api::api::thread_cleanup_loop(state.clone()));
    } else {
        tracing::info!("Upstream session auto-cleanup is disabled (thread_cleanup_interval_sec=0)");
    }

    let app = build_router(state);
    let listener = match tokio::net::TcpListener::bind(&listen_addr).await {
        Ok(l) => l,
        Err(e) => {
            // v0.8: gives a clear error when port binding fails (including a hint about the process holding the port), instead of a bare anyhow throw
            let hint = match e.kind() {
                std::io::ErrorKind::AddrInUse => {
                    format!(
                        "Port {} is already in use (another Freebuff2API instance may be running, or another program is using this port).\n\
                         Please change listen_addr in config.json to a different port and retry.\n\
                         To investigate: run `netstat -ano | findstr :{}` to find the PID holding the port,\n\
                         or use Task Manager to end the process using that port (careful not to kill your other instance by mistake).",
                        listen_addr, port_of(&listen_addr)
                    )
                }
                std::io::ErrorKind::PermissionDenied => {
                    format!(
                        "No permission to bind {} (ports <1024 on Windows usually require administrator privileges).\n\
                         Please change listen_addr to a higher port (e.g. 127.0.0.1:47821) or run as administrator.",
                        listen_addr
                    )
                }
                _ => format!(
                    "Failed to bind listen address {}: {e}\nPlease check that listen_addr in config.json is valid.",
                    listen_addr
                ),
            };
            eprintln!("\n[Freebuff2API] Startup failed: {hint}\n");
            anyhow::bail!(hint)
        }
    };
    tracing::info!("HTTP service ready");
    // v0.9 §1.5: injects the real TCP peer (ConnectInfo<SocketAddr>) for admin endpoint loopback checks
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Extracts the port from listen_addr (for the findstr troubleshooting command in error hints)
fn port_of(listen_addr: &str) -> String {
    listen_addr
        .rsplit_once(':')
        .map(|(_, p)| p.trim_end_matches(']').to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "47821".into())
}

// Uses a plain reqwest client for registry fetches (to avoid confusion with the upstream http client)
// Timeout protection: a registry sync failure should not block gateway startup
fn client_http() -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .user_agent("freebuff2api-registry")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10));
    if let Ok(p) = std::env::var("HTTPS_PROXY") {
        if !p.is_empty() {
            if let Ok(proxy) = reqwest::Proxy::all(&p) {
                b = b.proxy(proxy);
            }
        }
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}
