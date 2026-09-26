//! Log/telemetry redaction (v0.8): replaces sensitive credential fragments with `***` before writing to the log bus/telemetry.
//!
//! On by default (`config.redact_logs`, can be disabled via the `REDACT_LOGS` env var).
//! What gets redacted:
//! - Cookie values like `__Secure-next-auth.session-token=...` / `session-token=...`
//! - `authorization: Bearer xxx` / `"authorization":"..."` in request bodies
//! - Plaintext Bearer / `sk-`-prefixed key strings
//!
//! Only does "fragment-level" replacement, keeping readable context (e.g. error reason prefixes) intact, without breaking log structure.

/// Whether redaction is enabled (the process-level cached decision lives with the caller -- avoids reading the env var on every log call)
pub const DEFAULT_REDACT_LOGS: bool = true;

/// Redact a piece of text: replaces Cookie values, Bearer tokens, authorization values.
pub fn redact(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = text.to_string();
    // 1) Cookie values (next-auth trio): `name=value` -> `name=***`
    for marker in [
        "__Secure-next-auth.session-token",
        "__Host-next-auth.session-token",
        "next-auth.session-token",
        "session-token",
        ".next-auth.callback-url",
        "callback-url",
        "next-auth.csrf-token",
        "csrf-token",
    ] {
        out = redact_assigned_value(&out, marker);
    }
    // 2) authorization header/field values
    for pat in ["authorization:", "authorization\"", "Authorization:"] {
        out = redact_bearer_after(&out, pat);
    }
    // 3) Bare Bearer token (appears in message body/error body)
    out = redact_bearer_after(&out, "Bearer ");
    // 4) sk--prefixed key strings (OpenAI-style; only treated as a real key when len >= 20)
    out = redact_sk_tokens(&out);
    // 5) Bare JWT (CSP/DEPTH fallback): a three-segment token with no marker/Bearer prefix that
    //    starts with eyJ (the base64url signature of a JWT header) -- Freebuff's web cookie value
    //    is itself a JWT, so if a bare JWT without a marker shows up in an upstream error body/log
    //    (defense in depth), this fallback catches and replaces it.
    out = redact_jwt(&out);
    out
}

/// Replaces a bare JWT shaped like `eyJ<seg1>.<seg2>.<seg3>` with `eyJ***` (keeps the prefix for recognizability).
/// Requirement: every segment must be base64url characters (A-Za-z0-9_-), and the total length must be >= 60 to be treated as a real JWT, to avoid misfiring on ordinary text.
fn redact_jwt(text: &str) -> String {
    if !text.contains("eyJ") {
        return text.to_string();
    }
    let b64 = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("eyJ") {
        out.push_str(&rest[..pos]);
        let seg = |s: &str| s.chars().take_while(|c| b64(*c)).count();
        let after = &rest[pos..];
        let seg1 = seg(after);
        if seg1 >= 10 {
            if let Some(dot1) = after.get(seg1..).and_then(|s| s.strip_prefix('.')) {
                let seg2 = seg(dot1);
                if seg2 >= 10 {
                    if let Some(dot2) = dot1.get(seg2..).and_then(|s| s.strip_prefix('.')) {
                        let seg3 = seg(dot2);
                        // Total length of all three segments >= 60 and last segment non-empty -> treated as JWT
                        if seg1 + seg2 + seg3 >= 60 && seg3 >= 10 {
                            let end = seg1 + 1 + seg2 + 1 + seg3;
                            out.push_str("eyJ***");
                            rest = &after[end.min(after.len())..];
                            continue;
                        }
                    }
                }
            }
        }
        // Not a JWT: keep the already-scanned "eyJ" verbatim and advance one char (prevents infinite loop)
        out.push_str("eyJ");
        rest = &after[3..];
    }
    out.push_str(rest);
    out
}

