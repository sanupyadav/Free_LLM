//! Full config export/import (v0.9 §2.3): migrating to a new machine, plaintext JSON (v0.9 doesn't encrypt; encryption is backlogged)
//!
//! Export payload (the `data` of POST /api/export):
//! `{schema_version, exported_at, config(redacted), tokens, skills:[{id,enabled}], memory_enabled, note}`
//!
//! Import (POST /api/import body `{data}`):
//! 1. Validate schema_version / field types / size <=5MB
//! 2. Auto-backup before writing to `data/backup-<ts>/` (config.json / tokens.json / skills directory)
//! 3. Atomic write-back: tokens.json replaced wholesale, skill enabled states, config whitelisted fields
//! 4. **Minimal security set**: never overwrite `api_keys` / `auth_tokens` (plaintext credentials only migrate via explicit user action)

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Export schema version (used for import validation; bump and migrate when the format is upgraded)
pub const EXPORT_SCHEMA_VERSION: &str = "1";
/// Max size for the import body (5MB)
pub const MAX_IMPORT_BYTES: usize = 5 * 1024 * 1024;

/// A single skill's enabled state (collected from SkillsManager.list() at export time)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillState {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Export payload (serializes directly as the `data` field)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPayload {
    pub schema_version: String,
    pub exported_at: String,
    /// Redacted config (whitelisted fields only; api_keys/auth_tokens are never written)
    pub config: serde_json::Value,
    #[serde(default)]
    pub tokens: Vec<crate::import::ExtractedAuth>,
    #[serde(default)]
    pub skills: Vec<SkillState>,
    pub memory_enabled: bool,
    #[serde(default)]
    pub note: String,
}

/// Import result summary (returned directly as the handler's `imported` field)
#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    /// Restored entries (human-readable list)
    pub imported: Vec<String>,
    /// Entries intentionally skipped (unknown skill/security field)
    pub skipped: Vec<String>,
    /// Backup directory (Some only on success)
    pub backed_up_to: Option<String>,
    /// Number of entries written back to tokens.json
    pub tokens: usize,
    /// Number of skills whose enabled state was applied
    pub skills_toggled: usize,
    /// Whether memory_enabled was written back (None = payload didn't carry it)
    pub memory_enabled: Option<bool>,
    /// Whitelisted fields merged into config.json
    pub config_fields: Vec<String>,
}

/// Import context: all host dependencies apply_import needs (assembled by api.rs)
pub struct ImportContext<'a> {
    pub config_path: Option<String>,
    pub tokens_path: &'a str,
    pub data_dir: &'a str,
    pub skills: &'a crate::skills::SkillsManager,
    pub skills_dir: &'a str,
    pub memory_runtime_enabled: &'a std::sync::atomic::AtomicBool,
}

/// Validate the import body (`{data: ...}` or the payload directly), returns a normalized ExportPayload.
/// Only does schema/type/required-field validation, no writes.
pub fn validate_import_body(body: &serde_json::Value) -> Result<ExportPayload> {
    let raw = body.get("data").unwrap_or(body);
    if !raw.is_object() {
        return Err(anyhow!("data must be a JSON object"));
    }
    let payload: ExportPayload =
        serde_json::from_value(raw.clone()).map_err(|e| anyhow!("data field has an invalid type: {e}"))?;
    if payload.schema_version.is_empty() {
        return Err(anyhow!("schema_version is missing or invalid"));
    }
    // Version check: only accepts exports with a matching major version for now (future format upgrades migrate/reject here)
    if payload.schema_version.split('.').next().unwrap_or("")
        != EXPORT_SCHEMA_VERSION.split('.').next().unwrap_or("")
    {
        return Err(anyhow!(
            "unsupported export schema_version={} (currently supports major version {})",
            payload.schema_version,
            EXPORT_SCHEMA_VERSION
                .split('.')
                .next()
                .unwrap_or(EXPORT_SCHEMA_VERSION)
        ));
    }
    if payload.exported_at.is_empty() {
        return Err(anyhow!("exported_at is missing"));
    }
    for t in &payload.tokens {
        if t.token.trim().is_empty() {
            return Err(anyhow!("tokens contains an empty token"));
        }
    }
    Ok(payload)
}

