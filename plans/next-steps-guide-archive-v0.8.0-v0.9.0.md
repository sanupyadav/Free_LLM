# Next-Step Improvement Guide (Freebuff2API v0.8.0 → v0.9.x)

> This document is the **actionable improvement checklist** (the core deliverable). Each item includes: priority, problem/evidence, change location, behavior diff, acceptance criteria (TDD checklist in §4).
> The previous version (v0.7.3→v0.8.0) has been fully executed and archived: `plans/next-steps-guide-archive-v0.7.3-v0.8.0.md`.
> Companion reference: `plans/0-project-overview-and-roadmap.md`, `6-feature-expansion.md` (this document is the main execution entry point for v0.9).
> Document scope: **plans and proposals only, no code changes**; implementation is carried out by the AI you choose, and this document provides enough evidence and acceptance criteria to avoid "changing things from memory".

---

## 0. Overview (v0.8.0 measured baseline, all verified this round)

| Item | Value | Evidence |
|----|----|------|
| Version | **0.8.0** | Cargo.toml / desktop/package.json / CHANGELOG all in sync; HEAD=ef15ebc (git log) |
| Unit tests | **238 passed / 0 failed** | measured via `cargo test --lib` (2026-09-17, local machine) |
| Integration tests | `tests/core_test.rs` 8 items all green | measured via `cargo test --test core_test` |
| Router-level integration | `tests/router_test.rs` 10 items all green | measured via `cargo test --test router_test` (mock upstream) |
| Lint | `cargo clippy --all-targets -- -D warnings` zero warnings; fmt passes | measured |
| Real gateway smoke test | healthz 200 (20 models), /ui 200, /v1/models 200 | measured with the release exe + a temp config; /ui carries CSP/nosniff/Referrer-Policy |
| Working tree | clean (no uncommitted changes) | git status |

**Conclusion**: engineering quality is high and verification is thorough, **no refactor needed**. v0.9 improvements focus on six things:

1. **Missing web Cookie multi-account capability** (the README already admits "the bridge path uses the first valid credential"; the account pool only covers Bearer) — this version's biggest capability gap.
2. **Missing dynamic upstream model "catalog/policy" contract**: the gateway only syncs model additions/removals, not the availability window, the efforts ladder, the multimodal flag, or unavailableFallback (new debt found by this round's comparison against the upstream source).
3. **Panel UX leap**: multimodal/multi-turn test bench, model recommendations, account health dashboard, full config export/import.
4. **Security depth**: panel auth boundaries for the 0.0.0.0-listen scenario, config-import validation.
5. **Quality gates**: a coverage gate, adding fmt to CI, documentation number drift, archiving old acceptance reports.
6. **Release wrap-up**: a real Docker run, the desktop auto-update feed, promotional assets, cleaning up old artifacts.

> All changes are "additive"; the default configuration and behavior remain unchanged, and old config.json files need no migration.

---

## 1. Fixes (P0/P1 · can be done immediately)

### 1.1 [P1] web Cookie credential pooling + multi-account round-robin + health scoring (top priority)
- **Evidence**: README_zh explicitly states "web Cookie credentials are currently handled by the bridge path using the first valid credential"; `src/api.rs::pick_web_cookie` (L5179)'s logic is: the first entry in `config.auth_tokens` containing `session-token` → otherwise the **first** matching entry in the imported `tokens.json` library. There is no health score, no 401 cooldown, no round-robin, and no per-account failure isolation; `src/pool.rs`'s Bearer account pool (scoring + three-state circuit breaking + exponential cooldown) doesn't cover web Cookies at all. The api.rs L1521 comment confirms "putting a Cookie into the Bearer pool will always fail and trip the breaker; web Cookies are handled by the bridge path".
- **Impact**: after a user imports multiple web accounts via "one-click browser login", the moment the first one fails, the entire chain gets 401s (upstream rejects with `free_mode_cli_required`/`auth`); the panel's account page shows no health feedback for web accounts; this doesn't match the README's claim of "automatic multi-account round-robin, health scoring, failure cooldown" (that claim only holds for Bearer).
- **Fix** (suggest reusing the existing pool abstraction, don't build a parallel system):
  1. Fold `ExtractedAuth{kind:"web-cookie"}` entries into a `WebCookiePool` (a new file, `src/web_pool.rs`), with fields: cookie, a stable id (the existing `import::cred_id`), health_score, a three-state circuit breaker, cooldown_until, last_error, last_ok_at.
  2. Change `pick_web_cookie` to pick from the pool by "not tripped + highest score + cooldown expired"; on request failure (401/403/network) → lower the score/trip the breaker/cool down (reusing `pool.rs`'s `mark_failure` semantics and `tier_is_sub` check).
  3. Route all three chains through the pool uniformly: the `/v1/chat/completions` web bridge (around api.rs L830), the `/v1/messages` bridge (around L3132), `/api/account/balance` (L334), and `/api/account/detail` (L2220).
  4. Have the panel's account page render web accounts into the list too (currently it only shows the Bearer pool).
- **Acceptance**: a) import 2 web Cookies (one good, one bad), and consecutive requests all land on the good account (verified via mock-upstream call counts); b) after the bad one gets a 401 it enters cooldown, is logged, and its status is visible in the panel; c) it automatically recovers once the cooldown expires; d) unit tests cover selection/circuit-breaking/cooldown/recovery (≥8 cases).
- **See also**: `6-feature-expansion.md §6.3 Multi-upstream gateway` (abstracting `UpstreamProvider` is a v1.0 direction; this item delivers the first 80% of that value).