/// Replaces the assigned value in `marker=...` form with `***` (keeps the key name and the = sign)
fn redact_assigned_value(text: &str, marker: &str) -> String {
    if !text.contains(marker) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(marker) {
        out.push_str(&rest[..pos + marker.len()]);
        let after = &rest[pos + marker.len()..];
        // Expect `=` to follow immediately
        if let Some(eq_rest) = after.strip_prefix('=') {
            out.push('=');
            // Take up to the next delimiter (; space & " ' newline or end) as the value
            let end = eq_rest
                .find([';', ' ', '&', '"', '\'', '\n', '\r'])
                .unwrap_or(eq_rest.len());
            out.push_str("***");
            rest = &eq_rest[end..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Replaces `prefix xxx` (from after prefix to the delimiter/end of line): keeps the prefix, changes the value to `***`
fn redact_bearer_after(text: &str, prefix: &str) -> String {
    if !text.contains(prefix) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(prefix) {
        out.push_str(&rest[..pos + prefix.len()]);
        let after = &rest[pos + prefix.len()..];
        let end = after
            .find([';', ' ', '\n', '\r', '"', ',', '}'])
            .unwrap_or(after.len());
        let val = &after[..end];
        // Only redact when the value looks like a token (avoids misfiring on ordinary English words, e.g. "Bearer token not found")
        if val.len() >= 12 && val.chars().any(|c| c.is_ascii_alphanumeric()) {
            out.push_str("***");
        } else {
            out.push_str(val);
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Replaces a long `sk-...` string (>=20 chars) with `sk-***`
fn redact_sk_tokens(text: &str) -> String {
    if !text.contains("sk-") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("sk-") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 3..];
        // From after sk- to the delimiter
        let end = after
            .find([';', ' ', '\n', '\r', '"', ',', '}', '='])
            .unwrap_or(after.len());
        let tail = &after[..end];
        if tail.len() >= 17 {
            out.push_str("sk-***");
            rest = &after[end..];
        } else {
            out.push_str("sk-");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_cookie_values() {
        let s = "Cookie: __Secure-next-auth.session-token=abc123def; other=1";
        let out = redact(s);
        assert!(!out.contains("abc123def"), "cookie value must be redacted: {out}");
        assert!(
            out.contains("__Secure-next-auth.session-token=***"),
            "key name must be kept: {out}"
        );
        assert!(out.contains("other=1"), "non-sensitive cookie must be kept: {out}");
    }

    #[test]
    fn redacts_bearer_authorization() {
        let s = "authorization: Bearer sk-verysecretlongtoken123456";
        let out = redact(s);
        assert!(
            !out.contains("sk-verysecretlongtoken123456"),
            "Bearer value must be redacted: {out}"
        );
        assert!(out.contains("Bearer ***"), "Bearer prefix must be kept: {out}");
    }

    #[test]
    fn redacts_authorization_json_field() {
        let s = r#"{"authorization":"Bearer abcdefghijklmnopqrstuvwxyz123"}"#;
        let out = redact(s);
        assert!(
            !out.contains("abcdefghijklmnopqrstuvwxyz123"),
            "authorization in JSON must be redacted: {out}"
        );
    }

    #[test]
    fn redacts_sk_style_tokens() {
        let s = "key=sk-proj-abcdefghijklmnopqrstuvwxyz1234567890";
        let out = redact(s);
        assert!(
            !out.contains("sk-proj-abcdefghijklmnopqrstuvwxyz"),
            "long sk- string must be redacted: {out}"
        );
        assert!(out.contains("sk-***"), "sk- prefix must be kept: {out}");
    }

    #[test]
    fn keeps_normal_text_untouched() {
        let s = "upstream queueing waiting_room, please retry in 15 seconds";
        assert_eq!(redact(s), s);
        let s2 = "Bearer token not found"; // value too short to look like a token -> kept
        assert_eq!(redact(s2), s2);
    }

    #[test]
    fn empty_input_ok() {
        assert_eq!(redact(""), "");
    }

    #[test]
    fn redacts_bare_jwt_without_marker() {
        // Bare JWT (no marker/Bearer prefix): eyJ three-segment form, total length >= 60 treated as JWT and redacted
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert!(jwt.len() >= 60);
        let out = redact(jwt);
        assert!(!out.contains("eyJhbGci"), "bare JWT must be redacted: {out}");
        assert!(out.starts_with("eyJ***"), "eyJ prefix must be kept: {out}");
        // Shorter base64 strings (e.g. ordinary text containing eyJxxx.xx.x) must not be misfired on
        let short = "randomly mentioned eyJab.xyz.abc";
        assert!(redact(short).contains("eyJab"), "short string must not be misfired on");
        // Normal plain text is unaffected
        let plain = "upstream queueing waiting_room, please wait";
        assert_eq!(redact(plain), plain);
    }

    #[test]
    fn jwt_inside_text_is_redacted_but_context_kept() {
        let s = "Error: credential expired token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let out = redact(s);
        assert!(out.contains("credential expired"), "context must be kept");
        assert!(!out.contains("eyJhbGci"), "JWT must be redacted: {out}");
    }
}
