#!/usr/bin/env node
/**
 * Panel inline JS syntax check (no server startup): extract the <script> block from src/web.rs and parse it with node.
 * Usage: node tests/check_panel_js.cjs
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const src = fs.readFileSync(path.join(__dirname, '..', 'src', 'web.rs'), 'utf8');
const m = src.match(/<script>([\s\S]*?)<\/script>/);
if (!m) { console.error('❌ No <script> block found in src/web.rs'); process.exit(1); }
const js = m[1];
const tmp = path.join(os.tmpdir(), `freebuff-panel-${Date.now()}.js`);
fs.writeFileSync(tmp, js);
try {
  execFileSync(process.execPath, ['--check', tmp], { stdio: 'pipe' });
  console.log(`✅ Panel JS syntax check passed (${js.length} chars)`);
  process.exit(0);
} catch (e) {
  console.error('❌ Panel JS syntax error:');
  console.error(String(e.stderr || e.message));
  process.exit(1);
} finally {
  try { fs.unlinkSync(tmp); } catch (_) { /* ignore */ }
}
