//! Embedded-browser one-click login (Windows / WebView2 / wry) -- compiled only under cfg(windows)
//!
//! The user ask: "Is one-click login only possible via the extension? Can't an inline browser
//! capture it directly?" -- this module is the answer: the gateway spawns an independent login
//! child process (avoiding the WebView2 event loop blocking the gateway's tokio runtime), loads
//! freebuff.com in the window, and once the user completes GitHub login, it uses the WebView2
//! CookieManager to capture **all cookies (including the HttpOnly session-token)**, automatically
//! POSTs them to the local gateway's `/api/tokens/import` to store them, then closes the window.
//!
//! Behavior baseline: matches the desktop Electron implementation (desktop/main.js:219-293).
//! HttpOnly readability: ICoreWebView2CookieManager is on the same tier as Electron's
//! session.cookies -- an OS-level WebView component (not page JS), so it isn't subject to the
//! browser's same-origin JS restrictions.
//!
//! Process model: the gateway's main process spawns a child process via
//! `Command::new(current_exe) --login-window`; the child process runs self-contained (its own
//! independent tao event loop), and **reports the result directly via its process exit code**:
//!   0 = login and storage succeeded (result message on stdout); 1 = failed/timed out/user closed
//!   the window (reason on stderr).

use anyhow::{anyhow, Result};

/// Login-success test: the cookie string must contain the session token (goes by the Cookie, not URL features)
const SESSION_MARK: &str = "__Secure-next-auth.session-token";

/// Default gateway probe ports (matches the extension's background.js DEFAULT_PORTS)
const GATEWAY_PORTS: &[u16] = &[47821, 47822, 8787];

/// Upper bound for polling login state (same order of magnitude as the extension's 180s, given generously)
const LOGIN_TIMEOUT_SECS: u64 = 600;

/// Whether this is running as a "login window" child process
pub fn is_login_mode() -> bool {
    std::env::args().any(|a| a == "--login-window")
}

/// Runs the login window child process (blocking; the tao event loop's `run()` returns `!`, so it
/// never actually returns to the caller).
/// The return type is kept for testing and future changes; actual control flow ends the process
/// directly via `process::exit`.
pub fn run_login_window(gateway_port: Option<u16>) -> i32 {
    match run_login_window_inner(gateway_port) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("login window failed: {e:#}");
            1
        }
    }
}

fn run_login_window_inner(gateway_port: Option<u16>) -> Result<String> {
    use wry::WebViewBuilder;

    let ports: Vec<u16> = gateway_port.map(|p| vec![p]).unwrap_or_else(|| {
        // GATEWAY_PORT env var takes priority (the gateway passes it when spawning); otherwise probe the default sequence
        if let Ok(p) = std::env::var("GATEWAY_PORT") {
            if let Ok(p) = p.parse() {
                return vec![p];
            }
        }
        GATEWAY_PORTS.to_vec()
    });

    // tao's EventLoop::new() panics internally on failure (same usage as the official examples)
    let event_loop = tao::event_loop::EventLoop::new();
    let window = tao::window::WindowBuilder::new()
        .with_title("Freebuff Login -- stores automatically once complete")
        .with_inner_size(tao::dpi::LogicalSize::new(1000.0, 720.0))
        .build(&event_loop)
        .map_err(|e| anyhow!("failed to create login window: {e}"))?;

    let webview = WebViewBuilder::new()
        .with_url("https://freebuff.com/")
        .build(&window)
        .map_err(|e| {
            anyhow!("failed to initialize WebView2: {e} (please confirm the Microsoft Edge WebView2 Runtime is installed)")
        })?;

    // Captures the current cookies and tries to store them; returns Some((exit_code, message)) if the flow has concluded.
    // A standalone function (borrows webview), reused by both the "first-load probe" and "event loop polling" -- to sidestep run()'s 'static closure constraint.
    fn try_capture(webview: &wry::WebView, ports: &[u16]) -> Option<(i32, String)> {
        let Ok(cookies) = webview.cookies_for_url("https://freebuff.com/") else {
            return None;
        };
        let header = cookies_to_header(&cookies);
        if !header.contains(SESSION_MARK) {
            return None; // Not logged in yet, keep waiting
        }
        for port in ports {
            match post_import(*port, &header) {
                Ok(added) => {
                    let msg = if added > 0 {
                        format!("\u{2705} Login succeeded: {added} credential(s) stored automatically (port {port}); you can close this window and refresh the panel")
                    } else {
                        "\u{2705} Login succeeded: credential already existed (deduplicated automatically)".to_string()
                    };
                    return Some((0, msg));
                }
                Err(e) => {
                    // Connection failed -> try the next port; gateway explicitly rejected -> stop and report accurately
                    if !e.to_string().contains("connection") {
                        return Some((1, format!("gateway rejected the credential (port {port}): {e}")));
                    }
                }
            }
        }
        Some((
            1,
            format!("could not connect to the local gateway (tried ports {ports:?}) -- please confirm the gateway is running"),
        ))
    }

    // Probe once on first load (the user may already arrive with a valid session)
    if let Some((code, msg)) = try_capture(&webview, &ports) {
        report(code, &msg);
    }

    let started = std::time::Instant::now();
    let ports_for_loop = ports.clone();
    event_loop.run(move |event, _, control_flow| {
        use tao::event::Event::*;
        match event {
            WindowEvent {
                event: tao::event::WindowEvent::CloseRequested,
                ..
            } => {
                report(1, "window was closed before login completed (you can retry, or use the extension/manual import instead)");
            }
            NewEvents(..) => {
                // Wakes again after 600ms (WaitUntil: zero-load UI thread idle, doesn't hurt frame rate)
                *control_flow = tao::event_loop::ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(600),
                );
                if started.elapsed() > std::time::Duration::from_secs(LOGIN_TIMEOUT_SECS) {
                    report(1, "timed out waiting for login -- please retry, or use the extension/manual import instead");
                }
                if let Some((code, msg)) = try_capture(&webview, &ports_for_loop) {
                    report(code, &msg);
                }
            }
            _ => {}
        }
    })
}