### 1.2 [P1] Dynamic upstream model "catalog + policy" contract (new debt found in this round's verification)
- **Evidence**: `src/models.rs` only has a static `HARDCODED_MODELS` + `refresh_from_upstream` (pulling `CodebuffAI/codebuff/common/src/constants/free-agents.ts`, doing only model id additions/removals); whereas the local upstream source `freebuff/common/src/constants/freebuff-models.ts` has, per model row, **availability (always/deployment_hours/off_peak_only), a reasoningEffort ladder, a multimodal flag, premium, unavailableFallback**, plus `freebuff-peak-hours.ts` (DeepSeek's peak window 00:00-10:00 UTC at double price) and `resolveAvailableFreebuffModel` (falls back per `fallback` when closed).
- **Drift already observed** (stated in black and white in upstream comments): GLM 5.3 Flash defaults to and pins `reasoningEffort` at `high`, and **`max` has been removed from its ladder** (the gateway's matrix still allows max); DeepSeek V4 Pro has been withdrawn (the gateway's list still carries it); Muse Spark 1.3 has been 404'd/delisted, Ox Alpha/Gemini 3.8 Flash have been paused, yet the gateway's list still exposes them; MiMo 2.5 is gated behind a toggle in the upstream UI.
- **Impact**: clients pick "paused/delisted" models via `/v1/models` → upstream returns 403/404, and the gateway just passes the error through, a poor experience; effort clamping only kicks in when out of range, and the ladder itself doesn't match upstream.
- **Fix**:
  1. Expand `models.rs` into a **model metadata registry**: `ModelMeta { id, agent, premium, multimodal, available, efforts: Vec<String>, fallback: Option<String> }`; fill out the metadata in the hardcoded base, and have `refresh_from_upstream` parse upstream `free-agents.ts` **and optionally fetch the policy rows from `freebuff-models.ts`** (also via GitHub raw).
  2. Add an **availability check** in the routing layer (`router.rs`) before "model resolution → effort clamping → account selection": for an unavailable model, either a) return a readable error (`model_unavailable` + `availableAt` if upstream provides it), or b) automatically fall back per `unavailableFallback` and label it as such. Default to b) (consistent with upstream behavior).
  3. Have `/v1/models` output `multimodal`/`premium`/`availability` metadata, for the panel's test bench and recommendation logic to consume.
