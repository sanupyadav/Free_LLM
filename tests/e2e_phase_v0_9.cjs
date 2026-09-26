#!/usr/bin/env node
/**
 * v0.9 smoke test -- covers the capabilities added this round:
 *  1. Web-cookie credential pool: /api/accounts/health includes both bearer + web-cookie, each with a history timeline
 *  2. Full config export/import: schema validation (bad version -> 400), round-trip 200, backup directory before import
 *  3. /v1/models metadata contract: meta array has available/efforts/multimodal/fallback; paused models marked unavailable
 *  4. Auth depth: /api/doctor includes a listen_scope check
 *  5. v0.9 panel controls: multi-turn/image/effort playground + health dashboard + today's recommendation + data migration
 *
 * Prerequisite: gateway is running (default 47871), config has one web-cookie credential (needed for the web-cookie kind to show up).
 * Usage: node tests/e2e_phase_v0_9.cjs [port] [config.json]
 */
const http = require('node:http');
const PORT = parseInt(process.argv[2] || '47871', 10);
const HOST = '127.0.0.1';
const CFG = process.argv[3] || 'config.json';

let passed = 0, failed = 0;
const results = [];
function ok(name, cond, detail) {
  if (cond) { passed++; results.push(`  ✅ ${name}`); }
  else { failed++; results.push(`  ❌ ${name}${detail ? ' — ' + String(detail).slice(0, 300) : ''}`); }
}
function req(method, path, { body, headers } = {}) {
  return new Promise((resolve, reject) => {
    const data = body == null ? null : (typeof body === 'string' ? body : JSON.stringify(body));
    const h = Object.assign({ 'content-type': 'application/json' }, headers || {});
    if (data != null) h['content-length'] = Buffer.byteLength(data);
    const r = http.request({ host: HOST, port: PORT, path, method, headers: h }, (res) => {
      const chunks = [];
      res.on('data', c => chunks.push(c));
      res.on('end', () => {
        const buf = Buffer.concat(chunks);
        resolve({ status: res.statusCode, headers: res.headers, text: buf.toString('utf8'), json: (() => { try { return JSON.parse(buf.toString('utf8')); } catch (_) { return null; } })() });
      });
    });
    r.on('error', reject);
    r.setTimeout(30000, () => r.destroy(new Error('timeout')));
    if (data != null) r.write(data);
    r.end();
  });
}
async function json(method, p, body) { return req(method, p, { body }); }

(async () => {
  console.log(`\n=== Freebuff2API v0.9 smoke test (port ${PORT}) ===\n`);

  const hz = await json('GET', '/healthz');
  ok('Gateway alive, healthz 200', hz.status === 200);

  // 1) Credential health dashboard
  const health = await json('GET', '/api/accounts/health');
  ok('GET /api/accounts/health 200', health.status === 200, health.text);
  const hAccs = health.json && health.json.accounts || [];
  ok('health includes an accounts array', Array.isArray(hAccs) && hAccs.length >= 1);
  ok('health includes a web-cookie credential', hAccs.some(a => a.kind === 'web-cookie'), 'kinds=' + hAccs.map(a => a.kind).join(','));
  ok('health entries each carry a history timeline', hAccs.every(a => Array.isArray(a.history)));
  ok('health fields are complete', hAccs.every(a => 'circuit_state' in a && 'health_score' in a && 'trips' in a && 'last_error' in a && 'cooldown_until' in a));

  // 2) Export/import
  const exp = await json('POST', '/api/export');
  ok('POST /api/export 200', exp.status === 200, exp.text);
  const data = exp.json && exp.json.data || {};
  ok('export schema_version present', typeof data.schema_version === 'string' && data.schema_version.length > 0);
  ok('export includes a skills array', Array.isArray(data.skills));
  ok('export does not include api_keys in plaintext', !JSON.stringify(data.config || {}).includes('api_keys'));
  ok('export does not include auth_tokens in plaintext', !JSON.stringify(data).includes('auth_tokens'));
  const bad = Object.assign({}, data, { schema_version: '99' });
  const badImp = await json('POST', '/api/import', { data: bad });
  ok('import with a bad schema -> 400', badImp.status === 400, badImp.text);
  const imp = await json('POST', '/api/import', { data });
  ok('import round-trip 200', imp.status === 200, imp.text);
  ok('import has a backup directory', !!(imp.json && imp.json.imported && imp.json.imported.backed_up_to));

  // 3) /v1/models metadata
  const models = await json('GET', '/v1/models');
  ok('GET /v1/models 200', models.status === 200);
  const meta = models.json && models.json.meta || [];
  ok('meta array has >= 19 entries', meta.length >= 19, 'len=' + meta.length);
  ok('meta fields are complete', meta.every(m => 'id' in m && 'available' in m && 'efforts' in m && 'multimodal' in m && 'premium' in m));
  const paused = meta.filter(m => m.available === false).map(m => m.id);
  ok('paused models are marked unavailable', paused.includes('stealth/ox-alpha') && paused.includes('google/gemini-3.8-flash') && paused.includes('deepseek/deepseek-v4-pro'), paused.join(','));
  ok('default model is available', meta.some(m => m.id === 'z-ai/glm-5.3-flash' && m.available === true));

  // 4) Auth-depth doctor check
  const doctor = await json('GET', '/api/doctor');
  ok('doctor includes a listen_scope check', doctor.status === 200 && /listen_scope/.test(doctor.text));

  // 5) v0.9 panel controls
  const ui = await req('GET', '/ui');
  const html = ui.text || '';
  for (const [name, marker] of [
    ['Playground multi-turn/effort control', 'play-effort'],
    ['Playground image drop zone', 'play-drop'],
    ['Credential health dashboard container', 'health-wrap'],
    ["Today's recommendation container", 'recommend-panel'],
    ['Data migration (export/import)', 'migrate-status'],
    ['Image picker input', 'play-file'],
  ]) {
    ok(`Panel includes ${name}`, html.includes(marker));
  }

  console.log(results.join('\n'));
  console.log(`\nResults: ${passed} passed / ${failed} failed\n`);
  process.exit(failed ? 1 : 0);
})().catch(e => { console.error('E2E error:', e); process.exit(1); });
