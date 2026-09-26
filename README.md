# Freebuff2API

> English documentation (Rust version). Chinese docs: [README_zh.md](README_zh.md)

Freebuff2API reverse-engineers the [Freebuff](https://freebuff.com) free tier into a local **OpenAI-compatible** and **Anthropic-compatible** API gateway. **Implemented in Rust (axum)** — single binary, zero dependencies — so you can use Freebuff's free models from any OpenAI/Claude client (Claude Code, Codex, Cursor, LobeChat, etc.).

## Core features

- **Dual-protocol egress** — `POST /v1/chat/completions` (OpenAI, streaming/non-streaming) + `POST /v1/messages` (Claude), works with any OpenAI SDK.
- **Smart multi-account rotation** — multiple Bearer tokens / web cookies, health scoring + cooldown circuit-breaking + best-account selection.
- **Dual-bucket concurrency semaphore** — reverse-engineered from the desktop client and shipped (v0.8): free tier `{slots:1, concurrency:3}`, subscriber tier `{slots:3, concurrency:8}`, gateway-global. Each request holds one "slot" and one "concurrency" permit at the same time, so the **real concurrency cap equals slot capacity** (1 for free tier, 3 for subscribers); a 2s timeout returns 429.
- **Session keepalive** — 45s heartbeat + ad refresh to extend quota; queued requests return Retry-After; 401 triggers automatic cooldown.
- **Reasoning-effort downgrade** — reverse-engineered from the upstream `efforts` field: glm/deepseek support `low/high/max`, solar/minimax/mimo don't support it and have it stripped automatically; Codex requests with an out-of-range effort are auto-downgraded.
- **Balance/credit query** — `GET /api/account/balance`: freebucks credits, daily remaining per model, plan, regional restrictions.
- **One-click token import** — paste a curl / HAR / Cookie string and it's parsed and stored automatically; the desktop tray's "one-click login" opens an embedded browser and captures cookies automatically.
- **Web-protocol adapter** — `POST /api/chat/stream` (cookie-authenticated SSE, 11 event types), multimodal upload, tool-call mapping.
- **Usage stats** — SQLite records requests/tokens/latency/errors + a built-in control panel (`/ui`).
- **Desktop installer** — Electron shell auto-starts the gateway + tray icon + one-click OAuth login + update checking.
- **Docker / CI** — multi-stage image + GitHub Actions auto-builds the installer.

## Quick start

### Desktop (recommended)
1. Download the latest `Freebuff2API Setup x64.exe` (Releases page, currently v0.8.x)
2. Install and double-click → the gateway auto-starts and the console opens
3. Tray "One-click login for new account" → log in to freebuff.com in the browser → cookies are captured and stored automatically

### From source
```bash
build.bat                      # Windows build
./target/release/freebuff2api  # Linux/macOS build via cargo build --release
start.bat                      # Windows start
```

### Docker
```bash
docker build -t freebuff2api -f docker/Dockerfile .
docker run -d -p 47821:47821 -v /data:/data freebuff2api
```

## Configuration (config.json)

```jsonc
{
  "listen_addr": "127.0.0.1:47821",
  "upstream_base_url": "https://www.codebuff.com",
  "auth_tokens": ["bearer-token-1", "bearer-token-2"],
  "api_keys": ["sk-local"],
  "http_proxy": "http://127.0.0.1:10808",
  "ad_providers": ["gravity"],
  "sqlite_path": "data/freebuff2api.sqlite",
  "token_saver": false,
  "memory_enabled": false
}
```

Environment variables take precedence: `AUTH_TOKENS` / `API_KEYS` / `HTTP_PROXY` / `LISTEN_ADDR` / `AD_PROVIDERS` / `SQLITE_PATH` / `MEMORY_ENABLED`.

### Memory layer (optional, off by default)

The gateway ships a **zero-LLM, rule-based** local memory store (SQLite, `data/memory.sqlite`): purely deterministic rules automatically record "frequently used models / reasoning-effort downgrades / your corrections (`remember...`, `stop doing...`, `always/never`)" plus manual entries, and inject them as a low-authority system prefix into relevant conversations.

- **Off by default**: `memory_enabled: false`. Not everyone needs memory — keep it off when you don't, for zero extra injection into requests.
- **How to enable it**:
  1. The switch at the top of the panel's "Memory" page (`POST /api/memory/toggle`, writes back to config.json and **takes effect immediately, no restart needed**);
  2. Or set `"memory_enabled": true` in config.json and restart;
  3. Or set the `MEMORY_ENABLED=true` environment variable.
- While off, nothing is recorded or injected automatically; existing data stays in `data/memory.sqlite` and becomes usable again once re-enabled.

## API

| Endpoint | Method | Description |
|------|------|------|
| `/v1/chat/completions` | POST | OpenAI chat |
| `/v1/messages` | POST | Claude chat |
| `/v1/models` | GET | Model list |
| `/api/tokens/import` | POST | Import curl/HAR/Cookie |
| `/api/account/balance` | GET | Account credits / per-model remaining |
| `/api/account/detail` | POST | Account detail card |
| `/api/usage/*` | GET | Usage stats |
| `/ui` | GET | Control panel |
| `/healthz` | GET | Health check |

### curl examples

Set these once (use your deployed URL instead of localhost, e.g. `https://your-app.onrender.com`):

```bash
export BASE=http://127.0.0.1:47821
export KEY=sk-local-xxxxxxxx   # a value from api_keys; any string works if api_keys is empty and you're on localhost
```

Every endpoint except `/ui` and `/healthz` needs the key, as `Authorization: Bearer $KEY` (Claude-style `x-api-key: $KEY` also works). Admin `POST`s must send `content-type: application/json` (CSRF protection).

**`POST /v1/chat/completions`**: OpenAI chat. `model` and `messages` are required; `stream` and `reasoning_effort` are optional.

```bash
curl $BASE/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{"model":"z-ai/glm-5.3-flash","messages":[{"role":"user","content":"Say hi"}]}'

# streaming (Server-Sent Events)
curl -N $BASE/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{"model":"z-ai/glm-5.3-flash","stream":true,"messages":[{"role":"user","content":"Say hi"}]}'
```

**`POST /v1/messages`**: Claude (Anthropic) chat. `model`, `max_tokens` and `messages` are required.

```bash
curl $BASE/v1/messages \
  -H "x-api-key: $KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "content-type: application/json" \
  -d '{"model":"z-ai/glm-5.3-flash","max_tokens":1024,"messages":[{"role":"user","content":"Say hi"}]}'
```

**`GET /v1/models`**: list the model ids you can pass as `model`.

```bash
curl $BASE/v1/models -H "Authorization: Bearer $KEY"
```

**`POST /api/tokens/import`**: add a Freebuff account. Send the full `Cookie` header copied from freebuff.com (it must contain `__Secure-next-auth.session-token=`); use `"text"` instead of `"cookie"` to paste a whole curl command or HAR.

```bash
curl $BASE/api/tokens/import \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{"cookie":"__Secure-next-auth.session-token=PASTE_VALUE_HERE"}'
```

**`GET /api/account/balance`**: credits and remaining daily quota per model (uses the first imported cookie).

```bash
curl $BASE/api/account/balance -H "Authorization: Bearer $KEY"
```

**`POST /api/account/detail`**: account card (plan, limits, balance). The body may be `{}` (first imported account) or `{"cookie":"..."}` for a specific one.

```bash
curl $BASE/api/account/detail \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{}'
```

**`GET /api/usage/*`**: usage stats. Available: `totals`, `daily` (last 7 days), `requests` (last 50), `requests/{id}`, `models`, `cost`, `insights`, `accounts`.

```bash
curl $BASE/api/usage/totals -H "Authorization: Bearer $KEY"
curl $BASE/api/usage/requests -H "Authorization: Bearer $KEY"
```

**`GET /ui`**: control panel; open `$BASE/ui` in a browser.

**`GET /healthz`**: health check, no key needed (with the key it also returns accounts and model count).

```bash
curl $BASE/healthz
```

Full guide: [docs/API_GUIDE.md](docs/API_GUIDE.md).

## Multi-account rotation and concurrency
- Each request automatically picks the healthiest account
- Upstream dual-bucket concurrency limit (shipped in v0.8): free tier `{slots:1, concurrency:3}`, subscriber tier `{slots:3, concurrency:8}` (real concurrency cap = slot count, see above)
- Waiting room: 429 + retry-after automatic backoff

## Reasoning-effort support matrix (reverse-engineered from upstream)

| Model | Supported efforts |
|------|-------------|
| deepseek/*, z-ai/glm, stealth/ox-alpha | `low, high, max` |
| openai/gpt-5.6*, gemini-3.8, claude-fable-5 | `low, medium, high, xhigh, max` |
| meta/muse-spark* | `minimal, low, medium, high, xhigh` |
| solar-pro4, minimax-m3, mimo-v2.5, kimi-k3 | Not supported (stripped automatically) |

## Testing and verification

```bash
cargo test        # 253 unit tests + 8 integration + 11 router-level integration, all green (v0.9.0)
cargo clippy --all-targets -- -D warnings  # zero warnings
```

Real E2E has been verified: token import (curl/HAR/Cookie) OK, balance query OK, account detail OK, panel OK, upstream smoke test OK.

## Directory layout

```
src/                Rust gateway source
  api.rs            HTTP routes
  web_protocol.rs   web-version protocol (cookie auth chat/stream/balance)
  import.rs         token import parsing
  usage.rs          SQLite usage stats
desktop/            Electron desktop shell
legacy-go/          old Go implementation (archived)
reference/          archived upstream reverse-engineering sources
docs/               API guides
```

## Disclaimer

This project has no official affiliation with OpenAI, Codebuff, or Freebuff. It is provided "as is" for communication, experimentation, and learning purposes only; use at your own risk.

## License

MIT
</content>