- **Acceptance**: a) unit test: mock upstream changing availability → the gateway follows suit (closed → falls back/errors); b) the efforts ladder matches upstream (GLM has no max, etc.); c) `/v1/models` includes the metadata fields; d) a real-upstream smoke test: requesting a paused model gets a readable error instead of a bare 403.
- **Risk**: the upstream file path may change — consolidate the raw URL into a constant and add a fallback for parse failures (keep the hardcoded base usable); don't let a failed fetch affect startup.

### 1.3 [P1] Align the reasoning_effort ladder with upstream
- **Evidence**: the static `clamp_effort` matrix around `src/api.rs` L2431 (README's table of low/high/max, low..max, minimal..xhigh); each row in upstream `freebuff-models.ts` has its own independent ladder, and **the ladders get adjusted operationally** (GLM 5.3 pinning to high/removing max is one example). A static matrix is bound to drift.
- **Fix**: fold this into §1.2's `ModelMeta.efforts`; have `clamp_effort` read the metadata; models with no efforts field have the field stripped (keeping the current logic).
- **Acceptance**: unit tests cover "downgrade when out of the ladder / stripped when there's no ladder / the alignment table matches upstream".

### 1.4 [P1] Panel test bench: multimodal + multi-turn + effort + model metadata
- **Evidence**: `src/web.rs`'s `playSend()` (around L520) only builds a **single user text message**: no images, no conversation history, no reasoning_effort, no system prompt, no copy button; whereas the gateway **already has a multimodal upload API** (`/v1/uploads`, around api.rs L5069, requires a web Cookie), which the panel doesn't expose at all.
- **Fix** (in the panel's `src/web.rs` test-bench tab):
  1. Image upload: drag/paste into the input area → call `/v1/uploads` (when a web Cookie is available) or the `images` field of `/v1/chat/completions` → thumbnail + removable; disabled with a hint when there's no Cookie.
  2. Multi-turn conversation: keep a messages array in memory, supporting "clear conversation / copy the whole thing / export as Markdown".
  3. An effort dropdown: rendered dynamically from the metadata returned by `/v1/models` (§1.2), linked to model switching.
  4. A system prompt input box (optional, empty by default).
  5. Add "copy" and "regenerate" to the result area; a send/stop button state machine (partially implemented in v0.8, fill in the missing feedback).
- **Acceptance**: click through in a real browser (edge/headless): pick a model → upload an image → streamed reply → copy; the image control is disabled with a hint when there's no Cookie; no overflow at 320px.
- **See also**: the interaction spec in `plans/3-panel-modernization.md`.

### 1.5 [P2] Panel auth depth when listening on 0.0.0.0
- **Evidence**: `src/api.rs::admin_authorized` (L88): when `api_keys` is empty, it only checks `is_loopback_request` (**which looks at whether the X-Forwarded-For/X-Real-IP headers are present, not the actual TCP source address**); `origin_allowed` lets through the local-machine Origin. If a user changes `listen_addr` to `0.0.0.0` (the README explicitly suggests "switch to 0.0.0.0 only when serving externally") without configuring `api_keys`, **a client on the same LAN connecting directly without proxy headers can access the panel and admin endpoints** (change config/view credentials).
- **Impact**: this is a "depth gap in a configuration-guided scenario", not a default-configuration vulnerability (only lets through with the default 127.0.0.1 + no XFF). Rated P2; primarily add a warning, don't change default behavior.
- **Fix**:
  1. When the settings page changes `listen_addr` to `0.0.0.0`, pop a strong confirmation ("visible on the LAN; please also set an API Key");
  2. Add a **real TCP source address** check to `is_loopback_request` (`ConnectInfo` + `SocketAddr`, only counting as local when `peer.ip().is_loopback()`) — doesn't change default behavior, only tightens the 0.0.0.0 scenario;
  3. Add a warning item to the health check (`/api/doctor`) for "listening on a non-loopback address but api_keys isn't configured".
- **Acceptance**: bound to 0.0.0.0 + no key: the local machine can access it; a simulated remote peer (a request constructed with a non-loopback address) gets 401/403 (Router test); the panel warning is visible.

### 1.6 [P2] Archive old acceptance/delivery docs + fix numbers
- **Evidence**: `README.md` L103 states "236 unit tests", but measured is **238**; `FINAL_ACCEPTANCE.md`/`DELIVERY_SUMMARY.md`/`ROUND2_SUMMARY.md` are old drafts **from the v0.1.0 era** (listing port 8787, 19 unit tests, 20 models, etc.), badly out of sync with the current v0.8.0 state, and likely to mislead whichever AI picks this up.
- **Fix**: change the README's test numbers to `238 + 8 + 10`; move the three old reports into `docs/archive/` and note "v0.1.0 historical"; add "v0.9 in progress" to the top of `workflow_status.md` (or have the implementing AI update it when wrapping up Phase L).
- **Acceptance**: `rg "236|8787" README*.md docs` finds nothing left over; the archive directory exists.

---

## 2. Optimizations (P1/P2 · experience and operations)

### 2.1 Account health dashboard (P2)
- **Evidence**: already listed in `6-feature-expansion.md §6.2`; the account page (the account tab in `src/web.rs`) only has a list + check/detail/delete, with no circuit-breaker/cooldown/error timeline.
- **Fix**: reuse `data/account_history.jsonl` (config `account_history_path`) + telemetry's account events, add a timeline to the account-detail drawer (a timed bar of trips/cooldown/429/401/last_error), and aggregate the "top-3 most recent errors" per account.
- **Acceptance**: inject 20 fake history entries → the timeline renders correctly; empty-state text when there's no history. Note: web Cookie accounts (once pooled per §1.1) also need to show up in this table.

### 2.2 Model recommendations + quota futures (P2)
- **Evidence**: `/api/account/balance`'s `rateLimitsByModel` (today's remaining quota) + usage latency data (`/api/usage/*`) already exist; the data is all there, only the display is missing; `6-feature-expansion.md §6.4 Innovation point 1`.
- **Fix**: add a "how many more turns today" card and a "today's best model" card (a combined ranking of remaining quota × price × recent-30-minute latency) to the overview page. The "futures" framing must be honest: estimated as remaining-today × roughly 2k tokens/turn, labeled "an estimate".
- **Acceptance**: shows a real estimate when there are accounts; shows an import prompt when there are none; the estimate formula is unit-testable.

### 2.3 Full config export/import (P2, for migrating to a new machine)
- **Evidence**: `6-feature-expansion.md §6.2`; config + credentials + skills + memory are scattered across multiple files, and migrating to a new machine currently relies entirely on manual copying.
- **Fix**: add `POST /api/export` (returns a JSON or zip containing config (after redaction)/tokens/the skills directory/optionally memory) + `POST /api/import` (validates schema/version/size limits then writes atomically; credential encryption is optional — suggest v0.9 ships plaintext + zstd compression first, with encryption left as a backlog item). Add export/import buttons to the panel's "settings" page; before importing, show which items will be overwritten and require a second confirmation.
- **Acceptance**: export → clear tokens → import → restart → the account pool/skills/memory are restored consistently (asserted by an E2E script).

### 2.4 Request-detail timeline + "top-3s" (P2)
- **Evidence**: `6-feature-expansion.md §6.2/§6.4`; telemetry already stores TTFT/total duration/bytes/the event chain, and the request-detail page (already present in v0.8) can be extended.
- **Fix**: add a mini timeline to the request detail view (upstream connect → first byte → completion), plus three "slowest account / most-used model / highest error-rate time window" cards.
- **Acceptance**: a single request's detail view renders the timeline; the top-3s data matches the usage stats.

### 2.5 Panel interaction/accessibility audit items (cross-cutting)
- Every clickable control gives feedback (disabled/loading/success/error toast) — mostly already in place, needs a dedicated pass;
- Empty states/loading skeletons: needed for the account list, logs, memory, and request list (some already exist);
- Keyboard accessibility: Tab order, `focus-visible` (already added in v0.8), Esc closes drawers, Enter submits;
- Mobile at 320px: horizontal nav scrolling already exists; check table horizontal scrolling and that touch targets are ≥44px;
- `prefers-reduced-motion` (already respected in v0.8) stays respected.
- **Acceptance**: headless Edge + keyboard Tab through the main tabs; manual review of screenshots at the 320/768/1024 breakpoints.

---

## 3. Operations and release

### 3.1 A real Docker run verification (P2)
- **Evidence**: `FINAL_ACCEPTANCE.md`/`DELIVERY_SUMMARY.md` both admit "no docker locally, never run for real"; CI has a docker.yml.
- **Action**: once a Docker environment is available, run `docker build -f docker/Dockerfile .` → start the container → healthz 200 → mount a data volume → demonstrate overriding `listen_addr` via an environment variable. Record the results in workflow_status.
- **Acceptance**: the build log + a screenshot/output of healthz 200 are committed.

### 3.2 Desktop auto-update feed E2E (P2)
- **Evidence**: v0.8 fixed the NSIS artifactName (ef15ebc); the `electron-updater` dependency is present, but the latest.yml feed has never been verified end to end.
- **Action**: run a local static file server hosting `desktop/dist/*` + `latest.yml` → install the old version → trigger "check for updates" → observe the download/install/version-change logs.
- **Acceptance**: the update flow's logs are complete; the tray gives a clear notice on failure.

### 3.3 Fill out the CI gates (P1)
- **Evidence**: `.github/workflows/build-release.yml` already has test + clippy (pinned to version 1.95.0, with caching and error reporting); **it's missing `cargo fmt --check` and a coverage gate**; `workflow_status.md` notes llvm-cov isn't installed, and `--fail-under-lines 80` has already been written into the CI suggestion.
- **Action**: a) add `cargo fmt --check` to CI; b) add a new job/step that installs `cargo llvm-cov` (`cargo install cargo-llvm-cov`) and runs `cargo llvm-cov --fail-under-lines 80`; c) install llvm-cov locally first to produce the v0.8 coverage baseline, avoiding an immediate red on the first merge.
- **Acceptance**: CI is all green including fmt+coverage; the local coverage report is archived at `docs/coverage-v0.9.md`.

