# Workflow Status — Freebuff2API

> Single source of truth (long-running task recovery / node collaboration / final acceptance). Records only facts and evidence.

## Phase M (v0.10.3, completed 2026-09-19)

- **Source**: `plans/next-steps-guide.md` (v0.9.0 → v0.10.x)
- **Scope**: real-time upstream model policy contract (availability time windows/availableAt/snapshot sync + mimo inclusion + 5 upstream drift fixes) / panel accessibility & UX (ARIA/keyboard/44px/log enhancements/recommendation availability column/credential cooldown reminders/upload validation) / telemetry "top-3" aggregation endpoint / web credential pool full-cooldown structured degradation / request hot-path unwrap tightening / CI (E2E job + --doc + coverage discipline) / docs cleanup and v0.10.0 release
- **Status**: Done (v0.10.0 released + v0.10.1 audit-fix release, 2026-09-19)
- **Evidence**:
  - Unit/integration: 275 unit tests + core 8 + router 11 + web_pool 5 + model_meta 7 all green; clippy `-D warnings` zero warnings; `cargo fmt --check` passes
  - Real E2E: `e2e_phase_v0_8` 26/26, `e2e_phase_v0_9` 26/26 (preset fake web-cookie triggers the merge path) all green
  - New endpoint smoke tests: `/v1/models` meta includes availability/available_at (mimo/mimo-v2.5 now included); `/api/accounts/health` includes web-cookie entries + cooldown_seconds; `/api/usage/insights` contract complete (window_hours=24); `/ui` includes role="tablist"/aria-live/min-height:44px
  - Model contract: `scripts/check_model_contract.mjs` passes (fixture catalog=18 / models.rs meta=20 fully aligned); vendored snapshot applied with zero drift
  - Code audit: code-reviewer re-review (conclusion below)

## Historical phases

> Phases G/H/I/J/K/L already released; K=v0.8.0, L=v0.9.0.

## Phase L (v0.9.0, completed 2026-09-18)

- **Source**: `plans/next-steps-guide.md` (v0.8.0 → v0.9.x actionable checklist)
- **Scope**: web cookie credential pooling + multi-account round-robin / upstream model metadata contract & efforts alignment / panel test-bench multimodal multi-turn / account health dashboard / model recommendations / full config export/import / auth depth / docs and CI gating / v0.9.0 release
- **Status**: DONE (all items delivered, evidence below)

### Phase L delivery checklist (with evidence)

| ID | Goal | Deliverable | Status | Evidence |
|----|------|--------|------|------|
| L1 | web cookie credential pooling + multi-account round-robin | `src/web_pool.rs` (WebCookiePool: health score/circuit breaker/cooldown/round-robin) + api.rs wired end-to-end (chat/messages/balance/details/upload/cleanup) + hot-reload on import | Done | 7 unit tests + 5 integration tests all green; `/api/accounts/health` empirically confirmed to include web-cookie entries |
| L2 | upstream model metadata contract + efforts alignment | `ModelMeta` static authoritative table (premium/multimodal/available/efforts/fallback) + clamp ladder alignment + `/v1/models` meta | Done | models 4 unit tests + model_meta_test 4 all green; /v1/models empirically shows 19 meta entries, 6 correctly marked unavailable |
| L3 | panel test bench multimodal/multi-turn/effort | web.rs play tab: multi-turn context, system, effort dropdown, image upload (drag-drop/paste/select), copy/export | Done | check_panel_js passes; verified via headless Chrome DOM rendering |
| L4 | account health dashboard + model recommendations + data migration + duration timeline | /api/accounts/health + panel health/recommendations/migration/timeline | Done | e2e_phase_v0_9 26 assertions all green |
| L5 | auth depth | inject_peer (ConnectInfo→x-fb-peer) + is_loopback_request peer determination + doctor listen_scope | Done | doctor empirically includes listen_scope; default behavior unchanged |
| L6 | full config export/import | src/export.rs + /api/export + /api/import (schema validation/backup/security minimal set) | Done | export 4 unit tests + E2E round-trip empirically verified |
| L7 | docs/CI/archive | README count corrections, old reports archived to docs/archive, CI adds fmt + llvm-cov gate, CHANGELOG 0.9.0, three-way version sync | Done | rg 236 no remnants; YAML parsing passes |

