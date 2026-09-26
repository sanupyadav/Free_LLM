//! Non-Windows stub for the embedded login window (WebView2 is Windows-only).
//!
//! The panel's `/api/login/embed` returns an explicit failure on non-Windows platforms,
//! and the frontend falls back to the extension / clipboard / manual wizard (the login wizard copy already covers this platform note).
//!
//! This stub keeps a **matching signature** with `login_window_windows` (re-exported via the `login_window`
//! facade, so callers don't need to be aware of platform differences); on Windows these functions have real
//! callers through the facade, so the stub side allows dead_code without triggering a warning.

/// Never in login mode on non-Windows (the --login-window flag also routes to this stub hint in main's dispatch)
#[allow(dead_code)]
pub fn is_login_mode() -> bool {
    false
}

/// Non-Windows stub: reports unsupported (never actually called -- main's dispatch checks the platform first)
#[allow(dead_code)]
pub fn run_login_window(_gateway_port: Option<u16>) -> i32 {
    eprintln!("The embedded login window only supports Windows (requires the Microsoft Edge WebView2 Runtime) -- please use the browser extension or manual import instead");
    1
}

/// Non-Windows stub: returns false -> /api/login/embed returns ok:false to prompt a fallback
#[allow(dead_code)]
pub fn spawn_login_window(_gateway_port: u16) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_unsupported() {
        assert!(!spawn_login_window(47821));
        assert!(!is_login_mode());
        assert_eq!(run_login_window(Some(47821)), 1);
    }
}