### 3.4 Promotional asset updates (P3)
- **Evidence**: `宣传视频/` at 43MB is the old version; already listed in `6-feature-expansion.md §6.5`.
- **Action**: update the screenshots (including the new test-bench/settings/about pages) + re-render or remove the promo video, to avoid misleading the Release page.

### 3.5 Old-artifact cleanup (P3, **requires human confirmation before execution, do not delete on your own**)
- Candidates: `legacy-go/` (a 22MB archive, delete if already preserved in Git), `reference/` (71MB of reverse-engineered source, deletable if already archived in Git), `宣传视频/` (43MB, update or remove), `target/` (build cache, not committed).
- **Action**: first confirm in git **whether these directories are already committed** (`git ls-files legacy-go | head`); if they are, deletion needs to be handled together with the git history (`git rm -r` + commit), **do not use `git filter-branch`/force push**; if not committed, just delete them directly.
- **Acceptance**: repo size decreases; the README's directory-structure section is kept in sync.

---

## 4. Testing and quality (TDD checklist, write tests before implementing)

| Item | Unit tests (write first) | Integration/Router | E2E/real | Acceptance |
|----|--------------|-------------|----------|----------|
| §1.1 web Cookie pool | selection/circuit-breaking/cooldown/recovery/empty pool ≥8 cases | Router: two Cookies, one good one bad → all traffic goes to the good one, the bad one cools down after a 401 | works end to end with a real dual-Cookie import | web account health is visible on the panel's account page |
| §1.2 model metadata | the parser (free-agents + freebuff-models policy rows), unavailable fallback, the fallback chain | Router: model unavailable → a readable error/fallback | /v1/models includes the metadata | matches the upstream policy table |
| §1.3 efforts alignment | clamp-ladder/stripping regression + the new ladder table | — | picking a paused model gets a readable error | the README table is in sync |
| §1.4 test bench | JS logic extracted into functions (if any) | — | browser image upload/multi-turn/effort/copy | click feedback + 320px |
| §1.5 auth depth | unit test for the peer-address check | Router: 0.0.0.0 + a non-loopback peer → 403 | the panel warning is visible | default behavior regression stays unchanged |
| §2.1 account dashboard | history parsing/aggregation unit tests | — | timeline rendering | empty state + 20-entry injection |
| §2.2 recommendations | estimate-formula unit tests | — | card data matches balance | empty state with no accounts |
| §2.3 export/import | serialization round-trip unit tests | Router: export → import → validate | consistent after a real restart | idempotency/limits/overwrite confirmation |
| §2.4 request timeline | telemetry field-mapping unit tests | — | detail-page rendering | matches usage |
| §3.3 coverage | — | — | CI green + a report | ≥80% |

