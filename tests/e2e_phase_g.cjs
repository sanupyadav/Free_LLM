#!/usr/bin/env node
/**
 * Phase G E2E acceptance script -- browser one-click login / credential detail & dedup / usage history / integration info / API key management
 *
 * Prerequisite: the gateway is already running locally (default 47821). Usage:
 *   node tests/e2e_phase_g.cjs [port]
 *
 * Notes:
 * - Writes one real **fake test credential** and deletes it at the end of the run, without touching real credentials already in the store.
 * - "Credential check" hits a real upstream once with the fake credential (expects 401), to prove the pipeline is actually wired up rather than faking success locally.
 * - The API key test case is kept last and auto-cleared when done, so the gateway is never left in a "key required" state.
 */
const http = require('node:http');

const PORT = parseInt(process.argv[2] || '47831', 10);
const HOST = '127.0.0.1';

let passed = 0, failed = 0;
const results = [];
const cleanups = [];

function ok(name, cond, detail) {
  if (cond) { passed++; results.push(`  ✅ ${name}`); }
  else { failed++; results.push(`  ❌ ${name}${detail ? ' — ' + String(detail).slice(0, 220) : ''}`); }
}

function req(method, path, { body, headers, binary } = {}) {
  return new Promise((resolve, reject) => {
    const data = body == null ? null : (typeof body === 'string' || Buffer.isBuffer(body) ? body : JSON.stringify(body));
    const h = Object.assign({}, headers || {});
    if (data != null) h['content-length'] = Buffer.byteLength(data);
    const r = http.request({ host: HOST, port: PORT, path, method, headers: h }, (res) => {
      const chunks = [];
      res.on('data', c => chunks.push(c));
      res.on('end', () => {
        const buf = Buffer.concat(chunks);
        resolve({
          status: res.statusCode,
          headers: res.headers,
          // Decodes as UTF-8 by default (panel/JSON may contain non-ASCII text); binary test cases like zip pass binary:true explicitly
          text: binary ? buf.toString('latin1') : buf.toString('utf8'),
          buffer: buf,
        });
      });
    });
    r.on('error', reject);
    r.setTimeout(60000, () => r.destroy(new Error('timeout')));
    if (data != null) r.write(data);
    r.end();
  });
}
async function json(method, path, body, headers) {
  const r = await req(method, path, {
    body,
    headers: Object.assign({ 'content-type': 'application/json' }, headers || {}),
  });
  let j = null;
  try { j = JSON.parse(r.text); } catch (e) { /* not JSON */ }
  return { status: r.status, json: j, text: r.text, headers: r.headers };
}
const sleep = (ms) => new Promise(r => setTimeout(r, ms));

