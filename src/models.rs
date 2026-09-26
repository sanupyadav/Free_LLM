//! Model registry: upstream free-agents.ts fetch + hardcoded authoritative baseline
//!
//! Reverse-engineered from upstream (Freebuff-0.0.98 orchestrator.js):
//! - SUPPORTED_FREEBUFF_MODELS list
//! - Default free model z-ai/glm-5.3-flash
//! - Actual availability is determined per-account by rateLimitsByModel

use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Hardcoded authoritative baseline (verified working upstream)
pub const ROOT_AGENT_ID: &str = "base2-free";

pub const HARDCODED_MODELS: &[&str] = &[
    "z-ai/glm-5.3-flash",
    "google/gemini-3.8-flash",
    "google/gemini-3.1-flash-lite",
    "google/gemini-3.5-flash-lite",
    "deepseek/deepseek-v4-flash",
    "deepseek/deepseek-v4-flash-max",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-pro-max",
    "minimax/minimax-m3",
    "openai/gpt-5.6-luna",
    "openai/gpt-5.6-luna-es",
    "openai/gpt-5.6-luna-max",
    "upstage/solar-pro4",
    "meta/muse-spark-1.2-contributor",
    "meta/muse-spark-1.3-contributor",
    "anthropic/claude-fable-5",
    "stealth/ox-alpha",
    "crof/kimi-k3-eco",
    "z-ai/glm-5.2",
    "mimo/mimo-v2.5",
];

/// Sub-agent mapping (run-level)
pub const SUB_AGENTS: &[(&str, &str)] = &[
    ("file-picker", "google/gemini-3.1-flash-lite"),
    ("researcher-web", "google/gemini-3.8-flash"),
    ("basher", "google/gemini-3.8-flash"),
    ("browser-use", "google/gemini-3.8-flash"),
];

/// Default model
pub const DEFAULT_MODEL: &str = "z-ai/glm-5.3-flash";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub agent: String,
    pub premium: bool,
}

#[derive(Debug)]
pub struct ModelRegistry {
    inner: Arc<RwLock<RegistryInner>>,
    /// Runtime policy overrides (std Mutex, short critical section; consumed synchronously by meta_for/routing to avoid blocking async)
    overrides: std::sync::Mutex<HashMap<String, MetaOverride>>,
}

impl Clone for ModelRegistry {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            overrides: std::sync::Mutex::new(
                self.overrides.lock().map(|g| g.clone()).unwrap_or_default(),
            ),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RegistryInner {
    /// model -> agent
    model_to_agent: HashMap<String, String>,
    /// agent -> models
    agent_models: HashMap<String, Vec<String>>,
    all_models: Vec<String>,
    updated_at: Option<String>,
}

/// Model metadata (authoritative static table, aligned with upstream freebuff-models.ts current snapshot)
///
/// Field sources:
/// - `available`: upstream `FREEBUFF_PAUSED_FREE_MODEL_IDS` (paused/removed from free mode = false)
/// - `efforts`: upstream per-model reasoningEffort ladder (None = thinking tiers unsupported, caller should strip the field)
/// - `multimodal`: upstream per-model multimodal flag (only a hint for panel image-upload availability, not enforced)
/// - `fallback`: upstream unavailableFallback; if absent, our own product choice (noted in comments)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMeta {
    pub id: String,
    pub agent: String,
    pub premium: bool,
    pub multimodal: bool,
    /// Whether it can be used in free mode
    pub available: bool,
    /// Supported reasoning_effort ladder (None = unsupported)
    pub efforts: Option<Vec<String>>,
    /// Fallback model when unavailable
    pub fallback: Option<String>,
    /// Upstream availability policy (always / deployment_hours / off_peak_only)
    pub availability: String,
    /// Estimated recovery time when unavailable (ISO8601 UTC; deployment_hours/unknown never fabricated -> None)
    pub available_at: Option<String>,
}

struct MetaRow {
    id: &'static str,
    agent: &'static str,
    premium: bool,
    multimodal: bool,
    available: bool,
    availability: &'static str,
    efforts: Option<&'static [&'static str]>,
    fallback: Option<&'static str>,
}

const EFFORTS_GLM: &[&str] = &["low", "high", "max"];
const EFFORTS_FULL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const EFFORTS_MUSE: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];

