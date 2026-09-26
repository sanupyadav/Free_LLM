> Historical archive: acceptance/delivery report from the v0.1.0 era, now outdated (port 8787, old test counts), kept for reference only. See the README and workflow_status.md for the current version.

# Real-Path E2E Verification Results (2026-09-10)

## Verified and Passed (with real output)

| # | Verification item | Command | Result |
|---|--------|------|------|
| 1 | Service startup | `./target/release/freebuff2api.exe --config config.json` | ✅ listening on 127.0.0.1:8787 |
| 2 | Health check | `curl /healthz` | ✅ `{"ok":true,...model_count:20}` |
| 3 | Model list | `curl /v1/models` | ✅ 20 models (reverse-engineered from the 0.0.98 manifest) |
| 4 | Upstream model supplementation | startup log | ✅ `1 model supplemented into the model registry from upstream` |
| 5 | Control panel | `curl /ui` | ✅ HTTP 200, title `Freebuff2API Console` |
| 6 | Usage statistics | `curl /api/usage/totals` | ✅ genuine record `{"total_requests":1,"errors":1}` |
| 7 | Request detail | `curl /api/usage/requests` | ✅ includes time/account/model/status code |
| 8 | **Token import (curl)** | `POST /api/tokens/import` pasting a curl command | ✅ `{"added":1,"token_masked":"tok2_a...5678"}` |
| 9 | **Token import (HAR)** | `POST /api/tokens/import` pasting HAR JSON | ✅ `{"added":1,"token_masked":"fa82b5...f6a7"}` |
| 10 | Duplicate import dedup | posting the same token again | ✅ `{"added":0,"message":"token already exists"}` |
| 11 | Account pool hot reload | `curl /api/usage/accounts` | ✅ new account `import-fa82b5` appears |
| 12 | Upstream 401 smoke test | `POST /v1/chat/completions` | ✅ received genuine upstream `{"error":"unauthorized"}` (fake token) |
| 13 | Upstream 401 smoke test | `POST /v1/messages` | ✅ received genuine upstream `"Invalid Codebuff API key"` |
| 14 | Port-in-use detection | second startup | ✅ correctly reports `os error 10048` |

## Verified: Full Request Path Genuinely Reached Upstream

A request sent with a test token **genuinely reached** `https://www.codebuff.com/api/v1/freebuff/session`
and received a real upstream response (HTTP 401 `Invalid API key`), proving:

- ✅ Upstream URL/path/header assembly is correct (`x-freebuff-model`, `x-freebuff-multi-session`, Bearer)
- ✅ The TLS path through the local proxy on 10808 works
- ✅ The session state machine (create -> 401 -> error handling -> score penalty -> usage recording) works end to end

## Not Yet Verified (requires a valid Freebuff token)

| # | Item | Reason | Resolution |
|---|-----|------|---------|
| 1 | Session created successfully -> chat -> streaming response | requires a valid Freebuff Auth Token | provide via `/api/tokens/import` or config.json |
| 2 | Ad keep-alive genuinely refreshing quota | requires a valid token | same as above |
| 3 | Heartbeat keep-alive returning 200 | requires a valid token | same as above |

**Where to get a token**: `hb_ff84...` seen in the network capture is a posthog/humanbehavior telemetry key (not a Freebuff API token);
the freebuff.com web version uses a **Cookie session**; the desktop SDK uses a **Bearer token** (after logging in via the
`freebuff` CLI, the `authToken` field in `~/.config/manicode/credentials.json`, see the README_zh for details).

## Conclusion

- Code layer 100% landed (build/clippy/tests all green)
- Protocol layer 100% verified to "a genuine request reached upstream and received a genuine response"
- Business layer (a successful conversation on the free quota) requires a valid token; one-click import capability has been provided
