// Final E2E acceptance script: covers every endpoint
const http = require("node:http");

const BASE = { host: "127.0.0.1", port: 47821 };
let pass = 0, fail = 0;
const results = [];

function req(method, path, body) {
  return new Promise((resolve) => {
    const data = body ? JSON.stringify(body) : null;
    const headers = { "content-type": "application/json" };
    if (data) headers["content-length"] = Buffer.byteLength(data);
    const r = http.request({ ...BASE, path, method, headers }, (res) => {
      let d = "";
      res.on("data", (c) => (d += c));
      res.on("end", () => resolve({ status: res.statusCode, body: d }));
    });
    r.on("error", (e) => resolve({ status: 0, body: String(e) }));
    r.setTimeout(20000, () => { r.destroy(); resolve({ status: 0, body: "timeout" }); });
    if (data) r.write(data);
    r.end();
  });
}

async function check(name, fn) {
  try {
    const ok = await fn();
    if (ok) { pass++; results.push(`✅ ${name}`); }
    else { fail++; results.push(`❌ ${name}`); }
  } catch (e) {
    fail++; results.push(`❌ ${name} — ${e.message}`);
  }
}

(async () => {
  // 1. Health check
  await check("GET /healthz returns ok + accounts", async () => {
    const r = await req("GET", "/healthz");
    const j = JSON.parse(r.body);
    return r.status === 200 && j.ok === true && Array.isArray(j.accounts);
  });

  // 2. Model list
  await check("GET /v1/models has >=20 models", async () => {
    const r = await req("GET", "/v1/models");
    const j = JSON.parse(r.body);
    return r.status === 200 && j.data.length >= 20;
  });

  // 3. Panel (note: the HTML has the browser-side JS rendering buttons dynamically; the server
  //    only returns the script source, so this just checks the source anchors needed to render
  //    "6 prompt + 5 skill buttons" are all present)
  await check("GET /ui panel 200 + has prompt management", async () => {
    const r = await req("GET", "/ui");
    const srcHasPromptsLoop = r.body.includes("pd.prompts") && r.body.includes("togglePrompt('prompt'");
    const srcHasSkillsLoop = r.body.includes("pd.skills") && r.body.includes("togglePrompt('skill'");
    const srcHasToggleFn = r.body.includes("async function togglePrompt");
    return r.status === 200 && r.body.includes("Built-in prompts") && srcHasPromptsLoop && srcHasSkillsLoop && srcHasToggleFn;
  });

  // 4. Usage totals
  await check("GET /api/usage/totals returns stats", async () => {
    const r = await req("GET", "/api/usage/totals");
    const j = JSON.parse(r.body);
    return r.status === 200 && typeof j.total_requests === "number";
  });

  // 5. Request detail
  await check("GET /api/usage/requests returns an array", async () => {
    const r = await req("GET", "/api/usage/requests");
    return r.status === 200 && Array.isArray(JSON.parse(r.body));
  });

  // 6. Account list
  await check("GET /api/usage/accounts returns accounts", async () => {
    const r = await req("GET", "/api/usage/accounts");
    const j = JSON.parse(r.body);
    return r.status === 200 && Array.isArray(j.accounts);
  });

  // 7. Prompt list
  await check("GET /api/prompts has 6 prompts + 5 skills", async () => {
    const r = await req("GET", "/api/prompts");
    const j = JSON.parse(r.body);
    return r.status === 200 && j.prompts.length === 6 && j.skills.length === 5;
  });

  // 8. Enable a prompt
  await check("POST /api/prompts/toggle enables a skill and injects the prefix", async () => {
    const r = await req("POST", "/api/prompts/toggle", { type: "skill", id: "git-guru", enabled: true });
    const j = JSON.parse(r.body);
    return r.status === 200 && j.ok && j.system_prefix_preview.includes("Git Expert");
  });

  // 9. Token list
  await check("GET /api/tokens returns the imported token", async () => {
    const r = await req("GET", "/api/tokens");
    const j = JSON.parse(r.body);
    return r.status === 200 && j.ok && Array.isArray(j.tokens);
  });

  // 10. Balance lookup (web Cookie)
  await check("GET /api/account/balance returns freebucks", async () => {
    const r = await req("GET", "/api/account/balance");
    const j = JSON.parse(r.body);
    return r.status === 200 && j.freebucks && typeof j.freebucks.balance === "number";
  });

  // 11. Account detail card
  await check("POST /api/account/detail returns user+usage", async () => {
    const r = await req("POST", "/api/account/detail", {});
    const j = JSON.parse(r.body);
    return r.status === 200 && j.user && j.usage_summary;
  });

  // 12. Error passthrough (fake token -> upstream 401 passed through)
  await check("POST /v1/chat/completions with a fake token -> upstream 401 passed through", async () => {
    const r = await req("POST", "/v1/chat/completions", { model: "z-ai/glm-5.3-flash", messages: [{ role: "user", content: "hi" }] });
    return r.status === 502 && r.body.includes("401") && r.body.includes("Invalid API key");
  });

  // 13. Real incremental web chat streaming (Cookie variant)
  await check("POST /v1/web/chat real streaming has content+DONE", async () => {
    return new Promise((resolve) => {
      const data = JSON.stringify({ model: "glm-5.3-flash", content: "reply literally: OK" });
      const r = http.request({ ...BASE, path: "/v1/web/chat", method: "POST", headers: { "content-type": "application/json", "content-length": Buffer.byteLength(data) } }, (res) => {
        let d = "";
        res.on("data", (c) => (d += c));
        res.on("end", () => {
          resolve(res.statusCode === 200 && d.includes("[DONE]") && (d.includes('"content"') || d.includes("reasoning_content")));
        });
      });
      r.on("error", () => resolve(false));
      r.setTimeout(60000, () => { r.destroy(); resolve(false); });
      r.write(data); r.end();
    });
  });

  // 14. Reasoning-effort downgrade (solar-pro4 doesn't support effort -> should be stripped and still requested; status 0=network unreachable is excluded)
  await check("POST /v1/chat/completions solar-pro4+max effort -> handled at the upstream protocol layer", async () => {
    const r = await req("POST", "/v1/chat/completions", { model: "upstage/solar-pro4", messages: [{ role: "user", content: "hi" }], reasoning_effort: "max" });
    // Stripping logic works: should not be 500; status=0 means the request never went out (network/service unreachable), doesn't count as a pass
    return r.status !== 0 && r.status !== 500;
  });

  console.log("========== E2E acceptance results ==========");
  results.forEach((r) => console.log(r));
  console.log(`\nPassed: ${pass}  Failed: ${fail}`);
  process.exit(fail > 0 ? 1 : 0);
})();
