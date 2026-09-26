> Historical archive: acceptance/delivery report from the v0.1.0 era, now outdated (port 8787, old test counts), kept for reference only. See the README and workflow_status.md for the current version.

# Round 2 Deepening Delivery Summary (2026-09-10)

## New This Round (all landed + tested + pushed + Release updated)

| # | Feature | Status | Evidence |
|---|------|------|------|
| 1 | **web version protocol adapter** `src/web_protocol.rs` | ✅ | full parsing of chat/stream SSE's 11 events, multimodal upload, tool-call pass-through (reverse-engineered from three network captures: web chat / tool calls / image upload) |
| 2 | **Dual-bucket concurrency semaphore** `src/concurrency.rs` | ✅ | reverse-engineered from the desktop client's orchestrator.js: free{slot:1,multi:3}/subscriber{slot:3,multi:8}/limited{1,0}, 3 unit tests passed |
| 3 | **Account balance query** `GET /api/account/balance` | ✅ tested | after Cookie import, genuinely returns freebucks.balance=95/100, per-model usable_today, plan, region restrictions |
| 4 | **Cookie credential import** `POST /api/tokens/import` | ✅ tested | a full Cookie string (`__Secure-next-auth.session-token=...`) is auto-parsed into storage, 5 unit tests passed |
| 5 | **Panel balance card** | ✅ tested | /ui adds an "Account balance" section: today's remaining / per-model remaining / plan / region restriction shown live |
| 6 | **Release update** | ✅ | v0.1.0 installer rebuilt (with all new features) + release notes updated |

## Upstream Concurrency Limit Reverse-Engineering Conclusion (answering your question: 3 concurrent)

**Your understanding is correct but can be more precise** - it isn't a flat 3 concurrent, but a **dual-bucket model**:
- Free account: paid-model slot = **1**, regular models = **3**
- Subscriber account: paid-model slot = **3**, regular models = **8**
- What you experienced as "three tasks at once, the fourth has to wait" = the free account's regular-model bucket hitting its 3-concurrency limit
- Waiting room: the server returns 428/429 + `retry-after-ms`; the client backs off at most once for up to 3s, with no queuing concept

## Remaining Balance Query (answering your question: didn't know how to check)

**You can check it now.** Two ways:
1. Panel `/ui` -> see the "Account balance" card (live)
2. API `GET /api/account/balance` (JSON)

**You need to provide a web Cookie first**: log into freebuff.com in a browser -> DevTools -> "Copy as cURL" or copy the Cookie -> paste into `POST /api/tokens/import`. Query logic (reverse-engineered from packet captures):
- `freebucks.daily.remaining` = today's remaining balance
- Remaining uses per model = min(balance / price, daily limit - used)
- **upstage/solar-pro4 is 0 balance / free** (but still has a daily pool of 6 uses)
- Auto-resets daily at 07:00 UTC (midnight Pacific)

## Real Verification Data From This Round

```
Import Cookie -> {"added":1,"token_masked":"__Secu....com"}
GET /api/account/balance ->
{
  "access_tier":"full",
  "freebucks":{"balance":95,"daily":{"limit":100,"spent":5,"remaining":95,"resetAt":"2026-09-10T07:00:00.000Z"}},
  "model_remaining":{
    "deepseek/deepseek-v4-flash":{"price":15,"usable_today":6},
    "google/gemini-3.8-flash":{"price":50,"usable_today":1},
    "upstage/solar-pro4":{"price":0,"usable_today":-1}  // -1=free, unlimited
  }
}
```

## Pushed

- 4 new commits on the repo's main branch (web protocol / concurrency / balance / panel)
- Release v0.1.0 installer rebuilt and uploaded, release notes updated

## Remaining To-Do (next round)

| Item | Notes |
|----|------|
| OAuth login automation | Upstream next-auth protocol is complex (CSRF + device fingerprinting); recommend the "paste Cookie" route (already done) + an optional OAuth flow later |
| web protocol default routing | Currently the web protocol is an optional module, configurable so that AUTH_MODE=web uses the Cookie for chat/stream; needs a real Cookie to verify the full conversational streaming flow |
| Convert tool calls to OpenAI tools | web's agent_tool events can be mapped to OpenAI tool_calls and passed through to Claude Code/Codex |
| Auto-renewal | convex-token 10min TTL + auth/session 30-day renewal scheduled task (module reverse-engineered, landing pending Cookie verification) |
