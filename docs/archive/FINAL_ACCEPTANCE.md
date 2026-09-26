> Historical archive: acceptance/delivery report from the v0.1.0 era, now outdated (port 8787, old test counts), kept for reference only. See the README and workflow_status.md for the current version.

# Freebuff2API Final Delivery Acceptance Report (Round 4 - E2E Final Verification)

> Date: 2026-09-10 | Repo: https://github.com/lza6/Freebuff-2API | Release: v0.1.0

## 1. E2E Final Verification Results (tests/e2e_final.cjs - all 14 items passed ✅)

| # | Acceptance item | Result |
|---|--------|------|
| 1 | GET /healthz returns ok + accounts | ✅ |
| 2 | GET /v1/models >=20 models (including upstream auto-sync) | ✅ |
| 3 | GET /ui panel 200 + includes prompt management section | ✅ |
| 4 | GET /api/usage/totals usage statistics | ✅ |
| 5 | GET /api/usage/requests request detail | ✅ |
| 6 | GET /api/usage/accounts account health | ✅ |
| 7 | GET /api/prompts 6 prompts + 5 skills | ✅ |
| 8 | POST /api/prompts/toggle enabling injects Git expert prefix | ✅ |
| 9 | GET /api/tokens list of imported credentials | ✅ |
| 10 | GET /api/account/balance freebucks balance / remaining per model | ✅ |
| 11 | POST /api/account/detail user + usage + plan | ✅ |
| 12 | Fake token -> genuine upstream 401 error passed through | ✅ |
| 13 | POST /v1/web/chat genuine incremental streaming (content + [DONE] fully terminated) | ✅ |
| 14 | solar-pro4 + max effort -> thinking downgrade/stripped, no 500 | ✅ |

Quality gate: cargo test 19 items all green | clippy zero warnings | release build 9.6MB

## 2. Four-Round Cumulative Delivery Overview

### Round 1 (Rust gateway rewrite)
- Go version archived to legacy-go/; brand-new Rust (axum) implementation: multi-account round-robin pool (scoring + cooldown circuit breaker), session management (queued/active/45s heartbeat), SQLite usage stats, built-in panel, Docker, release CI
- Real-world verification: upstream smoke test (genuinely reached codebuff.com and received a 401 response), installer package installed and run on real hardware

### Round 2 (web protocol + balance)
- Reverse-engineered three network captures (web chat / tool calls / image upload / advanced features) -> web_protocol.rs: full parsing of Cookie-authenticated chat/stream SSE's 11 events, multimodal upload protocol, tool-call events
- Dual-bucket concurrency semaphore (reverse-engineered from the desktop client: free {1,3} / subscriber {3,8}) + 3 unit tests
- Balance query GET /api/account/balance + Cookie import + panel balance card
- Real-world verification: network capture Cookie tested live with balance=95/100, per-model usable_today, plan, region restrictions

### Round 3 (OAuth + deep dive)
- Desktop tray "one-click login for new account": opens an embedded browser to freebuff.com -> on successful login automatically captures the next-auth Cookie -> POST into storage
- POST /api/account/detail: concurrently fetches balance + usage + user + plan (data for the account detail card)
- Thinking-effort matrix (reverse-engineered `efforts` field): glm/deepseek=[low,high,max], gpt/gemini/fable=[low..max], muse=[minimal..xhigh], solar/minimax/mimo/kimi=not supported, auto-stripped; clamp_effort auto-downgrades out-of-range values
- Session cache cache.rs (30min reuse)
- Upstream agent_tool SSE -> OpenAI tool_calls mapping

### Round 4 (this round - recall fix + final verification)
- **Model registry now supports upstream delisting** (no longer additive-only; hardcoded baseline authority retained, models removed upstream are auto-delisted with a warning)
- **Upstream error pass-through** (non-2xx responses pass through message/type/code/model/upstream verbatim; tested with a real 401 Invalid API key)
- **Streaming long connections have no total timeout** (read_timeout is per read-chunk at 5min, so multi-hundred-KB documents aren't truncated)
- **web chat streaming full-termination fix** (Done event -> [DONE] terminates the stream; upstream EOF triggers a supplemental [DONE]; tested with a 38K-length response containing content+DONE)
- **Built-in prompts and skills** (6 prompts + 5 skills, API toggle + system-prefix injection into chat + panel management section)
- **Desktop auto-update** (electron-updater + tray update check)
- README/README_en updated for the Rust version
- start.bat/build.bat (tested cmd startup with healthz 200)
- E2E final verification script all 14 items passed

## 3. Deliverables List

| Form | Location | Status |
|------|------|------|
| Desktop installer | Release v0.1.0 asset Freebuff2API.Setup.0.1.0.exe (81MB, includes gateway) | ✅ Uploaded |
| Source code | GitHub main branch (30+ commits) | ✅ Pushed |
| Docker | docker/Dockerfile multi-stage + CI auto-build (docker.yml) | ✅ Configured |
| CI/CD | build-release.yml (tagging auto-generates Windows package + tests + clippy) | ✅ Configured |
| Docs | docs/API_GUIDE.md + README (Rust version) + 4-round delivery reports | ✅ Committed |
| Reverse-engineering archive | reference/reverse/ (app-src + orchestrator.js) | ✅ Committed |
| Tests | 19 unit tests + 14 E2E final verification script items | ✅ All green |

## 4. Honest Disclosure (Remaining Limitations)

| Item | Notes |
|----|------|
| Image upload E2E | /api/chat/upload times out both directly and via proxy (upstream responds slowly / risk-controls this endpoint); protocol implementation is ready, needs retesting under a logged-in desktop session |
| OAuth login on real hardware | Electron login window logic is implemented, needs actual click-through verification (depends on a GUI environment) |
| Docker live run | No docker on this machine; CI is configured for auto-build, triggered on push |
| Built-in prompt injection scope | Already injected into /v1/chat/completions; /v1/web/chat is a pass-through protocol and is not yet injected (the web-side system prompt is managed upstream) |
| Concurrency-scoring automation | Dual-bucket semaphore implemented + unit tested; the interface for automatic linkage with account-pool scoring is left in place (pick_best already selects by score) |

## 5. Quick Usage Reference

```bash
# Start (Windows)
start.bat                    # or double-click the desktop shortcut (installed version)
# Import credentials (pick one: curl / HAR / Cookie string)
curl -X POST http://127.0.0.1:8787/api/tokens/import -d "<paste content>"
# Check balance
curl http://127.0.0.1:8787/api/account/balance
# Panel
http://127.0.0.1:8787/ui     # account health / balance / prompt toggles / usage / models
# Chat (OpenAI-compatible)
curl -X POST http://127.0.0.1:8787/v1/chat/completions -H "content-type: application/json" -d '{"model":"z-ai/glm-5.3-flash","messages":[{"role":"user","content":"hi"}]}'
# web Cookie version, genuine streaming
curl -X POST http://127.0.0.1:8787/v1/web/chat -d '{"model":"glm-5.3-flash","content":"hi"}'
```