/// Backup before import: copy config.json / tokens.json / skills directory to `data_dir/backup-<ts>/`.
/// Skipped if the target file/directory doesn't exist; only an Err if everything fails to copy (partial success is acceptable, returns actual copied items).
pub fn backup_data(
    data_dir: &str,
    config_path: Option<&str>,
    tokens_path: &str,
    skills_dir: &str,
) -> Result<(String, Vec<String>)> {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_root = Path::new(data_dir).join(format!("backup-{ts}"));
    std::fs::create_dir_all(&backup_root)?;
    let mut copied = Vec::new();
    if let Some(p) = config_path {
        if Path::new(p).exists() {
            let dest = backup_root.join("config.json");
            std::fs::copy(p, &dest)?;
            copied.push("config.json".into());
        }
    }
    if Path::new(tokens_path).exists() {
        let dest = backup_root.join("tokens.json");
        std::fs::copy(tokens_path, &dest)?;
        copied.push("tokens.json".into());
    }
    if Path::new(skills_dir).is_dir() {
        copy_dir_recursive(Path::new(skills_dir), &backup_root.join("skills"))?;
        copied.push("skills/".into());
    }
    Ok((backup_root.to_string_lossy().into_owned(), copied))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Atomic write (temp file + rename; on Windows, remove the target first before renaming to prevent truncation)
fn atomic_write(path: &Path, data: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    #[cfg(windows)]
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Write back tokens.json (wholesale replace with the imported list; failure doesn't block subsequent steps)
fn write_tokens(path: &str, tokens: &[crate::import::ExtractedAuth]) -> Result<()> {
    let json = serde_json::to_string_pretty(tokens)?;
    atomic_write(Path::new(path), &json)
}

/// Merge config.json's non-sensitive fields (memory_enabled is decided by the caller; api_keys/auth_tokens are never written).
/// Returns the names of fields actually merged. Skipped if the file doesn't exist (in-memory state is already handled by the handler).
fn merge_config_fields(
    config_path: Option<&str>,
    imported: &serde_json::Value,
) -> Result<Vec<String>> {
    let Some(p) = config_path else {
        return Ok(Vec::new());
    };
    if !Path::new(p).exists() {
        return Ok(Vec::new());
    }
    let mut root: serde_json::Value = std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !root.is_object() {
        root = serde_json::json!({});
    }
    // Whitelist: kept in sync with api.rs CONFIG_EDITABLE, never overwrites api_keys/auth_tokens
    const SAFE_FIELDS: &[&str] = &[
        "listen_addr",
        "memory_enabled",
        "token_saver",
        "skills_inject_mode",
        "max_roster_tokens",
        "thread_cleanup_interval_sec",
        "thread_max_age_hours",
        "redact_logs",
        "concurrency_free_slots",
        "concurrency_free_multi",
        "concurrency_sub_slots",
        "concurrency_sub_multi",
    ];
    let mut applied = Vec::new();
    if let Some(obj) = imported.as_object() {
        for (k, v) in obj {
            if !SAFE_FIELDS.contains(&k.as_str()) {
                continue;
            }
            if !v.is_string() && !v.is_boolean() && !v.is_number() {
                continue;
            }
            root[k.as_str()] = v.clone();
            applied.push(k.clone());
        }
    }
    if applied.is_empty() {
        return Ok(applied);
    }
    let json = serde_json::to_string_pretty(&root)?;
    atomic_write(Path::new(p), &json)?;
    Ok(applied)
}

/// Apply an import: backup -> write tokens -> skill enabled states -> config whitelisted fields + memory_enabled.
/// A failure in any write step doesn't block the rest (best-effort recovery); the caller decides whether the overall result is a failure.
pub fn apply_import(payload: &ExportPayload, ctx: &ImportContext<'_>) -> Result<ImportSummary> {
    let mut summary = ImportSummary {
        imported: Vec::new(),
        skipped: Vec::new(),
        backed_up_to: None,
        tokens: 0,
        skills_toggled: 0,
        memory_enabled: None,
        config_fields: Vec::new(),
    };

    // 1) Backup (skip the write-back if this fails -- ensure rollback is possible first)
    let (backup_dir, copied) = backup_data(
        ctx.data_dir,
        ctx.config_path.as_deref(),
        ctx.tokens_path,
        ctx.skills_dir,
    )?;
    summary.backed_up_to = Some(backup_dir.clone());
    summary.imported.push(format!(
        "Backed up to {} ({})",
        backup_dir,
        if copied.is_empty() {
            "no files needed backup".into()
        } else {
            copied.join(", ")
        }
    ));

    // 2) Wholesale replace tokens.json
    if !payload.tokens.is_empty() {
        write_tokens(ctx.tokens_path, &payload.tokens)?;
        summary.tokens = payload.tokens.len();
        summary.imported.push(format!(
            "tokens.json written with {} credentials",
            payload.tokens.len()
        ));
    } else {
        summary
            .skipped
            .push("tokens (payload empty, existing credentials not overwritten)".into());
    }

    // 3) Skill enabled states (only applies to skills that already exist on the target; never creates/deletes skill files)
    for s in &payload.skills {
        // toggle returns Ok(false) for an unknown id (not Err), so existence must be checked first
        if ctx.skills.get(&s.id).is_none() {
            summary
                .skipped
                .push(format!("skill {} (does not exist on target)", s.id));
            continue;
        }
        match ctx.skills.toggle(&s.id, s.enabled) {
            Ok(_) => {
                summary.skills_toggled += 1;
                summary.imported.push(format!(
                    "skill {} -> {}",
                    s.id,
                    if s.enabled { "enabled" } else { "disabled" }
                ));
            }
            Err(e) => {
                summary
                    .imported
                    .push(format!("skill {} left unchanged (toggle failed: {e})", s.id));
            }
        }
    }

    // 4) Config whitelisted fields + memory_enabled
    summary.config_fields = merge_config_fields(ctx.config_path.as_deref(), &payload.config)?;
    if !summary.config_fields.is_empty() {
        summary.imported.push(format!(
            "config.json merged fields: {}",
            summary.config_fields.join(", ")
        ));
    }
    summary.memory_enabled = Some(payload.memory_enabled);
    ctx.memory_runtime_enabled
        .store(payload.memory_enabled, std::sync::atomic::Ordering::Relaxed);
    let mem_json = merge_config_field_bool(
        ctx.config_path.as_deref(),
        "memory_enabled",
        payload.memory_enabled,
    )?;
    if mem_json {
        summary.config_fields.retain(|f| f != "memory_enabled");
        if !summary.config_fields.iter().any(|f| f == "memory_enabled") {
            summary.config_fields.push("memory_enabled".into());
        }
        summary.imported.push(format!(
            "memory layer set to {}",
            if payload.memory_enabled {
                "on"
            } else {
                "off"
            }
        ));
    }

    Ok(summary)
}

/// Write a single bool config field back to config.json (dedicated to memory_enabled, idempotent)
fn merge_config_field_bool(config_path: Option<&str>, key: &str, value: bool) -> Result<bool> {
    let Some(p) = config_path else {
        return Ok(true);
    };
    if !Path::new(p).exists() {
        return Ok(false);
    }
    let mut root: serde_json::Value = std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !root.is_object() {
        root = serde_json::json!({});
    }
    if root.get(key).and_then(|v| v.as_bool()) == Some(value) {
        return Ok(false); // already consistent, no rewrite needed
    }
    root[key] = serde_json::json!(value);
    let json = serde_json::to_string_pretty(&root)?;
    atomic_write(Path::new(p), &json)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> ExportPayload {
        ExportPayload {
            schema_version: EXPORT_SCHEMA_VERSION.into(),
            exported_at: "2026-09-18T00:00:00+00:00".into(),
            config: serde_json::json!({
                "memory_enabled": true,
                "token_saver": true,
                "api_keys": ["should-never-write"],
                "auth_tokens": ["should-never-write"],
            }),
            tokens: vec![
                crate::import::ExtractedAuth {
                    token: "__Secure-next-auth.session-token=ac; x=1".into(),
                    source: "cookie".into(),
                    host: "freebuff.com".into(),
                    path: "/api/web/freebuff-session".into(),
                    method: "GET".into(),
                    added_at: Some("2026-09-01T00:00:00+00:00".into()),
                },
                crate::import::ExtractedAuth {
                    token: "sk-bearer-abc123".into(),
                    source: "curl".into(),
                    host: "www.codebuff.com".into(),
                    path: "/api/v1/chat/completions".into(),
                    method: "POST".into(),
                    added_at: None,
                },
            ],
            skills: vec![
                SkillState {
                    id: "git-guru".into(),
                    enabled: false,
                },
                SkillState {
                    id: "no-such-skill".into(),
                    enabled: true,
                },
            ],
            memory_enabled: true,
            note: "round-trip test".into(),
        }
    }

    #[test]
    fn validate_accepts_our_own_export() {
        let body = serde_json::json!({ "data": sample_payload() });
        let payload = validate_import_body(&body).unwrap();
        assert_eq!(payload.schema_version, EXPORT_SCHEMA_VERSION);
        assert_eq!(payload.tokens.len(), 2);
    }

    #[test]
    fn validate_rejects_bad_schema_and_types() {
        assert!(validate_import_body(&serde_json::json!({})).is_err());
        assert!(validate_import_body(&serde_json::json!({ "data": "x" })).is_err());
        assert!(validate_import_body(&serde_json::json!({
            "data": { "schema_version": "9", "exported_at": "t", "config": {}, "tokens": [], "skills": [], "memory_enabled": false }
        })).is_err(), "mismatched major version should be rejected");
        assert!(validate_import_body(&serde_json::json!({
            "data": { "schema_version": EXPORT_SCHEMA_VERSION, "exported_at": "", "config": {}, "tokens": [], "skills": [], "memory_enabled": false }
        })).is_err(), "missing exported_at should be rejected");
    }

    #[test]
    fn round_trip_apply_restores_tokens_skills_and_memory() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_str().unwrap().to_string();
        let tokens_path = p("tokens.json");
        let data_dir = p("data");
        let skills_dir = p("skills");
        let skills_db = p("skills.sqlite");

        // Target: skills library already seeded (git-guru enabled by default), config.json carries api_keys
        let manager = crate::skills::SkillsManager::open(
            std::path::PathBuf::from(&skills_dir),
            std::path::PathBuf::from(&skills_db),
        )
        .unwrap();
        let config_path = p("config.json");
        std::fs::write(
            &config_path,
            r#"{ "api_keys": ["sk-original"], "auth_tokens": ["tok-original"], "token_saver": false }"#,
        )
        .unwrap();

        let runtime = std::sync::atomic::AtomicBool::new(false);
        let payload = sample_payload();
        let ctx = ImportContext {
            config_path: Some(config_path.clone()),
            tokens_path: &tokens_path,
            data_dir: &data_dir,
            skills: &manager,
            skills_dir: &skills_dir,
            memory_runtime_enabled: &runtime,
        };
        let summary = apply_import(&payload, &ctx).unwrap();

        // tokens restored
        let written: Vec<crate::import::ExtractedAuth> =
            serde_json::from_str(&std::fs::read_to_string(&tokens_path).unwrap()).unwrap();
        assert_eq!(written.len(), 2);
        assert!(written[0].token.contains("session-token"));
        // skill enabled state: git-guru disabled
        let git = manager.get("git-guru").unwrap();
        assert!(!git.enabled, "git-guru should be disabled after import");
        // unknown skill skipped
        assert!(summary.skipped.iter().any(|s| s.contains("no-such-skill")));
        // memory_enabled: config.json + runtime
        assert!(runtime.load(std::sync::atomic::Ordering::Relaxed));
        let cfg_text = std::fs::read_to_string(&config_path).unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&cfg_text).unwrap();
        assert_eq!(cfg["memory_enabled"], true);
        assert_eq!(cfg["api_keys"][0], "sk-original", "api_keys must never be overwritten");
        assert_eq!(
            cfg["auth_tokens"][0], "tok-original",
            "auth_tokens must never be overwritten"
        );
        // backup exists
        let backup_dir = summary.backed_up_to.as_ref().unwrap();
        assert!(Path::new(backup_dir).join("config.json").exists());
        assert!(summary.imported.iter().any(|i| i.contains("tokens.json")));
    }

    #[test]
    fn limited_size_guard_constant() {
        assert_eq!(MAX_IMPORT_BYTES, 5 * 1024 * 1024);
    }
}