/// Reports the result and ends the process (run() cannot return; the exit code IS the result protocol)
fn report(code: i32, msg: &str) {
    if code == 0 {
        println!("{msg}");
    } else {
        eprintln!("{msg}");
    }
    std::process::exit(code);
}

/// Converts the `Vec<cookie::Cookie>` returned by wry's `cookies_for_url` into a Cookie request-header string.
/// The HttpOnly session-token is included (CookieManager is an OS-level component, not subject to page JS restrictions).
fn cookies_to_header(cookies: &[wry::cookie::Cookie<'static>]) -> String {
    cookies
        .iter()
        .filter(|c| !c.value().is_empty())
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// POSTs to the local gateway's /api/tokens/import; returns the added count.
/// An Err text containing "connection" means the network is unreachable (should try another port); otherwise the gateway explicitly rejected it.
/// Note: the child process is still an #[tokio::main] binary (it has a runtime context), but this runs in the
/// tao event loop's sync context, so it can't `.await` -- `futures::executor::block_on` drives the async reqwest
/// call on the current thread instead (don't change this to spawn, it would escape the runtime context).
fn post_import(port: u16, cookie: &str) -> Result<usize> {
    let url = format!("http://127.0.0.1:{port}/api/tokens/import");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = futures::executor::block_on(async {
        client
            .post(&url)
            .header("content-type", "application/json")
            .body(serde_json::json!({ "cookie": cookie }).to_string())
            .send()
            .await
            .map_err(|e| anyhow!("connection failed: {e}"))
    })?;
    let status = resp.status();
    let text = futures::executor::block_on(resp.text()).unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "HTTP {}: {}",
            status,
            text.chars().take(160).collect::<String>()
        );
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
    Ok(v.get("added").and_then(|a| a.as_u64()).unwrap_or(0) as usize)
}

/// Gateway side: spawns the login window child process (non-blocking).
/// Returns true if spawned; false if the environment doesn't support it (should fall back to the extension/manual wizard).
/// Reentrancy guard: at most one login window at a time (mashing the button won't stack up N WebView2 processes).
/// Released once the child process exits normally (stored successfully/timed out/window closed).
static LOGIN_SPAWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn spawn_login_window(gateway_port: u16) -> bool {
    use std::sync::atomic::Ordering;
    // CAS preemption: a login window is already running -> reject outright (frontend shows a toast)
    if LOGIN_SPAWNED
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        tracing::warn!("login window is already running, ignoring duplicate request");
        return false;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => {
            LOGIN_SPAWNED.store(false, Ordering::Release);
            return false;
        }
    };
    let child = std::process::Command::new(exe)
        .arg("--login-window")
        .env("GATEWAY_PORT", gateway_port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match child {
        Ok(mut c) => {
            // Background thread waits for the child process to exit: writes the failure reason to a result file
            // (readable by the panel's openEmbedLogin polling), otherwise on a machine without the WebView2
            // Runtime, the child's exit(1) gives the user no visible feedback at all (the panel just times out waiting).
            std::thread::spawn(move || {
                let code = c.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
                LOGIN_SPAWNED.store(false, Ordering::Release);
                if code != 0 {
                    // The window already showed a message on success; on failure, write a result file for the panel to display
                    let msg = serde_json::json!({
                        "ok": false,
                        "exit": code,
                        "message": format!("embedded login window exited (code {code}) -- please check the message in the window, or use the extension/clipboard/manual import instead"),
                    });
                    let path = std::path::Path::new("data");
                    if path.exists() {
                        let _ =
                            std::fs::write(path.join("login_window_result.json"), msg.to_string());
                    }
                }
            });
            true
        }
        Err(_) => {
            LOGIN_SPAWNED.store(false, Ordering::Release);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_to_header_filters_empty() {
        let c1 = wry::cookie::Cookie::new("__Secure-next-auth.session-token", "abc123");
        let c2 = wry::cookie::Cookie::new("other", "");
        let c3 = wry::cookie::Cookie::new("x", "y");
        let header = cookies_to_header(&[c1, c2, c3]);
        assert!(header.contains("__Secure-next-auth.session-token=abc123"));
        assert!(header.contains("x=y"));
        assert!(!header.contains("other="), "empty-value cookies should be filtered out");
    }

    #[test]
    fn cookies_to_header_empty() {
        assert_eq!(cookies_to_header(&[]), "");
    }

    #[test]
    fn login_mode_detection() {
        // The current test process has no --login-window argument
        assert!(!is_login_mode());
    }
}
