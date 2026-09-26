# Freebuff2API — Rust Edition

A ground-up implementation built on the reverse-engineered protocol of the upstream Freebuff-0.0.98 desktop client, providing a local API gateway with **OpenAI compatibility + Anthropic compatibility + smart multi-account round-robin + ad-based keep-alive + a usage stats panel**.

## Core features

| Feature | Description |
|------|------|
| 🚀 Rust (axum) high-performance gateway | Single binary, memory-friendly, ~17MB, no runtime dependencies |
| 🔄 Smart multi-account round-robin | Session health scoring + cooldown circuit breaker + optimal token selection |
| ⏱️ Session keep-alive | Heartbeat every 45s + `x-freebuff-heartbeat`, ad refresh extends quota before expiry |
| 📊 Usage stats | SQLite records requests/tokens/latency/errors, built-in control panel |
| 🧭 Model routing fallback | Automatically switches to an alternate free model if the primary model fails |
| 🖥️ Desktop installer | Electron shell + process auto-start (`desktop/` directory) |
| 🐳 Docker image | Multi-stage build, fully static binary |

## Quick start

```bash
# 1. Configure
cp config.example.json config.json
# Fill in AUTH_TOKENS (comma-separated for multiple)

# 2. Run
cargo run --release -- --config config.json
# Or run the precompiled binary directly
./target/release/freebuff2api.exe --config config.json
```

## Configuration

```jsonc
{
  "listen_addr": "127.0.0.1:47821",     // listen address (fixed to localhost for the desktop build)
  "upstream_base_url": "https://www.codebuff.com",
  "auth_tokens": ["token1","token2"],  // multi-account round-robin
  "api_keys": ["sk-local"],            // gateway auth (unchecked if empty)
  "http_proxy": "http://127.0.0.1:10808",  // supports http/socks5
  "ad_providers": ["gravity"],         // ad keep-alive provider
  "sqlite_path": "data/freebuff2api.sqlite",
  "token_saver": false
}
```

## API

| Endpoint | Description |
|------|------|
| `GET /healthz` | health + account health snapshot |
| `GET /v1/models` | OpenAI model list |
| `POST /v1/chat/completions` | OpenAI-compatible chat |
| `POST /v1/messages` | Anthropic-compatible chat |
| `GET /` `GET /ui` | built-in control panel |
| `GET /api/usage/totals` | usage totals |
| `GET /api/usage/daily` | per-day/per-model stats |
| `GET /api/usage/requests` | recent requests |
| `GET /api/usage/accounts` | account health |

## Protocol reverse-engineering source

Protocol details are entirely reverse-engineered from `reference/reverse/orchestrator/orchestrator.js` (a Freebuff-0.0.98 desktop client bundle):

- `POST /api/v1/freebuff/session` with `x-freebuff-model` `x-freebuff-instance-id` `x-freebuff-multi-session: 1`
- `GET /api/v1/freebuff/session` with `x-freebuff-include-unused-rate-limits` to fetch the account's available RateLimits
- Heartbeat: `GET` + `x-freebuff-heartbeat: 1`
- `POST /api/v1/agent-runs` (START/FINISH + `ancestorRunIds`)
- `POST /api/v1/ads` (ad auction) → `/api/v1/ads/impression` (first_party confirmation)
- `POST /api/v1/chat/completions` (OpenAI-compatible, injects `codebuff_metadata`)

## Directory structure

```
src/
├── main.rs      # startup and dependency wiring
├── config.rs    # config loading (JSON + env vars)
├── models.rs    # model registry (upstream fetch + hardcoded base)
├── upstream.rs  # upstream Codebuff HTTP client
├── session.rs   # Freebuff session management (queueing/active/heartbeat)
├── pool.rs      # multi-account pool (scoring/cooldown/circuit breaker)
├── ads.rs       # ad-for-token keep-alive
├── router.rs    # model routing fallback + token saving
├── usage.rs     # SQLite usage stats
├── web.rs       # embedded control panel
└── api.rs       # HTTP routes
desktop/       # Electron desktop shell (packages the NSIS installer)
legacy-go/     # previous Go implementation (archived)
reference/     # archived upstream reverse-engineering sources
```

## Testing

```bash
cargo test        # unit + integration tests
cargo clippy      # static checks, zero warnings
```
