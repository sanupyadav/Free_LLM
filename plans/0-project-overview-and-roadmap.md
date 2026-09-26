# Freebuff2API project overview and version roadmap (refreshed to v0.10.0)

> Entry index for "plans" (planning docs). Reading order: 0 -> 2/3/4 -> 5 -> 6 -> next-step improvement guide. This file must be refreshed after every version release (to prevent drift).

## 1. Project in one sentence
Reverse-engineers Freebuff's (codebuff.com) free-tier model capabilities into a local **OpenAI-compatible + Anthropic-compatible** gateway: a single Rust (axum) binary + embedded single-page panel + Electron desktop shell + browser-extension login.

## 2. Current version baseline (v0.10.0 - 2026-09-19)

| Item | Value | Evidence |
|----|----|----|
| Version | **0.10.0** | Cargo.toml / desktop/package.json / CHANGELOG synced |
| Language/stack | Rust 1.95 (axum 0.8) + embedded panel | Cargo.toml |
| Unit tests | **275** | `cargo test --lib` (measured 2026-09-19) |
| Integration tests | core 8 + router 11 + web_pool 5 + model_meta 7 | `cargo test --test *` |
| Lint | clippy -D warnings: zero warnings; fmt passes | `cargo clippy --all-targets -- -D warnings` |
| Real E2E | e2e_phase_v0_8/09, 26 assertions each + headless rendering | tests/ + CI e2e-win job |
| Model contract | tests/fixtures/freebuff-models.snapshot.json (21 lines) + scripts/check_model_contract.mjs aligned | `node scripts/check_model_contract.mjs` |
| Code size | src/ approx. 19k lines (api.rs largest) | wc -l |

## 3. Capability map (implemented as of v0.10)
- Dual-protocol egress (OpenAI/Anthropic, streaming + non-streaming + protocol conversion)
- Smart multi-account rotation: Bearer pool (health score + three-state circuit breaker + exponential cooldown) + web cookie pool (v0.9, 401 cooldown/rotation/health dashboard)
- **Real-time upstream model policy contract (v0.10)**: availability (always/deployment_hours/off_peak_only time windows) + availableAt + available_at surfaced + snapshot sync (refresh_strategy_from_snapshot) + "not policy-verified" labeling for off-list models + mimo/mimo-v2.5 added
- Dual-bucket concurrency semaphore (slot/concurrent, free 1+3 / subscription 3+8)
- Session keepalive + ad-based quota top-up; memory layer (off by default, hot-switchable)
- Skills system (file-as-source-of-truth + roster injection + quality gate)
- Panel (v0.10 accessibility: ARIA tabs/keyboard/44px/aria-live; log filter/pause/export/error badge; usability column on recommendation cards; credential cooldown reminders; upload validation)
- Observability: telemetry SQLite + "top three" aggregation (/api/usage/insights) + real-time log SSE + system doctor + usage stats
- Login: embedded WebView2 / browser extension / clipboard / manual import; full config export/import

## 4. Version roadmap
| Version | Theme | Status |
|------|------|------|
| v0.2->v0.3 | Skills/observability/Claude streaming/multimodal/panel | done |
| v0.4 | Memory/circuit breaker/retry/MCP/cost | done |
| v0.5 | Web-cookie bridging/extension login/credential management | done |
| v0.6 | Auth/atomic write/keys/E2E | done |
| v0.7 | WebView2 login/login wizard/accounting/memory toggle | done |
| v0.8 | Semaphore/Claude retry+accounting/panel modernization/security | done |
| v0.9 | Web cookie pooling/model metadata contract/multimodal test bench/health dashboard/export-import/auth depth | done |
| **v0.10** | **Real-time model policy contract / panel accessibility / top-three observability / pool degradation / CI E2E+--doc / doc cleanup** | done (2026-09-19) |
| v1.0 | Multi-upstream UpstreamProvider, multi-user quotas, OTLP (backlog, see 6-feature-expansion) | pending |

## 5. Document index
- `next-steps-guide.md` -- the actionable improvement checklist for the current version (core deliverable)
- `2-architecture-hardening.md` / `3-panel-modernization.md` / `4-high-availability-and-security.md` / `5-testing-and-verification.md` / `6-feature-expansion.md`
- `docs/TESTING.md` -- test ledger; `scripts/verify_release.ps1` -- one-click local verification