/// Static authoritative metadata table (covers every entry in HARDCODED_MODELS)
const MODEL_META_ROWS: &[MetaRow] = &[
    // -- Available in free mode --
    MetaRow {
        id: "z-ai/glm-5.3-flash",
        agent: ROOT_AGENT_ID,
        premium: false,
        multimodal: true,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_GLM),
        fallback: None,
    },
    MetaRow {
        id: "google/gemini-3.1-flash-lite",
        agent: "file-picker",
        premium: false,
        multimodal: true,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_FULL),
        fallback: None,
    },
    MetaRow {
        id: "google/gemini-3.5-flash-lite",
        agent: ROOT_AGENT_ID,
        premium: false,
        multimodal: true,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_FULL),
        fallback: None,
    },
    MetaRow {
        id: "deepseek/deepseek-v4-flash",
        agent: ROOT_AGENT_ID,
        premium: false,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_GLM),
        fallback: Some("openai/gpt-5.6-luna"),
    },
    MetaRow {
        id: "deepseek/deepseek-v4-flash-max",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_GLM),
        fallback: None,
    },
    MetaRow {
        id: "deepseek/deepseek-v4-pro-max",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_GLM),
        fallback: None,
    },
    MetaRow {
        id: "openai/gpt-5.6-luna",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: true,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_FULL),
        fallback: None,
    },
    MetaRow {
        id: "openai/gpt-5.6-luna-es",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_FULL),
        fallback: None,
    },
    MetaRow {
        id: "openai/gpt-5.6-luna-max",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_FULL),
        fallback: None,
    },
    MetaRow {
        id: "upstage/solar-pro4",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: None,
        fallback: None,
    },
    MetaRow {
        id: "meta/muse-spark-1.2-contributor",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_MUSE),
        fallback: Some("deepseek/deepseek-v4-flash"),
    },
    MetaRow {
        id: "anthropic/claude-fable-5",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: true,
        available: true,
        availability: "always",
        efforts: Some(EFFORTS_FULL),
        fallback: None,
    },
    MetaRow {
        id: "crof/kimi-k3-eco",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: true,
        availability: "always",
        efforts: None,
        fallback: None,
    },
    // -- Paused/removed upstream (FREEBUFF_PAUSED_FREE_MODEL_IDS); keep recognizable and give a fallback --
    MetaRow {
        id: "google/gemini-3.8-flash",
        agent: "researcher-web",
        premium: true,
        multimodal: true,
        available: false,
        availability: "always",
        efforts: Some(EFFORTS_FULL),
        fallback: Some("google/gemini-3.1-flash-lite"),
    },
    MetaRow {
        id: "deepseek/deepseek-v4-pro",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: false,
        availability: "always",
        efforts: Some(EFFORTS_GLM),
        fallback: Some("z-ai/glm-5.3-flash"),
    },
    MetaRow {
        id: "minimax/minimax-m3",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: true,
        available: false,
        availability: "always",
        efforts: None,
        fallback: Some("z-ai/glm-5.3-flash"),
    },
    MetaRow {
        id: "meta/muse-spark-1.3-contributor",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: false,
        availability: "always",
        efforts: Some(EFFORTS_MUSE),
        fallback: Some("deepseek/deepseek-v4-flash"),
    },
    MetaRow {
        id: "stealth/ox-alpha",
        agent: ROOT_AGENT_ID,
        premium: false,
        multimodal: true,
        available: false,
        availability: "always",
        efforts: Some(EFFORTS_GLM),
        fallback: Some("z-ai/glm-5.3-flash"),
    },
    MetaRow {
        id: "z-ai/glm-5.2",
        agent: ROOT_AGENT_ID,
        premium: true,
        multimodal: false,
        available: false,
        availability: "always",
        efforts: None,
        fallback: Some("z-ai/glm-5.3-flash"),
    },
    MetaRow {
        id: "mimo/mimo-v2.5",
        agent: ROOT_AGENT_ID,
        premium: false,
        multimodal: true,
        available: true,
        availability: "always",
        efforts: None,
        fallback: None,
    },
];

/// DeepSeek expensive window (per upstream freebuff-models.ts comment): 00:00-10:00 UTC, half-open interval [0,10)
pub const DEEPSEEK_EXPENSIVE_WINDOW_UTC: (u32, u32) = (0, 10);

/// The DeepSeek expensive window does not apply on weekends in Beijing time (UTC+8) (upstream comment semantics)
fn is_beijing_weekend(now: DateTime<Utc>) -> bool {
    let bj = now + chrono::Duration::hours(8);
    matches!(bj.weekday(), Weekday::Sat | Weekday::Sun)
}