### Phase L verification baseline (v0.9.0)

- **Unit/integration**: 253 unit tests + 8 core + 4 model_meta + 11 router + 5 web_pool **all green** (`cargo test`)
- **Lint**: `cargo clippy --all-targets -- -D warnings` zero warnings; `cargo fmt --check` passes
- **Real E2E**: `tests/e2e_phase_v0_9.cjs` 26 assertions all green (real gateway 47871); `tests/e2e_phase_v0_8.cjs` 26 assertions regression all green
- **Real browser**: headless Chrome renders the panel → JS executes fully (model-count placeholder→20, model chips rendered, all v0.9 controls in DOM)
- **New endpoints empirically verified**: /api/accounts/health (bearer+web-cookie+history), /api/export, /api/import (bad schema 400, round-trip 200+backup), /v1/models meta, doctor listen_scope
- **Environment limitation (disclosed honestly)**: `cargo test --doc` fails on this machine (fails identically on the HEAD baseline: chocolatey Rust is missing rustdoc.exe), not introduced by this change

## Task Contract — Phase K (v0.8.0, completed 2026-09-15)

- **Source**: `plans/next-steps-guide.md` (v0.7.3 → v0.8+ actionable checklist)
- **Goal**: deliver "claimed but not implemented" capability (dual-bucket concurrency semaphore), close the Claude path's capability gap (retry/accounting/memory), modernize panel UX (add test bench/settings/about pages + branding + windowed rendering), harden security/availability details (CSP, redaction, port conflict hints, desktop single-instance).
- **Authorization**: full (including testing, acceptance, commit, release).

## Phase K delivery checklist (with evidence)

| ID | Goal | Deliverable | Status | Evidence |
|----|------|--------|------|------|
| K1 | dual-bucket concurrency semaphore delivered | `src/semaphore.rs` (TieredSemaphore + TierGuard RAII) wired into chat/claude/web_bridge paths; config 4 items + env | DONE | 6 unit tests all green (bucket independence/timeout 429/RAII no leaks/subscription check) |
| K2 | Claude path retry + accounting + memory | handle_claude_messages rewritten: retry loop/waiting_room 503/non-stream+stream usage persisted/memory.observe | DONE | Router tests messages_waiting_room 503 + messages_non_stream 200 + api unit tests |
| K3 | cookie detection narrowed (%3A false-positive fix) | `looks_like_cookie` narrowed to three markers | DONE | api unit test cookie_detection_requires_known_markers |
| K4 | web_threads capacity cap + TTL | `MAX_BINDINGS=2000` + 24h TTL + pruning | DONE | web_threads unit tests prune_caps / prune_removes_expired |
| K5 | panel modernization + new pages | test bench/settings/about tabs + /api/config read/write + CSS tokens + windowed rendering | DONE | e2e_phase_v0_8.cjs 23 assertions all green (real gateway 47871) |
| K6 | security headers + CSP + redaction | secure_headers layer + `redact.rs` wired into logs/telemetry | DONE | e2e verified nosniff/Referrer-Policy/CSP; redact 6 unit tests |
| K7 | cross-site 403 semantics | origin_blocked() replaces the data-plane cross-site branch | DONE | Router test cross_site_origin_blocked_403 |
| K8 | desktop single-instance + tray health-check hash | `requestSingleInstanceLock` + `openConsoleAt` | DONE | `node --check desktop/main.js` passes |
| K9 | port-in-use error message | main.rs bind failure classified with a clear error message | DONE | empirically tested with 47871 occupied, produces a clear error + netstat hint |
| K10 | Router-level integration tests | `tests/router_test.rs` 10 cases (Mock TCP upstream) | DONE | cargo test --test router_test 10/10 all green |
| K11 | doc version drift fix | README EN/CN version numbers + semaphore description; CHANGELOG 0.8.0; API_GUIDE new endpoints/config | DONE | `rg 0.3.0` no remnants (README EN/CN); config.example synced |
| K12 | three-way version sync + release | Cargo.toml / desktop/package.json = 0.8.0 | DONE | release build v0.8.0 |

## Phase K independent review (Critic, 2026-09-15)

