//! Platform entry point for the embedded login window: Windows uses the WebView2 implementation, other platforms use the stub.
//!
//! Unified API (`is_login_mode` / `run_login_window` / `spawn_login_window`);
//! callers (main.rs / api.rs) don't need to be aware of platform differences.

#[cfg(windows)]
pub use crate::login_window_windows::*;

#[cfg(not(windows))]
pub use crate::login_window_stub::*;
