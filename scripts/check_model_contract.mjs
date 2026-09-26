#!/usr/bin/env node
// check_model_contract.mjs - verifies id/availability alignment between src/models.rs
// MODEL_META_ROWS and tests/fixtures/freebuff-models.snapshot.json
// Usage: node scripts/check_model_contract.mjs [path to models.rs] [path to fixture]
import fs from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const modelsPath = process.argv[2] || path.join(root, 'src', 'models.rs');
const fixturePath = process.argv[3] || path.join(root, 'tests', 'fixtures', 'freebuff-models.snapshot.json');

if (!fs.existsSync(modelsPath)) { console.error('✗ models.rs not found: ' + modelsPath); process.exit(2); }
if (!fs.existsSync(fixturePath)) { console.error('✗ fixture not found: ' + fixturePath); process.exit(2); }

const src = fs.readFileSync(modelsPath, 'utf8');
const fixture = JSON.parse(fs.readFileSync(fixturePath, 'utf8'));

// Extracts id / availability for each MetaRow in MODEL_META_ROWS
const rows = [];
const blockRe = /MetaRow\s*\{([^}]*)\}/g;
let m;
while ((m = blockRe.exec(src)) !== null) {
  const id = m[1].match(/id:\s*"([^"]+)"/);
  const avail = m[1].match(/availability:\s*"([^"]+)"/);
  if (id) rows.push({ id: id[1], availability: avail ? avail[1] : null });
}

const catalog = fixture.models.filter((r) => r.catalog === true);
const errors = [];
const metaById = new Map(rows.map((r) => [r.id, r]));

for (const row of catalog) {
  const meta = metaById.get(row.id);
  if (!meta) { errors.push(`fixture catalog row missing from models.rs: ${row.id}`); continue; }
  if (meta.availability !== row.availability) {
    errors.push(`availability mismatch: ${row.id}  fixture=${row.availability}  models.rs=${meta.availability}`);
  }
}
for (const r of rows) {
  if (!fixture.models.some((x) => x.id === r.id)) {
    errors.push(`models.rs metadata row not in fixture: ${r.id} (please update the snapshot)`);
  }
}

if (errors.length) {
  console.error('✗ Model contract drift, ' + errors.length + ' issue(s):');
  for (const e of errors) console.error('  - ' + e);
  process.exit(1);
}
console.log(`✓ Model contract aligned: fixture catalog=${catalog.length} rows / models.rs meta=${rows.length} rows (id+availability fully aligned)`);