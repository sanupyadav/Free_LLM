# 2. Architecture hardening plan (v0.8 P0/P1)

> Goal: close the gap between what was claimed but not landed, plus upstream anti-abuse/stability shortfalls, in one pass, **without changing existing module boundaries** (no refactor), only "wiring in" on top of the existing structure.

---

## 2.1 Dual-bucket concurrency semaphore (landing the plan README already claims is implemented)

### Current problem
Both README and README_en claim "dual-bucket concurrency semaphore -- reverse-engineered from upstream: free `{slot:1, normal:3}`, subscription `{slot:3, normal:8}`", but `rg "Semaphore|semaphore|slot" src/` gets **zero hits** -- the api.rs forwarding paths (`handle_chat_completions` / `handle_claude_messages` / `web_bridge_openai`) have no concurrency governor at all. Under high multi-account concurrency this can hit the upstream concurrency wall (429 / worse waiting_room / risk-control fingerprinting).

### Design (minimal change, no refactor)

New module `src/semaphore.rs`:

```rust
pub struct TieredSemaphore {
    free: Arc<tokio::sync::Semaphore>,    // free tier {slot:1, multi:3}
    subscriber: Arc<tokio::sync::Semaphore>, // subscription tier {slot:3, multi:8}
}

impl TieredSemaphore {
    pub async fn acquire(&self, is_subscriber: bool) -> Result<TierGuard, AcquireError>;
    // TierGuard holds the permit; auto-returned on Drop (RAII, prevents leaks)
}
```

- **Tier determination**: when a request comes in, first check whether the account pool's `AccountEntry` carries subscription info (session `access_tier` or `unique_subscription`). Default to the free bucket if there's no subscription info (conservative).
- **Integration points**: `handle_chat_completions` (the Bearer main path), `handle_claude_messages`, and `web_bridge_openai` -- acquire **before writing the first byte**; return the permit via Drop after the stream finishes (background finalization task).
- **Timeout**: `try_acquire` + 2s timeout; on timeout return a 429-level error (aligned with `waiting_room` semantics), no unbounded queuing.
- **Multi-account concurrency**: the semaphore is **gateway-global** (shared across all accounts), because the upstream concurrency wall is measured per **gateway IP/account**, not per single session.
- Config: add `concurrency_free_slots / concurrency_free_multi / concurrency_sub_slots / concurrency_sub_multi` to `config.rs` (defaults 1/3/3/8), and keep the panel's "How it works" page docs in sync.
- Env vars: four `CONCURRENCY_*` vars, following the same env-override pattern as `parse_duration_sec`.

### Acceptance criteria
- [ ] Unit tests: the free and subscriber buckets don't interfere with each other; acquire blocks/times out correctly when permits are exhausted; can acquire again after Drop returns the permit; no guard leaks (semaphore count restores after a fixed-count loop).
- [ ] Integration test: with the mock upstream responding slowly, 50 concurrent requests -> number passing through at once <= bucket capacity.
- [ ] The panel's "How it works" page description matches actual behavior, matching the README.

---

## 2.2 Claude path (/v1/messages) retry + accounting + memory (currently missing)

### Current problem (verified at the code level)
Compared with the OpenAI path, `handle_claude_messages` (api.rs:2271) is **missing three things**:
1. **No retry**: the error from `ensure_session` is swallowed by `let _ = ...`; upstream 5xx / waiting_room goes straight to a 502, no account swap.
2. **No usage accounting**: the success path returns `Json(claude_resp)` directly, with no `usage_db.record_ex` / `telemetry.record`; the streaming path has telemetry but no usage persisted to the DB.
3. **No memory observe**: `memory.observe` is only called on the OpenAI path, so Claude users' preferences/corrections are never learned.

### Design
- **Retry**: extract the OpenAI path's "request-level retry loop" (`for attempt in 0..max_attempts` + account reselection + circuit breaker/cooldown + waiting_room handling) **into a shared async helper** (in api.rs or a new `src/request_pipeline.rs`), reused by both the OpenAI and Claude paths; convert the Claude path's error response body to the Claude error shape (the reusable parts of the existing `openai_error_to_claude` apply).
- **Accounting**: parse `prompt_tokens / completion_tokens` out of the non-streaming Claude response (reuse `extract_usage`), and add `usage_db.record_ex` + `telemetry.record` (including `client_ip`). The streaming path already records telemetry but is missing the usage persistence -- fill that in too.
- **Memory**: add `memory.observe(&resolved, effort_downgraded.as_deref(), &mem_query, None)` to the success path (honoring the existing runtime toggle).

### Acceptance criteria
- [ ] Claude non-streaming requests are visible in panel usage/request detail.
- [ ] Claude queuing (waiting_room) returns 503 + a readable message instead of a raw 502.
- [ ] Claude auto-retries with an account swap on 5xx (verified via mock upstream call count).
- [ ] When the memory toggle is on, Claude conversations trigger observe (test-case count increases).

---

## 2.3 Debt-fix checklist (P1, small individual changes)

| # | Location | Problem | Fix |
|---|------|------|------|
| 1 | api.rs `handle_account_balance` | `looks_like_cookie` containing `t.contains("%3A")` is too broad, misclassifying any URL-encoded string as a cookie | Narrow to `session-token` / `.next-auth` / `callback-url` |
| 2 | models.rs `models_sync()` | Returns only a hardcoded list, semantically drifted from the snapshot | Add a comment: "// Note: hardcoded baseline only, for fast non-async reads; use models()/snapshot() for the real registry state"; no behavior change |
| 3 | api.rs `handle_claude_messages` non-streaming success | No telemetry/usage | See 2.2 |
| 4 | desktop/main.js tray "System Doctor" | `loadURL('#doctor')` doesn't work for hash routing | Change to loading `./ui#doctor` + frontend `showTab('doctor')` reading the hash |
| 5 | README.md / README_en.md | Version number/`Setup 0.3.0` out of date, "dual-bucket semaphore" not actually implemented | Unify version to 0.8.0 wording; claim the semaphore only once it's landed |
| 6 | web_threads.rs | Binding table has no capacity cap or TTL cleanup | Add `MAX_BINDINGS` (default 2000) + evict entries older than 24h; trim on file write |

---

## 2.4 Out of scope (to avoid scope creep)

- Not refactoring the giant functions in api.rs (the 4.2K-line function stays as-is, only wired internally)
- Not introducing new heavyweight frameworks/dependencies (keep `Cargo.toml`'s dependency surface unchanged) -- the semaphore uses tokio's own `Semaphore`
- Not touching the upstream protocol layer (`upstream.rs` / `web_protocol.rs` request-header and protocol details are reverse-engineering results, left untouched)
- Not doing SSE batch commits in this round (workflow_status already assessed the benefit/risk ratio as insufficient, kept as backlog)

## 2.5 Rollback and compatibility

- Every new config item has a default value (matching current behavior exactly); old config.json runs with no migration needed
- The semaphore addition is "additive": when unconfigured = current behavior (no limit), configured = takes effect; **or enabled by default (safer), with defaults 1/3/3/8, toggleable in the panel**
- Each fix is an independent commit and can be reverted individually
