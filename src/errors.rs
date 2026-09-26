//! Upstream error rule table: text takes priority, status code is the fallback
//!
//! Codebuff upstream sometimes returns a text error code in an HTTP 200 body
//! (e.g. `free_mode_invalid_agent_model`, `waiting_room_queued`),
//! and also returns descriptive text in 4xx/5xx bodies or SSE fragments;
//! classifying by status code alone would miss these cases.
//! This module treats the response body text as the primary signal (case-insensitive
//! substring matching plus a few regexes), with the HTTP status code only as a fallback.
//!
//! Error kinds align semantically with `retry::FailureKind` (this module is more granular),
//! and additionally provide retry hints: [`ErrorKind::is_retryable`] / [`ErrorKind::retry_after_hint`].

use std::sync::OnceLock;

use regex::Regex;

/// Max character count for the error excerpt; truncated and suffixed with `...` beyond this
const EXCERPT_MAX_CHARS: usize = 300;

/// Suggested backoff seconds for RateLimit
const RATE_LIMIT_RETRY_AFTER_SECS: u64 = 60;
/// Suggested backoff seconds for WaitingRoom (the free queue usually clears quickly)
const WAITING_ROOM_RETRY_AFTER_SECS: u64 = 15;
/// Suggested backoff seconds for Upstream5xx (short wait then retry)
const UPSTREAM_RETRY_AFTER_SECS: u64 = 5;

/// Strong queue keywords: any match classifies as WaitingRoom (can appear even in a 200 body)
const WAITING_ROOM_STRONG: &[&str] = &["waiting_room", "waiting room", "排队"];
/// Weak queue keyword: only evaluated on 429/503, to avoid false positives on normal 200 responses
const WAITING_ROOM_WEAK: &[&str] = &["queue"];
/// Rate-limit keywords
const RATE_LIMIT_HINTS: &[&str] = &["rate limit", "rate_limit", "too many requests", "限流"];
/// Model-unavailable keywords (narrowed: avoid 503 "Service Unavailable" being misclassified as a model issue)
const MODEL_UNAVAILABLE_HINTS: &[&str] = &[
    "invalid_agent_model",
    "free_mode_invalid",
    "model not available",
    "only available for",
    "model_not_found",
    "no such model",
];
/// Auth-expired keywords
const AUTH_HINTS: &[&str] = &[
    "unauthorized",
    "invalid token",
    "session expired",
    "token expired",
    "authentication",
];
/// Bad-request keywords
const BAD_REQUEST_HINTS: &[&str] = &["invalid_request", "bad request"];

/// Normalized error kind (semantically aligned with `retry::FailureKind` but more granular)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Rate limited (retryable with backoff)
    RateLimit,
    /// Queued in the free queue (should honor Retry-After)
    WaitingRoom,
    /// Auth expired (should cool down and switch accounts)
    AuthExpired,
    /// Model unavailable (should switch models)
    ModelUnavailable,
    /// Request problem (not retryable)
    BadRequest,
    /// Upstream server error (short wait then retry)
    #[serde(rename = "upstream_5xx")]
    Upstream5xx,
    /// Network/timeout
    Network,
    /// Cannot be classified
    Unknown,
}

