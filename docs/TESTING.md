# Testing - Freebuff2API Test Ledger (v0.10)

> Single source of truth for testing: counts, commands, baselines, environment limitations. **Check here before changing code.**

## 1. Test Scale (v0.10.0 Baseline)

| Layer | Count | Command |
|----|------|------|
| Unit tests (within src) | 275 | `cargo test --lib` |
| Router-level integration (Mock TCP upstream) | 11 | `cargo test --test router_test` |
| Core integration | 8 | `cargo test --test core_test` |
| web Cookie pool integration | 5 | `cargo test --test web_pool_test` |
| Model metadata contract | 7 | `cargo test --test model_meta_test` |
| Doc tests | environment-limited | `cargo test --doc` (CI has the rust-docs component and can run this; skipped locally if chocolatey/rustup is missing rustdoc) |
| Real gateway E2E | 26+26 assertions | `node tests/e2e_phase_v0_8.cjs <port>` / `e2e_phase_v0_9.cjs <port>` (requires starting the gateway first) |
| Panel JS syntax | 1 | `node tests/check_panel_js.cjs` |
| Model contract alignment | 1 | `node scripts/check_model_contract.mjs` |

## 2. Local One-Click Verification

```powershell
powershell -ExecutionPolicy Bypass -File scripts/verify_release.ps1
```

Coverage: fmt --check -> clippy -D warnings -> lib unit tests -> 4 integration tests -> panel JS -> model contract -> release smoke test (healthz/ui/models 200).

## 3. Running E2E Manually

```powershell
# 1) Start the gateway (temporary port)
target\release\freebuff2api.exe --config <temp config (listen_addr 127.0.0.1:47980, skip_upstream_check=true)>
# 2) Run the assertions
node tests/e2e_phase_v0_8.cjs 47980 config.e2e.json
node tests/e2e_phase_v0_9.cjs 47980 config.e2e.json
```

## 4. Coverage (v0.10.2 Baseline, measured locally 2026-09-19)

- Tool: `cargo-llvm-cov 0.9.1` (`cargo install cargo-llvm-cov --locked`; requires `rustup component add llvm-tools-preview`), run with: `cargo llvm-cov --fail-under-lines 70`
- **TOTAL line coverage 72.25% (v0.10.3)**; **CI real gate**: `--fail-under-lines 70` + `continue-on-error: false`
- After adding 22 Router-level test cases this round: **api.rs 30.6% -> 45.8%**; router_test 11 -> **33** cases
- Key business modules: errors 97.9 / redact 98.5 / mcp 98.6 / telemetry 94.4 / memory 93.9 / import 92.8 / router 89.0 / web_pool 84.6 / export 84.0 / models 83.2
- Dragging the overall average down: main.rs 0 (startup path), upstream 51.6 / session 65.6 / web_protocol 76.9 (network layer, needs a mock upstream)
- **Improvement path (backlog)**: api.rs 45.8 -> 60 (auth/routing error branches) -> overall 75% -> 80%

## 5. Environment Limitations (Honest Disclosure)

- The local cargo is rustup 1.95.0 stable-msvc; `cargo test --doc` needs the rust-docs component (CI has it installed; locally, if missing, run `rustup component add rust-docs`).
- E2E requires either genuine upstream connectivity or an empty-credentials config; the mock-upstream scenario is covered by router_test (does not hit the real paid API).
