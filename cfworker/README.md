# Freebuff2API — Cloudflare Worker (experimental)

> ⚠️ **Current status: not usable against the live upstream, for research reference only.**
>
> The Codebuff upstream rejects requests from Cloudflare Workers with `free_mode_cli_required`. This is detection at the **TLS fingerprint layer (Client Hello / JA3)** — the Worker's `fetch` uses Cloudflare's own TLS stack, whose fingerprint differs from the official CLI (Node.js undici/OpenSSL), and **this cannot be bypassed by changing the User-Agent or any HTTP header**. The local Go binary uses the native TLS stack, is not on the blocklist, and passes normally.
>
> **Please use the local Go server in the repo root** (see [../README_zh.md](../README_zh.md)). This directory's code keeps the protocol translation and run/session management implementation for future research.

---

## Contents of this directory

- `worker.js` — stateless routing relay version (D1/KV/DO dependencies removed). Accepts OpenAI-format requests, injects `codebuff_metadata`, and forwards to the Codebuff upstream; credentials are carried directly by the caller in the `Authorization` header.
- `migrations/` — D1 database migrations (leftover from an earlier SaaS version with credential management/audit stats).
- `test-api.js`, `test-upstream.js` — E2E test scripts.

## Reverse-engineering notes (covered by this implementation)

1. **Session creation**: `POST /api/v1/freebuff/session`, with an empty `{}` body + `x-freebuff-model` header, returns `instanceId`.
2. **Run hierarchy**: create a root run first (`base2-free`, `ancestorRunIds: []`); a child run's `ancestorRunIds` may only contain the root run id, otherwise it errors with `free_mode_invalid_agent_hierarchy`.
3. **metadata injection**: the chat payload must carry `codebuff_metadata`: `run_id`, `cost_mode: "free"`, `client_id`, `freebuff_instance_id`.
4. **Model restrictions**: testing shows only `google/gemini-2.5-flash-lite` and `google/gemini-3.1-flash-lite-preview` work; other models return `free_mode_invalid_agent_model`.

---

## License

MIT
</content>