Regression baseline (must not drop below): `cargo test --lib` 238, `--test core_test` 8, `--test router_test` 10, clippy 0 warnings, fmt passes, `tests/e2e_phase_v0_8.cjs` (26 assertions) all green.

---

## 5. Suggested execution order (to avoid compounding bugs)

```
Batch 1 (purely additive, doesn't touch core behavior, lowest risk): 1.6 doc fixes / 2.2 recommendations / 2.4 timeline / 3.4 promo assets / 3.5 old artifacts (needs human confirmation)
Batch 2 (core chains, tests first): 1.2 model metadata contract → 1.3 efforts alignment (depends on 1.2) → 1.1 web Cookie pool (reusing the pool abstraction)
Batch 3 (panel, independent, can run in parallel): 1.4 test-bench enhancements / 2.1 account dashboard / 2.3 export/import / 2.5 accessibility
Batch 4 (security + quality): 1.5 auth depth / 3.1 Docker / 3.2 auto-update / 3.3 coverage and CI gates
Batch 5 (wrap-up): README/API_GUIDE/CHANGELOG/workflow_status + version triple-sync + release (§8)
```

> Why this order: clear out documentation and small debt first (no behavior changes) → then tackle the two core chains "model policy + web credentials" (highest risk, must have §4 tests in place first) → the panel progresses independently → security and quality gates → a unified release. Each batch is an independent, revertible commit.