impl ErrorKind {
    /// Stable string identifier (used for logs and telemetry error_kind, matches the serde output)
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::RateLimit => "rate_limit",
            ErrorKind::WaitingRoom => "waiting_room",
            ErrorKind::AuthExpired => "auth_expired",
            ErrorKind::ModelUnavailable => "model_unavailable",
            ErrorKind::BadRequest => "bad_request",
            ErrorKind::Upstream5xx => "upstream_5xx",
            ErrorKind::Network => "network",
            ErrorKind::Unknown => "unknown",
        }
    }

    /// Whether this is suitable for backoff-and-retry
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::RateLimit
                | ErrorKind::WaitingRoom
                | ErrorKind::Upstream5xx
                | ErrorKind::Network
        )
    }

    /// Suggested backoff seconds (returns `None` when no reliable hint is available)
    pub fn retry_after_hint(self) -> Option<u64> {
        match self {
            ErrorKind::RateLimit => Some(RATE_LIMIT_RETRY_AFTER_SECS),
            ErrorKind::WaitingRoom => Some(WAITING_ROOM_RETRY_AFTER_SECS),
            ErrorKind::Upstream5xx => Some(UPSTREAM_RETRY_AFTER_SECS),
            _ => None,
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Authoritative classification: text rules take priority, status code is the fallback
///
/// - `body` can be the full upstream error body, or an error fragment from an SSE stream;
///   it is lowercased before matching, so keyword matching is case-insensitive.
/// - `status` is the HTTP status code; `0` means a network error (no response).
pub fn classify(status: u16, body: &str) -> ErrorKind {
    let lowered = body.to_lowercase();
    if let Some(kind) = classify_text(&lowered, status) {
        return kind;
    }
    classify_status(status)
}

/// Extract Retry-After (seconds) from the error body/response headers
///
/// The response header takes priority; falls back to the body's
/// `"retryAfter":30` / `"retry_after": 30` / `"retry-after":"7"` field when the header is missing or invalid.
pub fn extract_retry_after(body: &str, headers_retry_after: Option<&str>) -> Option<u64> {
    if let Some(secs) = headers_retry_after.and_then(parse_retry_after_secs) {
        return Some(secs);
    }
    retry_after_re()
        .and_then(|re| re.captures(body))
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
}

/// Extract a human-readable excerpt from the upstream error body (truncated to [`EXCERPT_MAX_CHARS`] characters)
///
/// Prefers the JSON `message` / `detail` / `error_description` / `error` field;
/// for non-JSON text (including SSE fragments), uses a regex to find a `"message":"..."`-style field;
/// falls back to the raw text if both fail. Consecutive whitespace is collapsed to a single space.
pub fn error_excerpt(body: &str) -> String {
    let text = extract_message(body).unwrap_or_else(|| body.to_string());
    let normalized = collapse_whitespace(&text);
    truncate_chars(&normalized, EXCERPT_MAX_CHARS)
}

/// Text-rule classification; returns `None` on no match, leaving it to the status-code fallback
fn classify_text(lowered: &str, status: u16) -> Option<ErrorKind> {
    if contains_any(lowered, WAITING_ROOM_STRONG)
        || (matches!(status, 429 | 503) && contains_any(lowered, WAITING_ROOM_WEAK))
    {
        return Some(ErrorKind::WaitingRoom);
    }
    if contains_any(lowered, RATE_LIMIT_HINTS) {
        return Some(ErrorKind::RateLimit);
    }
    if contains_any(lowered, MODEL_UNAVAILABLE_HINTS) {
        return Some(ErrorKind::ModelUnavailable);
    }
    if contains_any(lowered, AUTH_HINTS) {
        return Some(ErrorKind::AuthExpired);
    }
    if contains_any(lowered, BAD_REQUEST_HINTS)
        || missing_required_re().is_some_and(|re| re.is_match(lowered))
    {
        return Some(ErrorKind::BadRequest);
    }
    None
}

/// Status-code fallback
fn classify_status(status: u16) -> ErrorKind {
    match status {
        0 => ErrorKind::Network,
        429 => ErrorKind::RateLimit,
        401 | 403 => ErrorKind::AuthExpired,
        404 => ErrorKind::ModelUnavailable,
        400 | 422 => ErrorKind::BadRequest,
        s if (500..600).contains(&s) => ErrorKind::Upstream5xx,
        _ => ErrorKind::Unknown,
    }
}

/// Parse the Retry-After response header (seconds only; an HTTP-date form returns `None`)
fn parse_retry_after_secs(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<u64>().ok()
}

/// Extract the message field: full JSON first, regex fragment as fallback
fn extract_message(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(message) = message_from_value(&value) {
            return Some(message);
        }
    }
    message_from_fragment(trimmed)
}

/// Recursively take the first non-empty message field from a JSON value
fn message_from_value(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return non_empty(s);
    }
    let obj = value.as_object()?;
    for key in ["message", "detail", "error_description", "error"] {
        if let Some(found) = obj.get(key).and_then(message_from_value) {
            return Some(found);
        }
    }
    None
}