/// Whether we're currently in the DeepSeek expensive window (UTC; exempt on Beijing weekends)
pub fn is_deepseek_expensive_window(now: DateTime<Utc>) -> bool {
    if is_beijing_weekend(now) {
        return false;
    }
    let h = now.hour();
    h >= DEEPSEEK_EXPENSIVE_WINDOW_UTC.0 && h < DEEPSEEK_EXPENSIVE_WINDOW_UTC.1
}

/// When the expensive window ends (same UTC day 10:00:00, window never crosses midnight -- consistent with upstream comment)
pub fn deepseek_expensive_window_ends_at(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(10, 0, 0)
        .map(|d| d.and_utc())
        .unwrap_or(now)
}

/// availability policy -> whether available within the window at a given time
///
/// - always -> true
/// - off_peak_only -> !is_deepseek_expensive_window
/// - deployment_hours -> true (upstream ops window; the gateway can't compute it precisely, so it
///   never rejects on this basis and never fabricates availableAt, matching upstream freebuffModelUnavailableAt behavior)
/// - other -> false (unknown policy conservatively rejects)
pub fn availability_now(availability: &str, now: DateTime<Utc>) -> bool {
    match availability {
        "always" => true,
        "off_peak_only" => !is_deepseek_expensive_window(now),
        "deployment_hours" => true,
        // Audit L2: unknown policy is not over-rejected (treated as always), avoiding silently disabling
        // a model when upstream adds a new policy value
        _ => true,
    }
}

/// Runtime policy override (obtained from upstream snapshot sync; only overrides explicitly provided fields, keeps existing values)
#[derive(Debug, Clone, Default)]
struct MetaOverride {
    availability: Option<String>,
    premium: Option<bool>,
    multimodal: Option<bool>,
    /// Some(Some(ladder)) / Some(None) = explicitly no ladder / None = not declared (keep)
    efforts: Option<Option<Vec<String>>>,
    fallback: Option<Option<String>>,
}

/// Single row of the upstream snapshot (camelCase, aligned with fixture)
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotModelMeta {
    id: String,
    #[serde(default)]
    availability: Option<String>,
    #[serde(default)]
    premium: Option<bool>,
    #[serde(default)]
    multimodal: Option<bool>,
    #[serde(default)]
    efforts: Option<Option<Vec<String>>>,
    #[serde(default)]
    unavailable_fallback: Option<Option<String>>,
    #[serde(default)]
    #[allow(dead_code)] // Snapshot experimental flag, not yet consumed by the policy layer
    experimental: Option<bool>,
    /// false = gateway self-managed/registry row (not part of policy override)
    #[serde(default)]
    catalog: bool,
}

/// Upstream snapshot file (_source/_vended_at are metadata)
#[derive(Debug, serde::Deserialize)]
struct SnapshotFile {
    #[serde(rename = "_source")]
    #[allow(dead_code)]
    source: String,
    #[serde(rename = "_vended_at")]
    #[allow(dead_code)]
    vended_at: String,
    models: Vec<SnapshotModelMeta>,
}

fn override_from_snapshot(r: &SnapshotModelMeta) -> MetaOverride {
    MetaOverride {
        availability: r.availability.clone(),
        premium: r.premium,
        multimodal: r.multimodal,
        efforts: r.efforts.clone(),
        fallback: r.unavailable_fallback.clone(),
    }
}

/// Merge two override layers (patch's Some fields override base)
fn merge_override(base: &MetaOverride, patch: &MetaOverride) -> MetaOverride {
    MetaOverride {
        availability: patch
            .availability
            .clone()
            .or_else(|| base.availability.clone()),
        premium: patch.premium.or(base.premium),
        multimodal: patch.multimodal.or(base.multimodal),
        efforts: patch.efforts.clone().or_else(|| base.efforts.clone()),
        fallback: patch.fallback.clone().or_else(|| base.fallback.clone()),
    }
}

