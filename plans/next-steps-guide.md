# Next-Step Improvement Guide (Freebuff2API v0.9.0 → v0.10.x)

> This document is the **actionable improvement checklist** (the core deliverable). Each item includes: priority, problem/evidence, change location, behavior diff, acceptance criteria (TDD checklist in §4).
> The previous version (v0.8.0→v0.9.x) has been fully executed and archived: `plans/next-steps-guide-archive-v0.8.0-v0.9.0.md`.
> Companion reference: `plans/0-project-overview-and-roadmap.md` (note: this file is stuck at v0.7.3; this checklist's §3.3 already lists a refresh task) and `6-feature-expansion.md`.
> Document scope: **plans and proposals only, no code changes**; implementation is carried out by the AI you choose, and this document provides enough evidence and acceptance criteria to avoid "changing things from memory".

---

## 0. Overview (v0.9.0 measured baseline · fully re-verified on 2026-09-19)

| Item | Value | Evidence |
|----|----|------|
| Version | **0.9.0** | Cargo.toml / desktop/package.json / CHANGELOG all in sync; git HEAD=c6e201c (2026-09-18) |
| Test scale | **253 unit tests + 8 core + 4 model_meta + 11 router + 5 web_pool, all green** | CHANGELOG 0.9.0 + workflow_status Phase L (the full suite was not rerun this time; see honest disclosure below) |
| Lint | clippy zero warnings; fmt passes | workflow_status Phase L |
| release binary | freebuff2api.exe = 13,203,968 B | target/release (built 2026-09-18) |
| Real smoke test (this round) | healthz 200 (**ready in 1.9s**, model_count=20), /ui 200 (103,192 B, CSP in effect), /v1/models data=20 + meta=19, /api/doctor 10 items, /api/export schema v1, cross-origin admin request blocked with 401 | release run against a temp directory + port 47899, see Appendix B |
| Working tree | clean | git status (2026-09-19) |
| Environment limitation (honest disclosure) | local `cargo test --doc` fails (chocolatey Rust is missing rustdoc.exe); the HEAD baseline fails the same way, not introduced by this round of changes | workflow_status Phase L |

**Conclusion**: engineering quality is high and verification is thorough, **no refactor needed**. v0.10 improvements focus on five things:

1. **[P0] Real-time upstream model "policy contract"**: v0.9's `ModelMeta` is a static snapshot, but the upstream freebuff-models.ts is now a **time-window contract** (availability = always / deployment_hours / off_peak_only + unavailableFallback + efforts ladder + premium gating). During peak hours upstream closes models like DeepSeek on a schedule, and the gateway currently cannot anticipate this → a bare `model_unavailable`. This is the **biggest new debt** found in this verification round.
2. **Model catalog drift**: upstream has already added `mimo-v2.5` (free, unlimited, the upstream FALLBACK landing point) and the dynamic model `google/gemini-2.5-flash-lite`; the gateway's HARDCODED_MODELS/MODEL_META_ROWS don't cover them; the README's capability description needs to be synced.
3. **Panel accessibility/UX**: zero ARIA attributes across the whole panel, no keyboard navigation management, no aria-live on the live log; touch target size/contrast not audited. Functionality is complete, **accessibility is the "last mile"**.
4. **Tighten quality gates**: the CI coverage gate is still continue-on-error; E2E (tests/*.cjs) is not in CI; documentation drift (plans 0-project-overview-and-roadmap stuck at v0.7.3, API_GUIDE missing v0.9 endpoints).
5. **Release wrap-up**: a real Docker run, re-verifying the auto-update feed, cleaning up old artifacts (old Setup files in desktop/dist, test databases in data/, a strategy for the large promo video files).

> All changes are "additive"; the default configuration and behavior remain unchanged, and old config.json files need no migration.

---


## 1. Fixes (P0/P1/P2 · by priority)

### 1.1 [P0] Real-time upstream model "policy contract" (time windows + dynamic sync) — the biggest new debt found in this verification round

- **Evidence (upstream side)**: `上游本体（源代码等）/freebuff/common/src/constants/freebuff-models.ts` (4139 lines):
  - `availability: 'always' | 'deployment_hours' | 'off_peak_only'` (L59); `premium: boolean` (L81); `multimodal: boolean` (L84); `reasoningEffort` (L103) / `efforts` (L117) / `defaultEffort` (L124).
  - `freebuffModelUnavailableWindow()` / `freebuffModelUnavailableAt()` return a machine-readable ISO timestamp for "when it comes back"; `resolveAvailableFreebuffModel()` checks availability against the current time and falls back via `unavailableFallback` (in the function block at the end of the file).
  - The DeepSeek premium-price-window semantics are stated explicitly in comments: 00:00-10:00 UTC is the premium-price period; as of 2026-08-21 the V4 Flash row's availability was changed back to 'always' with an `unavailableFallback` (around L1338-1339, the comment notes "removed the peak-hours shutdown decision").
  - The `FREEBUFF_MIMO_V25_MODEL_ID` (L167) row has availability='always', premium=false, multimodal=true, and the comment explicitly states **"MiMo is the only UNLIMITED row, and is the landing point for FALLBACK_FREEBUFF_MODEL_ID"** (L1266-1271).
  - `KIMI_K3_ECO_MODEL` premium=true (L1694); `GPT_5_6_LUNA_ES_MODEL` premium=true, displayName='Codex (test)' (L1667-1686).
- **Evidence (gateway side)**: `src/models.rs`'s `MODEL_META_ROWS` is a **static table** (available is a boolean, no time window, no availableAt); `refresh_from_upstream` only syncs "model id additions/removals", not policy; models outside the table (this smoke test surfaced `google/gemini-2.5-flash-lite`, data=20 but meta=19) default to `model_available()=true`, which bypasses the pause list. `src/router.rs::resolve/resolve_available` is likewise unaware of time windows.
- **Impact**: a) during DeepSeek's peak hours/upstream deployment windows, the gateway still treats the model as available and selects it for the user → upstream returns `model_unavailable`/a bare 4xx, a poor experience that's hard to attribute; b) upstream has already moved some models into the premium pool and gated them behind `FREE_MODE_PREMIUM_RATE_LIMITS` (comment at freebuff-models.ts L1604-1611); if the gateway keeps calling them for free as before, it effectively leaves the user "exposed" to risk-control surface; c) the panel's "today's recommendation" ranking is based on stale availability.
- **Fix** (reuse models.rs's existing abstractions, don't build a parallel system):
  1. Add an `availability: 'always'|'deployment_hours'|'off_peak_only'` enum to `MetaRow`; change `available` to a derived result (`available = availability==always || currently within the window`), keeping the boolean field for backward-compatible serialization.
  2. Time-window function: DeepSeek's premium-price window is 00:00-10:00 UTC (per upstream comment semantics); deployment_hours has no machine-computable timestamp → for that window only give a human-readable hint, with `availableAt` returning null (aligned with upstream `freebuffModelUnavailableAt` behavior, not fabricating a time).
  3. On startup + on a schedule (suggested every 6 hours, configurable), incrementally sync upstream `freebuff-models.ts` (HTTP fetch; on failure, silently degrade to the static base and warn, **never blocking startup** — isomorphic with the existing `refresh_from_upstream`).
  4. `resolve()` / `resolve_available()` check the time window first; when unavailable, return a structured reason + `availableAt` (ISO).
  5. Add `availableAt`/explanatory text to `/v1/models`'s meta field (data stays compatible; clients ignore unknown fields).
  6. Vendor an upstream snapshot into `tests/fixtures/freebuff-models.ts` + a CI diff job (see 1.2).
- **Acceptance (TDD)**: a) ≥6 unit tests for the time-window function (inside/outside window, boundaries 00:00 and 10:00, deployment_hours, unknown availability); b) 4 sync-worker cases (successful merge, failure degrades with a warn, idempotent, meta preserved across id additions/removals); c) 3 routing cases (rejected and falls back outside the window, availableAt is readable, unchanged models don't flap); d) 1 E2E case (mock upstream with 2 models: one outside its window → /v1/models meta.available=false + fallback takes effect).

### 1.2 [P1] Full alignment of the model catalog and metadata + drift detection

- **Evidence**: this smoke test shows `/v1/models` data=20 entries (including `google/gemini-2.5-flash-lite`), meta=19 entries (the unit test asserting `MODEL_META_ROWS` fully covers HARDCODED_MODELS still passes, but "dynamically added models" have no meta); upstream has `mimo-v2.5` (free, unlimited, the FALLBACK landing point) which is missing from the gateway's list; the README_zh reasoning_effort matrix still lists `mimo-v2.5` as "no effort tiers supported", but the model isn't even in the list → both the docs and the implementation are inconsistent.
- **Fix**: a) decide on and add `mimo-v2.5` (suggested: add to HARDCODED_MODELS + MODEL_META_ROWS, efforts=None, multimodal=true, premium=false, available=true); b) settle the policy for "dynamically added models": default-available outside the table (current state) → change to "default-available outside the table + panel labels it 'policy unverified'", or maintain an editable whitelist; c) pin the upstream snapshot version: vendor it into `tests/fixtures/`, add a CI diff job, so upstream changes trigger a PR reminder, avoiding the static table silently going stale.
- **Acceptance**: the meta-table vs. vendored-snapshot consistency script (`scripts/check_model_contract.mjs`) passes; `meta_table_covers_all_hardcoded_models` stays green.

### 1.3 [P1] Panel accessibility fundamentals (starting from WCAG 2.1 Level A)

- **Evidence**: `src/web.rs` (INDEX_HTML, L10-1783) has **zero `aria-*` attributes across the whole file** (this run measured `HAS_ARIA=False`); tab navigation is a bare `<button data-tab onclick="showTab()">` (L125-135), with no `role=tablist/tab`, no `aria-selected`, no keyboard left/right switching; the test-bench send button has no aria-live announcement for its loading state; the live log stream has no `aria-live`; the only accommodations are `prefers-reduced-motion` (L33) and two @media breakpoints at 920/640 (L62/L111); touch target size (44px) has not been audited.
- **Impact**: screen-reader users cannot operate the core flows (importing credentials/the test bench/the health dashboard); keyboard users have no focus-visibility management for tab order.
- **Fix** (incremental, no refactor):
  1. Add `role=tablist` to the tab container and `role=tab` + `aria-selected` + `aria-controls` to the buttons; have showTab() keep these attributes in sync and support ←/→ switching plus Home/End;
  2. Add `aria-live=polite` to the live log area (keep the windowed rendering, only announce the count of new entries and error-level entries, to avoid flooding);
  3. A global focus-visible style (currently there is no :focus-visible rule);
  4. A minimum height of 44px for primary buttons/inputs (on mobile);
  5. High contrast: check error-red/warning-yellow against a white background item by item to pass WCAG AA (4.5:1).
- **Acceptance**: running axe-core in headless Chrome (`npx @axe-core/cli http://127.0.0.1:PORT/ui`) has 0 Level-A critical issues; a 12-item manual scripted checklist for the full keyboard flow (Tab order + left/right tab switching + Enter to send).

### 1.4 [P2] Readable degradation for the web Cookie pool when "fully cooling down / empty"

- **Evidence**: `src/web_pool.rs::pick()` (L206-238) returns None when everything is cooling down → the bridge path returns a bare 401/400 error directly (e.g. `handle_upload` L5399-5408 returns `multimodal_requires_web_cookie`); `snapshot()`'s `cooldown_until` is a `"{n}s"` string (L293, `cooldown_str`), so the panel cannot show it on a timeline.
- **Fix**: a) unify bridge errors into structured JSON: `{error:{type:'pool_exhausted', message:'all web accounts are cooling down (n of them), available again in about m seconds at the earliest', code:'web_pool_exhausted', meta:{cooldown_seconds_min}}}`; b) have `cooldown_until` output both an ISO timestamp and a remaining-seconds field (keep the old field for compatibility); c) add an "estimated recovery" sort to the panel's health dashboard.
- **Acceptance**: +2 web_pool unit tests (correct error metadata when fully cooling down, ISO timestamp parses); an E2E assertion on the pool_exhausted message text.

### 1.5 [P2] Narrow the panic surface on the request hot path (unwrap audit)

- **Evidence**: `src/api.rs` has about 7 runtime unwrap/expect calls (around L286/944/1287/1372/3266/3372/4018; the rest are all in test modules); `src/memory.rs` 50, `src/skills/store.rs` 45, `src/import.rs` 32, `src/mcp.rs` 27, `src/web_threads.rs` 25, `src/telemetry.rs` 21 (mostly in tests, but library code has some too). Zero clippy warnings doesn't cover "panic risk".
- **Fix**: a) change every unwrap on the hot path (chat/messages/bridge/upload) to `unwrap_or_default`/an error return; b) have background write paths (memory/skills/telemetry/import) swallow errors, degrade, and warn; c) add 2 fuzz-input unit tests (malformed header/body doesn't panic).
- **Acceptance**: the full `cargo test` passes + zero clippy warnings + new panic-regression test cases.

---


## 2. Experience/Features (P1/P2 · optional, can run in parallel)

### 2.1 [P1] Quota "futures view": how many more turns today (reuse existing data, zero new collection)

- **Evidence**: upstream `rateLimitsByModel` gives each account's remaining quota for today + `FREE_MODE_PREMIUM_SESSION_LIMIT=5` (freebuff-models.ts L890, resets on Pacific Day) + `FREEBUFF_PREMIUM_MODEL_IDS`; the gateway already has `account_meta::ModelQuota` (cached in cred_meta.json) and `/api/account/overview` (aggregating 4 upstream endpoints), plus the panel overview's "today's recommendation" card (sorted by remaining quota).
- **Fix**: convert "remaining count × average per-turn token consumption (the mean over the last 7 days of usage)" into "about N more turns today"; the recommendation card also notes "this model is unavailable during peak hours (availableAt)". Pure front-end computation + one aggregation endpoint, no upstream changes.
- **Acceptance**: E2E asserts the recommendation card's fields exist and are sane (no NaN/negative values).

### 2.2 [P2] "Semi-automatic" credential self-healing: alert on cooldown + one-click re-login (no auto re-login)

- **Evidence**: v0.9 already has 401/403 → a 10-minute `mark_cooldown` cooldown (web_pool.rs L260-271) and multiple desktop/panel login paths; but users currently "only discover everything is dead after opening the panel".
- **Fix**: a) add a "failed accounts" badge to the panel overview (the data already exists via `/api/accounts/health`); b) a desktop tray balloon alert (reuse tray.displayBalloon) + a tray menu item "re-login failed accounts" (opens the WebView2 login window); c) **explicitly not doing**: auto re-login (would conflict with upstream risk control/2FA; the README already honestly discloses this boundary).
- **Acceptance**: after simulating a 401, the health endpoint returns the failed state + the panel badge is visible; the tray alert text is correct.

### 2.3 [P2] Request-level failure attribution "top-3s" (slowest account / most-used model / highest error-rate time window)

- **Evidence**: telemetry.sqlite already stores request duration/event chains (TraceRow + event), and usage.sqlite stores model/error data; `/api/usage/cost` already has a 30-minute sliding window.
- **Fix**: add new aggregations to `/api/usage/insights`: top-3 slowest accounts (mean TTFT), top-5 most-used models, the hour with the highest error rate; a single "top-3s" card in the panel overview. All aggregation is local SQL, no new dependencies.
- **Acceptance**: after inserting known sample data, assert the aggregation is correct (unit test + 1 E2E case).

### 2.4 [P2] Fill out the multimodal upload experience

- **Evidence**: the test bench already supports drag-and-drop/paste/file-picker upload + automatic fallback to base64 on failure (v0.9); it's missing progress/validation/readable errors.
- **Fix**: a) an upload-progress bar (XHR or a fetch stream); b) upfront validation (a type whitelist of jpeg/png/gif/webp, a size limit of ≤20MB matching `/v1/uploads`'s cap, and empty-file checks); c) reuse the existing error style for failure toasts; d) compress large pasted images first (via canvas) before upload (saves upstream quota).
- **Acceptance**: a 6-item manual test checklist (drag/paste/pick/over-limit/bad type/no Cookie).

### 2.5 [P2] Live log page enhancements

- **Evidence**: windowed virtual scrolling was already implemented in v0.8 (smooth beyond 500 entries); it's missing filtering/pause/clear/export.
- **Fix**: a) level filtering (info/warn/error highlighting); b) a "pause scrolling" toggle (freezes the window so new logs don't interrupt viewing); c) one-click export of the current buffer (JSON/text, after redaction — LogBus already redacts, export can reuse it directly); d) an error-entry count badge.
- **Acceptance**: E2E asserts the filter/export DOM behavior (triggered by local events, no dependency on real upstream).

### 2.6 [P2] Fill in unavailability reasons on the overview's "today's recommendation"

- **Evidence**: v0.9's recommendation card is sorted by `rateLimitsByModel` remaining quota (paused models automatically rank lower); `router::unavailable_reason` (router.rs) already outputs a readable reason, but the panel doesn't consume it.
- **Fix**: attach "unavailability reason + availableAt" to each row of the recommendation card (consuming the meta extension from 1.1); label unverified models "policy unverified".
- **Acceptance**: the panel DOM assertion confirms the text appears.

---

## 3. Engineering/Quality/Process (P1/P2 · parallel with feature work)

### 3.1 [P1] Tighten the coverage gate (CI)

- **Evidence**: the `.github/workflows/build-release.yml` coverage job uses `cargo llvm-cov --fail-under-lines 80` but has **`continue-on-error: true`** (let through the first time to collect a baseline); `cargo llvm-cov` is not installed locally (as workflow_status notes).
- **Fix**: a) `cargo install cargo-llvm-cov` locally + run to get a real baseline number; b) once the baseline reaches 80%, remove continue-on-error; c) if llvm-cov is unstable on Windows, first collect coverage only for `cargo test --test router_test --test web_pool_test` and set `--fail-under-lines` on the key modules (models/router/web_pool/retry/errors).
- **Acceptance**: CI is green and the coverage number is written back into workflow_status and CHANGELOG.

### 3.2 [P1] Bring E2E into CI (currently only run locally)

- **Evidence**: `tests/e2e_phase_v0_9.cjs` (26 assertions), `e2e_phase_v0_8.cjs` (26 assertions), and `check_panel_js.cjs` are all absent from CI; CI currently only has cargo test/clippy/fmt + electron-builder.
- **Fix**: add a job to build-release.yml (windows-latest + Node 20): `cargo build --release` → start the gateway (temp port + empty config + skip_upstream_check) → `node tests/check_panel_js.cjs` + `node tests/e2e_phase_v0_8.cjs` + `node tests/e2e_phase_v0_9.cjs` (the scripts need to support a PORT env var; currently hardcoded to 47871, see the e2e scripts) → kill the process. The mock-upstream scenarios (router_test) already cover the real protocol; E2E only asserts against local endpoints.
- **Acceptance**: CI is fully green 3 times in a row; once the e2e scripts are parameterized with PORT, they can also be rerun locally on 47899.

### 3.3 [P1] Clear the documentation drift debt

- **Evidence** (verified item by item this round):
  - `plans/0-project-overview-and-roadmap.md` is stuck at **v0.7.3** (the capability map states "the Claude path has not yet integrated retry", but v0.8 already did; open-questions list items 1-3 are marked "pending" but have actually been closed out);
  - `docs/API_GUIDE.md` is missing the new v0.9 endpoints: `/api/export`, `/api/import`, `/api/accounts/health`, `/api/login/embed`, `/api/login/result`, and a description of the `/v1/models` meta field;
  - `README_zh.md`'s reasoning_effort matrix lists `mimo-v2.5` but it's not in the model list (see 1.2); README_zh's "multi-account round-robin" wording has already been updated for v0.9, but the paragraph "the Bearer account pool round-robins across all accounts; web Cookies are pooled the same way" needs to stay consistent with the catalog once 1.2 is done;
  - `workflow_status.md` has not opened a new phase after Phase L (v0.10 should create a new Phase M to carry this checklist forward).
- **Fix**: execute per the §8 release checklist; turn 0-project-overview-and-roadmap into a templated document that's "refreshed at every release"; auto-check the API_GUIDE endpoint table against build_router (api.rs L163-236) (a script that diffs the route table, to prevent future drift).
- **Acceptance**: `scripts/check_docs.mjs` passes (the route table, API_GUIDE, and README endpoints are all three aligned).

### 3.4 [P2] Scripting the test ledger + release checklist

- **Fix**: `docs/TESTING.md` records, as a fixed record: unit/integration/E2E test counts, the coverage baseline, headless rendering verification, and local environment limitations (rustdoc); `scripts/verify_release.ps1` runs everything in one go: fmt → clippy → test → check_panel_js → start the release build → healthz/ui/models assertions → kill the process.
- **Acceptance**: on a freshly cloned machine, the script completes all local verification within 20 minutes.

### 3.5 [P2] Add `cargo test --doc` to CI (the proper fix for the local environment limitation)

- **Evidence**: workflow_status records that local `cargo test --doc` fails (chocolatey Rust is missing rustdoc.exe); CI uses the official dtolnay toolchain (includes rustfmt/clippy, but the rust-docs component isn't installed).
- **Fix**: add the rust-docs component to CI + `cargo test --doc`; locally, switching to a rustup-based install is suggested (optional, not mandatory).
- **Acceptance**: CI is green.

### 3.6 [P2] Asset/old-artifact cleanup list (list first, **deletion needs your sign-off**)

- **Evidence**: `desktop/dist/` has both Setup-0.8.0.exe (85.7MB) and Setup-0.9.0.exe (85.7MB) + blockmap + latest.yml + win-unpacked/; there's also a top-level win-unpacked/ under `desktop/` (roughly a hundred MB); `data/` has leftover test databases (`freebuff2api2.sqlite`, `mock-demo.sqlite`, `rev_check.sqlite`, `rev_telemetry.sqlite`, `e2e/`); `宣传视频/` has about 60MB+ of multi-version mp4 files (old promo.mp4, etc.); `graft/.graph` and `graft/.cache` are rebuildable.
- **Suggestion** (three tiers by impact):
  - **Safe to delete**: the old Setup-0.8.0.* installer under desktop/dist (0.9.0 has already shipped, the feed only references the latest); the leftover test sqlite files in data/ (once confirmed nobody is using them);
  - **Needs your confirmation**: desktop/win-unpacked (can be repacked), old versions of the promo mp4s (just keep the final version), graft/.graph + .cache (rebuildable with graft build);
  - **Do not touch**: target/release (required for release), data/tokens.json / cred_meta.json / account_history.jsonl / the various sqlite files (runtime data), reference/reverse (reverse-engineering reference material).
- **Execution rule**: any deletion is snapshotted outside of git (moved into an `_archive_旧产物/` directory and observed for 7 days, rather than a direct Remove-Item); no large files get committed into the repo.

---


## 4. TDD checklist (write tests first → then implement → done only when all green)

> Write the test before landing each item; the acceptance command is always `cargo test` + `cargo clippy --all-targets -- -D warnings` + `cargo fmt --check`.

| ID | Related | Test file | # of cases | Key assertions |
|----|------|----------|--------|----------|
| T1.1 | 1.1 | src/models.rs (+tests/model_meta_test.rs) | ≥13 | Time-window function: 6 (inside/outside/boundary 00:00\|10:00/deployment_hours/unknown); sync worker: 4 (successful merge/failure degrades with warn/idempotent/meta preserved); routing: 3 (falls back outside the window/availableAt is readable/no flapping) |
| T1.2 | 1.2 | tests/model_meta_test.rs + scripts/check_model_contract.mjs | 4+2 | meta is correct once mimo-v2.5 is added; out-of-table models get the "unverified" label; the snapshot-consistency script works both ways (has diff/no diff) |
| T1.3 | 1.3 | tests/check_panel_js.cjs + a new axe script | 3 | panel JS passes syntax check; axe Level-A shows 0 issues; 8 scripted keyboard assertions (aria attributes present/tab focus visible) |
| T1.4 | 1.4 | src/web_pool.rs + tests/e2e_phase_v0_10.cjs (new) | 5 | error metadata when fully cooling down; ISO is parseable; remaining seconds; pool_state aggregation; E2E asserts the error-code text |
| T1.5 | 1.5 | src/api.rs etc. | 3 | bad token/bad skill file/empty-body upload doesn't panic, HTTP status codes are correct |
| T2.1 | 2.1 | unit tests for the new conversion module | 4 | 0/very small/very large input; premium pool vs. free pool distinction; NaN/negative-value guarding |
| T2.2 | 2.2 | tests/e2e_phase_v0_10.cjs | 2 | failed-account badge DOM; tray alert text |
| T2.3 | 2.3 | src/telemetry.rs + tests | 3 | top-3s aggregation ordering; hourly windowing; doesn't crash on empty data |
| T2.4 | 2.4 | tests/e2e_phase_v0_10.cjs | 2 | client-side blocking when over the limit; upload disabled when no Cookie is imported |
| T2.5 | 2.5 | tests/e2e_phase_v0_10.cjs | 2 | filter/export DOM behavior |
| T3.2 | 3.2 | tests/e2e_phase_v0_8.cjs / v0_9 regression | regression | none of the existing assertions regress (26+26) |
| T3.6 | 3.6 | scripts/check_artifacts.ps1 | 1 | scans the old-artifact list (reports only, doesn't delete) |

---

## 5. Risks and dependencies

- **1.1 (policy real-time sync)**: the upstream raw file's path/format may change → the sync worker must silently degrade + warn, and must never block startup; the time window is implemented per the upstream comment semantics; if upstream changes the window definition, this needs versioning (backstopped by the vendored snapshot + CI alerts). **Multi-account round-robin ≠ bypassing upstream rate limits; the docs must not claim "unlimited concurrency"**; no auto re-login, no bypassing risk control.
- **1.3 (accessibility)**: when changing aria attributes, don't break the windowed rendering or the existing e2e assertions → run the v0_8/v0_9 regression before merging.
- **1.5 (unwrap audit)**: keep behavior consistent when changing library code; before using `unwrap_or_default`, confirm the default value's semantics don't mask a real error (log first, then default is recommended).
- **3.2 (E2E into CI)**: the gateway process started on the Windows runner needs reliable cleanup (try/finally kill), otherwise it pollutes subsequent jobs.
- **3.6 (cleanup)**: you must confirm each item before deletion; use "move into _archive_ and observe" rather than deleting directly.

---

## 6. Explicitly not doing (Backlog / scope control)

- Not refactoring `api.rs` (splitting the 229KB monolith is deferred to v1.0, this document doesn't touch it).
- Not doing model training/fine-tuning, distributed/multi-machine setups, switching the Rust async framework, or DB migration.
- Not auto re-logging-in credentials, not bypassing upstream rate limits, not doing cfworker TLS fingerprint bypassing (already honestly disclosed in the README).
- SSH batch-committing (the benefit/risk ratio isn't worth it, stays in the backlog).
- Enhancing the desktop version's multi-account high-concurrency round-robin (recorded as P3; a risk-control boundary).
- Not claiming desktop capability improvements before the "4/6/8-second Bernini contract and type-check convergence" has been proven out (following the repo's existing release discipline).

---

## 7. Release checklist (go through this every time v0.10.0 is released)

- [ ] `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + `cargo test` (including tests/) all green
- [ ] after tightening the coverage gate, `cargo llvm-cov --fail-under-lines 80` is all green (the number is written back into workflow_status)
- [ ] once the E2E job is added to CI, it's all green 3 times in a row
- [ ] `Cargo.toml` / `desktop/package.json` / `CHANGELOG.md` version all in sync (0.10.0)
- [ ] README (Chinese/English) capability descriptions match the implementation (model catalog, the dual-pool multi-account round-robin, "out-of-table model" labeling)
- [ ] `docs/API_GUIDE.md` fills in the v0.9+v0.10 endpoints; `config.example.json` is fully aligned field-for-field with `config.rs`
- [ ] `plans/0-project-overview-and-roadmap.md` is refreshed to the v0.10 baseline (no longer v0.7.3)
- [ ] `workflow_status.md` opens Phase M and backfills evidence
- [ ] the desktop `electron-builder` artifacts + latest.yml auto-update feed are verified (0.10.0)
- [ ] old-artifact cleanup confirmed (§3.6, executed per your sign-off)
- [ ] a real Docker run: `docker/build` + healthz 200 inside the container (a new wrap-up item; docker/ currently only has a Dockerfile and hasn't been run for real)

---

## 8. Suggested execution order (each batch is an independent, revertible commit)

```
Batch 1 (highest risk first, tests before implementation): T1.1+T1.2 → 1.1 model policy real-time sync → 1.2 catalog alignment → 1.4 pool degradation
Batch 2 (panel, can run in parallel with batch 1): 1.3 accessibility → 2.1/2.2/2.6 overview and account experience
Batch 3 (observability + quality): 2.3/2.4/2.5 → 3.1 tighten coverage → 3.2 E2E into CI → 3.3 clear documentation debt
Batch 4 (wrap-up): 3.4/3.5/3.6 → release checklist §7 → Phase M update → version triple-sync + release
```

> Why this order: model policy is the foundation of data/routing correctness (get the T1.1 tests as a safety net before touching models.rs); the panel is independent and can run in parallel; quality gates are tightened after feature work converges, to avoid "padding numbers just to hit the bar"; cleanup always comes last and needs your confirmation.

---

## Appendix A: evidence index (file:line from this verification round)

- Static model table/availability: `src/models.rs` (HARDCODED_MODELS L15-36; MODEL_META_ROWS; model_available defaults to available for unknowns)
- Routing/efforts: `src/router.rs` (resolve/resolve_available/unavailable_reason/reasoning_efforts/clamp_effort)
- Upstream policy contract: `上游本体（源代码等）/freebuff/common/src/constants/freebuff-models.ts` (availability type L59; premium L81; multimodal L84; reasoningEffort L103; efforts L117; MIMO L1255-1283; the V4 Flash row L1285+; the availability+unavailableFallback block at L1338-1339; KIMI premium L1694; Luna-ES L1667-1686; the FREE_MODE_PREMIUM_RATE_LIMITS comment at L1604-1611; FREEBUFF_PREMIUM_SESSION_LIMIT L890)
- web Cookie pool: `src/web_pool.rs` (build_entries L111-136; pick L206-238; mark_ok/failure/cooldown L241-271; snapshot L274-299)
- Bearer pool/circuit breaker: `src/pool.rs` (CircuitBreaker allow/trip/trip_for; pick_best L227-249)
- Route table and auth: `src/api.rs` (AppState L33-59; admin_authorized L90-99; origin_allowed L107-120; inject_peer L128-144; is_loopback_request L150-161; build_router L163-236; CONFIG_EDITABLE L2309-2323; handle_upload L5381+; pick_web_cookie L5525-5528; web_pool_failure L5530-5540)
- Panel: `src/web.rs` (INDEX_HTML L10-1783; tabs L125-135; playSend L597+; windowed L1640+; @media L62/L111; prefers-reduced-motion L33) — this round measured HAS_ARIA=False
- Desktop shell: `desktop/main.js` (single instance L18; gateway spawn L161; login window L286-322; autoUpdater L354-377)
- CI: `.github/workflows/build-release.yml` (coverage continue-on-error:true; test/clippy/fmt gates)
- Baseline smoke test (this run): healthz model_count=20 / /v1/models data=20 meta=19 / doctor 10 items / export schema v1 / cross-origin 401 / ready in 1.9s

## Appendix B: this review's methodology (2026-09-19)

1. Inventory of the project structure (excluding caches and binaries like node_modules/target/.git/.codegraph); a full `graft skeleton` function listing across all 26 src files (~88k tokens of context compressed by graft).
2. Close reading of the core files: models.rs / router.rs / config.rs / pool.rs / web_pool.rs / main.rs / api.rs (L33-236 security+routing, L2309-2323 config whitelist, L5381-5411 upload auth, L5525-5540 web pool integration) / CHANGELOG / workflow_status / README: zh / config.example.
3. Upstream comparison: cross-checked `上游本体/…/freebuff/common/src/constants/freebuff-models.ts` (4139 lines) contract fields item by item against the gateway's MODEL_META_ROWS / README matrix, producing the drift evidence in 1.1/1.2.
4. Real execution: the release binary + a temp config (temporary listen port 47899, empty credentials) → six assertions across healthz/ui/models/doctor/export/cross-origin 401, ready in 1.9s.
5. Documentation-drift checklist: checked item by item — plans 0-project-overview-and-roadmap (v0.7.3), API_GUIDE (missing v0.9 endpoints), README (the mimo matrix vs. the catalog).
6. Not done (honest disclosure): the full cargo test suite was not rerun locally (reusing the 09-18 Phase L results); `cargo test --doc` is unavailable in the local environment.

---

*Generated: 2026-09-19 · based on real testing of v0.9.0 code (6-item release smoke test + comparison against upstream freebuff-models.ts + graft skeleton) · previous version archived to 《next-steps-guide-archive-v0.8.0-v0.9.0.md》*
