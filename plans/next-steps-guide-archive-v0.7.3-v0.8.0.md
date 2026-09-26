# Next-step improvement guide (Freebuff2API v0.7.3 -> v0.8+)

> This file is the **actionable improvement checklist** (core deliverable). Each item includes: priority, location, behavior diff, acceptance method.
> Companion docs in the `plans/` directory: 0 overview roadmap, 2 architecture hardening, 3 panel modernization, 4 HA/security, 5 test strategy, 6 feature expansion.

---

## 0. Overview

**Conclusion**: high project quality, clear architecture, thorough validation (219 unit tests all green + real E2E matrix), **no refactor needed**. Improvements focus on four things:

1. **Fill in capabilities that were "claimed but not landed"** (dual-bucket concurrency semaphore -- README wrote it but the code doesn't have it)
2. **Close the capability gap on the Claude path** (retry / accounting / memory -- weaker than the OpenAI path)
3. **Modernize the panel experience** (visual branding + interaction + accessibility + new pages)
4. **Security / HA details** (CSP, redaction, port-conflict hints, desktop multi-instance)

> All changes are "additive"; no existing module is refactored; default config matches current behavior exactly, no migration needed for old config.json.

---

## 1. Fixes (P0/P1 - do immediately)

### 1.1 [P0] Land the dual-bucket concurrency semaphore
- **Location**: new file `src/semaphore.rs`; wire into `api.rs` (chat / claude / web_bridge paths); add 4 config items to `config.rs`
- **Problem**: README/README_en claim "free tier `{slot:1, multi:3}`, subscription tier `{slot:3, multi:8}`", but `rg Semaphore` gets zero hits -- the forwarding path has no concurrency limiting at all
- **Fix**: tokio `Semaphore` (zero new dependencies), global scope, `TierGuard` RAII auto-return; acquire timeout of 2s returns 429 semantics
- **Acceptance**: with N concurrent requests, the number passing through at once <= bucket capacity (mock upstream test); unit tests cover the two buckets not interfering / returning permits / timeout
- **See**: `plans/2-architecture-hardening.md §2.1` + `5-测试策略 §5.2A`

### 1.2 [P1] Fill in retry + accounting + memory for the Claude path
- **Location**: `api.rs::handle_claude_messages`
- **Problem**: `let _ = ensure_session(...)` swallows errors; no account-swap retry; success path has no `usage_db.record_ex`/`telemetry.record`; no `memory.observe`
- **Fix**: extract the OpenAI path's retry loop into a shared helper for reuse; convert the error body to the Claude shape (the existing `openai_error_to_claude` can be reused); add accounting to the non-streaming success path; add the missing memory observe call
- **Acceptance**: Claude queuing returns 503 + readable message; auto account-swap on 5xx (verified via mock call count); Claude requests visible in panel usage; observe fires when the memory toggle is on
- **See**: `plans/2 §2.2`

### 1.3 [P1] `handle_account_balance` cookie detection too broad
- **Location**: `api.rs` `looks_like_cookie` closure
- **Problem**: `t.contains("%3A")` misclassifies any URL-encoded string as a cookie
- **Fix**: narrow to `session-token` / `.next-auth` / `callback-url`
- **Acceptance**: add a unit test asserting "a non-cookie string does not match"

### 1.4 [P1] `web_threads` binding table has no capacity cap/TTL
- **Location**: `src/web_threads.rs`
- **Problem**: binding entries grow unbounded, `web_threads.json` bloats, memory accumulates over long runs
- **Fix**: `MAX_BINDINGS` (default 2000) + evict entries older than 24h; trim on file write
- **Acceptance**: unit test injects 2001 entries -> oldest gets trimmed; expired TTL entries get cleared

### 1.5 [P2] `/v1/messages` non-streaming success path has no telemetry/usage
- **Location**: `api.rs::handle_claude_messages` non-streaming branch
- **Fix**: `extract_usage` parsing + `record_ex` + `telem.record`
- **Acceptance**: Claude non-streaming requests appear in panel usage/request detail

### 1.6 [P2] Desktop tray "System Doctor" navigation broken
- **Location**: `desktop/main.js` (`loadURL('#doctor')`)
- **Problem**: not a hash route, `#doctor` doesn't trigger the frontend tab
- **Fix**: change to `./ui#doctor` + frontend `showTab` reads `location.hash`
- **Acceptance**: clicking Doctor in the tray opens the panel and lands on the "System Doctor" tab

### 1.7 [P2] Doc versions out of date
- **Location**: `README.md` / `README_en.md` (`Setup 0.3.0` etc.)
- **Fix**: unify version numbers to current (0.8.0 semantics); remove the "semaphore already implemented" claim (add it back once landed)
- **Acceptance**: no remaining 0.3.0/0.2.0 in a full-text search; aligned with CHANGELOG

---

## 2. Optimizations (P1/P2 - experience and robustness)

### 2.1 Panel visual branding + component spec
- **Location**: `src/web.rs` (inline HTML/CSS/JS)
- **Fix**: layered CSS variable tokens (palette/spacing/radius/shadow/motion); unified class names for button/card/table/badge/empty-state/skeleton/drawer/toast components; respect `prefers-reduced-motion`
- **Acceptance**: no overflow at 320/768/1024/1440 breakpoints; keyboard Tab reachable + focus visible; existing E2E panel assertions keep passing
- **See**: `plans/3-panel-modernization.md`

### 2.2 New panel additions: chat test bench + settings page + about page
- **Location**: new tabs in `src/web.rs`
- **Fix**: test bench calls `/v1/chat/completions` with streaming render; settings page turns existing config items (listen address hint / memory toggle / token_saver / skill budget / proxy / cleanup interval) into a UI that writes back; about page shows version/upstream/disclaimer
- **Acceptance**: every control gives feedback after being clicked; writing back takes effect in config.json (persists after restart)
- **See**: `plans/3 §3.3`

### 2.3 Windowed rendering for large lists
- **Location**: log/memory/request lists in `src/web.rs`
- **Fix**: simple virtual scrolling (render only the visible area + buffer), no lag past 500 items
- **Acceptance**: injecting 2000 log entries scrolls smoothly; no memory spike

### 2.4 Response security headers + CSP
- **Location**: response construction in `api.rs` / `web.rs`
- **Fix**: add `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin` to `/ui`, `/healthz`, `/v1/*`; add CSP `default-src 'self'` to the panel page
- **Acceptance**: headers visible via curl -I; panel functionality unaffected (no external resources)

### 2.5 Log/telemetry redaction toggle
- **Location**: telemetry recording in `api.rs`
- **Fix**: `redact_logs` config item (default on): before recording, replace `session-token=...`, `Bearer xxx`, `authorization` values with `***`
- **Acceptance**: importing an error body containing a cookie writes no plaintext token into telemetry

### 2.6 Desktop multi-instance protection
- **Location**: `desktop/main.js`
- **Fix**: `app.requestSingleInstanceLock()`; second launch calls `show()` on the existing window
- **Acceptance**: launching the installed app twice in a row -> only one window pops up, one gateway process

### 2.7 Chinese error message for port bind failure
- **Location**: startup bind in `src/main.rs`
- **Fix**: catch `bind` failure -> print "Port X is in use, please change listen_addr in config.json" (with optional process hint) in Chinese
- **Acceptance**: occupy 47821 then start -> clear Chinese error instead of a raw anyhow dump

---

## 3. Testing and quality (cross-cutting)

| Item | Content | Acceptance |
|----|------|------|
| TDD for new modules | Semaphore/Claude retry/accounting: write tests before implementation | Unit tests all green |
| Router-level integration tests | `tests/router_test.rs`: mock upstream (httpmock or a homegrown TCP stub) covering 8 main paths | See `plans/5 §5.2C` |
| Regression | 219 unit tests + core_test + E2E d/e/f/g/i all kept | `cargo test` all green |
| Quality gate | `cargo fmt --check`, `cargo clippy -- -D warnings`, coverage >=80% (llvm-cov) | Add gate to CI |
| Coverage baseline | No llvm-cov record currently -> establish in v0.8 | `cargo llvm-cov --fail-under-lines 80` |

---

## 4. Release checklist (run through every release)

- [ ] `cargo fmt` + `cargo clippy -D warnings` + `cargo test` (incl. tests/) all green
- [ ] `Cargo.toml` / `desktop/package.json` / `CHANGELOG.md` version numbers synced
- [ ] README (CN/EN) feature descriptions match the implementation (especially claims like "semaphore")
- [ ] `docs/API_GUIDE.md` updated with new endpoints
- [ ] `workflow_status.md` wrapped up (Phase K all DONE + evidence)
- [ ] Desktop `electron-builder` artifact signed (optional) + GitHub Release published + auto-update feed verified
- [ ] Old artifact cleanup: delete `legacy-go/` (22MB archive), keep `reference/` (71MB reverse-engineered source, if already archived in git) as needed; update or remove `宣传视频/` (43MB)
- [ ] `config.example.json` synced with new config items

---

## 5. Recommended execution order (to avoid stacking bugs)

```
Batch 1 (pure additions, lowest risk): 1.3 / 1.4 / 1.5 / 1.6 / 1.7 / 2.4 / 2.5 / 2.6 / 2.7
Batch 2 (core path, needs tests first): 1.1 semaphore -> 1.2 Claude path (depends on the Router tests from test infra item 3)
Batch 3 (panel, independent): 2.1 / 2.2 / 2.3
Batch 4 (wrap-up): coverage gate / docs / release checklist
```

> Why this order: clear small debt and harden first (no behavior impact) -> then fill in capabilities touching the core forwarding path (higher risk, tests first) -> panel work proceeds independently -> finally unify the quality gate and release checklist. Each batch is an independently revertible commit.

---

## 6. Risks and dependencies

- The semaphore's integration point sits on the core forwarding path (acquire before the first byte) -- **must** be verified against a mock upstream test before changing live behavior; enabling default values of 1/3/3/8 could lower concurrency for a "single free-tier account" -- **defaults of 1/3/3/8 are fine** (a single account only has 1 session anyway), multi-account setups are what actually hits the buckets.
- Router-level tests need `AppState` to be injectable: pointing `UpstreamClient` at a mock address is enough (`base_url` is configurable), no structural changes required.
- Splitting the panel's 88KB inline string (`web_assets/`) is **pure file organization**, no build system introduced; if diff noise is a concern, this can be deferred and only the CSS variable layer done for now.
- Enabling telemetry redaction by default affects the readability of `error_excerpt` (cookie segments get masked) -- on balance, default on (security first), since it's no longer sensitive by the time it's shown in the panel.

---

*Generated: 2026-09-15 - based on live testing of v0.7.3 code (219 unit tests all green, CLI scan, line-by-line verification of critical paths)*
