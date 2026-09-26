# Freebuff2API API Docs and Usage Guide

Rust-based OpenAI/Anthropic-compatible gateway + multi-account rotation + balance lookup + desktop installer.

## Quick start

### Option 1: Desktop installer (recommended)
1. Download the latest `Freebuff2API Setup x64.exe` (Release page, currently v0.8.x)
2. After installing, double-click the desktop shortcut -> gateway launches automatically + console opens
3. Tray "One-click login for new account" -> log in to freebuff.com in the browser -> cookie is captured and stored automatically

### Option 2: From source
```bash
# Build
build.bat          # or cargo build --release
# Start
start.bat          # or ./target/release/freebuff2api.exe --config config.json
```

### Option 3: Docker
```bash
docker build -t freebuff2api -f docker/Dockerfile .
docker run -d -p 47821:47821 -v /data:/data freebuff2api
```

## Configuration (config.json)

```jsonc
{
  "listen_addr": "127.0.0.1:47821",
  "upstream_base_url": "https://www.codebuff.com",
  "auth_tokens": ["token1", "token2"],   // desktop-app API token (codebuff.com)
  "api_keys": ["sk-local"],               // auth for this gateway (empty = no check; once set, fill in the Key at the top right of the panel)
  "http_proxy": "http://127.0.0.1:10808", // supports http/socks5
  "ad_providers": ["gravity"],            // ad-based keepalive
  "sqlite_path": "data/freebuff2api.sqlite",
  "telemetry_path": "data/telemetry.sqlite",  // request detail / event chain
  "tokens_path": "data/tokens.json",          // imported credential storage location
  "skills_dir": "data/skills",                // skills directory (SKILL.md)
  "skills_inject_mode": "roster",             // roster (name+description) | full (everything)
  "max_roster_tokens": 2000,                  // roster injection budget
  "token_saver": false,
  "redact_logs": true,                        // log/telemetry redaction (default on)
  "concurrency_free_slots": 1,                // dual-bucket semaphore: free tier {paid slot, normal}
  "concurrency_free_multi": 3,
  "concurrency_sub_slots": 3,                 // subscription tier {paid slot, normal}
  "concurrency_sub_multi": 8
}
```

Environment variables take priority: `AUTH_TOKENS` / `API_KEYS` / `HTTP_PROXY` / `LISTEN_ADDR` / `UPSTREAM_BASE_URL` / `AD_PROVIDERS` / `SQLITE_PATH` / `TELEMETRY_PATH` / `TOKENS_PATH` / `SKILLS_DIR` / `SKILLS_INJECT_MODE` / `MAX_ROSTER_TOKENS` / `REDACT_LOGS` / `CONCURRENCY_FREE_SLOTS` / `CONCURRENCY_FREE_MULTI` / `CONCURRENCY_SUB_SLOTS` / `CONCURRENCY_SUB_MULTI`.

> **Dual-bucket concurrency semaphore** (landed in v0.8): free tier allows at most 1 paid slot + 3 normal concurrent requests at the same instant, subscription tier 3 + 8 (gateway-global, shared across all accounts). Capacity is adjustable (`concurrency_*`), editable on the panel's "Settings" page; exceeding capacity and not getting a slot within 2 seconds returns 429 (`concurrency_busy`), to avoid tripping upstream risk control. Conservative rule: if any credential in the account pool carries a plan feature such as `unique_subscription`, it goes to the subscription bucket, otherwise the free bucket.

## API endpoints

### OpenAI / Anthropic compatible
| Endpoint | Method | Description |
|------|------|------|
| `/v1/chat/completions` | POST | Chat (streaming/non-streaming), automatic multi-account rotation |
| `/v1/models` | GET | List of available models (`data` compatible; top-level `meta` array includes id/agent/premium/multimodal/available/efforts/fallback/availability/available_at) |
| `/v1/messages` | POST | Anthropic protocol chat (streaming is a standard Anthropic event stream) |
| `/v1/web/chat` | POST | Web-protocol chat (cookie auth; `images` supports multimodal) |
| `/v1/uploads` | POST | Upload a file to get a storageId (raw body + `x-file-name` header, up to 20MB) |