/// Take the first message field from non-complete-JSON text (e.g. SSE fragments)
fn message_from_fragment(text: &str) -> Option<String> {
    let re = message_fragment_re()?;
    let caps = re.captures(text)?;
    let raw = caps.get(1)?.as_str();
    // The captured content is a JSON string body; try to unescape it, falling back to the raw text on failure
    let decoded =
        serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_string());
    non_empty(&decoded)
}

/// `missing ... required` pattern (allows a small amount of arbitrary text/newlines in between)
fn missing_required_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)missing\b.{0,80}?required").ok())
        .as_ref()
}

/// Retry-After field within the error body (`"retryAfter":30` / `retry_after=45`)
fn retry_after_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)"?retry[_-]?after"?\s*[:=]\s*"?(\d{1,7})"#).ok())
        .as_ref()
}

/// Message field within an SSE / error-body fragment
fn message_fragment_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)"(?:message|error|detail|error_description)"\s*:\s*"((?:\\.|[^"\\])*)""#)
            .ok()
    })
    .as_ref()
}

/// Lowercase keyword substring match (any hit counts)
fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Returns an owned string if non-empty after trimming whitespace
fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Collapse consecutive whitespace (including newlines) into a single space
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` characters; ends with `...` when truncated (total length stays `max`)
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some(_) => {
            let keep = max.saturating_sub(3);
            let mut out: String = s.chars().take(keep).collect();
            out.push_str("...");
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_rules_are_case_insensitive() {
        // Queue
        assert_eq!(classify(200, "waiting_room_queued"), ErrorKind::WaitingRoom);
        assert_eq!(
            classify(200, "Waiting Room: position 3"),
            ErrorKind::WaitingRoom
        );
        assert_eq!(classify(502, "正在排队，请稍候"), ErrorKind::WaitingRoom);
        // Rate limit
        assert_eq!(classify(200, "RATE LIMIT exceeded"), ErrorKind::RateLimit);
        assert_eq!(classify(200, "rate_limit_exceeded"), ErrorKind::RateLimit);
        assert_eq!(classify(200, "Too Many Requests"), ErrorKind::RateLimit);
        assert_eq!(classify(400, "请求已被限流"), ErrorKind::RateLimit);
        // Model unavailable
        assert_eq!(
            classify(200, "free_mode_invalid_agent_model"),
            ErrorKind::ModelUnavailable
        );
        assert_eq!(
            classify(200, "MODEL_NOT_FOUND"),
            ErrorKind::ModelUnavailable
        );
        assert_eq!(
            classify(200, "No Such Model: gpt-x"),
            ErrorKind::ModelUnavailable
        );
        assert_eq!(
            classify(200, "model not available"),
            ErrorKind::ModelUnavailable
        );
        // Auth expired
        assert_eq!(classify(200, "UNAUTHORIZED"), ErrorKind::AuthExpired);
        assert_eq!(classify(200, "Invalid Token"), ErrorKind::AuthExpired);
        assert_eq!(classify(200, "session expired"), ErrorKind::AuthExpired);
        assert_eq!(classify(200, "Token Expired"), ErrorKind::AuthExpired);
        assert_eq!(
            classify(200, "authentication failed"),
            ErrorKind::AuthExpired
        );
        // Bad request
        assert_eq!(
            classify(200, "invalid_request_error"),
            ErrorKind::BadRequest
        );
        assert_eq!(classify(500, "Bad Request"), ErrorKind::BadRequest);
        assert_eq!(
            classify(200, "Missing required field: model"),
            ErrorKind::BadRequest
        );
    }

    #[test]
    fn text_overrides_status_code() {
        assert_eq!(classify(200, "rate limit"), ErrorKind::RateLimit);
        assert_eq!(classify(200, "waiting_room"), ErrorKind::WaitingRoom);
        assert_eq!(classify(500, "unauthorized"), ErrorKind::AuthExpired);
        assert_eq!(
            classify(500, "model_not_found"),
            ErrorKind::ModelUnavailable
        );
    }

    #[test]
    fn text_rule_priority_order() {
        // When multiple keyword categories match, take the first one per rule-table order
        assert_eq!(
            classify(200, "waiting_room rate limit unauthorized"),
            ErrorKind::WaitingRoom
        );
        assert_eq!(
            classify(200, "rate limit model_not_found unauthorized"),
            ErrorKind::RateLimit
        );
        assert_eq!(
            classify(200, "model_not_found unauthorized bad request"),
            ErrorKind::ModelUnavailable
        );
        assert_eq!(
            classify(200, "unauthorized invalid_request"),
            ErrorKind::AuthExpired
        );
    }

    #[test]
    fn queue_hint_requires_429_or_503() {
        assert_eq!(classify(200, "queue position 2"), ErrorKind::Unknown);
        assert_eq!(classify(429, "queue position 2"), ErrorKind::WaitingRoom);
        assert_eq!(classify(503, "queued"), ErrorKind::WaitingRoom);
        // 200 but explicit waiting_room -> still classified as WaitingRoom
        assert_eq!(classify(200, "waiting_room_queued"), ErrorKind::WaitingRoom);
    }

    #[test]
    fn status_fallback_covers_all_branches() {
        assert_eq!(classify(429, ""), ErrorKind::RateLimit);
        assert_eq!(classify(401, ""), ErrorKind::AuthExpired);
        assert_eq!(classify(403, ""), ErrorKind::AuthExpired);
        assert_eq!(classify(404, ""), ErrorKind::ModelUnavailable);
        assert_eq!(classify(400, ""), ErrorKind::BadRequest);
        assert_eq!(classify(422, ""), ErrorKind::BadRequest);
        assert_eq!(classify(500, ""), ErrorKind::Upstream5xx);
        assert_eq!(classify(503, ""), ErrorKind::Upstream5xx);
        assert_eq!(classify(599, ""), ErrorKind::Upstream5xx);
        assert_eq!(classify(200, "ok"), ErrorKind::Unknown);
        assert_eq!(classify(418, ""), ErrorKind::Unknown);
        assert_eq!(classify(301, ""), ErrorKind::Unknown);
    }

    #[test]
    fn empty_body_with_status_zero_is_network() {
        assert_eq!(classify(0, ""), ErrorKind::Network);
        assert_eq!(classify(0, "   "), ErrorKind::Network);
    }

    #[test]
    fn retry_after_header_takes_priority_over_body() {
        assert_eq!(
            extract_retry_after(r#"{"retryAfter":5}"#, Some("30")),
            Some(30)
        );
        assert_eq!(extract_retry_after("", Some(" 30 ")), Some(30));
    }

    #[test]
    fn retry_after_body_variants() {
        assert_eq!(extract_retry_after(r#"{"retryAfter":30}"#, None), Some(30));
        assert_eq!(
            extract_retry_after(r#"{"retry_after": 12}"#, None),
            Some(12)
        );
        assert_eq!(extract_retry_after(r#"{"retry-after":"7"}"#, None), Some(7));
        assert_eq!(
            extract_retry_after("please retry_after=45 seconds later", None),
            Some(45)
        );
    }

    #[test]
    fn retry_after_absent_or_invalid_is_none() {
        assert_eq!(extract_retry_after("", None), None);
        assert_eq!(extract_retry_after("no hint here", None), None);
        // Falls back to body when the header is invalid
        assert_eq!(
            extract_retry_after(r#"{"retryAfter":9}"#, Some("not-a-number")),
            Some(9)
        );
        assert_eq!(extract_retry_after("", Some("")), None);
    }

    #[test]
    fn excerpt_prefers_message_field() {
        assert_eq!(
            error_excerpt(r#"{"error":{"message":"model not available"}}"#),
            "model not available"
        );
        assert_eq!(error_excerpt(r#"{"message":"boom"}"#), "boom");
        assert_eq!(
            error_excerpt(r#"{"error":"plain failure"}"#),
            "plain failure"
        );
        assert_eq!(
            error_excerpt(r#"{"detail":"missing required field"}"#),
            "missing required field"
        );
    }

    #[test]
    fn excerpt_extracts_from_sse_fragment() {
        let body = "event: error\ndata: {\"error\":{\"message\":\"session expired\"}}\n\n";
        assert_eq!(classify(200, body), ErrorKind::AuthExpired);
        assert_eq!(error_excerpt(body), "session expired");
    }

    #[test]
    fn excerpt_falls_back_to_raw_body_and_normalizes_whitespace() {
        assert_eq!(
            error_excerpt("upstream   exploded\nbadly"),
            "upstream exploded badly"
        );
        assert_eq!(error_excerpt(""), "");
        assert_eq!(error_excerpt("   \n  "), "");
    }

    #[test]
    fn excerpt_decodes_json_escapes() {
        assert_eq!(
            error_excerpt(r#"{"message":"line1\nline2 \"q\""}"#),
            "line1 line2 \"q\""
        );
    }

    #[test]
    fn excerpt_truncates_to_300_chars() {
        let long = "a".repeat(400);
        let body = format!(r#"{{"message":"{long}"}}"#);
        let out = error_excerpt(&body);
        assert_eq!(out.chars().count(), 300);
        assert!(out.ends_with("..."));
        // Multi-byte characters are also truncated by character count, without producing invalid UTF-8
        let zh = "中".repeat(400);
        let body = format!(r#"{{"message":"{zh}"}}"#);
        let out = error_excerpt(&body);
        assert_eq!(out.chars().count(), 300);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn excerpt_short_message_is_untouched() {
        assert_eq!(error_excerpt(r#"{"message":"short"}"#), "short");
    }

    #[test]
    fn as_str_and_serde_are_stable_snake_case() {
        let kinds = [
            (ErrorKind::RateLimit, "rate_limit"),
            (ErrorKind::WaitingRoom, "waiting_room"),
            (ErrorKind::AuthExpired, "auth_expired"),
            (ErrorKind::ModelUnavailable, "model_unavailable"),
            (ErrorKind::BadRequest, "bad_request"),
            (ErrorKind::Upstream5xx, "upstream_5xx"),
            (ErrorKind::Network, "network"),
            (ErrorKind::Unknown, "unknown"),
        ];
        for (kind, expected) in kinds {
            assert_eq!(kind.as_str(), expected);
            assert_eq!(kind.to_string(), expected);
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn retryable_and_hint_matrix_is_complete() {
        let rows = [
            (ErrorKind::RateLimit, true, Some(60)),
            (ErrorKind::WaitingRoom, true, Some(15)),
            (ErrorKind::AuthExpired, false, None),
            (ErrorKind::ModelUnavailable, false, None),
            (ErrorKind::BadRequest, false, None),
            (ErrorKind::Upstream5xx, true, Some(5)),
            (ErrorKind::Network, true, None),
            (ErrorKind::Unknown, false, None),
        ];
        for (kind, retryable, hint) in rows {
            assert_eq!(kind.is_retryable(), retryable, "{}", kind.as_str());
            assert_eq!(kind.retry_after_hint(), hint, "{}", kind.as_str());
        }
    }
}