---

## 6. Risks and dependencies

- **web Cookie pool (1.1)**: high-frequency round-robin across multiple accounts might trigger upstream risk control. **Must** preserve the existing "per-account cooldown + exponential backoff" semantics, and not raise default concurrency; test with mocks before going live. Honest note: multi-account round-robin ≠ bypassing upstream rate limits; the docs must not claim "unlimited concurrency".
- **Model metadata (1.2)**: the upstream raw file path may change → a failed fetch must silently degrade to the hardcoded base and warn, **never blocking startup**; for model ids upstream has "paused but retained", the gateway should keep the ability to recognize them (consistent with upstream `FREEBUFF_PAUSED_FREE_MODEL_IDS` semantics) so it can give a readable error rather than a bare 404.
- **Auth depth (1.5)**: adding `ConnectInfo` changes the handler signature and the existing Router tests — the request construction in `tests/router_test.rs` needs to be updated in step (constructing the peer directly via `TestClient` or mocking it).
- **Export/import (2.3)**: importing credentials will overwrite local data — a second confirmation is required + an automatic backup to `data/backup-<ts>/` before importing.
- **Coverage gate (3.3)**: the first local run of `cargo llvm-cov` may be slow and affected by Windows (fallback: add only the fmt gate first, run coverage as a separate CI job with `continue-on-error` for one version before tightening it).

