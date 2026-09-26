//! Model routing + fallback chain + token savings (9router-style)
//!
//! - Automatically falls back to an alternate in fallback_models when the primary model fails
//! - Picks the best model based on free quota/cost
//! - Token savings: compress overly long tool_result (optional, off by default)

use crate::models::ModelRegistry;
use anyhow::Result;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RouterConfig {
    /// The frozen fallback chain (user-selected)
    pub fallback_chain: Vec<String>,
    /// Quota-aware (based on the account's remaining rateLimitsByModel)
    pub quota_aware: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            fallback_chain: vec![
                "z-ai/glm-5.3-flash".into(),
                "google/gemini-3.8-flash".into(),
                "google/gemini-3.1-flash-lite".into(),
                "deepseek/deepseek-v4-flash".into(),
            ],
            quota_aware: true,
        }
    }
}

impl RouterConfig {
    /// Build from the global config: prefers the user's fallback_models when configured (previously that config was parsed but never consumed)
    pub fn from_app_config(cfg: &crate::config::Config) -> Self {
        let mut rc = Self::default();
        if !cfg.fallback_models.is_empty() {
            rc.fallback_chain = cfg.fallback_models.clone();
        }
        rc
    }
}

pub struct ModelRouter {
    pub config: RouterConfig,
    pub registry: Arc<ModelRegistry>,
}

impl ModelRouter {
    pub fn new(registry: Arc<ModelRegistry>, config: RouterConfig) -> Self {
        Self { config, registry }
    }

    /// Resolve the requested model -> the actual model to use (including the fallback chain; time-aware)
    pub async fn resolve(&self, requested: &str) -> String {
        self.resolve_at(requested, chrono::Utc::now()).await
    }

    /// Resolve at a given time: use the requested model directly only if it's available then; otherwise take the first available entry in the fallback chain
    pub async fn resolve_at(&self, requested: &str, now: chrono::DateTime<chrono::Utc>) -> String {
        if self.registry.has_model(requested).await
            && self.registry.model_available_at(requested, now)
        {
            return requested.to_string();
        }
        for m in &self.config.fallback_chain {
            if self.registry.has_model(m).await && self.registry.model_available_at(m, now) {
                return m.clone();
            }
        }
        // Audit M2: the fallback default model must also be available at that time; if not, take the first available entry in the fallback chain; only return the default as-is if none are available (let upstream produce a readable error)
        if self
            .registry
            .model_available_at(crate::models::DEFAULT_MODEL, now)
        {
            return crate::models::DEFAULT_MODEL.to_string();
        }
        for m in &self.config.fallback_chain {
            if self.registry.has_model(m).await && self.registry.model_available_at(m, now) {
                return m.clone();
            }
        }
        crate::models::DEFAULT_MODEL.to_string()
    }

    /// Check whether a model can be used directly (prevents hallucination)
    pub fn is_available(&self, model: &str) -> bool {
        self.registry.models_sync().contains(&model.to_string())
    }

    /// Whether a model supports reasoning_effort (thinking level), reverse-engineered from the upstream orchestrator.js efforts field
    /// Supported (non-empty efforts): deepseek family/glm family/gpt-5.6/gemini-3.8/fable-5/ox-alpha/muse-spark
    /// Unsupported (no efforts field): solar-pro4 / minimax-m3 / mimo-v2.5 / kimi-k3
    pub fn supports_reasoning(&self, model: &str) -> bool {
        // Model prefixes that support efforts
        const SUPPORTED: &[&str] = &[
            "deepseek/",
            "z-ai/glm",
            "openai/gpt-5.6",
            "google/gemini-3.8",
            "anthropic/claude-fable",
            "stealth/ox-alpha",
            "meta/muse-spark",
        ];
        SUPPORTED.iter().any(|p| model.starts_with(p))
    }

