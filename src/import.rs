//! Token import: parses curl commands / HAR files, auto-extracts Freebuff auth tokens and stores them
//!
//! Scenario: the user "Copy as cURL"s a request from browser DevTools, or exports a HAR file,
//! and pastes it into this gateway's control panel -> it auto-extracts `authorization: Bearer <token>`
//! (only for requests to the freebuff.com / codebuff.com domains) -> appends it to the account pool and persists it.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const TARGET_HOSTS: &[&str] = &["freebuff.com", "codebuff.com", "www.codebuff.com"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedAuth {
    /// Bearer token (or falls back to the session cookie value)
    pub token: String,
    /// Extraction source: curl / har
    pub source: String,
    /// Host of the associated request
    pub host: String,
    /// Path of the associated request
    pub path: String,
    /// Method of the associated request
    pub method: String,
    /// Time added (RFC3339; None for old data that predates this field)
    #[serde(default)]
    pub added_at: Option<String>,
}

/// Extract a Bearer token from curl command text (freebuff/codebuff target domains only, to prevent importing cross-domain credentials)
pub fn parse_curl(text: &str) -> Vec<ExtractedAuth> {
    let mut out = Vec::new();
    // Extract all headers: supports `-H "name: value"`, `-H 'name: value'` (Chrome copy format), and cmd escaping `^"`
    let header_re = regex::Regex::new(
        r#"-H\s*\^?['"](?:authorization|Authorization):\s*Bearer\s+([A-Za-z0-9._-]+)"#,
    )
    .unwrap();
    // URL extraction: supports `curl 'URL'` / `curl "URL"` / `curl --url "URL"` / `curl -url "URL"`
    let url_re = regex::Regex::new(r#"curl\s+(?:--?url\s+)?\^?['"]?(https?://[^\s'"^]+)"#).unwrap();
    let method_re = regex::Regex::new(r#"(?:-X\s+|--request\s+)\^?([A-Z]+)"#).unwrap();

    let tokens: HashSet<String> = header_re
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect();
    if tokens.is_empty() {
        return out;
    }
    let url = url_re
        .captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    let host = if url.is_empty() {
        String::new()
    } else {
        parse_host(&url)
    };
    let path = if url.is_empty() {
        String::new()
    } else {
        parse_path(&url)
    };
    let method = method_re
        .captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "GET".into());
    // Let it through when the curl text can't reliably locate a host (single-user local scenario), but reject when the host is clearly a different domain
    let host_is_other_domain = !host.is_empty() && !TARGET_HOSTS.iter().any(|h| host.ends_with(h));
    if host_is_other_domain {
        tracing::warn!("curl import rejected: host={host} is not one of the target domains {TARGET_HOSTS:?}");
        return out;
    }

    for t in tokens {
        out.push(ExtractedAuth {
            token: t,
            source: "curl".into(),
            host: host.clone(),
            path: path.clone(),
            method: method.clone(),
            added_at: None, // filled in with the added time on persist
        });
    }
    out
}

#[derive(Debug, Default, Deserialize)]
struct HarRoot {
    #[serde(default)]
    log: HarLog,
}

#[derive(Debug, Default, Deserialize)]
struct HarLog {
    #[serde(default)]
    entries: Vec<HarEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct HarEntry {
    #[serde(default)]
    request: HarRequest,
}

#[derive(Debug, Default, Deserialize)]
struct HarRequest {
    #[serde(default)]
    method: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    headers: Vec<HarHeader>,
}

#[derive(Debug, Default, Deserialize)]
struct HarHeader {
    name: String,
    value: String,
}

/// Extract a Bearer token from HAR JSON (only requests to the freebuff/codebuff target domains are collected)
pub fn parse_har(json_text: &str) -> Result<Vec<ExtractedAuth>> {
    let har: HarRoot = serde_json::from_str(json_text)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in har.log.entries {
        let url_lower = entry.request.url.to_lowercase();
        // Domain check: credentials from non-target domains are always skipped (prevents cross-domain token mix-ups)
        let is_target = TARGET_HOSTS.iter().any(|h| url_lower.contains(h));
        if !is_target {
            continue;
        }
        for h in &entry.request.headers {
            if h.name.eq_ignore_ascii_case("authorization") {
                if let Some(token) = h
                    .value
                    .trim()
                    .strip_prefix("Bearer ")
                    .or_else(|| h.value.trim().strip_prefix("bearer "))
                {
                    let token = token.trim().to_string();
                    if token.len() >= 8 && !seen.contains(&token) {
                        seen.insert(token.clone());
                        out.push(ExtractedAuth {
                            token,
                            source: "har".into(),
                            host: parse_host(&entry.request.url),
                            path: parse_path(&entry.request.url),
                            method: entry.request.method.clone(),
                            added_at: None,
                        });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Stable credential identifier: FNV-1a 64-bit (pure local computation, zero dependencies, stable across Rust versions).
///
/// Not `DefaultHasher` -- its output is explicitly not guaranteed stable across versions/processes per
/// the Rust docs, but this id is persisted as the credential's durable primary key, so it must be reproducible.
pub fn cred_id(token: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for b in token.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    // Mix in the length to further reduce collision odds between different-length tokens with the same hash
    format!("{:016x}{:04x}", h, token.len().min(0xffff))
}

/// Credential type: web Cookie or Bearer token.
pub fn kind_of(token: &str) -> &'static str {
    if token.contains("session-token") {
        "web-cookie"
    } else {
        "bearer"
    }
}

fn parse_host(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
        .to_string()
}

fn parse_path(url: &str) -> String {
    // Fix: locate the part after "://" first, then take the path starting at the first '/' within it
    let start = url.find("://").map(|p| p + 3).unwrap_or(0);
    let rest = &url[start..];
    rest.find('/')
        .map(|i| rest[i..].to_string())
        .unwrap_or_default()
}

/// Credential file write lock: persist/delete/heal are all "read-modify-write the whole file",
/// which would clobber each other under concurrency (last write wins, earlier writes are lost); must be serialized.
/// An in-process lock is sufficient (single-process software).
static TOKENS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Atomic write: write to a temp file then rename, to avoid leaving a truncated tokens.json
/// if the write crashes midway (truncated = load_tokens fails to parse = all credentials unavailable).
fn atomic_write(path: &str, json: &str) -> Result<()> {
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    // On Windows, rename requires the target to not exist when overwriting; remove it first
    #[cfg(windows)]
    if p.exists() {
        let _ = std::fs::remove_file(p);
    }
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// Persist: append to data/tokens.json (with dedupe)
pub fn persist_tokens(path: &str, new_tokens: &[ExtractedAuth]) -> Result<Vec<ExtractedAuth>> {
    let _g = TOKENS_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("tokens lock poisoned"))?;
    let existing = load_tokens(path)?;
    let mut all: Vec<ExtractedAuth> = existing;
    let existing_set: HashSet<String> = all.iter().map(|t| t.token.clone()).collect();
    let now = chrono::Utc::now().to_rfc3339();
    let mut added = Vec::new();
    for t in new_tokens {
        if !existing_set.contains(&t.token) {
            let mut item = t.clone();
            if item.added_at.is_none() {
                item.added_at = Some(now.clone());
            }
            all.push(item.clone());
            added.push(item);
        }
    }
    let json = serde_json::to_string_pretty(&all)?;
    atomic_write(path, &json)?;
    Ok(added)
}

/// Read persisted tokens
pub fn load_tokens(path: &str) -> Result<Vec<ExtractedAuth>> {
    if !std::path::Path::new(path).exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    let parsed: Vec<ExtractedAuth> = serde_json::from_str(&text)?;
    Ok(parsed)
}

/// Read tokens and **heal historical data**: credentials added before the `added_at` field existed
/// have no stored time, so the panel can only show "-" and the user can't see "when it was added".
/// This backfills using the file's modified time and persists it (idempotent).
pub fn load_tokens_healed(path: &str) -> Result<Vec<ExtractedAuth>> {
    let mut tokens = load_tokens(path)?;
    if tokens.iter().all(|t| t.added_at.is_some()) {
        return Ok(tokens);
    }
    // Backfill source: file mtime (the closest reliable approximation of "first added"); falls back to the current time.
    // Note: multiple historical credentials will get the same backfilled value (roughly the last file-modified time) - an acceptable lower-bound guarantee.
    let fallback = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(chrono::DateTime::<chrono::Utc>::from)
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339());
    let mut changed = false;
    for t in tokens.iter_mut() {
        if t.added_at.is_none() {
            t.added_at = Some(fallback.clone());
            changed = true;
        }
    }
    if changed {
        let json = serde_json::to_string_pretty(&tokens)?;
        let _g = TOKENS_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("tokens lock poisoned"))?;
        atomic_write(path, &json)?;
        tracing::info!("Backfilled added time for historical credentials (source: tokens.json mtime)");
    }
    Ok(tokens)
}

/// Delete a credential by its stable id; returns whether it was deleted.
pub fn delete_token(path: &str, id: &str) -> Result<Option<ExtractedAuth>> {
    let _g = TOKENS_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("tokens lock poisoned"))?;
    let mut tokens = load_tokens(path)?;
    let before = tokens.len();
    let removed = tokens.iter().find(|t| cred_id(&t.token) == id).cloned();
    tokens.retain(|t| cred_id(&t.token) != id);
    if tokens.len() == before {
        return Ok(None);
    }
    let json = serde_json::to_string_pretty(&tokens)?;
    atomic_write(path, &json)?;
    Ok(removed)
}

/// Auto-sniff from arbitrary text: try curl first, then HAR, then a Cookie string, then a bare "Bearer xxx"
pub fn sniff_tokens(text: &str) -> Result<Vec<ExtractedAuth>> {
    if text.contains("curl") || text.contains("--url") {
        let v = parse_curl(text);
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if text.trim_start().starts_with('{') {
        if let Ok(v) = parse_har(text) {
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    // Full Cookie string (containing __Secure-next-auth.session-token etc.)
    if let Some(v) = parse_cookie(text) {
        return Ok(v);
    }
    // Bare Bearer token
    let re = regex::Regex::new(r"(?i)bearer\s+([A-Za-z0-9._-]{16,})").unwrap();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for cap in re.captures_iter(text) {
        let t = cap[1].to_string();
        if !seen.contains(&t) {
            seen.insert(t.clone());
            out.push(ExtractedAuth {
                token: t,
                source: "raw".into(),
                host: String::new(),
                path: String::new(),
                method: String::new(),
                added_at: None,
            });
        }
    }
    if out.is_empty() {
        Err(anyhow!("Could not extract any Bearer token or Cookie from the input"))
    } else {
        Ok(out)
    }
}

/// Parse a full Cookie string (web auth credential): extract the whole Cookie containing the session-token
pub fn parse_cookie(text: &str) -> Option<Vec<ExtractedAuth>> {
    // Extract the first Cookie fragment containing "__Secure-next-auth.session-token=" (may be a full string or partial)
    let re = regex::Regex::new(r#"__Secure-next-auth\.session-token=[^; \t"']+"#).unwrap();
    let csrf_re = regex::Regex::new(r#"__Host-next-auth\.csrf-token=[^; \t"']+"#).unwrap();
    let cb_re = regex::Regex::new(r#"__Secure-next-auth\.callback-url=[^; \t"']+"#).unwrap();

    let st = re.find(text)?;
    let token = st.as_str().to_string();
    // Assemble the full Cookie string: session-token + csrf + callback
    let mut parts = vec![token.clone()];
    if let Some(c) = csrf_re.find(text) {
        parts.push(c.as_str().to_string());
    }
    if let Some(c) = cb_re.find(text) {
        parts.push(c.as_str().to_string());
    }
    let cookie = parts.join("; ");
    Some(vec![ExtractedAuth {
        token: cookie,
        source: "cookie".into(),
        host: "freebuff.com".into(),
        path: "/api/web/freebuff-session".into(),
        method: "GET".into(),
        added_at: None,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_with_bearer() {
        let curl = r#"
curl --url "https://www.codebuff.com/api/v1/freebuff/session" \
  -H "accept: application/json" \
  -H "authorization: Bearer fa82b5c1-e39d-4c7a-961f-d2b3c4e5f6a7" \
  -X GET
"#;
        let out = parse_curl(curl);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].token, "fa82b5c1-e39d-4c7a-961f-d2b3c4e5f6a7");
        assert_eq!(out[0].method, "GET");
    }

    #[test]
    fn har_with_bearer() {
        let har = r#"{
  "log": {
    "entries": [{
      "request": {
        "method": "POST",
        "url": "https://www.codebuff.com/api/v1/freebuff/session",
        "headers": [
          {"name": "authorization", "value": "Bearer tok_har_test_123456"}
        ]
      }
    }]
  }
}"#;
        let out = parse_har(har).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].token, "tok_har_test_123456");
        assert_eq!(out[0].host, "www.codebuff.com");
    }

    #[test]
    fn sniff_raw_bearer() {
        let out = sniff_tokens("Authorization: Bearer abcdefghijklmnop1234567890").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "raw");
    }

    #[test]
    fn sniff_cookie() {
        let out = sniff_tokens(
            "__Secure-next-auth.session-token=abc123; __Host-next-auth.csrf-token=xyz",
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "cookie");
        assert!(out[0].token.contains("session-token"));
    }

    #[test]
    fn persist_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let path_str = path.to_str().unwrap().to_string();
        let t1 = ExtractedAuth {
            token: "t1".into(),
            source: "test".into(),
            host: "h".into(),
            path: "p".into(),
            method: "GET".into(),
            added_at: None,
        };
        let t2 = ExtractedAuth {
            token: "t2".into(),
            source: "test".into(),
            host: "h".into(),
            path: "p".into(),
            method: "GET".into(),
            added_at: None,
        };
        let added1 = persist_tokens(&path_str, &[t1.clone(), t2.clone()]).unwrap();
        assert_eq!(added1.len(), 2);
        // Writing again with t1 included should skip it
        let added2 = persist_tokens(&path_str, &[t1, t2]).unwrap();
        assert_eq!(added2.len(), 0);
        assert_eq!(load_tokens(&path_str).unwrap().len(), 2);
    }

    #[test]
    fn curl_cross_domain_rejected() {
        // curl for a non-target domain must not be imported (prevents cross-domain credential mix-ups)
        let curl = r#"
curl --url "https://evil.example.com/api/steal" \
  -H "authorization: Bearer crossdomain1234567890" \
  -X GET
"#;
        let out = parse_curl(curl);
        assert!(
            out.is_empty(),
            "cross-domain token should be rejected, but {} were imported",
            out.len()
        );
    }

    #[test]
    fn chrome_style_curl_imports_with_host() {
        // Chrome DevTools "Copy as cURL" format: single quotes + positional URL argument + backslash line continuations
        let curl = r#"curl 'https://www.codebuff.com/api/v1/chat/completions' \
  -H 'authorization: Bearer chromestyle1234567890' \
  -X POST"#;
        let out = parse_curl(curl);
        assert_eq!(out.len(), 1, "Chrome format should extract 1 token");
        assert_eq!(
            out[0].host, "www.codebuff.com",
            "host must be parsed correctly (otherwise cross-domain checking breaks)"
        );
        assert_eq!(out[0].path, "/api/v1/chat/completions");
        assert_eq!(out[0].method, "POST");
    }

    #[test]
    fn chrome_style_cross_domain_rejected() {
        // A cross-domain token in single-quote format must also be rejected
        let curl = "curl 'https://evil.example.com/steal' -H 'authorization: Bearer chromecross1234567890'";
        let out = parse_curl(curl);
        assert!(
            out.is_empty(),
            "single-quote-format cross-domain token should be rejected, but {} were found",
            out.len()
        );
    }

    #[test]
    fn har_cross_domain_rejected() {
        // Non-target-domain entries in HAR should be skipped
        let har = r#"{
  "log": {
    "entries": [{
      "request": {
        "method": "GET",
        "url": "https://evil.example.com/api/x",
        "headers": [
          {"name": "authorization", "value": "Bearer evilhar123456789"}
        ]
      }
    }]
  }
}"#;
        let out = parse_har(har).unwrap();
        assert!(out.is_empty(), "cross-domain HAR token should be rejected");
    }

    #[test]
    fn cred_id_is_stable_and_distinct() {
        // The same token must get the same id every time (the id is the durable primary key; instability would corrupt the credential list)
        let a1 = cred_id("__Secure-next-auth.session-token=abc");
        let a2 = cred_id("__Secure-next-auth.session-token=abc");
        assert_eq!(a1, a2);
        assert_ne!(a1, cred_id("__Secure-next-auth.session-token=abd"));
        // Different lengths with the same prefix must still be distinguished
        assert_ne!(cred_id("tok"), cred_id("tokx"));
    }

    #[test]
    fn kind_detects_web_cookie() {
        assert_eq!(
            kind_of("__Secure-next-auth.session-token=x; y=1"),
            "web-cookie"
        );
        assert_eq!(kind_of("sk-abcdef"), "bearer");
    }

    #[test]
    fn healed_load_backfills_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let path_str = path.to_str().unwrap().to_string();
        // Simulate data persisted by an earlier version: no added_at field
        std::fs::write(
            &path,
            r#"[{"token":"legacy-token","source":"cookie","host":"freebuff.com","path":"/p","method":"GET"}]"#,
        )
        .unwrap();

        let healed = load_tokens_healed(&path_str).unwrap();
        assert_eq!(healed.len(), 1);
        assert!(healed[0].added_at.is_some(), "historical credentials must have their added time backfilled");
        // Already persisted (reading again should not need backfilling, and the value should stay stable)
        let again = load_tokens_healed(&path_str).unwrap();
        assert_eq!(again[0].added_at, healed[0].added_at);
    }

    #[test]
    fn healed_load_is_noop_when_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let path_str = path.to_str().unwrap().to_string();
        let raw = r#"[{"token":"t","source":"cookie","host":"h","path":"p","method":"GET","added_at":"2026-01-01T00:00:00+00:00"}]"#;
        std::fs::write(&path, raw).unwrap();
        let out = load_tokens_healed(&path_str).unwrap();
        assert_eq!(
            out[0].added_at.as_deref(),
            Some("2026-01-01T00:00:00+00:00")
        );
        // File contents should not be rewritten
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }

    #[test]
    fn delete_token_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let path_str = path.to_str().unwrap().to_string();
        let t1 = ExtractedAuth {
            token: "del-me".into(),
            source: "cookie".into(),
            host: "h".into(),
            path: "p".into(),
            method: "GET".into(),
            added_at: None,
        };
        let t2 = ExtractedAuth {
            token: "keep-me".into(),
            source: "cookie".into(),
            host: "h".into(),
            path: "p".into(),
            method: "GET".into(),
            added_at: None,
        };
        persist_tokens(&path_str, &[t1, t2]).unwrap();

        let removed = delete_token(&path_str, &cred_id("del-me")).unwrap();
        assert!(removed.is_some(), "should delete successfully by id");
        let left = load_tokens(&path_str).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].token, "keep-me");
        // Deleting the same one again -> None (idempotent)
        assert!(delete_token(&path_str, &cred_id("del-me"))
            .unwrap()
            .is_none());
    }
}