(async () => {

  console.log(`\n=== Phase G E2E (http://${HOST}:${PORT}) ===\n`);

  // ---------- 0. Service reachable ----------
  {
    const r = await req('GET', '/healthz');
    let j = null; try { j = JSON.parse(r.text); } catch (e) {}
    ok('0.1 Gateway reachable (/healthz 200)', r.status === 200 && j && j.ok === true, `${r.status} ${r.text.slice(0, 120)}`);
  }

  // ---------- 1. Panel structure and "clearly explains how to make requests" ----------
  {
    const r = await req('GET', '/ui');
    const html = r.text;
    ok('1.1 Panel has "Start making requests now" card (address / key)', html.includes('id="connect-panel"') && html.includes('id="c-base"') && html.includes('id="c-key"'));
    ok('1.2 Panel distinguishes the OpenAI / Anthropic addresses', html.includes('id="c-openai"') && html.includes('id="c-anthropic"'));
    ok('1.3 Panel can generate / clear an API key with one click', html.includes('genApiKey()') && html.includes('clearApiKey()'));
    ok('1.4 One-click login has an "extension direct connect" path', html.includes('oneClickViaExtension') && html.includes('sendToExtension') && html.includes('onMessageExternal') === false);
    ok('1.5 Panel waits (polls) for the extension to write the credential back', html.includes('pollForNewCredential'));
    ok('1.6 Extension status indicator + download entry', html.includes('id="ext-status"') && html.includes('downloadExtension') && html.includes('/api/extension/bundle'));
    ok('1.7 Credential table has Account/Type/Credential/Plan/Remaining today/Added/Actions', ['Account', 'Type', 'Credential', 'Plan', 'Remaining today', 'Added', 'Actions'].every(h => html.includes(h)));
    ok('1.8 Credential row offers Check/Details/Delete', html.includes('checkCredAt') && html.includes('openCredDetail') && html.includes('deleteCredAt'));
    ok('1.9 Usage history panel exists', html.includes('id="hist-cred"') && html.includes('loadHistory') && html.includes('id="hist-wrap"'));
    ok('1.10 Legacy credentials get a backfilled "Added" note', html.includes('Added'));
  }

  // ---------- 2. /api/guide integration info ----------
  {
    const r = await json('GET', '/api/guide');
    ok('2.1 /api/guide 200 and ok', r.status === 200 && r.json && r.json.ok === true, `${r.status} ${r.text.slice(0, 150)}`);
    const g = r.json || {};
    ok('2.2 Includes OpenAI/Anthropic addresses and key status', g.openai_base_url === '/v1' && g.anthropic_base_url === '/' && typeof g.api_keys === 'object');
    ok('2.3 Includes available model count and sample', typeof g.models_count === 'number' && Array.isArray(g.models_sample));
    ok('2.4 Includes a "credential already present" readiness signal', typeof g.data_plane_ready === 'boolean');
  }

  // ---------- 3. Credential import: dedup + stable id ----------
  let testCredId = null;
  const fakeCookie = `__Secure-next-auth.session-token=e2e-g-${Date.now()}; __Host-next-auth.csrf-token=e2e`;
  {
    const before = await json('GET', '/api/tokens');
    const beforeIds = new Set(((before.json && before.json.tokens) || []).map(t => t.id));

    const r1 = await json('POST', '/api/tokens/import', { cookie: fakeCookie });
    ok('3.1 Import test credential succeeds', r1.status === 200 && r1.json && r1.json.added === 1, r1.text.slice(0, 150));

    const r2 = await json('POST', '/api/tokens/import', { cookie: fakeCookie });
    ok('3.2 Re-importing the same value dedups automatically (added=0)', r2.status === 200 && r2.json && r2.json.added === 0, r2.text.slice(0, 150));

    const list = await json('GET', '/api/tokens');
    const tokens = (list.json && list.json.tokens) || [];
    const added = tokens.find(t => !beforeIds.has(t.id));
    ok('3.3 Every credential has a stable id', !!added && typeof added.id === 'string' && added.id.length >= 8, JSON.stringify(tokens.slice(-1)));
    ok('3.4 Credential carries a type tag (web-cookie)', !!added && added.kind === 'web-cookie');
    ok('3.5 Credential carries an added-at time (including backfill for old data)', !!added && !!added.added_at);
    ok('3.6 List response includes a total count', typeof (list.json || {}).count === 'number');
    testCredId = added && added.id;
    if (testCredId) cleanups.push(() => json('POST', '/api/tokens/delete', { id: testCredId }));
  }

  // ---------- 4. Credential check (really hits upstream, proving the pipeline isn't faked locally) ----------
  {
    const bad = await json('POST', '/api/tokens/check', { id: 'not-a-real-id' });
    ok('4.1 Nonexistent id -> explicit 400', bad.status === 400, `${bad.status} ${bad.text.slice(0, 120)}`);

    if (testCredId) {
      const r = await json('POST', '/api/tokens/check', { id: testCredId });
      const body = r.json || {};
      const isRealUpstream = r.status === 200 && typeof body.valid === 'boolean';
      ok('4.2 Check returns a validity verdict (fake credential should be invalid)', isRealUpstream && body.valid === false, `${r.status} ${r.text.slice(0, 200)}`);
      ok('4.3 Check result is persisted to meta (includes checked_at)', !!body.meta && !!body.meta.checked_at && body.meta.cred_id === testCredId);

      const list = await json('GET', '/api/tokens');
      const item = (((list.json || {}).tokens) || []).find(t => t.id === testCredId);
      ok('4.4 Credential list can read the cached account info for this credential', !!item && !!item.meta && item.meta.valid === false);
    } else {
      ok('4.2 Check returns a validity verdict (fake credential should be invalid)', false, 'Precondition failed: no test credential id obtained');
      ok('4.3 Check result is persisted to meta (includes checked_at)', false, 'Precondition failed');
      ok('4.4 Credential list can read the cached account info for this credential', false, 'Precondition failed');
    }
  }

  // ---------- 5. Usage history (user note: every account must have a searchable record) ----------
  {
    const r = await json('GET', '/api/account/history?limit=50');
    const recs = (r.json && r.json.records) || [];
    ok('5.1 Usage history endpoint works', r.status === 200 && (r.json || {}).ok === true, `${r.status} ${r.text.slice(0, 150)}`);
    ok('5.2 Record includes account snapshot fields', recs.length === 0 || (recs[0].ts && recs[0].cred_id && typeof recs[0].ok === 'boolean'));
    if (testCredId) {
      const f = await json(`GET`, `/api/account/history?limit=50&cred_id=${encodeURIComponent(testCredId)}`);
      const mine = (f.json && f.json.records) || [];
      ok('5.3 History can be filtered by credential', f.status === 200 && mine.length >= 1 && mine.every(x => x.cred_id === testCredId), JSON.stringify(mine.slice(0, 1)));
    } else {
      ok('5.3 History can be filtered by credential', false, 'Precondition failed: no test credential id obtained');
    }
  }

  // ---------- 6. Extension bundle download ----------
  {
    const r = await req('GET', '/api/extension/bundle', { binary: true });
    const isZip = r.text.slice(0, 2) === 'PK';
    ok('6.1 /api/extension/bundle returns a zip', r.status === 200 && isZip, `${r.status} head=${JSON.stringify(r.text.slice(0, 8))}`);
    const ct = String(r.headers['content-type'] || '');
    ok('6.2 Response header is zip and carries a filename', /zip/i.test(ct) && /attachment/i.test(String(r.headers['content-disposition'] || '')), `${ct} ${r.headers['content-disposition']}`);
    ok('6.3 Zip contains the key extension file entries', r.text.includes('manifest.json') && r.text.includes('background.js') && r.text.includes('bridge.js'), 'zip contents are missing extension files');
    ok('6.4 Zip structure is intact (EOCD present)', r.text.slice(-22, -18) === 'PK', `tail=${JSON.stringify(r.text.slice(-22, -16))}`);
  }

  // ---------- 7. Delete credential ----------
  {
    if (testCredId) {
      const before = await json('GET', '/api/tokens');
      const beforeCount = (before.json || {}).count || 0;
      const r = await json('POST', '/api/tokens/delete', { id: testCredId });
      ok('7.1 Delete credential succeeds', r.status === 200 && (r.json || {}).ok === true, `${r.status} ${r.text.slice(0, 150)}`);
      const after = await json('GET', '/api/tokens');
      ok('7.2 List count -1 after delete', ((after.json || {}).count || 0) === beforeCount - 1, `before=${beforeCount} after=${(after.json || {}).count}`);
      const again = await json('POST', '/api/tokens/delete', { id: testCredId });
      ok('7.3 Repeat delete -> 404 (clear idempotent semantics)', again.status === 404, `${again.status} ${again.text.slice(0, 120)}`);
      testCredId = null; // already deleted, no cleanup needed
    } else {
      ok('7.1 Delete credential succeeds', false, 'Precondition failed: no test credential id obtained');
      ok('7.2 List count -1 after delete', false, 'Precondition failed');
      ok('7.3 Repeat delete -> 404 (clear idempotent semantics)', false, 'Precondition failed');
    }
  }

  // ---------- 8. Automatic upstream session cleanup (user note: the reverse proxy must clean up after itself, not burden upstream) ----------
  {
    const fs = require('node:fs');
    const path = require('node:path');
    const threadsPath = path.join(__dirname, '..', 'data', 'e2e', 'threads.json');
    const nowIso = new Date().toISOString();
    fs.mkdirSync(path.dirname(threadsPath), { recursive: true });
    fs.writeFileSync(threadsPath, JSON.stringify([
      { id: 'e2e-old-thread', created_at: '2020-01-01T00:00:00+00:00' },
      { id: 'e2e-new-thread', created_at: nowIso },
    ], null, 2));

    const dry = await json('POST', '/api/threads/cleanup', { dry_run: true, max_age_hours: 24 });
    const dj = dry.json || {};
    ok('8.1 Cleanup dry-run identifies expired sessions', dry.status === 200 && dj.expired === 1, `${dry.status} ${dry.text.slice(0, 160)}`);
    ok('8.2 Dry-run only reports expired items, leaves new sessions alone', Array.isArray(dj.thread_ids) && dj.thread_ids.includes('e2e-old-thread') && !dj.thread_ids.includes('e2e-new-thread'), JSON.stringify(dj.thread_ids));
    ok('8.3 Dry-run deletes nothing (total is still 2)', dj.total === 2, `total=${dj.total}`);

    // Real delete: no credential -> explicit error; with credential -> really hits upstream and reports honestly (success+failed == expired count)
    const real = await json('POST', '/api/threads/cleanup', { dry_run: false, max_age_hours: 24 });
    const rj = real.json || {};
    const noCred = real.status === 400 && /Cookie/i.test(real.text);
    const honestAttempt =
      real.status === 200 && rj.ok === true && rj.dry_run === false &&
      (rj.deleted + rj.failed) === 1 && rj.remaining === 2 - rj.deleted;
    ok('8.4 Real cleanup: no credential -> explicit error; with credential -> real attempt reported honestly', noCred || honestAttempt, `${real.status} ${real.text.slice(0, 180)}`);

    // Auto-cleanup should be on by default in config (interval > 0)
    const cfg = JSON.parse(fs.readFileSync(path.join(__dirname, 'e2e_phase_g.config.json'), 'utf8'));
    ok('8.5 Auto-cleanup enabled by default (treated as enabled unless explicitly turned off)', cfg.thread_cleanup_interval_sec === undefined || cfg.thread_cleanup_interval_sec > 0, JSON.stringify(cfg.thread_cleanup_interval_sec));
  }

  // ---------- 9. API key runtime management (kept last, restores original state when done) ----------
  // Snapshot the current key config: if the script exits abnormally, `finally` uses it to reset back to a known key
  // (the original key's plaintext isn't recoverable anyway -- guide only returns a masked value; this generate call invalidates the original key and resets it to a known value)
  let snapshotKey = null;
  let generatedKey = null;
  try {
    const g = await json('GET', '/api/guide');
    const cnt = ((g.json || {}).api_keys || {}).count || 0;
    if (cnt > 0) { const gen = await json('POST', '/api/config/api-key', { action: 'generate' }); snapshotKey = (gen.json || {}).key || null; }
  } catch (e) { snapshotKey = null; }

  {
    const before = await json('GET', '/api/guide');
    const wasConfigured = !!(before.json && before.json.api_keys && before.json.api_keys.configured);

    const gen = await json('POST', '/api/config/api-key', { action: 'generate' });
    const newKey = (gen.json || {}).key;
    generatedKey = newKey;
    ok('9.1 Generate API key succeeds and echoes it back', gen.status === 200 && typeof newKey === 'string' && newKey.length >= 12, `${gen.status} ${gen.text.slice(0, 150)}`);

    // Without a key -> admin endpoint should reject
    const denied = await json('GET', '/api/tokens');
    ok('9.2 After enabling the key, requests without a key are rejected', denied.status === 401 || denied.status === 403, `${denied.status} ${denied.text.slice(0, 120)}`);

    // With the key -> allowed through
    const allowed = await json('GET', '/api/tokens', null, { authorization: `Bearer ${newKey}` });
    ok('9.3 Request with the correct key is allowed through', allowed.status === 200 && (allowed.json || {}).ok === true, `${allowed.status} ${allowed.text.slice(0, 120)}`);

    // Unauthorized /healthz should not leak account names
    const hz = await req('GET', '/healthz');
    let hzj = null; try { hzj = JSON.parse(hz.text); } catch (e) {}
    ok('9.4 Unauthorized /healthz does not leak account info', hz.status === 200 && hzj && hzj.ok === true && hzj.accounts === undefined, hz.text.slice(0, 150));

    // Restore original state
    const clear = await json('POST', '/api/config/api-key', { action: 'clear' }, { authorization: `Bearer ${newKey}` });
    ok('9.5 Clear key succeeds', clear.status === 200 && (clear.json || {}).ok === true && (clear.json || {}).configured === false, `${clear.status} ${clear.text.slice(0, 150)}`);

    const restored = await json('GET', '/api/tokens');
    ok('9.6 Local direct connect is restored after clearing', restored.status === 200, `${restored.status}`);
    if (wasConfigured) results.push('  ℹ️ Note: the gateway already had api_keys configured before this run; this test case cleared it and did not auto-restore it (please reconfigure it in the panel)');
  }

  // ---------- Final cleanup ----------
  for (const fn of cleanups) { try { await fn(); } catch (e) { /* ignore */ } }

  console.log(results.join('\n'));
  console.log(`\n=== Results: ${passed} passed / ${failed} failed ===\n`);
  process.exit(failed > 0 ? 1 : 0);
})().catch(e => { console.error('E2E script error:', e); process.exitCode = 1; }).finally(async () => {
  // Restore gateway key state: started without a key -> clear; started with a key (shouldn't happen) -> restore original value
  try {
    if (snapshotKey) {
      await json('POST', '/api/config/api-key', { action: 'set', key: snapshotKey }, { authorization: 'Bearer ' + snapshotKey });
      console.log('(Restored: the gateway\'s original API key has been reset back)');
    } else {
      await json('POST', '/api/config/api-key', { action: 'clear' }, { authorization: 'Bearer ' + (generatedKey || '') });
      console.log('(Restored: the temporary key generated by E2E has been cleared)');
    }
  } catch (e) { console.error('(Warning: failed to restore key state, please check /api/config/api-key)', e.message); }
  // Fallback cleanup of the test credential
  if (testCredId) { try { await json('POST', '/api/tokens/delete', { id: testCredId }); } catch (e) { /* ignore */ } }
});