/// Static row + runtime override + given time -> final metadata
fn merge_into_meta(
    id: &str,
    static_row: Option<&MetaRow>,
    over: Option<&MetaOverride>,
    now: DateTime<Utc>,
) -> Option<ModelMeta> {
    let s = static_row;
    let o = over;
    let availability = o
        .and_then(|x| x.availability.clone())
        .or_else(|| s.map(|r| r.availability.to_string()))
        .unwrap_or_else(|| "always".to_string());
    let premium = o
        .and_then(|x| x.premium)
        .or_else(|| s.map(|r| r.premium))
        .unwrap_or(false);
    let multimodal = o
        .and_then(|x| x.multimodal)
        .or_else(|| s.map(|r| r.multimodal))
        .unwrap_or(false);
    let efforts: Option<Vec<String>> = match o.and_then(|x| x.efforts.clone()) {
        Some(inner) => inner,
        None => s
            .and_then(|r| r.efforts)
            .map(|e| e.iter().map(|x| x.to_string()).collect()),
    };
    let fallback: Option<String> = match o.and_then(|x| x.fallback.clone()) {
        Some(inner) => inner,
        None => s.and_then(|r| r.fallback).map(|x| x.to_string()),
    };
    let static_available = s.map(|r| r.available).unwrap_or(true);
    let window_ok = availability_now(&availability, now);
    let available = static_available && window_ok;
    // Audit M1: only give availableAt when "unavailable due to the time window" (not a static pause);
    // paused models never get a fabricated recovery time
    let available_at = if static_available && !window_ok && availability == "off_peak_only" {
        Some(deepseek_expensive_window_ends_at(now).to_rfc3339())
    } else {
        None
    };
    Some(ModelMeta {
        id: id.to_string(),
        agent: s
            .map(|r| r.agent.to_string())
            .unwrap_or_else(|| ROOT_AGENT_ID.to_string()),
        premium,
        multimodal,
        available,
        efforts,
        fallback,
        availability,
        available_at,
    })
}

/// Whether policy fields are equal (ignores time-derived available/available_at, used for idempotency checks)
fn strategy_eq(a: &ModelMeta, b: &ModelMeta) -> bool {
    a.availability == b.availability
        && a.premium == b.premium
        && a.multimodal == b.multimodal
        && a.efforts == b.efforts
        && a.fallback == b.fallback
}

/// Read the local snapshot: FREE_MODELS_SNAPSHOT env var > cwd/tests/fixtures > compile-time embedded
pub fn load_local_snapshot() -> Option<String> {
    if let Ok(p) = std::env::var("FREE_MODELS_SNAPSHOT") {
        if let Ok(s) = std::fs::read_to_string(&p) {
            return Some(s);
        }
    }
    if let Ok(s) = std::fs::read_to_string("tests/fixtures/freebuff-models.snapshot.json") {
        return Some(s);
    }
    Some(include_str!("../tests/fixtures/freebuff-models.snapshot.json").to_string())
}

