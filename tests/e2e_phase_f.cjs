#!/usr/bin/env node
/**
 * Phase F E2E acceptance script -- account overview / credential management / browser one-click login wizard / upload types
 *
 * Usage: node tests/e2e_phase_f.cjs [port]
 * Note: in a Cookie-less isolated environment this verifies the "not logged in" path and structure; with a Cookie it also verifies full overview retrieval.
 */
const http = require('node:http');

const PORT = parseInt(process.argv[2] || '47831', 10);
const BASE = `http://127.0.0.1:${PORT}`;

let passed = 0, failed = 0;
const results = [];
function ok(name, cond, detail) {
  if (cond) { passed++; results.push(`  ✅ ${name}`); }
  else { failed++; results.push(`  ❌ ${name}${detail ? ' — ' + detail : ''}`); }
}
function req(method, path, { body, headers } = {}) {
  return new Promise((resolve, reject) => {
    const data = body == null ? null : (typeof body === 'string' ? body : JSON.stringify(body));
    const h = Object.assign({}, headers || {});
    if (data != null) h['content-length'] = Buffer.byteLength(data);
    const r = http.request({ host: '127.0.0.1', port: PORT, path, method, headers: h }, (res) => {
      let buf = '';
      res.on('data', c => buf += c);
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, text: buf }));
    });
    r.on('error', reject);
    r.setTimeout(30000, () => { r.destroy(new Error('timeout')); });
    if (data != null) r.write(data);
    r.end();
  });
}
async function json(method, path, body, headers) {
  const r = await req(method, path, { body, headers: Object.assign({ 'content-type': 'application/json' }, headers || {}) });
  let j = null; try { j = JSON.parse(r.text); } catch (e) {}
  return { status: r.status, json: j, text: r.text };
}

(async () => {
  console.log(`\n=== Phase F E2E (port ${PORT}) ===\n`);

  // 1. Panel structure and wizard
  {
    const r = await req('GET', '/ui');
    const html = r.text;
    ok('1.1 Account page has the "Account overview" container', html.includes('id="overview-wrap"') && html.includes('refreshAccountOverview'));
    ok('1.2 One-click login wizard overlay present', html.includes('id="login-wizard"') && html.includes('browser-extension'));
    ok('1.3 Credential table has a count hint and a type column', html.includes('id="cred-count"') && html.includes('Web Cookie'));
    ok('1.4 Keep-alive check entry present', html.includes('refreshCredential') && html.includes('/api/account/refresh'));
    ok('1.5 Integration guide has the three-step walkthrough and curl/Node examples', html.includes('three steps') && html.includes('id="g-curl"') && html.includes('id="g-node"'));
  }

  // 2. Account overview (works for both the "no credential" and "credential already imported" environment states)
  {
    const r = await json('GET', '/api/account/overview');
    const noCred = r.status === 400 && r.json?.code === 'need_cookie';
    const withCredOk = r.status === 200 && r.json?.ok === true;
    const withCredInvalid = r.status === 200 && r.json?.ok === false && r.json?.code === 'credential_invalid';
    ok('2.1 Account overview: no credential -> guidance / has credential -> data or invalid notice', noCred || withCredOk || withCredInvalid, `${r.status} ${r.text.slice(0, 130)}`);
    ok('2.2 Message text is present and actionable', typeof (r.json?.message || '') === 'string' && (r.json?.message || '').length > 0);
  }

  // 3. Credential keep-alive check (also handles both states)
  {
    const r = await json('POST', '/api/account/refresh', {});
    const noCred = r.status === 400 && r.json?.code === 'need_cookie';
    const checked = r.status === 200 && typeof r.json?.valid === 'boolean';
    ok('3.1 Keep-alive check: no credential -> guidance / has credential -> validity verdict', noCred || checked, `${r.status} ${r.text.slice(0, 120)}`);
  }

  // 4. Credential management: added-at time + type + dedup
  {
    const cookie = '__Secure-next-auth.session-token=e2e-test-token-' + Date.now() + '; other=1';
    const r1 = await json('POST', '/api/tokens/import', { cookie });
    ok('4.1 Import Cookie succeeds', r1.status === 200 && r1.json?.added === 1, r1.text.slice(0, 140));

    const r2 = await json('POST', '/api/tokens/import', { cookie });
    ok('4.2 Re-importing the same value dedups automatically (added=0)', r2.status === 200 && r2.json?.added === 0, r2.text.slice(0, 120));

    const list = await json('GET', '/api/tokens');
    const item = (list.json?.tokens || []).find(t => (t.token_masked || '').includes('e2e-tes') || true);
    const withTime = (list.json?.tokens || []).filter(t => t.added_at);
    ok('4.3 Credential list has an added-at time', withTime.length >= 1, JSON.stringify((list.json?.tokens || []).slice(-2)));
    ok('4.4 Credential carries a type tag (web-cookie)', (list.json?.tokens || []).some(t => t.kind === 'web-cookie'));
  }

  // 5. Upload type passthrough (no Cookie -> explicit error code; with a Cookie, returns kind)
  {
    const r = await req('POST', '/v1/uploads', {
      body: Buffer.from('hello world 文档内容'),
      headers: { 'content-type': 'text/plain', 'x-file-name': 'note.txt' },
    });
    let j = null; try { j = JSON.parse(r.text); } catch (e) {}
    // No Cookie: 400 multimodal_requires_web_cookie
    // Real Cookie: 200 + kind
    // Fake Cookie (E2E scenario): 502 + upstream 401 -- proves the request really reached upstream and the mime path is wired up
    const okCase = (r.status === 400 && j?.error?.code === 'multimodal_requires_web_cookie')
      || (r.status === 200 && j?.kind)
      || (r.status === 502 && /401|unauthorized|sign in/i.test(r.text));
    ok('5.1 Document upload (text/plain) path is reachable (no longer rewritten by the mime whitelist)', okCase, `${r.status} ${r.text.slice(0, 120)}`);
  }

  // 6. Session cleanup endpoint (existence check; guides when there's no credential)
  {
    const r = await json('POST', '/api/threads/cleanup', { dry_run: true });
    ok('6.1 Session cleanup endpoint exists (dry_run mode)', r.status === 200 || r.status === 400, `${r.status} ${r.text.slice(0, 100)}`);
  }

  console.log(results.join('\n'));
  console.log(`\n=== Results: ${passed} passed / ${failed} failed ===\n`);
  process.exit(failed > 0 ? 1 : 0);
})().catch(e => { console.error('E2E script error:', e); process.exit(1); });