### Accounts and credentials
| Endpoint | Method | Description |
|------|------|------|
| `/api/tokens/import` | POST | Paste a curl/HAR/cookie to auto-extract and store credentials (also accepts `{"cookie":"..."}` JSON); duplicate values are auto-deduplicated, returns `added` |
| `/api/tokens` | GET | List imported credentials: stable `id`, mask, type, import time, cached `meta` from the most recent account lookup |
| `/api/tokens/check` | POST | Fetch the full account profile for a given credential and refresh the cache `{id}` -> `{ok, valid, meta}` |
| `/api/tokens/delete` | POST | Delete a credential `{id}` (also removes it from the running account pool) |
| `/api/account/overview` | GET | Full account profile (identity/usage stats/plan/quota points), aggregating 4 upstream endpoints |
| `/api/account/history` | GET | Account usage history (one snapshot per check/refresh) `?cred_id=&limit=` |
| `/api/accounts/health` | GET | Credential health dashboard (Bearer + web cookie combined: health_score/circuit_state/cooldown_until(ISO)/cooldown_seconds/trips/last_error + history timeline) |
| `/api/account/balance` | GET | Account points / remaining calls per model / plan / regional limits |
| `/api/account/detail` | POST | Account detail card (balance + usage + user + plan) |
| `/api/account/refresh` | POST | Credential keepalive check (calls upstream convex-token to verify the cookie is still valid) |
| `/api/guide` | GET | Client integration info: listen address, OpenAI/Anthropic addresses, key status, model count and samples |

### Browser extension
| Endpoint | Method | Description |
|------|------|------|
| `/api/extension/bundle` | GET | Download the one-click login extension zip (content is embedded at compile time, works as a single-file distribution too; use `?key=` when api_keys is set) |

### Configuration
| Endpoint | Method | Description |
|------|------|------|
| `/api/config/api-key` | POST | Manage the downstream API key at runtime: `{"action":"generate"\|"set"\|"clear","key"?}`; **takes effect immediately** and writes back to `config.json` (clearing is disallowed when not listening on localhost) |
| `/api/config` | GET | Returns editable config items for the settings page (redacted; includes semaphore capacity / redaction toggle / memory, etc.) |
| `/api/export` | POST | Export the full configuration (redacted config + credentials + skill enabled state + memory toggle; schema_version=1) |
| `/api/import` | POST | Import the full configuration (schema validation, <=5MB, auto-backs up to `data/backup-<ts>/` before writing, never overwrites api_keys/auth_tokens) |
| `/api/login/embed`, `/api/login/result` | POST/GET | Embedded WebView2 one-click login (desktop app) |
| `/api/config` | POST | Save a single config item `{"key":"listen_addr","value":"..."}`: whitelist validation + type/valid-value checks + atomic write-back to `config.json`; `memory_enabled` takes effect hot immediately, others take effect after restart |

### Skills
| Endpoint | Method | Description |
|------|------|------|
| `/api/skills` | GET | Skill list + roster injection preview (with token budget) |
| `/api/skills` | POST | Create/update a skill `{id?, name, description, body, force?}` |
| `/api/skills/toggle` | POST | Enable/disable `{id, enabled}` |
| `/api/skills/delete` | POST | Delete a custom skill `{id}` (built-in skills cannot be deleted) |
| `/api/skills/gate` | POST | Quality-gate precheck `{body}` -> `{issues: []}` |

### Observability
| Endpoint | Method | Description |
|------|------|------|
| `/api/logs/stream` | GET | Real-time log SSE (supports `Last-Event-ID` to resend after disconnect; use `?key=` when api_keys is set) |
| `/api/logs/recent` | GET | Recent logs (`?limit=200`) |
| `/api/usage/requests/{id}` | GET | Detail for a single request (includes rich telemetry fields and event chain) |
| `/api/usage/cost` | GET | Rate and error rate (30-minute sliding window; honestly labeled as estimated) |
| `/api/doctor` | GET | System health check (four states: ok/fault/unknown/fact) |

### Memory (AI that understands the user better)
| Endpoint | Method | Description |
|------|------|------|
| `/api/memory` | GET | Memory list + stats (total / stable facts / correction count) |
| `/api/memory` | POST | Manually add `{kind, title, content, is_static?}` |
| `/api/memory/delete` | POST | Delete `{id}` |
| `/api/memory/static` | POST | Mark/unmark as a stable fact `{id, is_static}` |

> Auto-recorded (zero LLM): preferred models used, reasoning-effort downgrades, user correction phrases ("remember.../don't ever again.../always/never"). Memory is retrieved based on the current question and injected as a system prefix (512-token budget, low-authority block).

### MCP (read-only tools)
| Endpoint | Method | Description |
|------|------|------|
| `/mcp` | POST | JSON-RPC 2.0 (`initialize` / `tools/list` / `tools/call` / `ping`); tools: `list_models`, `list_accounts`, `usage_summary`; auth matches /v1 |

### Usage statistics
| Endpoint | Method | Description |
|------|------|------|
| `/api/usage/totals` | GET | Total requests/tokens/errors |
| `/api/usage/daily` | GET | Stats by day/model |
| `/api/usage/requests` | GET | Recent request details |
| `/api/usage/models` | GET | Model list (proxy registry) |
| `/api/usage/insights` | GET | v0.10 "top three" aggregation: top 3 slowest accounts / top 5 most-used models / top 3 highest-error-rate time windows (local SQLite) |
| `/api/usage/accounts` | GET | Account health |

