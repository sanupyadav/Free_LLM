//! Built-in prompts and skills (system prompts + built-in skills)
//! For developers to enable via the panel/API, injected into the chat request's system prefix.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Built-in system prompt templates (freely enable/customize)
pub const BUILTIN_PROMPTS: &[(&str, &str, &str)] = &[
    (
        "default",
        "General Assistant (default)",
        "You are a helpful AI coding assistant. Follow the user's instructions precisely. Write clean, working code. When asked to explain, be concise and correct.",
    ),
    (
        "coding-expert",
        "Senior Engineer",
        "You are a senior software engineer with deep expertise in system design, algorithms, and production-grade engineering. Provide pragmatic solutions, flag trade-offs, and prefer simple working code unless complexity is justified.",
    ),
    (
        "code-reviewer",
        "Code Reviewer",
        "You are a meticulous code reviewer. Analyze code for correctness, security (injection, secrets, auth), performance, and style. Report issues by severity (CRITICAL/HIGH/MEDIUM/LOW) with evidence and concrete fixes. Never approve without evidence.",
    ),
    (
        "security-auditor",
        "Security Auditor",
        "You are a security auditor following OWASP Top 10. Review for injection, XSS, CSRF, auth bypass, IDOR, SSRF, and secret leaks. For each finding: attack scenario, impact, and fix. Verify findings; do not report theory as fact.",
    ),
    (
        "debug-master",
        "Debugging Expert",
        "You are a systematic debugger. Follow the scientific method: form hypotheses, gather evidence (logs, repro, code paths), test the cheapest hypothesis first, and only then fix. Never guess-fix without reproducing. After fixing, explain root cause in the document",
    ),
    (
        "test-tdd",
        "TDD Engineer",
        "You follow Test-Driven Development strictly: write failing test first (RED), implement minimal code to pass (GREEN), then refactor (IMPROVE). Aim for 80%+ coverage on core logic. Name tests by behavior.",
    ),
];

/// Built-in skills (enable/disable)
pub const BUILTIN_SKILLS: &[(&str, &str, &str)] = &[
    (
        "git-guru",
        "Git Expert",
        "Proficient with git workflows (branch, rebase, cherry-pick, bisect, reflog). Prefer small atomic commits with conventional messages. Help resolve conflicts and write clean PR descriptions.",
    ),
    (
        "docker-deploy",
        "Docker Deployment",
        "Expert in Docker and Docker Compose: multi-stage builds, healthchecks, volumes, secrets, and zero-downtime deploys. Prefer distroless images and minimal attack surface.",
    ),
    (
        "api-designer",
        "API Design",
        "Designs clean REST/OpenAPI APIs: consistent envelope, versioning, pagination, rate limiting, idempotency, and auth. Prefer battle-tested patterns over bespoke abstractions.",
    ),
    (
        "perf-tuner",
        "Performance Tuning",
        "Profiles and optimizes: identifies bottlenecks (N+1, cache misses, allocations), measures before/after, and prefers compositor-friendly or algorithmic wins over micro-tuning.",
    ),
    (
        "refactor-clean",
        "Refactor Cleanup",
        "Refactors for clarity and maintainability while preserving behavior: extracts functions, removes dead code, applies immutable patterns, and keeps diffs small and reviewable.",
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptConfig {
    pub id: String,
    pub name: String,
    pub content: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct PromptManager {
    prompts: Arc<RwLock<HashMap<String, PromptConfig>>>,
    skills: Arc<RwLock<HashMap<String, PromptConfig>>>,
}

impl PromptManager {
    pub fn new() -> Self {
        let prompts = BUILTIN_PROMPTS
            .iter()
            .map(|(id, name, content)| {
                (
                    id.to_string(),
                    PromptConfig {
                        id: id.to_string(),
                        name: name.to_string(),
                        content: content.to_string(),
                        enabled: false,
                    },
                )
            })
            .collect();
        let skills = BUILTIN_SKILLS
            .iter()
            .map(|(id, name, content)| {
                (
                    id.to_string(),
                    PromptConfig {
                        id: id.to_string(),
                        name: name.to_string(),
                        content: content.to_string(),
                        enabled: false,
                    },
                )
            })
            .collect();
        Self {
            prompts: Arc::new(RwLock::new(prompts)),
            skills: Arc::new(RwLock::new(skills)),
        }
    }

    /// Enabled prompts (concatenated in id order)
    pub async fn active_prompts(&self) -> String {
        let p = self.prompts.read().await;
        let mut s = String::new();
        let mut enabled: Vec<&PromptConfig> = p.values().filter(|c| c.enabled).collect();
        enabled.sort_by(|a, b| a.id.cmp(&b.id));
        for c in enabled {
            s.push_str(&c.content);
            s.push_str("\n\n");
        }
        s
    }

    /// Enabled skills
    pub async fn active_skills(&self) -> String {
        let s = self.skills.read().await;
        let mut out = String::new();
        let mut enabled: Vec<&PromptConfig> = s.values().filter(|c| c.enabled).collect();
        enabled.sort_by(|a, b| a.id.cmp(&b.id));
        for c in enabled {
            out.push_str(&format!("[Skill: {}]\n{}\n\n", c.name, c.content));
        }
        out
    }

    /// Assemble the final system prefix (default prompt + enabled custom prompts + skills)
    pub async fn system_prefix(&self) -> String {
        let mut s = self.system_prefix_prompts_only().await;
        // Skills (with their own constraints)
        let skills = self.active_skills().await;
        if !skills.is_empty() {
            s.push_str("You have these active skills available. Use them as guidance for how you respond in relevant situations:\n");
            s.push_str(&skills);
        }
        s
    }

    /// Prompts-only portion (default prompt + enabled custom prompts) -- reused by the skills system's roster mode,
    /// to avoid duplicating the skill injection done by the skills module.
    pub async fn system_prefix_prompts_only(&self) -> String {
        let mut s = String::new();
        // Default prompt is always enabled as the base
        if let Some(base) = self.prompts.read().await.get("default") {
            s.push_str(&base.content);
            s.push_str("\n\n");
        }
        // Other enabled prompts
        let extra = self.active_prompts().await;
        if !extra.is_empty() {
            s.push_str(&extra);
        }
        s
    }

    pub async fn prompts_snapshot(&self) -> Vec<PromptConfig> {
        let p = self.prompts.read().await;
        let mut v: Vec<PromptConfig> = p.values().cloned().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub async fn skills_snapshot(&self) -> Vec<PromptConfig> {
        let s = self.skills.read().await;
        let mut v: Vec<PromptConfig> = s.values().cloned().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Enable/disable (via API panel)
    pub async fn set_prompt_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut p = self.prompts.write().await;
        match p.get_mut(id) {
            Some(c) => {
                c.enabled = enabled;
                true
            }
            None => false,
        }
    }

    pub async fn set_skill_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut s = self.skills.write().await;
        match s.get_mut(id) {
            Some(c) => {
                c.enabled = enabled;
                true
            }
            None => false,
        }
    }
}

impl Default for PromptManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prompts_enable() {
        let m = PromptManager::new();
        assert!(m.set_prompt_enabled("coding-expert", true).await);
        assert!(!m.set_prompt_enabled("nope", true).await);
        let prefix = m.system_prefix().await;
        assert!(prefix.contains("senior software engineer"));
    }

    #[tokio::test]
    async fn skills_enable() {
        let m = PromptManager::new();
        assert!(m.set_skill_enabled("git-guru", true).await);
        let prefix = m.system_prefix().await;
        assert!(prefix.contains("Git Expert"));
        assert!(prefix.contains("active skills"));
    }
}
