# 5. Testing & Verification Strategy (v0.8 TDD checklist)

> Baseline: `cargo test --lib` 219 passed / 0 failed (empirically verified). This document lays out the v0.8 test expansion plan and coverage targets.

---

## 5.1 Current state

| Type | Current state | Notes |
|------|------|------|
| Unit tests | 219 items (embedded `#[cfg(test)]` in src) | Covers: circuit breaker, retry, error classification, compression, parsing, skills store, memory, router clamp, pool |
| Integration tests | `tests/core_test.rs` (8 items, includes pool/config/registry/usage smoke tests against the real upstream) | Network-independent parts already use tempfile |
| E2E scripts | `tests/e2e_phase_*.cjs` (d/e/f/g/i matrix, node http connects directly to the real gateway) | Depends on the real upstream (needs valid credentials / proxy) |

## 5.2 v0.8 required test checklist (TDD: write tests before changing code)

### A. Semaphore (new module)
- [ ] Concurrency limit of the free bucket at capacity 1: fire 3 concurrent acquires, only 1 succeeds, 2 wait
- [ ] Concurrency cap of the subscriber bucket at capacity 8
- [ ] `try_acquire` returns a 429-semantics timeout error when permits are exhausted
- [ ] Drop (TierGuard) returns the permit and the semaphore count is restored (assert inner count across 1000 loop iterations)
- [ ] The two buckets don't interfere with each other (free and subscriber counted independently)

### B. Claude path hardening
- [ ] `claude_to_openai_messages`: semantics before/after conversion for string / string-content-blocks / tool_use+tool_result combinations
- [ ] `handle_claude_messages` retry: Mock upstream returns 5xx on the 1st call, 200 on the 2nd → total of 2 calls, client receives 200
- [ ] waiting_room: Mock upstream returns queued JSON → 503 + `waiting_room` code
- [ ] Claude non-streaming success → `usage_db` has a record (isolated via tempfile)
- [ ] Claude non-streaming response with usage → telemetry/usage tokens are correct

### C. Router-level integration (new, currently missing)
- [ ] `tests/router_test.rs`: build an axum `Router` (using `build_router` + a Mock AppState), Mock upstream via `httpmock` or a self-built `tokio` TCP stub
  - [ ] `/v1/chat/completions` non-streaming works, 200
  - [ ] `/v1/chat/completions` streaming works, SSE
  - [ ] `/v1/messages` non-streaming works, 200 (Claude shape)
  - [ ] `/v1/messages` streaming test
  - [ ] 401 (invalid api_key) → 401
  - [ ] cross-site Origin → 403
  - [ ] `/healthz` unauthorized → minimal response
  - [ ] empty account pool + web cookie present → bridge path triggered (Mock upstream returns chat/stream SSE)
- [ ] The dependencies of `build_router` (each `AppState` field) need test doubles: `MemoryStore`/`SkillsManager` can genuinely open via tempfile; `Pool` uses an empty pool; `UpstreamClient` points at the Mock address

### D. Regression guarantees
- [ ] The existing 219 unit tests + core_test.rs all continue to pass (this change doesn't break them)
- [ ] After panel changes, run `node tests/check_panel_js.cjs` (if present) + the panel-related assertions in phase_g/phase_i
- [ ] `cargo clippy -- -D warnings` zero warnings (enforced by Rust rules)
- [ ] `cargo fmt --check`

## 5.3 Coverage targets

- No llvm-cov baseline currently exists; **v0.8 target: src/ line coverage ≥ 80%** (focus on core hot paths: api.rs main path + router + errors + retry)
- Command: `cargo llvm-cov --fail-under-lines 80` (can be added to CI)
- Exclusions: `login_window_*` (platform-gated FFI), `ads.rs` (pure external side effects)

## 5.4 E2E expansion (real upstream optional, prefer Mock)

| Scenario | Description |
|------|------|
| Bridge continued conversation | Two consecutive `/v1/chat/completions` calls (with assistant history) → assert the second call reuses the thread (Mock upstream records threadId) |
| Multi-account switchover | Mock upstream returns 500 for token-A, 200 for token-B → assert the second call uses token-B |
| API key rotation | generate → use → rotate (old key valid during grace period) → removed |
| Upload | small local file via `--data-binary` → assert success + returns storageId (real web-credential scenario) |
| Panel smoke test | every tab is clickable, SSE connects, memory toggle takes effect |

## 5.5 Test infrastructure recommendations (no heavy new dependencies)

- Prefer `httpmock` for the Mock upstream (dev-dependency, lightweight); if adding a new dev-dep is restricted, use `tokio::net::TcpListener` + a simple HTTP response (retry.rs already uses this pattern and it can be reused)
- No test may use real tokens or hit the real paid upstream (per the paid-API red line: verify via Mock/recording)
- New test files go in `tests/`, keeping `#![cfg(test)]` and the AAA structure (Arrange-Act-Assert)

## 5.6 Pre-delivery self-check (against the global rules)

- [ ] No TODOs/empty implementations/fake implementations; every new function has an execution path and a test
- [ ] Every feature claimed "done" has been run with an actual command, with output attached
- [ ] Unverified content is clearly marked "pending verification"
- [ ] Rust rules: `cargo fmt` + `cargo clippy -D warnings` + coverage target met
- [ ] Old artifacts cleaned up: no leftover debug output, no unused imports, `target/` excluded from the tarball