### Panel and health
| Endpoint | Method | Description |
|------|------|------|
| `/` `/ui` | GET | Built-in control panel (overview/accounts/skills/logs/doctor/onboarding guide) |
| `/healthz` | GET | Health check |
| `/api/prompts`, `/api/prompts/toggle` | GET/POST | Built-in prompts (legacy endpoint, kept for compatibility) |

## Multi-account rotation strategy
- Each request automatically picks the highest-scoring healthy account (cooling-down/error-prone accounts are down-weighted automatically)
- Upstream concurrency is capped by dual buckets: free `{paid slot:1, normal:3}`, subscription `{paid slot:3, normal:8}` (reverse-engineered from the desktop app)
- Waiting room: 429 + retry-after, automatic backoff

## Reasoning-effort support matrix (reverse-engineered from upstream)

| Model | Supported efforts |
|------|-------------|
| deepseek/*, z-ai/glm, stealth/ox-alpha | `low, high, max` |
| openai/gpt-5.6*, google/gemini-3.8, claude-fable-5 | `low, medium, high, xhigh, max` |
| meta/muse-spark* | `minimal, low, medium, high, xhigh` |
| solar-pro4, minimax-m3, mimo-v2.5, kimi-k3 | **not supported** (auto-stripped) |

> Selecting a model that doesn't support a reasoning effort in Codex/Claude Code -> the gateway automatically strips it or downgrades to the highest supported level.

## FAQ

### How do I do one-click login on the browser version?
The upstream login cookie is **HttpOnly**, so page scripts can't read it -- the browser version therefore needs the extension:

1. Panel "Accounts" page -> click "download extension" to get the zip (or use the project's `browser-extension/` directory directly) -> unzip
2. Open `chrome://extensions` in your browser (`edge://extensions` on Edge) -> turn on "Developer mode" -> "Load unpacked" -> select the unzipped directory
3. Back on the panel -> click "Re-check", the status becomes **Extension ready** -> click "One-click login"

After that it's **fully automatic**: the extension opens freebuff.com -> you complete GitHub login -> the extension writes the credential back to the gateway automatically -> the panel auto-refreshes the full account profile.

Also works without installing the extension: clicking "One-click login" opens freebuff.com and shows a 3-step copy wizard (about 30 seconds). The desktop app (Electron) reads it directly from the main process, fully automatic via the tray.

### Why can't I look up the balance?
The balance endpoint needs the **web-version cookie** (`__Secure-next-auth.session-token=...`), not the desktop-app Bearer token. Log in to freebuff.com in your browser -> DevTools -> Copy as cURL -> paste into `/api/tokens/import`.

### After importing a credential, how do I start making requests?
The card at the top of the panel's "Overview" page, "Start making requests now", gives you the base URL and API key directly (one-click copy):

- OpenAI protocol: `http://127.0.0.1:47821/v1` (Cursor / LobeChat / SDK)
- Anthropic protocol: `http://127.0.0.1:47821` (Claude Code)
- API key: when `api_keys` isn't set, any non-empty string works (e.g. `sk-local`); click "Generate and enable key" to generate one and enable it immediately

The "Onboarding Guide" page has ready-made config snippets for each client, just copy them.

> **Importing only a web cookie (one-click login) also works with /v1**: when the account pool has no Bearer token, the gateway automatically bridges `/v1/chat/completions` and `/v1/messages` to the upstream web protocol (multi-turn conversations reuse the same upstream thread, avoiding burning through the daily session quota).

### What does "remaining today" mean in the credential list?
It comes from upstream `/api/web/freebuff-session`'s `freebucks.daily` and per-model `rateLimitsByModel`:
- **Remaining today** = remaining daily points (freebucks) quota, resets at midnight Pacific time, refreshes automatically the next day
- Per-model "remaining calls today" = that model's daily call limit minus calls used (click "Details" on a credential row to view)
- Every "Refresh full account profile" or "Check" appends a snapshot to "Usage history", queryable by account

### How do I use multiple accounts?
Desktop app: fill in multiple Bearer tokens in `auth_tokens`. Web version: do "one-click login" or import a cookie multiple times. Credentials are auto-deduplicated by value; each row can be individually "checked / viewed in detail / deleted".

### 0-point models
`upstage/solar-pro4` is free at 0 points (still capped at 6 uses per day in the pool); `z-ai/glm-5.3-flash` runs through the Reward bonus pool.

## Updates
- Desktop app: tray checks for updates (electron-updater)
- Docker: `docker pull` the latest image
- GitHub Actions auto-build: tagging `v*` automatically produces the installer