    /// The efforts range a model supports; None = unsupported
    ///
    /// v0.9: prefers the authoritative metadata table (`ModelRegistry`'s static table, aligned with upstream freebuff-models.ts);
    /// a model in the table with efforts=None is unsupported (no longer guessed by prefix); a model not in the table
    /// (dynamically added upstream) falls back to prefix matching, so deepseek/glm variants don't lose their tier.
    pub fn reasoning_efforts(&self, model: &str) -> Option<Vec<&'static str>> {
        if self.registry.meta_for(model).is_some() {
            return self.registry.efforts_static(model).map(|e| e.to_vec());
        }
        // Model not in the table: fall back to prefix matching (for upstream dynamic additions)
        if model.starts_with("deepseek/")
            || model.starts_with("z-ai/glm")
            || model.starts_with("stealth/ox-alpha")
        {
            Some(vec!["low", "high", "max"])
        } else if model.starts_with("openai/gpt-5.6")
            || model.starts_with("google/gemini-3.8")
            || model.starts_with("anthropic/claude-fable")
        {
            Some(vec!["low", "medium", "high", "xhigh", "max"])
        } else if model.starts_with("meta/muse-spark") {
            Some(vec!["minimal", "low", "medium", "high", "xhigh"])
        } else {
            None
        }
    }

    /// Whether a model can be used for free (metadata; unknown models default to available, to avoid breaking upstream dynamic additions)
    pub fn model_available(&self, model: &str) -> bool {
        self.registry.model_available(model)
    }

    /// Returns the fallback model when the model is unavailable and has one; available / unknown model -> None (time-aware)
    pub fn resolve_available(&self, model: &str) -> Option<String> {
        self.resolve_available_at(model, chrono::Utc::now())
    }

    /// At a given time: unavailable with a fallback -> the fallback model; available / unknown -> None
    pub fn resolve_available_at(
        &self,
        model: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<String> {
        let meta = self.registry.meta_for_at(model, now)?;
        if meta.available {
            return None;
        }
        meta.fallback
    }

    /// Human-readable reason an unavailable model is unavailable (for panel display); available / unknown model -> None
    pub fn unavailable_reason(&self, model: &str) -> Option<String> {
        self.unavailable_reason_at(model, chrono::Utc::now())
    }

    /// Human-readable reason at a given time (including the availableAt text)
    pub fn unavailable_reason_at(
        &self,
        model: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<String> {
        self.unavailable_detail_at(model, now)
            .map(|(reason, _)| reason)
    }

    /// At a given time: the unavailability reason + expected recovery time (Some(ISO) within an off_peak_only window, otherwise None)
    pub fn unavailable_detail_at(
        &self,
        model: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<(String, Option<String>)> {
        let meta = self.registry.meta_for_at(model, now)?;
        if meta.available {
            return None;
        }
        let paused = meta.availability != "off_peak_only";
        let mut msg = if paused {
            format!("Model {model} has been paused/discontinued upstream (no longer offered in free mode)")
        } else {
            format!("Model {model} is currently outside its available window (upstream DeepSeek peak-price window 00:00-10:00 UTC)")
        };
        if let Some(fb) = &meta.fallback {
            msg.push_str(&format!("; consider switching to {fb}"));
        }
        if let Some(aa) = &meta.available_at {
            msg.push_str(&format!("; expected to resume at {aa} (availableAt)"));
        }
        Some((msg, meta.available_at))
    }

    /// Clamp effort: falls back to the nearest supported value when unsupported or out of range
    pub fn clamp_effort(&self, model: &str, requested: &str) -> Option<String> {
        let efforts = self.reasoning_efforts(model)?;
        let requested = requested.to_lowercase();
        if efforts.contains(&requested.as_str()) {
            return Some(requested);
        }
        // Out of range: map requests in the same tier as max -> the supported ceiling
        if requested == "max" || requested == "xhigh" || requested == "high" {
            return Some(efforts.last().unwrap().to_string());
        }
        if requested == "minimal" || requested == "low" {
            return Some(efforts.first().unwrap().to_string());
        }
        Some(efforts.first().unwrap().to_string())
    }
}

/// Compress an overly long tool_result (the core of token savings)
/// Note: split on character boundaries -- byte-slicing a Chinese string would panic (same issue class as api.rs tail_keep)
pub fn compress_tool_result(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let keep = max_chars / 2;
    // Head: step back from `keep` to a character boundary
    let mut head_end = keep.min(content.len());
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    // Tail: step forward from `len-keep` to a character boundary
    let mut tail_start = content.len() - keep;
    while tail_start < content.len() && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n\n[... compressed: original {} chars, kept {} head/tail chars ...]\n\n{}",
        &content[..head_end],
        content.len(),
        max_chars,
        &content[tail_start..]
    )
}

#[allow(dead_code)]
pub fn truncate_to_tokens(s: &str, approx_tokens: usize) -> String {
    // Rough estimate: 1 token ≈ 4 characters (English)
    let max_chars = approx_tokens * 4;
    compress_tool_result(s, max_chars)
}

#[allow(dead_code)]
async fn plausible_model(_registry: &ModelRegistry, _name: &str) -> Result<bool> {
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModelRegistry;
    use std::sync::Arc;

    fn router() -> ModelRouter {
        ModelRouter::new(Arc::new(ModelRegistry::new()), RouterConfig::default())
    }

    #[test]
    fn clamp_effort_within_range_kept() {
        let r = router();
        // glm supports low/high/max
        assert_eq!(
            r.clamp_effort("z-ai/glm-5.3-flash", "high").as_deref(),
            Some("high")
        );
        assert_eq!(
            r.clamp_effort("z-ai/glm-5.3-flash", "low").as_deref(),
            Some("low")
        );
    }

    #[test]
    fn clamp_effort_over_range_downgrades() {
        let r = router();
        // glm's ceiling is max; requesting xhigh -> takes the ceiling max; gpt supports up to max
        assert_eq!(
            r.clamp_effort("z-ai/glm-5.3-flash", "xhigh").as_deref(),
            Some("max")
        );
        // muse's ceiling is xhigh; requesting max -> xhigh
        assert_eq!(
            r.clamp_effort("meta/muse-spark-x", "max").as_deref(),
            Some("xhigh")
        );
        // muse's floor is minimal; requesting low is valid but minimal is the first entry
        assert_eq!(
            r.clamp_effort("meta/muse-spark-x", "minimal").as_deref(),
            Some("minimal")
        );
    }

    #[test]
    fn clamp_effort_unsupported_model_returns_none() {
        let r = router();
        // solar/minimax/mimo/kimi do not support effort
        assert!(r.clamp_effort("upstage/solar-pro4", "max").is_none());
        assert!(r.clamp_effort("minimax/minimax-m3", "high").is_none());
    }

    #[test]
    fn clamp_effort_unknown_value_falls_back() {
        let r = router();
        // Unknown tier -> take the first entry in the supported list
        assert_eq!(
            r.clamp_effort("z-ai/glm-5.3-flash", "bogus").as_deref(),
            Some("low")
        );
    }

    #[test]
    fn supports_reasoning_prefixes() {
        let r = router();
        assert!(r.supports_reasoning("deepseek/deepseek-v4-flash"));
        assert!(r.supports_reasoning("z-ai/glm-5.3-flash"));
        assert!(!r.supports_reasoning("upstage/solar-pro4"));
    }

    #[test]
    fn compress_short_content_untouched() {
        let s = "short";
        assert_eq!(compress_tool_result(s, 100), "short");
    }

    #[test]
    fn compress_long_content_keeps_head_tail() {
        let s = "a".repeat(1000);
        let out = compress_tool_result(&s, 100);
        assert!(out.len() < 1000);
        assert!(out.contains("compressed"));
        assert!(out.starts_with("aaa"));
    }

    #[test]
    fn compress_multibyte_content_does_not_panic() {
        // Regression: byte-slicing Chinese content would trigger a char boundary panic
        let s = "中文内容".repeat(500);
        let out = compress_tool_result(&s, 1000);
        assert!(out.contains("compressed"));
        assert!(out.starts_with("中文"));
        // emoji (4-byte characters) are equally safe
        let e = "🎉".repeat(300);
        let out2 = compress_tool_result(&e, 500);
        assert!(out2.contains("compressed"));
    }

    #[test]
    fn resolve_at_time_aware_fallback() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let reg = Arc::new(ModelRegistry::new());
            reg.init().await;
            let r = ModelRouter::new(reg.clone(), RouterConfig::default());
            // Paused model (gemini-3.8) -> falls back to the first entry in the chain that's available at the time (glm-5.3)
            assert_eq!(
                r.resolve("google/gemini-3.8-flash").await,
                "z-ai/glm-5.3-flash"
            );
            // Available model is returned as-is
            assert_eq!(r.resolve("z-ai/glm-5.3-flash").await, "z-ai/glm-5.3-flash");
            // Unknown model -> default
            assert_eq!(
                r.resolve("no/such-model").await,
                crate::models::DEFAULT_MODEL
            );
        });
    }

    #[test]
    fn resolve_available_at_window_fallback() {
        let reg = ModelRegistry::new();
        // Snapshot override: deepseek-v4-flash -> off_peak_only (fallback keeps gpt-5.6-luna)
        let snap = r#"{"_source":"t","_vended_at":"2026-09-19","models":[{"id":"deepseek/deepseek-v4-flash","availability":"off_peak_only","catalog":true}]}"#;
        reg.refresh_strategy_from_snapshot(snap).unwrap();
        let r = ModelRouter::new(Arc::new(reg), RouterConfig::default());
        let in_win = chrono::DateTime::parse_from_rfc3339("2026-09-16T03:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let out_win = chrono::DateTime::parse_from_rfc3339("2026-09-16T15:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            r.resolve_available_at("deepseek/deepseek-v4-flash", in_win)
                .as_deref(),
            Some("openai/gpt-5.6-luna")
        );
        assert_eq!(
            r.resolve_available_at("deepseek/deepseek-v4-flash", out_win),
            None
        );
        // Unknown model doesn't flap
        assert_eq!(r.resolve_available("no/such-model"), None);
    }

    #[test]
    fn unavailable_reason_at_has_parseable_available_at() {
        let reg = ModelRegistry::new();
        let snap = r#"{"_source":"t","_vended_at":"2026-09-19","models":[{"id":"deepseek/deepseek-v4-flash","availability":"off_peak_only","catalog":true}]}"#;
        reg.refresh_strategy_from_snapshot(snap).unwrap();
        let r = ModelRouter::new(Arc::new(reg), RouterConfig::default());
        let in_win = chrono::DateTime::parse_from_rfc3339("2026-09-16T03:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let (reason, aa) = r
            .unavailable_detail_at("deepseek/deepseek-v4-flash", in_win)
            .expect("should give a reason inside the window");
        assert!(reason.contains("window"), "window message: {reason}");
        let iso = aa.expect("should have availableAt inside the window");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&iso).is_ok(),
            "ISO should be parseable"
        );
        assert!(reason.contains("availableAt"));
    }
}