impl ModelRegistry {
    pub fn new() -> Self {
        let inner = RegistryInner::default();
        Self {
            inner: Arc::new(RwLock::new(inner)),
            overrides: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Initialize with the hardcoded baseline
    pub async fn init(&self) {
        let mut inner = self.inner.write().await;
        for m in HARDCODED_MODELS {
            inner
                .model_to_agent
                .entry(m.to_string())
                .or_insert_with(|| ROOT_AGENT_ID.to_string());
        }
        inner
            .agent_models
            .entry(ROOT_AGENT_ID.to_string())
            .or_insert_with(|| HARDCODED_MODELS.iter().map(|s| s.to_string()).collect());
        for (agent, model) in SUB_AGENTS {
            inner
                .model_to_agent
                .entry(model.to_string())
                .or_insert_with(|| agent.to_string());
            inner
                .agent_models
                .entry(agent.to_string())
                .or_insert_with(|| vec![model.to_string()]);
        }
        let mut all: Vec<String> = inner.model_to_agent.keys().cloned().collect();
        all.sort();
        inner.all_models = all;
        inner.updated_at = Some(now_iso());
    }

    /// Fetch incremental additions from upstream free-agents.ts
    pub async fn refresh_from_upstream(
        &self,
        client: &reqwest::Client,
    ) -> Result<(usize, usize), anyhow::Error> {
        const SRC: &str = "https://raw.githubusercontent.com/CodebuffAI/codebuff/main/common/src/constants/free-agents.ts";
        let resp = client.get(SRC).send().await?;
        if !resp.status().is_success() {
            return Ok((0, 0));
        }
        let text = resp.text().await?;
        let parsed = parse_free_agents(&text);
        if parsed.is_empty() {
            return Ok((0, 0));
        }
        let mut inner = self.inner.write().await;
        let mut added = 0;
        let mut removed = 0;
        // Merge hardcoded baseline (authoritative) + latest upstream parse
        let mut merged = hardcoded_fallback_map();
        for (agent, models) in parsed {
            merged.entry(agent).or_default().extend(models);
        }
        // Rebuild model->agent: upstream is authoritative; remove anything neither upstream nor hardcoded
        let mut new_model_to_agent = std::collections::HashMap::new();
        for (agent, models) in &merged {
            for model in models {
                if !new_model_to_agent.contains_key(model) {
                    new_model_to_agent.insert(model.clone(), agent.clone());
                }
            }
        }
        // Compute additions/removals
        for m in new_model_to_agent.keys() {
            if !inner.model_to_agent.contains_key(m) {
                added += 1;
            }
        }
        for m in inner.model_to_agent.keys() {
            if !new_model_to_agent.contains_key(m) {
                removed += 1;
                tracing::warn!("Model {m} was removed upstream and is not in the hardcoded baseline; delisting from the registry");
            }
        }
        inner.model_to_agent = new_model_to_agent;
        inner.agent_models = merged;
        let mut all: Vec<String> = inner.model_to_agent.keys().cloned().collect();
        all.sort();
        inner.all_models = all;
        inner.updated_at = Some(now_iso());
        Ok((added, removed))
    }

    pub async fn has_model(&self, model: &str) -> bool {
        self.inner.read().await.model_to_agent.contains_key(model)
    }

    pub async fn agent_for(&self, model: &str) -> Option<String> {
        self.inner.read().await.model_to_agent.get(model).cloned()
    }

    pub async fn models(&self) -> Vec<String> {
        self.inner.read().await.all_models.clone()
    }

    /// Synchronous read (for non-async contexts, e.g. route resolution)
    pub fn models_sync(&self) -> Vec<String> {
        // Fallback: take from the hardcoded list
        HARDCODED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    /// Synchronous read of the model metadata snapshot (time-aware; consumed by /v1/models)
    pub fn meta_snapshot(&self) -> Vec<ModelMeta> {
        self.meta_snapshot_at(Utc::now())
    }

    /// Metadata snapshot at a given time (for tests and window assertions)
    pub fn meta_snapshot_at(&self, now: DateTime<Utc>) -> Vec<ModelMeta> {
        let over = self.overrides.lock().map(|g| g.clone()).unwrap_or_default();
        let mut out: Vec<ModelMeta> = Vec::with_capacity(MODEL_META_ROWS.len() + 2);
        for r in MODEL_META_ROWS {
            if let Some(m) = merge_into_meta(r.id, Some(r), over.get(r.id), now) {
                out.push(m);
            }
        }
        let mut extra: Vec<ModelMeta> = Vec::new();
        for (id, o) in &over {
            if !MODEL_META_ROWS.iter().any(|r| r.id == id) {
                if let Some(m) = merge_into_meta(id, None, Some(o), now) {
                    extra.push(m);
                }
            }
        }
        // Audit NIT1: sort out-of-table rows by id for deterministic output
        extra.sort_by(|a, b| a.id.cmp(&b.id));
        out.extend(extra);
        out
    }

    /// Synchronous read of a single model's metadata (time-aware; unknown model returns None)
    pub fn meta_for(&self, id: &str) -> Option<ModelMeta> {
        self.meta_for_at(id, Utc::now())
    }

    /// A single model's metadata at a given time (for tests and window assertions)
    pub fn meta_for_at(&self, id: &str, now: DateTime<Utc>) -> Option<ModelMeta> {
        let s = MODEL_META_ROWS.iter().find(|r| r.id == id);
        let over = self.overrides.lock().ok().and_then(|g| g.get(id).cloned());
        if s.is_none() && over.is_none() {
            return None;
        }
        merge_into_meta(id, s, over.as_ref(), now)
    }

    /// Synchronous read of a model's efforts ladder ('static slice, for sync routing contexts)
    pub fn efforts_static(&self, id: &str) -> Option<&'static [&'static str]> {
        MODEL_META_ROWS
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.efforts)
    }

    /// Whether the model is currently available (static pause && time window; unknown models default to available, avoids false negatives for upstream dynamic additions)
    pub fn model_available(&self, id: &str) -> bool {
        self.model_available_at(id, Utc::now())
    }

    /// Availability at a given time (for time-aware routing)
    pub fn model_available_at(&self, id: &str, now: DateTime<Utc>) -> bool {
        self.meta_for_at(id, now)
            .map(|m| m.available)
            .unwrap_or(true)
    }

    /// Whether policy metadata exists (static table or upstream snapshot override); false = "not policy-verified"
    pub fn is_known(&self, id: &str) -> bool {
        MODEL_META_ROWS.iter().any(|r| r.id == id)
            || self
                .overrides
                .lock()
                .map(|g| g.contains_key(id))
                .unwrap_or(false)
    }

    /// Parse the upstream snapshot JSON and merge policy overrides (new models / updated availability/efforts/fallback; never removes hardcoded entries)
    ///
    /// Returns (added, updated); idempotent: reapplying the same snapshot returns (0, 0).
    /// On failure (bad JSON) returns Err; the caller silently falls back to the static baseline and warns.
    pub fn refresh_strategy_from_snapshot(&self, json: &str) -> anyhow::Result<(usize, usize)> {
        let file: SnapshotFile =
            serde_json::from_str(json).map_err(|e| anyhow::anyhow!("Failed to parse upstream model snapshot: {e}"))?;
        let now = Utc::now();
        let mut guard = self
            .overrides
            .lock()
            .map_err(|_| anyhow::anyhow!("Policy override table lock poisoned"))?;
        let mut added = 0usize;
        let mut updated = 0usize;
        for row in &file.models {
            if !row.catalog {
                continue;
            }
            let static_row = MODEL_META_ROWS.iter().find(|r| r.id == row.id);
            let patch = override_from_snapshot(row);
            // Audit NIT3: rows with no policy fields at all are not written as an override
            // (avoids treating "an empty override" as "known")
            if patch.availability.is_none()
                && patch.premium.is_none()
                && patch.multimodal.is_none()
                && patch.efforts.is_none()
                && patch.fallback.is_none()
            {
                continue;
            }
            let prev_override = guard.get(&row.id).cloned();
            let prev = merge_into_meta(&row.id, static_row, prev_override.as_ref(), now);
            let merged = match &prev_override {
                Some(b) => merge_override(b, &patch),
                None => patch,
            };
            let next = merge_into_meta(&row.id, static_row, Some(&merged), now);
            if static_row.is_none() && prev_override.is_none() {
                added += 1;
            } else if let (Some(a), Some(b)) = (prev, next) {
                if !strategy_eq(&a, &b) {
                    updated += 1;
                }
            }
            guard.insert(row.id.clone(), merged);
        }
        Ok((added, updated))
    }

    pub async fn snapshot(&self) -> ModelRegistrySnapshot {
        let inner = self.inner.read().await;
        ModelRegistrySnapshot {
            model_count: inner.model_to_agent.len(),
            agent_count: inner.agent_models.len(),
            all_models: inner.all_models.clone(),
            updated_at: inner.updated_at.clone(),
        }
    }
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRegistrySnapshot {
    pub model_count: usize,
    pub agent_count: usize,
    pub all_models: Vec<String>,
    pub updated_at: Option<String>,
}

/// Hardcoded baseline map (authoritative, doesn't disappear when upstream does)
fn hardcoded_fallback_map() -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    map.insert(
        ROOT_AGENT_ID.to_string(),
        HARDCODED_MODELS.iter().map(|s| s.to_string()).collect(),
    );
    for (agent, model) in SUB_AGENTS {
        map.entry(agent.to_string())
            .or_default()
            .push(model.to_string());
    }
    map
}

/// Parse the agent->models mapping from upstream free-agents.ts
pub fn parse_free_agents(source: &str) -> HashMap<String, Vec<String>> {
    // Supports three shapes: new Set([...]) / array [...] / constant reference (unparseable, skipped)
    let block = Regex::new(r"'([^']+)':\s*(?:new\s+Set\(\s*)?\[([^\]]*)\]").unwrap();
    let model = Regex::new(r"'([^']+)'").unwrap();
    let mut result = HashMap::new();
    for cap in block.captures_iter(source) {
        let agent = cap[1].to_string();
        let models_str = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let models: Vec<String> = model
            .captures_iter(models_str)
            .map(|m| m[1].to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !models.is_empty() {
            result.insert(agent, models);
        }
    }
    result
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_table_covers_all_hardcoded_models() {
        let meta = MODEL_META_ROWS.iter().map(|r| r.id).collect::<Vec<_>>();
        for m in HARDCODED_MODELS {
            assert!(meta.contains(m), "missing metadata: {m}");
        }
    }

    #[test]
    fn meta_for_known_and_unknown() {
        let reg = ModelRegistry::new();
        assert!(reg.meta_for("z-ai/glm-5.3-flash").is_some());
        assert!(reg.meta_for("no/such-model").is_none());
        let m = reg.meta_for("z-ai/glm-5.3-flash").unwrap();
        assert!(m.available);
        assert_eq!(
            m.efforts.as_deref(),
            Some(&["low".to_string(), "high".to_string(), "max".to_string()][..])
        );
    }

    #[test]
    fn paused_models_unavailable_only() {
        let reg = ModelRegistry::new();
        let paused = [
            "google/gemini-3.8-flash",
            "deepseek/deepseek-v4-pro",
            "minimax/minimax-m3",
            "meta/muse-spark-1.3-contributor",
            "stealth/ox-alpha",
            "z-ai/glm-5.2",
        ];
        for id in paused {
            assert!(!reg.model_available(id), "{id} should be unavailable");
        }
        for id in [
            "z-ai/glm-5.3-flash",
            "deepseek/deepseek-v4-flash",
            "openai/gpt-5.6-luna",
        ] {
            assert!(reg.model_available(id), "{id} should be available");
        }
        assert!(reg.model_available("unknown/x"), "unknown models default to available");
    }

    #[test]
    fn meta_snapshot_serializes_fields() {
        let reg = ModelRegistry::new();
        let snap = reg.meta_snapshot();
        assert_eq!(snap.len(), MODEL_META_ROWS.len());
        let json = serde_json::to_value(&snap).unwrap();
        assert!(json.is_array());
        let first = &json[0];
        for k in [
            "id",
            "agent",
            "premium",
            "multimodal",
            "available",
            "efforts",
            "fallback",
        ] {
            assert!(first.get(k).is_some(), "missing field {k}");
        }
    }

    #[test]
    fn availability_window_inside_weekday_false() {
        // 2026-09-16 is a Wednesday; 03:00 UTC is inside the [00:00,10:00) expensive window
        let t = DateTime::parse_from_rfc3339("2026-09-16T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(is_deepseek_expensive_window(t));
        assert!(!availability_now("off_peak_only", t));
        let m = ModelRegistry::new().meta_for_at("z-ai/glm-5.3-flash", t);
        assert!(m.is_some());
    }

    #[test]
    fn availability_window_outside_weekday_true() {
        let t = DateTime::parse_from_rfc3339("2026-09-16T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!is_deepseek_expensive_window(t));
        assert!(availability_now("off_peak_only", t));
    }

    #[test]
    fn availability_window_boundary_midnight_in_window() {
        // 00:00 is inside the expensive window (half-open [00:00, 10:00))
        let t = DateTime::parse_from_rfc3339("2026-09-16T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(is_deepseek_expensive_window(t));
    }

    #[test]
    fn availability_window_boundary_10_00_exclusive() {
        // 10:00 is not in the expensive window (half-open)
        let t = DateTime::parse_from_rfc3339("2026-09-16T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!is_deepseek_expensive_window(t));
    }

    #[test]
    fn availability_window_beijing_weekend_exempt() {
        // 2026-09-19 is a Saturday; 03:00 UTC = 11:00 Saturday Beijing time -> exempt from the expensive window
        let t = DateTime::parse_from_rfc3339("2026-09-19T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!is_deepseek_expensive_window(t));
        assert!(availability_now("off_peak_only", t));
    }

    #[test]
    fn availability_deployment_hours_true_unknown_lenient() {
        let t = DateTime::parse_from_rfc3339("2026-09-16T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            availability_now("deployment_hours", t),
            "deployment_hours never rejects on this basis"
        );
        assert!(
            availability_now("mystery_policy", t),
            "unknown policy treated as available (not over-rejected)"
        );
    }

    #[test]
    fn off_peak_override_drives_available_and_available_at() {
        // Override glm-5.3's availability to off_peak_only via a snapshot, and verify window/recovery time
        let reg = ModelRegistry::new();
        let snap = r#"{"_source":"t","_vended_at":"2026-09-19","models":[{"id":"z-ai/glm-5.3-flash","availability":"off_peak_only","catalog":true}]}"#;
        let (added, updated) = reg.refresh_strategy_from_snapshot(snap).unwrap();
        assert_eq!((added, updated), (0, 1), "static row update = 1");
        let in_win = DateTime::parse_from_rfc3339("2026-09-16T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let out_win = DateTime::parse_from_rfc3339("2026-09-16T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let m_in = reg.meta_for_at("z-ai/glm-5.3-flash", in_win).unwrap();
        assert!(!m_in.available, "unavailable inside the window");
        let aa = m_in
            .available_at
            .as_deref()
            .expect("should give availableAt inside the off_peak window");
        assert!(
            DateTime::parse_from_rfc3339(aa).is_ok(),
            "availableAt is parseable: {aa}"
        );
        let m_out = reg.meta_for_at("z-ai/glm-5.3-flash", out_win).unwrap();
        assert!(m_out.available, "available outside the window");
        assert!(m_out.available_at.is_none(), "no availableAt outside the window");
    }

    #[test]
    fn refresh_snapshot_merges_new_and_is_idempotent() {
        let reg = ModelRegistry::new();
        let snap = r#"{"_source":"t","_vended_at":"2026-09-19","models":[
            {"id":"fake/new-model-a","availability":"always","premium":true,"multimodal":false,"catalog":true},
            {"id":"z-ai/glm-5.3-flash","efforts":["low","high","max"],"catalog":true}
        ]}"#;
        let (a1, u1) = reg.refresh_strategy_from_snapshot(snap).unwrap();
        assert_eq!((a1, u1), (1, 0), "1 added, static row unchanged");
        assert!(
            reg.meta_for("fake/new-model-a").is_some(),
            "new model enters meta"
        );
        assert!(reg.is_known("fake/new-model-a"));
        let (a2, u2) = reg.refresh_strategy_from_snapshot(snap).unwrap();
        assert_eq!((a2, u2), (0, 0), "reapplying is idempotent");
    }

    #[test]
    fn refresh_snapshot_bad_json_returns_err() {
        let reg = ModelRegistry::new();
        assert!(reg.refresh_strategy_from_snapshot("{ not json").is_err());
    }

    #[test]
    fn refresh_snapshot_preserves_missing_fields() {
        // When a snapshot row lacks unavailableFallback, the existing fallback must not be overwritten
        let reg = ModelRegistry::new();
        let before = reg.meta_for("deepseek/deepseek-v4-flash").unwrap();
        assert_eq!(before.fallback.as_deref(), Some("openai/gpt-5.6-luna"));
        let snap = r#"{"_source":"t","_vended_at":"2026-09-19","models":[{"id":"deepseek/deepseek-v4-flash","availability":"always","catalog":true}]}"#;
        reg.refresh_strategy_from_snapshot(snap).unwrap();
        let after = reg.meta_for("deepseek/deepseek-v4-flash").unwrap();
        assert_eq!(
            after.fallback.as_deref(),
            Some("openai/gpt-5.6-luna"),
            "missing fields must not clear fallback"
        );
    }

    #[test]
    fn mimo_meta_is_present_and_correct() {
        let reg = ModelRegistry::new();
        let m = reg.meta_for("mimo/mimo-v2.5").expect("mimo should have metadata");
        assert!(m.available);
        assert!(!m.premium);
        assert!(m.multimodal);
        assert!(m.efforts.is_none(), "mimo has no efforts ladder");
        assert!(reg.is_known("mimo/mimo-v2.5"));
    }

    #[test]
    fn is_known_marks_dynamic_models_unverified() {
        let reg = ModelRegistry::new();
        assert!(reg.is_known("z-ai/glm-5.3-flash"));
        // Upstream free-agents dynamically added but with no static meta -> not policy-verified
        assert!(!reg.is_known("google/gemini-2.5-flash-lite"));
    }

    #[test]
    fn vendored_snapshot_applies_cleanly() {
        // Builtin/local snapshot aligned with the static table -> (0,0) idempotent self-check
        let snap = load_local_snapshot().expect("builtin snapshot exists");
        let reg = ModelRegistry::new();
        let (added, updated) = reg.refresh_strategy_from_snapshot(&snap).unwrap();
        assert_eq!(added, 0, "snapshot should not add rows outside the static table's catalog");
        assert_eq!(updated, 0, "an aligned snapshot should produce no updates");
    }

    #[test]
    fn meta_snapshot_includes_availability_fields() {
        let reg = ModelRegistry::new();
        let snap = reg.meta_snapshot();
        assert!(snap.len() >= 20, "at least 20 entries including mimo");
        let v = serde_json::to_value(&snap[0]).unwrap();
        assert!(v.get("availability").is_some(), "meta includes availability");
        assert!(v.get("available_at").is_some(), "meta includes available_at");
    }
}