---

## 7. Explicitly not doing (Backlog / scope control)

- Not refactoring `api.rs` (splitting the 229KB monolith is a v1.0 topic, not touched here).
- Not doing model training/fine-tuning, not doing distributed/multi-machine clustering, not switching the Rust async framework, not doing DB migration.
- cfworker isn't usable in production (upstream's TLS fingerprinting rejects it with `free_mode_cli_required`): keep the "experimental reference" label, don't invest in bypassing it (may violate upstream's rules, and the README already honestly discloses this).
- SSE batch-committing (the benefit/risk ratio isn't worth it, stays in the backlog).
- Enhancing desktop multi-account round-robin, a cryptographic key-rotation grace period (recorded as P3).

---

## 8. Release checklist (go through this every time v0.9.0 is released)

- [ ] `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + `cargo test` (including tests/) all green
- [ ] `Cargo.toml` / `desktop/package.json` / `CHANGELOG.md` version all in sync (0.9.0)
- [ ] README (Chinese/English) capability descriptions match the implementation (especially the "web multi-account" and "model policy" sections; write them back after making the changes)
- [ ] `docs/API_GUIDE.md` adds the new endpoints (export/import, documenting uploads, new config fields)
- [ ] `config.example.json` is synced with the newly added config items
- [ ] `workflow_status.md` is wrapped up (Phase L fully DONE + evidence)
- [ ] the desktop `electron-builder` artifacts + the auto-update feed are verified (§3.2)
- [ ] old-artifact cleanup confirmed (§3.5)
- [ ] the new CI gates (fmt/coverage) are all green

---

## Appendix A: evidence index (file:line from this verification round)

- The account pool covers only Bearer: `src/pool.rs` (the Pool struct + mark_failure); `src/api.rs:1521` (the comment that a Cookie in the Bearer pool always trips the breaker)
- web Cookie single-selection: `src/api.rs:5179` `pick_web_cookie` (config takes priority → otherwise the first entry in the imported library)
- Static model list: `src/models.rs` `HARDCODED_MODELS`; `refresh_from_upstream` only adds/removes ids
- Upstream model policy: `上游本体（源代码等）/freebuff/common/src/constants/freebuff-models.ts` (SUPPORTED_FREEBUFF_MODELS, availability, unavailableFallback, reasoningEffort, multimodal, peak-hours)
- effort clamp: around `src/api.rs:2431` (the static `clamp_effort` matrix)
- Test-bench single-turn text: `src/web.rs` `playSend()` (around L520, a single user message)
- The multimodal upload API already exists: around `src/api.rs:5069` (/v1/uploads, requires a web Cookie)
- Panel tab list: `src/web.rs:125-135` (overview/test bench/accounts/skills/memory/live log/how it works/system health check/integration guide/settings/about)
- Settings whitelist: `src/api.rs:2010` `CONFIG_EDITABLE`; `src/web.rs:594` `SETTINGS_SPEC`
- Auth boundary: `src/api.rs:88` `admin_authorized` / `is_loopback_request` (XFF header check)
- Security headers/CSP: `src/api.rs` `secure_headers` (around L170+)
- Test counts: `README.md:103` (236 → measured 238); `tests/core_test.rs` 8, `tests/router_test.rs` 10
- CI: `.github/workflows/build-release.yml` (has test+clippy, missing fmt/coverage)

---

*Generated: 2026-09-17 · based on real testing of v0.8.0 code (238 unit tests + 8 + 10 all green, zero clippy warnings, release smoke test healthz/ui/models 200) + verification against upstream freebuff source*