| Level | Finding | Resolution |
|------|------|------|
| CRITICAL | CSP `default-src 'self'` blocked **all inline scripts/styles** in the panel (impossible to satisfy without a build step for a single-file setup), breaking the entire panel (headless Edge empirically found 116 violations; E2E only did HTTP text assertions and missed this) | Fixed: CSP now allows `script-src/style-src 'unsafe-inline'`, keeping `object-src 'none'`/`base-uri`/`frame-ancestors` protections; re-verified with headless Edge that model-count is filled by JS, 0 violations |
| MEDIUM | Semaphore semantics: README claimed {slot:1,multi:3} implying concurrency of 4, but the implementation occupies both per request → actual = min(slots,multi)=1 | Fixed: README/CHANGELOG/module comments clarified "actual concurrency = slot capacity"; multi reserved as an upstream policy dimension |
| MEDIUM | Claude streaming telemetry used a new UUID (claude_req_id) disconnected from the request-level req_id, breaking the event chain | Fixed: streaming now reuses req_id |
| LOW | Bare JWTs (no marker prefix) weren't redacted (defense-in-depth gap) | Fixed: added redact_jwt to redact (eyJ three-segment + length threshold) + 2 unit tests |
| MEDIUM | renderLogs scroll threshold `>200` inconsistent with the windowed-rendering effective range (~100 rows onward) | Fixed: threshold aligned (>100) |
| LOW | telemetry always redacts regardless of the redact_logs toggle (stricter than documented) | Kept as-is (safer; documented) |
| LOW | `claude_upstream` only assigned on 2xx → `!is_success()` branch is dead code | Kept as-is (mirrors the chat path's structure, a safe fallback) |
| P1 note | E2E needs real-browser rendering verification added | Added: headless Edge panel DOM verification is now part of the acceptance flow |

**Critic conclusion**: CONDITIONAL PASS → upgraded to **PASS** after fixes (all findings closed)

## Verification baseline (v0.8.0)

- **Unit tests**: 238 passed / 0 failed (`cargo test --lib`)
- **Integration tests**: `tests/core_test.rs` 8 items all green
- **Router-level integration**: `tests/router_test.rs` 10 items all green (Mock upstream, no real paid API calls)
- **Lint**: `cargo clippy --all-targets -- -D warnings` zero warnings; `cargo fmt --check` passes
- **Real E2E**: `tests/e2e_phase_v0_8.cjs` 26 assertions all green (real gateway 47871: security headers/CSP/config read-write whitelist/new tabs/windowed/redaction/healthz)
- **Real browser**: headless Edge renders the panel → JS executes fully (model-count placeholder→20), 0 CSP violations
- **Port conflict empirically tested**: a second instance binding the same port → clear error message + `netstat -ano` troubleshooting hint
- **Config persistence empirically tested**: write back thread_max_age_hours=48 → clean restart reads back 48, confirmed

## Explicitly out of scope / backlog (carried forward)

- SSE batch-commit (NewAPI-Gateway pattern) → kept in backlog (cost/benefit ratio insufficient)
- web-version agent-runs/stream, web-version ad chain, video uploads
- cryptographically random key rotation grace period, desktop multi-account round-robin enhancement (P3 record)
- llvm-cov coverage gate (this machine lacks `cargo llvm-cov`; the `--fail-under-lines 80` command has been added to CI recommendations)

### Phase L release (v0.9.0, 2026-09-18)

- **Release**: https://github.com/lza6/Freebuff-2API/releases/tag/v0.9.0
- **Assets**: Freebuff2API-Setup-0.9.0.exe (85.7MB NSIS installer) · freebuff2api.exe (13.2MB gateway binary) · latest.yml (auto-update feed)
- **Note**: the tag was built by CI; due to local network restrictions, the installer assets were built locally with electron-builder and uploaded manually (softprops has a known bug in the workflow_dispatch + moved-tag scenario, `Error creating asset temp dir`; a normal tag push is unaffected, verified in v0.8.0)

## Phase J archive (v0.7.0 released)

- WebView2 one-click login (commit d09397a); J6 full success path (manual login) awaiting one round of user cooperation
- Baseline: 216 unit tests all green · verified in a real window across two rounds

## Phase H/I archive (v0.6.0 released, commit 1435a8f)

- 213 unit tests all green · clippy 0 warnings · phase_g 46/46 + phase_i 18/18
