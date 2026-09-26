# Freebuff2API

> English version. Chinese docs: [README_zh.md](README_zh.md)

Freebuff2API reverse-engineers the [Freebuff](https://freebuff.com) free tier into **OpenAI-compatible** and **Anthropic-compatible** local API endpoints. Implemented in **Rust (axum)** — single binary, zero runtime deps — usable from Claude Code, Codex, Cursor, LobeChat, or any OpenAI SDK.

## Features

- Dual protocol: `POST /v1/chat/completions` (OpenAI) + `POST /v1/messages` (Claude)
- Multi-account rotation: health-scored pool with cooldown/circuit-breaking
- Dual-bucket concurrency semaphore (reverse-engineered from desktop, implemented in v0.8): free `{slot:1, concurrency:3}`, subscriber `{slot:3, concurrency:8}` — gateway-global. Each request takes 1 slot + 1 concurrent permit → real concurrency cap = slot capacity (free 1, subscriber 3), 2s timeout → 429
- Session keepalive: 45s heartbeat + ad-based quota refresh
- Reasoning-effort downgrade (from upstream efforts field)
- Balance/quota query: `GET /api/account/balance` (freebucks, per-model daily remaining)
- One-click credential import: curl / HAR / Cookie
- Web protocol adapter: Cookie-auth SSE chat, multimodal upload, tool-call mapping
- SQLite usage stats + built-in dashboard (`/ui`)
- Electron desktop shell + Docker + GitHub Actions CI/CD

## Quick Start

### Desktop (recommended)
Download the latest `Freebuff2API Setup x64.exe` from Releases (currently v0.9.x) → install → launch → gateway auto-starts → dashboard opens. Use tray "Login new account" to auto-capture cookies.

### Source
```bash
cargo build --release
./target/release/freebuff2api --config config.json
```

### Docker
```bash
docker build -t freebuff2api -f docker/Dockerfile .
docker run -d -p 47821:47821 -v /data:/data freebuff2api
```

## API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/chat/completions` | POST | OpenAI chat |
| `/v1/messages` | POST | Claude chat |
| `/v1/models` | GET | Model list |
| `/api/tokens/import` | POST | Import curl/HAR/Cookie |
| `/api/account/balance` | GET | Credits / per-model quota |
| `/api/account/detail` | POST | Account detail card |
| `/api/usage/*` | GET | Usage stats |
| `/ui` | GET | Dashboard |
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

Full guide: [docs/API_GUIDE.md](docs/API_GUIDE.md)

## License
MIT
