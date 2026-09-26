# Easy Setup Guide

Get Freebuff's free models working as an OpenAI- and Claude-compatible API in about 10 minutes.

**You need:** a free [freebuff.com](https://freebuff.com) account (log in with GitHub) and either Docker (to run it on your computer) or a free [Render](https://render.com) account (to host it online).

---

## Step 1: Get your Freebuff cookie

The gateway uses your browser login cookie to talk to Freebuff on your behalf.

1. Open [freebuff.com](https://freebuff.com) in Chrome/Edge/Firefox and log in.
2. Press **F12** to open DevTools, then go to the **Network** tab.
3. Refresh the page (F5) and click any request to `freebuff.com`.
4. Under **Request Headers**, find `Cookie:` and copy its whole value.

It must contain `__Secure-next-auth.session-token=`. Keep it private: anyone with this cookie can use your account.

> Want more daily free usage? Repeat this with other GitHub accounts and add each cookie. The gateway rotates between them automatically.

---

## Step 2 (option A): Run on your computer with Docker

```bash
git clone https://github.com/sanupyadav/Free_LLM.git
cd Free_LLM
docker build -f docker/Dockerfile -t freebuff2api .
```

Create `data/config.json` (replace the key with any secret string you like):

```json
{
  "listen_addr": "0.0.0.0:47821",
  "api_keys": ["sk-local-change-me"],
  "skip_upstream_check": true,
  "sqlite_path": "/data/freebuff2api.sqlite",
  "tokens_path": "/data/tokens.json",
  "telemetry_path": "/data/telemetry.sqlite",
  "memory_path": "/data/memory.sqlite",
  "threads_path": "/data/threads.json",
  "cred_meta_path": "/data/cred_meta.json",
  "account_history_path": "/data/account_history.jsonl",
  "web_threads_path": "/data/web_threads.json",
  "skills_dir": "/data/skills"
}
```

Start it:

```bash
docker run -d --name freebuff2api --restart unless-stopped \
  -p 127.0.0.1:47821:47821 -v "$(pwd)/data:/data" freebuff2api
curl http://127.0.0.1:47821/healthz   # → {"ok":true,...}
```

Then add your cookie: open **http://127.0.0.1:47821/ui** → **Accounts** tab → paste the cookie from Step 1. Or use curl:

```bash
curl http://127.0.0.1:47821/api/tokens/import \
  -H "Authorization: Bearer sk-local-change-me" \
  -H "content-type: application/json" \
  -d '{"cookie":"PASTE_THE_WHOLE_COOKIE_HERE"}'
```

Accounts you add are saved in `data/` and survive restarts.

---

## Step 2 (option B): Host online on Render (free)

1. Fork or push this repo to your GitHub.
2. On [dashboard.render.com](https://dashboard.render.com) click **New → Blueprint** and pick the repo. Render reads `render.yaml`.
3. When asked for **`AUTH_TOKENS`**, paste the cookie from Step 1. For several accounts, separate the cookies with commas.
4. Click **Apply** and wait for the first build (about 10-15 minutes, since it compiles Rust).
5. Your URL is at the top of the service page (e.g. `https://freebuff2api-xxxx.onrender.com`). Your API key is the `API_KEYS` value in the service's **Environment** tab (Render generates it).

Free-plan limits:
- The service sleeps after about 15 minutes idle, and the first request afterwards takes about a minute.
- There is no disk: accounts added in the panel are lost on restart, so keep them in `AUTH_TOKENS`.
- Freebuff may rate-limit datacenter IPs more than home connections.

---

## Step 3: Test it

```bash
export BASE=http://127.0.0.1:47821        # or your Render URL
export KEY=sk-local-change-me             # or your Render API_KEYS value

curl $BASE/v1/chat/completions \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{"model":"z-ai/glm-5.3-flash","messages":[{"role":"user","content":"Say hi"}]}'
```

You should get a JSON reply with the model's answer. You can also chat in the browser: **`$BASE/ui` → Playground**.

---

## Step 4: Connect your apps

**Claude Code:**

```bash
export ANTHROPIC_BASE_URL=$BASE      # no /v1
export ANTHROPIC_API_KEY=$KEY
claude
```

Claude Code's own model names aren't Freebuff models, so the gateway falls back to a default free model (`z-ai/glm-5.3-flash`). To choose one, also set e.g. `export ANTHROPIC_MODEL=anthropic/claude-fable-5`.

**Cursor, LobeChat, Continue, or any OpenAI SDK:**

- Base URL: `$BASE/v1`
- API key: `$KEY`
- Model: any id from `curl $BASE/v1/models -H "Authorization: Bearer $KEY"`

Python example:

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:47821/v1", api_key="sk-local-change-me")
reply = client.chat.completions.create(
    model="z-ai/glm-5.3-flash",
    messages=[{"role": "user", "content": "Say hi"}],
)
print(reply.choices[0].message.content)
```

---

## Troubleshooting

| Problem | Fix |
|---|---|
| `no healthy upstream auth token available` | No working account. Add a cookie (Step 2), or your cookie expired: log in to freebuff.com again and re-import it. |
| `invalid proxy api key` / `unauthorized` | Send the key as `Authorization: Bearer $KEY` (or `x-api-key: $KEY`). |
| `write operations require content-type: application/json` | Add `-H "content-type: application/json"` to admin POSTs such as `/api/tokens/import`. |
| `Could not extract any Bearer token or Cookie` | The pasted text doesn't contain `__Secure-next-auth.session-token=`. Copy the full `Cookie:` header again. |
| 429 "too many concurrent requests" | Only 1 request runs at a time by default. With several accounts, raise `concurrency_free_slots` (panel → Settings, or env `CONCURRENCY_FREE_SLOTS`). |
| Container exits at start with "Safety refusal: listen_addr=... is not a local address" | Listening on `0.0.0.0` requires `api_keys` to be set. |
| "Login window failed to start" | One-click login needs Windows. On Linux, Docker or Render, paste the cookie instead. |

Health overview: open **`$BASE/ui` → Diagnostics**. All endpoints with curl examples: [README → curl examples](../README.md#curl-examples). Full API reference: [API_GUIDE.md](API_GUIDE.md).
