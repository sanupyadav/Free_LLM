# 6. Feature Expansion and Innovation (v0.8 -> v1.0 Roadmap)

> After "hardening + debt repayment + panel modernization" have landed, the feature list for the jump to the next version. Sorted by ROI, no pure showmanship.

---

## 6.1 v0.8 Mainline (aligned with 2/3/4/5, short-term)

1. Land the dual-bucket concurrency semaphore (2.1) - **do this first, highest risk, most direct payoff**
2. Claude path retries + accounting + memory (2.2)
3. Debt repayment (2.3)
4. Panel modernization (3)
5. High availability/security hardening (4)
6. Test expansion (5)

## 6.2 v0.9 (Medium-Short Term - Experience Leap)

| Direction | Specific item | Value |
|------|--------|------|
| **Multimodal enhancement** | Upload images directly in the panel (drag-and-drop) + preview + one-click fill `images` into the test bench | Usability for beginners |
| **Chat test bench** | Embed a "send a message to try it" feature in the panel (calls /v1/chat/completions, streaming render), no client needed | First-time experience for new users |
| **Model bidding/recommendation** | Recommend "today's best model" ranked by remaining quota + price + latency (data already available: rateLimitsByModel + usage latency) | Saves money and time |
| **Account health dashboard** | Visualize per-account circuit-breaker/cooldown timeline (trips, cooldown, last_error time series chart) | Operations |
| **Export/import full config** | One-click export of config + credentials + skills + memory (optional encryption), for migrating machines | Convenience |
| **Real-time usage animation** | Add a mini timeline of TTFT/total time/bytes to the request detail drawer | Observability feel |

## 6.3 v1.0 (Long-Term - Platformization)

| Direction | Specific item | Value |
|------|--------|------|
| **Multi-upstream gateway** | Abstract an `UpstreamProvider` trait: codebuff / other free tiers / your own API, all pluggable | First step toward platformization |
| **Team/multi-user** | Gateway listens on 0.0.0.0 + per-user independent api_key + quota limits (an api_keys prototype already exists) | Sharing within a small team |
| **OpenTelemetry export** | Telemetry/usage optionally exportable as OTLP (for observability platforms) | Production-grade |
| **Plugin system** | skills already has a file-based source of truth - extend it into "gateway plugins" (middleware hooks: request rewriting/response rewriting/auditing) | Ecosystem |
| **Statistical reports** | Weekly/monthly report export (JSON/CSV/Markdown), usage trend insights | Operations/sharing |
| **One-click deployment** | Docker Compose template + systemd service file + desktop silent-install parameters | Deployment experience |

## 6.4 Innovation Points (From First Principles)

The user's pain points are: **free model quotas are scattered and fragmented, credentials expire easily, client configuration is tedious, and there's no visibility into how much each request costs.**

1. **Quota futures view**: convert rateLimitsByModel's "today's remaining x price" into "how many more chat rounds you can have today", shown directly on the panel, answering "how much longer can I chat today"
2. **Credential self-healing**: Cookie expiration detection (after a 401, automatically trigger a WebView2 re-login? No - instead "proactive reminder + one-click re-login" semi-automatic), so the user doesn't discover everything is dead only when they open the app
3. **Per-request failure attribution**: add a one-sentence explanation of "why is it slow / why did it fail" to the request detail view (a prototype already exists - the "plain-language explanation" prompt), expand it into "three worsts" (slowest account, most-used model, highest error-rate time window)
4. **Memory visualization**: draw the zero-LLM-memory observe rules as a flowchart (panel's "Memory" page), letting the user understand "how the AI learned" - **a differentiating highlight, zero cost since it reuses existing data**

## 6.5 Open Source and Ecosystem

| Item | Action |
|----|------|
| License | Keep MIT (LICENSE already present) |
| CI | build-release.yml / docker.yml already exist; add `cargo fmt --check` + `clippy -D warnings` + `llvm-cov` gates |
| Docs | Full rewrite of README_zh (kept in sync with versions), README_en to follow, docs/API_GUIDE updated with new endpoints |
| Community | Set up GitHub Discussions / templates (Bug/Feature); release checklist (auto-generated CHANGELOG) |
| Promotion | Update promo video/screenshots (the current 43MB video directory is from an old version) |

## 6.6 Explicitly Not Doing (to Prevent Scope Creep)

- ❌ No model training/fine-tuning (pointless)
- ❌ No cross-platform GUI rewrite (Electron is already sufficient)
- ❌ No distributed/multi-machine clustering (this tool is positioned as a local, single-machine tool)
- ❌ No swapping in a Rust async framework / database migration before v1.0

## 6.7 Suggested Version Cadence

| Phase | Timeline | Delivery |
|------|------|------|
| v0.8 | 2-4 weeks | hardening + debt repayment + panel + tests (this plan's sections 2/3/4/5) |
| v0.9 | 3-5 weeks | experience leap (6.2) + docs/CI gates |
| v1.0 | 4-8 weeks | platformization (first two items of 6.3) + ecosystem (6.5) |
