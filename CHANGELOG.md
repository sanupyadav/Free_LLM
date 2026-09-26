# Changelog

This project follows [Semantic Versioning](https://semver.org/lang/zh-CN/).

## [0.10.3] - 2026-09-19

### Testing/Quality

- **Coverage-improvement loop closed**: added 22 Router-level integration cases (config read/write allowlist/invalid values, skills list+gate injection detection, memory CRUD+toggle, tokens import+list, accounts health, usage totals/daily/models/cost/insights, logs recent, doctor, export schema, import bad schema, prompts list+toggle, threads cleanup, guide, models data+meta, web-chat degrade without credentials, upload 400 without credentials, usage accounts, account history, cross-site write rejection)
  - router_test 11 -> **33 cases**; api.rs line coverage 30.6% -> **45.8%**; TOTAL line coverage 66.8% -> **72.25%**
- **CI coverage gate tightened 65% -> 70%** (`--fail-under-lines 70` + `continue-on-error: false`; local measurement 72.25% leaves a 2.25pt buffer)
- docs/TESTING.md baseline updated; the path to improve further (api.rs -> 60, then overall 75/80) is tracked as backlog

## [0.10.2] - 2026-09-19

### Added

- Dockerfile container health check: installs `curl` in the run stage + `HEALTHCHECK` (`/healthz`, interval 30s / start_period 10s)
- CI coverage **real gate**: `--fail-under-lines 65` + `continue-on-error: false` (replacing the previous always-true gate; local baseline 66.8%, key modules >= 80%)
- `docs/DOCKER.md` guide for running the container locally (when Docker isn't available locally, CI's docker.yml does the multi-arch build → ghcr.io/lza6/freebuff2api)

### Engineering

- Coverage baseline recorded in `docs/TESTING.md` (TOTAL 66.8%; improvement path 70→75→80 tracked as backlog)

## [0.10.1] - 2026-09-19

### Fixed (code-audit loop closed: CRITICAL 0 / HIGH 1 / MEDIUM 2 / LOW 6 / NIT 5)

- **[HIGH] Production wiring**: `main.rs` startup path now loads the vendored upstream model snapshot → `refresh_strategy_from_snapshot` (time-window/efforts/fallback strategy now actually takes effect; failure degrades silently with a warning, doesn't block startup)
- **[MEDIUM] availableAt attribution**: only gives a recovery time when unavailability is actually caused by a time window; statically paused models no longer fabricate an availableAt
- **[MEDIUM] Fallback model validation**: `resolve_at`'s fallback to DEFAULT_MODEL now also runs availability validation, only returning the default as-is when everything is unavailable (letting upstream produce a readable error)
- **[LOW] SQLite robustness**: telemetry's `open_db` now sets busy_timeout(5s); `/api/usage/insights` switched to `spawn_blocking` so it doesn't occupy a tokio worker
- **[LOW] Unknown strategies no longer over-rejected**: `availability_now` now treats unrecognized strategies as available (avoiding silently disabling models when upstream adds a new strategy), with wording distinct from "paused"
- **[LOW] XSS surface**: the recommendation table's `price`/`usable_today` now get `esc()` (upstream fields are only semi-trusted)
- **[LOW] ARIA completeness**: 11 panels now have `role=tabpanel`+`aria-labelledby`; tabs implement roving tabindex (active item 0 / others -1)
- **[LOW] CI E2E reliability**: health-poll timeout now explicitly emits `::error::`+exit 1; a `trap` cleans up the gateway process as a fallback
- **[NIT]** deterministic ordering for snapshot output; empty strategy overrides are skipped; upload filename log sanitization (strips CR/LF); cooldown-expired wording changed to "expired"

### Verification

- 275 unit tests + core 8 + router 11 + web_pool 5 + model_meta 7 all green; clippy `-D warnings` zero warnings; fmt passes; check_panel_js passes
- Real E2E (v0_8 26 + v0_9 26) all green in the CI e2e-win job; Release v0.10.1 auto-built by CI

## [0.10.0] - 2026-09-19

### Added

- **Real-time upstream model strategy contract** (`src/models.rs` / `src/router.rs`, checked against the upstream freebuff-models.ts snapshot):
  - `ModelMeta` adds `availability` (always/deployment_hours/off_peak_only) + `available_at` (gives an ISO recovery time within an off_peak_only window, aligned with upstream's freebuffModelUnavailableAt, never fabricated)
  - DeepSeek high-price window 00:00–10:00 UTC (exempted on Beijing-time weekends); `refresh_strategy_from_snapshot` snapshot sync (idempotent, falls back to the static baseline on failure)
  - `resolve/resolve_available/unavailable_reason` are now time-aware; the downgrade chain skips paused/peak-hour models
  - Catalog alignment: added `mimo/mimo-v2.5` (unlimited free upstream, a FALLBACK landing spot); corrected 5 drift points against the upstream snapshot (deepseek-v4-flash premium, kimi premium, ox-alpha premium/multimodal, fable multimodal, glm-5.2 multimodal)
  - Added `tests/fixtures/freebuff-models.snapshot.json` (21 lines) + `scripts/check_model_contract.mjs` + `scripts/extract_upstream_models.mjs` drift detection
- **Panel accessibility and UX** (`src/web.rs`): ARIA tabs (role=tablist/aria-selected/keyboard ←/→/Home/End), log aria-live, focus-visible, 44px touch targets; log page level-filter button group/pause scrolling/export JSON/error-count badge; recommendation card availability column (paused/peak-hour + availableAt + not yet strategy-verified); credential-cooldown warning bar (one click to the accounts page); upload allowlist/20MB/empty-file validation + sending/uploading button states
- **Telemetry "top three" aggregation**: `src/telemetry.rs::insights()` + `/api/usage/insights` (top 3 slowest accounts / top 5 most-used models / top 3 highest error-rate time slots)
- **Structured degradation when the entire web credential pool is cooling down**: `web_pool_exhausted` (503 + code=web_pool_exhausted + shortest recovery in seconds); `cooldown_until` now output as ISO + new `cooldown_seconds`
- **Narrowed panics on the request hot path**: 7 runtime unwraps in api.rs → unwrap_or_default
- **CI/engineering**: new `e2e-win` job (real gateway E2E + panel JS check), `cargo test --doc` gate (rust-docs component), coverage discipline comments; `docs/TESTING.md`, `scripts/verify_release.ps1`, `scripts/check_artifacts.ps1` (report only, never deletes)

### Testing

- Unit tests 253 → **275** (models time-window/synchronizer/route time-awareness + telemetry insights + 3 router time-routing items)
- model_meta_test 4 → **7** (mimo catalog/full meta coverage/fixture zero drift); router_test 11, web_pool_test 5, core_test 8 unchanged
- `cargo clippy --all-targets -- -D warnings` zero warnings; `cargo fmt --check` passes

### Docs

- `docs/API_GUIDE.md` adds /api/usage/insights, /api/accounts/health, /api/export, /api/import, /api/login/embed/result, /v1/models meta
- `README_zh.md` model matrix adds mimo/glm-5.2; v0.10 feature description; `plans/0-project-overview-and-roadmap.md` refreshed to v0.10
- `workflow_status.md` opens Phase M

## [0.9.0] - 2026-09-18

### Added

- **Web-cookie credential pooling + multi-account rotation** (`src/web_pool.rs`, fixing the long-standing README-acknowledged issue that "the bridging path only used the first valid credential"):
  - Reuses the Bearer pool's circuit-breaker semantics (Closed/Open/HalfOpen, exponential cooldown capped at 10 minutes, HalfOpen probe gate)
  - All web-cookie paths (chat/messages bridging, balance, detail, upload, session cleanup) now pick from the pool by health score/circuit state/cooldown
  - Deterministic 401/403 failures cool down immediately; consecutive network/5xx failures accumulate toward tripping the breaker; success calls mark_ok to recover gradually
  - Hot-reload after importing/deleting credentials (`reload` preserves existing health state); account list and health dashboard show web credentials
- **Upstream model-metadata contract** (`src/models.rs` / `src/router.rs`, checked against the current upstream freebuff-models.ts snapshot):
  - Added a static authoritative `ModelMeta` table: premium / multimodal / available / efforts ladder / fallback
  - Paused/retired models are marked unavailable with a fallback: gemini-3.8-flash, deepseek-v4-pro, minimax-m3, muse-spark-1.3, ox-alpha, glm-5.2
  - GLM 5.3 ladder aligned to the current upstream `['low','high','max']` (max kept as-is); solar/minimax/kimi/glm-5.2 have no ladder and are stripped automatically
  - `/v1/models` response includes `meta` (stable fields, `data` stays backward compatible); `router::resolve_available/unavailable_reason` consumed by routing and the panel
- **Panel (v0.9)**:
  - Chat test bench upgraded: multi-turn conversation context, system prompt, reasoning_effort dropdown (linked to each model's ladder), image upload (drag/paste/select → /v1/uploads, falls back to base64 automatically on failure), copy reply / export Markdown / new conversation
  - Credential health dashboard: Bearer + web cookie shown together (circuit-breaker badge/score/failure count/cooldown + per-account history timeline)
  - Overview "today's recommendations" card: sorted by rateLimitsByModel remaining (paused models automatically sink to the bottom)
  - Settings page "data migration": one-click export/import (import requires a second confirmation + automatic backup)
  - Request detail adds a timing timeline (time to first byte / total duration)
- **Deeper auth hardening**: the `inject_peer` middleware writes the real TCP peer (ConnectInfo) into `x-fb-peer`; `is_loopback_request` now requires "no proxy headers && peer is loopback" to count as local (the default 127.0.0.1 behavior is unchanged); `/api/doctor` adds a `listen_scope` check (fault + remediation advice if listening on a non-loopback address without api_keys configured)
- **Full config export/import** (`src/export.rs` + `/api/export` + `/api/import`): schema version validation, 5MB size cap, automatic backup to `data/backup-<ts>/` before writing, safe minimal set (never overwrites api_keys/auth_tokens)

### Testing

- Added 7 unit tests in `src/web_pool.rs` (multi-account preference/cooldown skip/half-open recovery/consecutive-failure trip/redacted snapshot/empty pool)
- Added 4 unit tests in `src/models.rs` (full metadata coverage/single-model lookup/paused list/meta_snapshot fields)
- Added `tests/model_meta_test.rs` with 4 cases (ladder alignment/availability fallback/readable reason/unknown model defaults to available)
- Added `tests/web_pool_test.rs` with 5 cases; `tests/router_test.rs` grew to 11 cases (including new endpoints)
- Added `tests/e2e_phase_v0_9.cjs` **26 assertions, all green against a real gateway**; `tests/e2e_phase_v0_8.cjs` 26-assertion regression all green
- Real browser (headless Chrome) rendering of the panel: full JS execution (model-count placeholder → 20, model chips render, all v0.9 controls present in the DOM)
- **Scale: 253 unit tests + 8 core + 4 model_meta + 11 router + 5 web_pool, all green**; `cargo clippy --all-targets -- -D warnings` zero warnings; `cargo fmt --check` passes

### Fixed

- `tests/e2e_phase_v0_9.cjs` contract alignment: the health endpoint returns merged accounts (including kind + per-record history timeline)
- Docs: README test count 236→253; the old v0.1.0 acceptance report moved to `docs/archive/` (marked as historical archive at the top)
- CI: `cargo fmt --check` + llvm-cov coverage gate (first run continue-on-error to collect a baseline)

## [0.8.0] - 2026-09-15

### Added

- **Dual-bucket concurrency semaphore shipped** (backfilling a capability the README already claimed, `src/semaphore.rs`):
  - Uses tokio `Semaphore`, zero new dependencies; free tier `{slots:1, concurrency:3}`, subscriber tier `{slots:3, concurrency:8}` (configurable via `concurrency_free_slots/free_multi/sub_slots/sub_multi`, env vars `CONCURRENCY_*`)
  - Wired into all three paths — `/v1/chat/completions`, `/v1/messages`, web bridging — **acquired before the first byte is written**; `TierGuard` RAII returns it automatically (released only when the streaming task ends)
  - 2s timeout returns 429 (`concurrency_busy`) instead of queueing forever; subscriber detection is conservative (routed to the subscriber bucket if any credential in the account pool has plan characteristics)
  - Note: each request holds one "slot" and one "concurrency" permit at the same time, so the real concurrency cap equals slot capacity (free 1 / subscriber 3); the "concurrency" bucket is a dimension reserved for upstream's own strategy
- **Claude path now has retry + accounting + memory** (`/v1/messages`, previously missing all three):
  - Request-level retry loop (same strategy as OpenAI): switches account on failure, auto-retries on 5xx/rate-limit/network errors, circuit-breaker cooldown
  - waiting_room queueing now returns 503 + a readable `overloaded_error` message (no longer a bare 502)
  - Non-streaming success now records `usage_db.record_ex` + `telemetry.record`; streaming records usage too; success path now calls `memory.observe`
- **Panel modernization** (`src/web.rs`):
  - New **chat test bench** (calls `/v1/chat/completions` and streams the reply), **settings page** (listen address/memory toggle/token_saver/redaction/skills mode/budget/proxy/cleanup interval/semaphore capacity, all UI-editable and written back to config.json), **about page** (version/uptime/upstream/disclaimer)
  - New `GET /api/config` + `POST /api/config/save`: allowlist validation + type/valid-value checks + atomic write-back; `memory_enabled` takes effect immediately
  - Visual branding: layered CSS tokens (palette/spacing/radius/shadow/motion), respects `prefers-reduced-motion`, `focus-visible` focus ring, horizontal-scroll nav on narrow screens
  - Large logs now use **windowed virtual scrolling** (renders only the visible area + buffer, smooth past 1000 entries)
- **Security hardening**:
  - All responses get `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`; panel pages get CSP `default-src 'self'`
  - **Log/telemetry redaction** (`src/redact.rs`, `redact_logs` on by default): cookie values / Bearer / authorization / long sk- strings are replaced with `***` before hitting the log bus and telemetry
  - Cross-site Origin blocking changed to **403 Forbidden** (CSRF, semantically distinct from 401)
- **Desktop shell hardening**:
  - Multi-instance protection: `app.requestSingleInstanceLock()`; a second launch activates the existing window instead of starting a second gateway
  - Tray "System check" now uses a hash navigation (`location.hash='#doctor'`) instead of a full page reload
- **Robustness**:
  - Port-binding failures now give a clear error message (with a hint to check the occupying process via `netstat -ano | findstr :port`)
  - `web_threads` binding table gets a capacity cap (2000) and TTL cleanup (24h) to prevent file/memory growth
  - Cookie detection tightened: `handle_account_balance` no longer falls back on `%3A` (prevents misidentifying URL-encoded strings)

### Testing

- Added 6 unit tests in `src/semaphore.rs` (bucket independence/timeout/RAII no-leak/subscriber detection)
- Added 6 unit tests in `src/redact.rs` (cookie/Bearer/sk- redaction, normal text unaffected)
- Added `tests/router_test.rs` with **10 router-level integration tests** (mock TCP upstream): chat non-streaming/streaming, messages non-streaming, queueing 503, 401, cross-site 403, healthz, bridging trigger, 5xx retries exhausted, 401 credential invalidation
- Added more api unit tests (cookie-detection tightening regression, Claude tool round-trip semantics), web_threads unit tests (TTL/capacity)
- **Test scale: 238 unit tests + 8 integration + 10 router-level integration, all green**; `cargo clippy -- -D warnings` zero warnings
- Added `tests/e2e_phase_v0_8.cjs` (26-assertion smoke test against a real gateway: security headers/CSP/config read-write/new tab/windowed rendering/redaction) + real-browser panel verification with headless Edge

### Fixed

- `desktop/main.js` tray "System check" navigation was broken (`loadURL('#doctor')` doesn't trigger hash routing → switched to `executeJavaScript` to set the hash)
- README version was stale (`Setup 0.3.0` → current version; semaphore description aligned with the implementation)

## [0.7.3] - 2026-09-11

### Fixed

- **Web streams no longer truncated by the 300s total timeout** (`ERR_INCOMPLETE_CHUNKED_ENCODING`): `WebClient` changed from reqwest's `.timeout(300s)` (total request timeout, which cuts off a stream even while it's still emitting increments) to `read_timeout(300s)` (per-read-chunk timeout, matching the upstream client). As long as increments keep arriving the connection stays open; it's only closed after 5 minutes of complete silence.
- **Bridged token accounting no longer always shows 0**: the upstream web protocol's SSE doesn't return usage (the done event is empty), so token counts are now estimated from content length (input = content sent, output = the converted chunk body + reasoning; roughly 1 token per 2 characters, deliberately conservative rather than overreporting). Panel usage, trends, and request detail display correctly again.

### Changed

- **Memory layer gets its own independent master switch (off by default)**:
  - `config.rs` now defaults `memory_enabled: false` (per user note: not everyone needs memory)
  - Added `POST /api/memory/toggle`: hot-toggle on/off; when off, nothing is recorded or injected into the system prompt; writes back to config.json and takes effect immediately, no restart needed
  - `GET /api/memory` now returns an `enabled` status; panel's "Memory" page gets a switch at the top
  - Added a `MEMORY_ENABLED` environment variable override

## [0.7.0] - 2026-09-11

### Added

- **Embedded-browser one-click login (Plan A — zero install, zero copy-paste)**: browser users no longer have to install the extension —
  - Panel "one-click login" → `POST /api/login/embed` → the gateway spawns a `--login-window` child process (tao window + wry WebView2, a separate process that doesn't block the gateway's tokio runtime)
  - Complete the GitHub login normally in the window → the child process reads **all cookies (including the HttpOnly session-token, an OS-level component not subject to page JS restrictions)** via the WebView2 CookieManager → auto POSTs to `/api/tokens/import` → the window closes itself
  - Multi-port probing (panel port > 47821/47822/8787, tries the next port on connection failure, stops on explicit rejection); polls login state every 600ms with a 10-minute timeout; result reported via the process exit-code protocol (0 success / 1 failure)
  - Clear error and fallback guidance (extension / clipboard / manual) when WebView2 is unavailable
- **Panel login wizard reorganized** (four paths ordered by experience):
  - **Plan A — embedded window** (most recommended, zero install zero copy-paste)
  - **Plan B — clipboard auto-detect** (recommended, no install needed, about 30 seconds): copy the cookie, click the button, it auto-fills and imports, **validating immediately on paste** (automatically checks the new credential's validity, shows account name/email; falls back to guided manual paste for empty clipboard / permission denied / no clipboard API)
  - **Plan C — Chrome/Edge extension** (fully automatic, install once and forget it, about 2 minutes the first time)
  - Manual paste fallback (cookie string / cURL / HAR, about 1 minute)
  - Animated outline highlight for wizard fallbacks; the HttpOnly explanation is kept in sync with the referenced plan

### Phase I archive (shipped with v0.6.0)

- Full-feature real E2E matrix `tests/e2e_phase_i.cjs` (18 assertions: keepalive/model list/tool calls/multi-turn memory/long agent/Anthropic streaming)

## [0.6.0] - 2026-09-11

### Wrap-up (all Phase G review findings cleared)

- **`/v1/models` auth completed**: when `api_keys` is configured, the model list is no longer exposed to unauthorized callers (unchanged direct-access semantics when unconfigured). Verified: no key → 401 / correct key → 200 / wrong key → 401 / cleared → direct access restored.
- **Atomic config.json writes**: the panel's generate/clear API key writes now use temp-file + rename, so a crash mid-write no longer corrupts the config.
- **Cryptographically random API keys**: one-click generation upgraded from UUIDv4 (122 bits, recognizable format) to OsRng full-entropy randomness (`sk-fb-` + 32 base64url characters, 192 bits of effective entropy).
- **Fixed a false-positive in bridging error detection**: the web-bridging stream's success check no longer misfires just because the response body happens to contain the word "Unauthorized" (the old logic would mark a successful request as a 502 if the model's own reply text mentioned that word); the error-classification function `detect_bridge_error` now follows the existing error-rule table (the upstream error envelope currently doesn't reach the classified text in the conversion pipeline — moving the detection point earlier is tracked as a follow-up).
- **E2E infrastructure**: `tests/e2e_phase_g.cjs` now force-restores state in a finally block (no leftover test key even if the script crashes); added `tests/e2e_phase_i.cjs`, a full-feature real E2E matrix (token keepalive / model list / tool calls / multi-turn context memory / long agent / Anthropic streaming, 18 assertions).

### Phase G (v0.5.0) — see the [0.5.0] section; all of its review findings (including 4 P1, 4 P2, 3 LOW) were fixed and verified in earlier commits.

## [0.5.0] - 2026-09-11

### Added

- **Web-cookie bridging (closing a critical gap)**: when only web cookies (from the one-click login path) are imported and the account pool is empty, `/v1/chat/completions` and `/v1/messages` **automatically bridge to the upstream web protocol**, so browser users can chat directly just by pointing at `/v1` per the guide (previously this returned `no healthy upstream auth token available`).
  - **Upstream session reuse**: follow-up turns send only the latest user message and reuse the same thread (bound via `data/web_threads.json`), avoiding burning through the daily session quota (free tier: 6 sessions/day) by opening a new thread on every request — this was the direct cause of "starts 429'ing after a little use"
  - Anthropic streaming is converted in real time into the standard event stream (message_start → content_block_delta → message_stop); non-streaming gets a full message conversion
  - A bound thread is reset automatically if upstream clears it; the client's retry recovers transparently
  - Full-path telemetry (route reason tagged continue/new thread, usage, threadId added to the auto-cleanup list)

- **Browser one-click-login extension** (`browser-extension/`, Chrome/Edge MV3): reads freebuff.com login credentials (including **HttpOnly** cookies, unreadable by page JS) and sends them to the local gateway; includes an options page (custom port / optional API key) and security notes (only reads freebuff.com, only sends to localhost).
- **Browser "true one-click login" loop** (panel ↔ extension direct connection):
  - The extension broadcasts its own id via `externally_connectable` + a content script; once the panel has the id it can command the extension directly
  - Panel "one-click login" click → extension **auto-opens freebuff.com** → polls for login (up to 3 minutes) → on success **auto-writes the credential back to the gateway** → panel polling picks up the new credential and refreshes account info automatically
  - Falls back to a 3-step manual wizard automatically if the extension isn't installed (all three paths are documented in the UI); requests between the extension and panel carry the gateway API key (relayed by the panel); `Origin: chrome-extension://` is now on the CSRF allowlist
- **One-click extension distribution**: `GET /api/extension/bundle` packages the extension (embedded at compile time, also usable as a single-file distribution) into a downloadable zip; panel's "⬇ Download extension" works out of the box.
- **Credential management improvements**:
  - Each credential gets a stable `id` (FNV-1a 64, reproducible across versions)
  - The credential list now shows **account nickname / email / type / plan / today's remaining credits / import time** directly, and can expand for detail (per-model quota, streak days, last-7-days tokens, regional restrictions, error reason)
  - **Backfilled import time for old data**: credentials imported before this field existed are backfilled from `tokens.json`'s mtime and persisted, no longer showing "—"
  - `POST /api/tokens/check` checks a single credential; `POST /api/tokens/delete` deletes a credential (also removing it from the running account pool)
  - Duplicate values are auto-deduplicated (re-importing the same one gives a clear notice)
- **Per-account usage history** (per user note: "of course you also need a usage log for each account"): every check/refresh writes a JSONL snapshot; `GET /api/account/history?cred_id=&limit=` queries by account; the panel's "Usage history" page visualizes it (plan/remaining/used/tokens/streak days/success-failure).
- **"Start requesting right now" onboarding card + runtime API key management**: the overview page's first screen shows the Base URL, the OpenAI/Anthropic endpoints, and the API key directly, all one-click copyable; `GET /api/guide` returns the real listen address, key status, and model count; the panel can **generate/clear the API key with one click**, taking effect **immediately with no restart** (also written back to `config.json`; clearing is blocked when not listening locally).
- **Protocol-fingerprint completeness**: derive `x-freebuff-instance-id` per account (the upstream web client sends this on every request; the gateway previously never sent it at all — one of the easiest tells for risk control); gravity's `client_context` screen/viewport/DPR/memory/core-count are now also derived per account instead of every account sharing the same fake environment.
- **Automatic upstream session cleanup** (per user note: "we need automatic cleanup, so as not to put pressure on upstream through the proxy"): sessions older than 24 hours are cleaned up automatically every hour (`thread_cleanup_interval_sec` / `thread_max_age_hours` configurable, interval 0 disables it); the manual endpoint `POST /api/threads/cleanup` still supports dry-run mode; the "How it works" page documents this.
- **Full account overview panel** ("Account" page, rendered entirely in Chinese):
  - Identity: nickname / email / avatar / user ID / credential expiry
  - Usage stats: consecutive-use streak / total active days / last-7-days message count and token consumption (input/output/cached/total) / per-model session counts
  - Today's quota: account tier / subscription plan / remaining and max credits / reset time (midnight Pacific time, refreshes automatically the next day) / **per-model remaining count today, limit, used, credit price, next reset**
- **Credential keepalive check** (`POST /api/account/refresh`): calls the upstream convex-token endpoint to verify the cookie is still valid, prompting re-login if it isn't.
- **Full account overview API** (`GET /api/account/overview`): concurrently aggregates 4 upstream endpoints (auth/session, usage-summary, subscriptions, freebuff-session); a single failure degrades gracefully instead of failing the whole call.
- **Stronger onboarding guide**: a three-step overview, API key explanation (including whether validation is actually enabled), Node.js SDK and curl examples, all filled in with real addresses and a real key.

### Fixed (browser one-click login / credential detection)

- **Web-cookie credentials no longer pollute the account pool**: imported session-tokens used to get stuffed into the desktop Bearer account pool, so /v1 requests would get routed to the desktop protocol where they were bound to fail (reporting "no healthy token" once tripped), and this also blocked bridging from triggering — now web cookies are only used by the bridging path, and /v1 works immediately after import.
- **Data-plane CSRF gap closed**: `/v1/chat/completions`, `/v1/messages`, `/v1/uploads` previously didn't check Origin, so a malicious page could blind-fire simple `text/plain` requests (burning the user's upstream quota / triggering risk control using their cookie) — these now block cross-site Origin the same way `handle_web_chat` already did (SDK/curl calls without an Origin header are unaffected).
- **tokens.json concurrency safety**: import/delete/backfill now use an in-process mutex + atomic temp-file replace — concurrent imports no longer overwrite each other and lose credentials (tested with 7-way concurrency, zero loss); a crash mid-write no longer corrupts all credentials.
- **threads.json concurrency safety**: session-cleanup writeback now re-reads and merges inside the lock — thread records created during a cross-network sweep-and-delete are no longer overwritten by the stale snapshot (previously that session would never get cleaned up again).
- **history/binding file atomicity**: usage-history compaction and appending are now serialized; the web session-binding snapshot is cloned inside the lock before an atomic write, closing the stale-snapshot-overwrite window.
- **Loopback detection tightened**: prefix-spoofed addresses like `localhost.evil.com:47821` are no longer treated as local (now uses exact host matching + IP resolution).
- **E2E config isolation**: `tests/e2e_phase_g.config.json` added to .gitignore (test-generated keys never enter git).
- **Invalid credentials were misjudged as "valid"**: upstream `/api/auth/session` returns **HTTP 200 + `{}`** for an unauthenticated/invalid credential (not 401) — previously only "did the request succeed" was checked, so any forged cookie showed as "credential valid" (confirmed by testing). Now it requires an actual user subject in the response (id/email/name), with the quota endpoint used only as a secondary signal.
- **`/healthz` information disclosure**: once `api_keys` is configured, unauthorized requests no longer return account names and model composition, only uptime and version.
- **Extension requests rejected by CSRF protection**: `Origin: chrome-extension://` wasn't previously on the allowlist, so extension imports were rejected — now allowed (the extension still needs to send the key if api_keys is configured).
- **Short-credential redaction leak**: `mask()` used to reveal the "last 4 characters" for credentials <=8 characters long (e.g. `sk-local` → leaking 5 of 8 characters). Changed so short strings only reveal 2 characters, and anything <=4 characters is fully masked (borrowing the maskKey fix noted in the reference project freellmapi).
- **Delete-thread endpoint verified**: `DELETE /api/chat/threads/{id}` confirmed to exist via a real-credential probe (a nonexistent thread returns JSON 404, an unknown route returns HTML); removed the ineffective `POST /api/chat/threads/delete` fallback (tested, returns 405).
- **Panel auth-failure experience**: after enabling an API key, first opening the panel no longer shows a "red light + toast flashing every 6 seconds" — instead a one-time guidance banner appears and auto-refresh pauses, resuming once the key is entered; one-click login no longer spins for 3 minutes reporting "timeout" when a credential already exists (the extension's already-synced path echoes the result immediately).

### Fixed (web protocol alignment with upstream captures)

- **`agent_delta` body no longer dropped**: upstream tool (web_search/read_url) research results arrive in the agent_delta event body, which was previously silently discarded, so users only saw the tool call and not its result.
- **Parallel tool-call `index` now increments**: it was previously always 0, so 5 parallel tool calls would overwrite each other on the client.
- **A `finish_reason` chunk is now sent at stream end** (stop / tool_calls), so strict clients no longer treat it as an abnormal end.
- **threadId now surfaced** (meta/title events): enables multi-turn follow-up (`WebClient::last_thread_id()`).
- **Second title update now wins on arrival** (keeps the model-generated summary rather than the user's original text).
- **Uploads now support arbitrary file types**: the mime allowlist previously only permitted image/pdf, so document uploads got rewritten to image/png and upstream couldn't return `kind:"document"` — the document path was effectively dead.
- **Upload response now surfaces full fields**: `kind` (image/document) / `url` (image) / `chars`, `truncated` (document) / `descriptionStorageId`, with usage notes attached.
- **`attachments` parsing**: `/v1/web/chat` now supports document attachment references (previously always an empty array).

## [0.4.0] - 2026-09-11

### Added

- **Memory layer (the AI understands you better)**: `data/memory.sqlite` is an independent store; **zero-LLM rule-based observe** (automatically records frequently used model preferences, reasoning-effort downgrades, user correction signals like "remember...", "stop doing...", "always/never") plus manual entries; trigram FTS5 search for Chinese text; bounded injection (512-token budget, low-authority tagging, marker escaping, sorted by id for byte-stable output); panel's "Memory" page can view/add/delete/pin-as-stable-fact.
- **Three-state circuit breaker**: the account pool upgraded from a bare cooldown timestamp to a Closed/Open/HalfOpen circuit breaker (opens after 4 consecutive failures, cooldown grows exponentially capped at 10 minutes, half-open recovers after 2 consecutive successful probes); `mark_success`/`mark_failure` wired throughout.
- **Request-level retry loop**: automatically switches accounts and retries on upstream failure (up to 3 attempts, covering 429/5xx/network errors; 401/403 cools down that account then switches); strict committed-boundary semantics — only retries before any bytes have been written to the client.
- **Error-rule table**: text-first with status-code fallback for classifying upstream errors (8 categories including waiting_room/rate_limit/model_unavailable/auth_expired), with a retryable flag and Retry-After hints.
- **Minimal MCP exposure**: `POST /mcp` (JSON-RPC 2.0, hand-written, zero new dependencies) exposes 3 read-only tools: `list_models` / `list_accounts` / `usage_summary`, so external agents (Claude Code/Cursor) can query gateway state directly.
- **Cost/rate visualization**: `GET /api/usage/cost` (30-minute sliding-window request count/error rate/average latency/rate); panel overview page shows a rate row (honestly noting "the free tier has no monetary cost").
- **Explainer page (how it works)**: panel adds a "How it works" tab, 6 sections explaining the gateway's mechanics (request path/multi-account rotation/injection mechanism/black box/ad-based keepalive/where data lives).
- **Config options wired up for real**: `fallback_models` (downgrade chain) and `token_saver` (tool_result compression), previously parsed but never consumed, are now wired in; added a `memory_path` config option.

### Fixed

- **`compress_tool_result` multi-byte panic**: now splits on character boundaries (no longer panics on Chinese tool_result text; the same class of issue as the tail-truncation fix in v0.3.0).
- **Skills library path**: avoids `with_extension` truncation (wrong path when the directory name contains a `.`).
- **`/v1/uploads` error body truncation**: upstream error messages capped at 300 characters (prevents leaking account/internal detail via echo).

### Engineering

- New modules: `memory.rs` (memory layer), `mcp.rs` (read-only MCP service), `errors.rs` (error-rule table), `pool.rs` circuit breaker.
- Tests: 108 → **130+ unit tests** (added circuit breaker 4 / memory 9 / MCP 10 / error table 12 / multi-byte compression regression 1), clippy zero warnings.

## [0.3.0] - 2026-09-11

### Added

- **Skills system (persistent)**: panel's "Skills" page can create/edit/enable-disable/delete skills; files (`data/skills/<id>/SKILL.md`) are the source of truth + a SQLite index; **roster mode** injects on demand (only name and description, 2000-token budget, configurable); built-in quality gates (injection phrases, oversized content, format checks); persists across restarts.
- **Black-box logging (observability)**:
  - `GET /api/logs/stream` (real-time SSE logs) + `GET /api/logs/recent` (historical replay), panel's "Live logs" page;
  - Request detail drawer: `GET /api/usage/requests/{id}` returns route/account/latency/first-byte/tokens/error plus a plain-language explanation (e.g. "queued in the upstream free tier, not a gateway fault");
  - `GET /api/doctor` system check (four states: ok / fault / unknown / fact), panel's "System check" page.
- **Real usage stats**: streaming/non-streaming token counts now actually collected (previously always 0); Claude path `/v1/messages` usage recording (previously entirely missing); upstream error classification persisted (`error_kind`).
- **Claude streaming protocol conversion**: `/v1/messages` streaming requests no longer pass through raw OpenAI SSE; converted into the canonical Anthropic event stream (`message_start` / `content_block_start|delta|stop` / `message_delta` / `message_stop`), with full tool-call block support.
- **Multimodal upload**: `POST /v1/uploads` (raw body + `x-file-name` header, returns a storageId); `/v1/web/chat` supports an `images` parameter (an array of storageIds or objects).
- **Panel rewrite**: tab navigation (Overview / Accounts / Skills / Live logs / System check / Onboarding guide); accounts page supports pasting Cookie/cURL/HAR to import (previously no import entry point); onboarding guide has one-click-copy config snippets built in for Claude Code / Cursor / OpenAI SDK / LobeChat.
- **Config options**: `telemetry_path`, `skills_dir`, `skills_inject_mode`, `max_roster_tokens` (all have defaults, backward compatible with old configs).

### Fixed

- **README_zh.md** rewritten from the old Go version to the Rust version (port 47821 / `--config` / cargo commands / client onboarding guide / FAQ) — previously Chinese users were misdirected on their very first step.
- **4 old panel bugs**: literal `{model_count}` placeholder; two buttons using `location.href` that navigated users away from the panel to a raw JSON page; DOM piling up endlessly on every refresh; empty state rendering the literal string "undefined".
- **Desktop client**: startup/login failures now show a popup (previously only console.error); tray gains "System check / Open logs / Open config / Open data folder"; gateway stdout/stderr now written to `userData/logs/gateway.log`; auto-update loop closed (auto-download + prompt to install once downloaded); non-default port detection.
- **Startup robustness**: model-registry network sync now has timeout protection (connect 5s / total 10s) — previously a network hiccup could block startup for 30+ seconds.

### Engineering

- New modules: `protocol/` (streaming conversion), `skills/` (skills persistence), `retry.rs` (failure classification/backoff/committed semantics), `logbus.rs` (log broadcast + ring buffer), `telemetry.rs` (dedicated-writer-thread telemetry store).
- Tests: unit tests 24 → **105**, integration tests 8; `cargo clippy --all-targets -- -D warnings` zero warnings.
- Added E2E acceptance script `tests/e2e_phase_d.cjs` (29 assertions: panel elements / skills CRUD / SSE / doctor / auth / upload error codes / model list).
- Desktop shell adds `preload.js` (contextBridge allowlisted IPC).

### Known limitations (next batch)

- Request-level "switch account and retry" only has failure classification + cooldown wiring landed; the full retry loop is still pending.
- The telemetry events table is currently only written on the failure path (success-path event chaining is an enhancement item).
- The web protocol (`/v1/web/chat`) doesn't return a usage field from upstream, so tokens are recorded as 0 (latency/bytes/first-byte are recorded correctly).
- `data/tokens.json`'s path is still hardcoded (every other path is already configurable).

## [0.2.0] - 2026-09-10

- CI fixes (Docker multi-arch / GHCR naming / rust 1.95 pin / .cargo proxy removed from git).
- Security and engineering hardening: admin endpoint auth, API key redaction, cross-origin token import rejected, circuit breaker wired in, Claude non-streaming protocol conversion.
- Default port changed 8787 → 47821; panel has built-in prompt/skills management; upstream errors passed through; streaming long connections have no overall timeout.
</content>
