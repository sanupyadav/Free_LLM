//! WebCookiePool integration tests (v0.9 §1.1): building from a real Config/tokens.json + contract fields + concurrency stability.
//!
//! Unit-level tests (pick / circuit-break / cooldown / recovery / empty pool >= 8) live in src/web_pool.rs;
//! this file covers cross-module behavior such as "building from disk", "health snapshot contract fields",
//! and "concurrent pick stability".

use freebuff2api::config::Config;
use freebuff2api::web_pool::WebCookiePool;
use std::time::Duration;

fn cookie_a() -> String {
    "__Secure-next-auth.session-token=aaaa; __Host-next-auth.csrf-token=zzz".to_string()
}
fn cookie_b() -> String {
    "__Secure-next-auth.session-token=bbbb; __Host-next-auth.csrf-token=zzz".to_string()
}

/// Assembles a temporary Config with a tokens_path (auth_tokens has one Cookie, the import library has one web-cookie)
fn tmp_cfg(cfg_auth_token: &str, tokens_json: &str) -> (Config, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let tokens_path = dir.path().join("tokens.json");
    std::fs::write(&tokens_path, tokens_json).unwrap();
    let cfg = Config {
        listen_addr: "127.0.0.1:47821".into(),
        auth_tokens: vec![cfg_auth_token.to_string()],
        tokens_path: tokens_path.to_str().unwrap().to_string(),
        skip_upstream_check: true,
        ..Default::default()
    };
    (cfg, dir)
}

#[tokio::test]
async fn builds_from_config_cookie_and_imported_web_cookie() {
    // Both the Cookie entry in config.auth_tokens and the web-cookie in tokens.json must enter the pool
    let tokens_json = format!(
        r#"[{{"token":"{b}","source":"cookie","host":"freebuff.com","path":"/p","method":"GET","added_at":"2026-09-01T00:00:00Z"}}]"#,
        b = cookie_b()
    );
    let (cfg, _dir) = tmp_cfg(&cookie_a(), &tokens_json);
    let pool = WebCookiePool::new(&cfg);
    let snap = pool.snapshot().await;
    assert_eq!(snap.len(), 2, "credentials from both config and the import library should enter the pool");
    let sources: Vec<&str> = snap.iter().map(|s| s.source).collect();
    assert!(sources.contains(&"config"), "the config entry should have source=config");
    assert!(sources.contains(&"imported"), "the imported entry should have source=imported");
}

#[tokio::test]
async fn ignores_bearer_tokens_in_config_and_import() {
    // A Bearer token (no session-token) must not enter the web Cookie pool
    let tokens_json = r#"[{"token":"sk-bearer-abc123","source":"curl","host":"h","path":"p","method":"POST","added_at":"2026-09-01T00:00:00Z"}]"#;
    let (cfg, _dir) = tmp_cfg("sk-plain-bearer", tokens_json);
    let pool = WebCookiePool::new(&cfg);
    assert!(
        pool.pick().await.is_none(),
        "no web Cookie -> pick should be None"
    );
}

#[tokio::test]
async fn snapshot_contract_fields_for_health_endpoint() {
    // Web-side contract for /api/accounts/health: field names and types are immutable (the frontend draws its timeline from these)
    let (cfg, _dir) = tmp_cfg(&cookie_a(), "[]");
    let pool = WebCookiePool::new(&cfg);
    let id = pool.pick().await.unwrap().id;
    pool.mark_cooldown(
        &id,
        Duration::from_secs(600),
        "web chat HTTP 401 Unauthorized",
    )
    .await;
    let snap = pool.snapshot().await;
    let s = &snap[0];
    assert_eq!(s.kind, "web-cookie");
    assert_eq!(s.id, id);
    assert!(!s.masked.is_empty());
    assert_eq!(s.circuit_state, "open");
    assert!(s.cooldown_until.is_some(), "cooldown_until should be set after tripping");
    assert_eq!(s.trips, 1);
    assert!(s.last_error.as_deref().is_some());
    // last_ok_at starts as None (never succeeded)
    assert!(s.last_ok_at.is_none());
    // The added_at field must exist (contract); config entries may have it as null (only imported entries have a real ingest time)
    assert!(
        serde_json::to_value(s).unwrap().get("added_at").is_some(),
        "the added_at field must exist"
    );
    let v = serde_json::to_value(s).unwrap();
    for field in [
        "id",
        "kind",
        "source",
        "added_at",
        "masked",
        "health_score",
        "circuit_state",
        "cooldown_until",
        "trips",
        "last_error",
        "last_ok_at",
    ] {
        assert!(v.get(field).is_some(), "contract field {field} is missing");
    }
}

#[tokio::test]
async fn concurrent_picks_never_panic_and_respect_cooldown() {
    let (cfg, _dir) = tmp_cfg(
        &cookie_a(),
        &format!(
            r#"[{{"token":"{b}","source":"cookie","host":"h","path":"p","method":"GET","added_at":"2026-09-01T00:00:00Z"}}]"#,
            b = cookie_b()
        ),
    );
    let pool = WebCookiePool::new(&cfg);
    // Bad account gets a 401: trips into cooldown immediately; concurrent picks must reliably land on the good account (no panic / no None)
    let idb = freebuff2api::import::cred_id(&cookie_b());
    pool.mark_cooldown(&idb, Duration::from_secs(600), "web chat HTTP 401")
        .await;
    let pool = std::sync::Arc::new(pool);
    let mut handles = Vec::new();
    for _ in 0..16 {
        let p = pool.clone();
        handles.push(tokio::spawn(async move { p.pick().await }));
    }
    for h in handles {
        let picked = h.await.unwrap();
        assert!(picked.is_some(), "concurrent pick must not return None while a good account exists");
        let picked = picked.unwrap();
        assert_eq!(picked.cookie, cookie_a(), "after the bad account trips, concurrent picks should all land on the good account");
    }
}

#[tokio::test]
async fn mark_ok_sets_last_ok_at() {
    let (cfg, _dir) = tmp_cfg(&cookie_a(), "[]");
    let pool = WebCookiePool::new(&cfg);
    let id = pool.pick().await.unwrap().id;
    pool.mark_cooldown(&id, Duration::from_millis(1), "boom")
        .await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    // Cooldown expires -> HalfOpen probe let through -> two consecutive successes restore Closed
    let p1 = pool.pick().await.unwrap();
    pool.mark_ok(&p1.id).await;
    pool.mark_ok(&p1.id).await;
    let snap = pool.snapshot().await;
    let s = snap.iter().find(|x| x.id == id).unwrap();
    assert!(s.last_ok_at.is_some(), "last_ok_at should be recorded after success");
    assert_eq!(s.circuit_state, "closed", "consecutive successes should restore Closed");
}
