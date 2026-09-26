# Reverse-engineering Codebuff's free tier: turning Freebuff's free models into an OpenAI-compatible API

> A complete protocol reverse-engineering log: from packet capture analysis and bypassing CLI detection, to fixing run hierarchy and model tightening, ending with a locally runnable OpenAI / Claude dual-protocol proxy.
>
> Project: [Freebuff2API](https://github.com/lza6/Freebuff-2API) (Go implementation, single-file binary)

---

## Background

[Codebuff](https://www.codebuff.com) is an AI coding assistant whose free tier, **Freebuff**, offers a batch of free-to-call models (Gemini, DeepSeek, GLM, Kimi, etc.). Officially there's only a CLI client, no public API.

**Goal**: reverse-engineer Freebuff's free models into standard `/v1/chat/completions` (OpenAI protocol) and `/v1/messages` (Claude protocol), so any compatible client (LobeChat, NextChat, Claude Code, Codex, etc.) can use them directly.

---

## I. Full protocol picture: four key endpoints

By capturing the CLI's real traffic, we mapped out the complete call chain of the Freebuff backend. All requests carry `Authorization: Bearer <authToken>`.

| Step | Endpoint | Purpose |
|------|------|------|
| 1 | `POST /api/v1/freebuff/session` | Create/keep alive the free session, returns `instanceId` and `expiresAt` |
| 2 | `POST /api/v1/agent-runs` (action=START) | Start an agent run, returns `runId` |
| 3 | `POST /api/v1/chat/completions` | The actual chat request, with `codebuff_metadata` injected into the payload |
| 4 | `POST /api/v1/agent-runs` (action=FINISH) | End the run, reporting steps/credits |

### 1. Session creation

```http
POST /api/v1/freebuff/session
Authorization: Bearer <token>
x-freebuff-model: deepseek/deepseek-v4-flash
Content-Type: application/json

{}
```

Response:

```json
{
  "status": "active",
  "instanceId": "xxxx",
  "model": "...",
  "expiresAt": "2026-07-27T...",
  "rateLimit": {...}
}
```

Note it's a **POST with an empty `{}` body + `x-freebuff-model` header**, not a GET. The session may enter a queue (`status: "queued"`, with `position`/`queueDepth`), which requires polling.

### 2. Starting a run

```http
POST /api/v1/agent-runs
Authorization: Bearer <token>

{
  "action": "START",
  "agentId": "base2-free",
  "ancestorRunIds": []
}
```

Response:

```json
{"runId": "2b56444d-..."}
```

### 3. Chat request (the core is metadata injection)

The chat payload is basically OpenAI format, but `codebuff_metadata` must be injected — all four fields are required:

```json
{
  "model": "google/gemini-2.5-flash-lite",
  "messages": [...],
  "stream": false,
  "codebuff_metadata": {
    "run_id": "<runId obtained in step 2>",
    "cost_mode": "free",
    "client_id": "<random 13-char hex>",
    "freebuff_instance_id": "<instanceId from step 1>"
  }
}
```

---

## II. Pitfalls encountered: three real hurdles

### Hurdle 1: `free_mode_invalid_agent_hierarchy` — a run must attach under the session root

Initially I called `START` directly for a run on each agent and then used it, but every chat request was rejected:

```json
{"error":"free_mode_invalid_agent_hierarchy","message":"Free mode subagents must run under an active freebuff session root."}
```

**Root cause**: Freebuff enforces a run tree rooted at the session —

```
Session (instanceId)
└── root run: base2-free          ← must be created first, ancestorRunIds: []
    ├── child run: file-picker      ← ancestorRunIds: [root runId]
    └── child run: code-reviewer-*  ← ancestorRunIds: [root runId]
```

A child run's `ancestorRunIds` can **only contain the root run's id**. My initial mistake was stuffing every sibling run's id into the ancestor list, which the upstream immediately flagged as an invalid hierarchy.

**Fix**: keep alive exactly **one root run** (`base2-free`) per token; create child runs lazily, with ancestors pointing only to the root.

### Hurdle 2: `400 Invalid request body` — the difference between null and []

The root run has no ancestors, so what should `ancestorRunIds` be? In Go, `var ancestors []string` is `nil`, and `json.Marshal` turns that into `null`:

```json
{"ancestorRunIds": null}
```

The upstream returns a flat 400:

```json
{"error":"Invalid request body","details":{"ancestorRunIds":{"_errors":["Invalid input: expected array, received null"]}}}
```

**Fix**: initialize it as an empty slice `ancestors := []string{}`, so it serializes to `[]` instead of `null`. A classic case of a strongly-typed language tripping over a weakly-typed schema check.

### Hurdle 3: `free_mode_invalid_agent_model` — the free tier's model list was tightened significantly

According to the official open-source repo's `free-agents.ts`, the free tier should support a large batch of models. When I registered all of them, **everything failed except Gemini**:

```json
{"error":"free_mode_invalid_agent_model","message":"Free mode is only available for specific agent and model combinations."}
```

The actual results after testing each one individually (2026-07):

| Model | Result |
|------|------|
| `google/gemini-2.5-flash-lite` | Works |
| `google/gemini-3.1-flash-lite-preview` | Works |
| deepseek-v4-pro/flash, minimax-m3, glm-v5.2, kimi-k2-thinking, mimo-v2.5, hy3, laguna-s-2-1 | All rejected |

**Lesson**: the `free-agents.ts` source list does not equal the combinations the backend actually allows. The free tier has been tightened — **only empirical testing can converge on the truth**, don't trust the docs.

---

## III. CLI detection: why a Cloudflare Worker doesn't work

Initially I wanted to deploy the proxy as a Cloudflare Worker (no ops overhead, global distribution). No matter how it was disguised, requests from the Worker were always rejected:

```json
{"error":"free_mode_cli_required"}
```

Bypass attempts, all ineffective:

- `User-Agent: Freebuff-CLI/0.0.105` (identical to the official CLI)
- Adding a full set of browser/CLI headers: `Origin` / `Referer` / `Host` / `Accept-Encoding`, etc.
- Stripping all headers that could expose the Worker's identity
- Handling gzip response bodies

**Conclusion**: the detection isn't at the HTTP header layer, it's at the **TLS fingerprint layer** (Client Hello / JA3). Cloudflare Worker's `fetch` uses Cloudflare's own TLS stack, whose fingerprint is completely different from Node.js's undici/OpenSSL, so it can't be bypassed by changing headers.

**The breakthrough**: the local Go binary uses Go's native TLS stack, whose fingerprint isn't on the upstream blacklist, so it passes right through. So the final form is a **local proxy service**, not Serverless.

---

## IV. Final architecture

```
┌─────────────┐   OpenAI/Claude protocol   ┌──────────────────┐   Freebuff private protocol   ┌────────────┐
│ Any client  │ ────────────────────▶ │  Freebuff2API     │ ───────────────────▶ │ Codebuff   │
│(LobeChat etc)│ ◀──────────────────── │  (Go, local :8080) │ ◀─────────────────── │  upstream  │
└─────────────┘                        └──────────────────┘                      └────────────┘
                                              │
                       ┌──────────────────────┼──────────────────────┐
                       │  ModelRegistry       │  RunManager          │  SessionPool
                       │  hardcoded base +    │  keeps one root run  │  session keep-alive/
                       │  remote supplement   │  alive per token,    │  queueing, auto
                       │  agent→model mapping │  child runs created  │  refresh on expiry
                       │                      │  lazily              │
                       └──────────────────────┴──────────────────────┘
```

### Key design decisions

**1. Model registry: hardcoding as the base, remote fetch as a supplement**

After upstream's `free-agents.ts` was refactored to use `FREEBUFF_*_MODEL_ID` constant references, regex could no longer extract literal values. The strategy changed to: hardcode an authoritative base mapping of agent→model pairs that are **empirically confirmed working**, and only merge in remote-parsed results incrementally, ensuring the list only ever grows, never shrinks.

**2. Run lifecycle management**

- On startup, only 1 root run is pre-warmed (not all 28 agents, avoiding a pointless START/FINISH storm).
- Child runs are created only on first use, with ancestors pointing to the current root.
- When the root run rotates on expiry, child runs rotate along with it to point to the new root.
- Leases are released when a request ends; old runs left over after rotation are FINISHed once drained.

**3. Session management**

- Each token caches one session, auto-refreshed 5 seconds before expiry.
- The queued state (waiting room) returns `Retry-After` to the client instead of blocking.
- On 401, the token is put into a 30-minute cooldown to avoid cascading failures.

**4. Dual-protocol exit**

- `/v1/chat/completions`: standard OpenAI, both streaming and non-streaming supported.
- `/v1/messages`: Claude protocol, with bidirectional request/response format conversion.
- `/v1/models`: returns the list of empirically confirmed working models.

---

## V. Empirical verification

```
GET /v1/models
→ gemini-2.5-flash-lite, gemini-3.1-flash-lite-preview

POST /v1/chat/completions  (gemini-2.5-flash-lite, "Say OK")
→ {"choices":[{"message":{"content":"Alright"}}], "usage":{...}}

POST /v1/chat/completions  (stream: true, "17*23?")
→ data: {...391...}  data: [DONE]   ← streaming works fine

POST /v1/messages  (Claude protocol)
→ {"content":[{"text":"All right.","type":"text"}], "role":"assistant", ...}
```

---

## VI. Lessons learned

1. **Packet captures beat documentation**. `free-agents.ts` listing something doesn't mean the backend allows it — everything must be empirically verified.
2. **Private protocol constraints often hide in undocumented fields**. Missing any of the four `codebuff_metadata` fields, or getting the `ancestorRunIds` hierarchy semantics wrong, results in a 400.
3. **`null` ≠ `[]`**. Go's nil slice serializes to null, which the upstream schema check rejects — fixed by using an empty slice instead.
4. **TLS fingerprinting is an unavoidable hurdle**. Serverless (Worker) TLS stacks get flagged; only local native TLS gets through — this dictated the deployment form.
5. **Hierarchical resources need to be modeled correctly**. Explicitly modeling the "session → root run → child run" tree constraint in code (a dedicated root-run field), rather than flattening it into a map governed by convention, avoids an entire class of bugs.

---

## Project links

- **Freebuff2API**: https://github.com/lza6/Freebuff-2API
- Single-file Go binary, just fill in authToken in `config.json` and run
- Token retrieval: https://freebuff.llm.pm (shown directly after login)

> Disclaimer: this project is for learning and discussion purposes only and has no official affiliation with Codebuff/Freebuff. Please comply with upstream terms of service and keep call frequency in check.
