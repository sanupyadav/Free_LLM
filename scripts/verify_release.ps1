# verify_release.ps1 - Freebuff2API v0.10 local pre-release verification (exits non-zero on failure)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
if (!(Test-Path $cargo)) { $cargo = 'cargo' }
Write-Host "==> cargo fmt --check" -ForegroundColor Cyan
& $cargo fmt --check; if ($LASTEXITCODE -ne 0) { throw "fmt failed, run cargo fmt first" }
Write-Host "==> clippy" -ForegroundColor Cyan
& $cargo clippy --all-targets -- -D warnings; if ($LASTEXITCODE -ne 0) { throw "clippy failed" }
Write-Host "==> cargo test --lib" -ForegroundColor Cyan
& $cargo test --lib; if ($LASTEXITCODE -ne 0) { throw "lib unit tests failed" }
Write-Host "==> integration tests" -ForegroundColor Cyan
& $cargo test --test core_test; if ($LASTEXITCODE -ne 0) { throw "core_test failed" }
& $cargo test --test router_test; if ($LASTEXITCODE -ne 0) { throw "router_test failed" }
& $cargo test --test web_pool_test; if ($LASTEXITCODE -ne 0) { throw "web_pool_test failed" }
& $cargo test --test model_meta_test; if ($LASTEXITCODE -ne 0) { throw "model_meta_test failed" }
Write-Host "==> panel JS syntax" -ForegroundColor Cyan
node tests/check_panel_js.cjs; if ($LASTEXITCODE -ne 0) { throw "panel JS syntax check failed" }
Write-Host "==> model contract alignment" -ForegroundColor Cyan
node scripts/check_model_contract.mjs; if ($LASTEXITCODE -ne 0) { throw "model contract drift" }
Write-Host "==> release smoke test (port 47990)" -ForegroundColor Cyan
$tmp = Join-Path $env:TEMP "fba-release-smoke"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$cfg = '{"listen_addr":"127.0.0.1:47990","upstream_base_url":"https://www.codebuff.com","auth_tokens":[],"api_keys":[],"skip_upstream_check":true,"memory_enabled":false,"sqlite_path":"data/freebuff2api.sqlite","telemetry_path":"data/telemetry.sqlite","memory_path":"data/memory.sqlite","skills_dir":"data/skills","redact_logs":true,"token_saver":false}'
Set-Content -LiteralPath "$tmp\config.json" -Value $cfg -Encoding ASCII
if (!(Test-Path "$root\target\release\freebuff2api.exe")) { throw "release binary missing, run cargo build --release first" }
$proc = Start-Process -FilePath "$root\target\release\freebuff2api.exe" -ArgumentList "--config","config.json" -WorkingDirectory $tmp -PassThru -WindowStyle Hidden
try {
  $ok = $false
  for ($i = 0; $i -lt 60; $i++) { Start-Sleep -Milliseconds 500; try { $r = Invoke-WebRequest "http://127.0.0.1:47990/healthz" -UseBasicParsing -TimeoutSec 2; if ($r.StatusCode -eq 200) { $ok = $true; break } } catch {} }
  if (-not $ok) { throw "gateway didn't become ready within 60 seconds" }
  $ui = Invoke-WebRequest "http://127.0.0.1:47990/ui" -UseBasicParsing -TimeoutSec 5
  if ($ui.StatusCode -ne 200) { throw "/ui returned non-200" }
  $models = Invoke-WebRequest "http://127.0.0.1:47990/v1/models" -UseBasicParsing -TimeoutSec 5
  if ($models.StatusCode -ne 200) { throw "/v1/models returned non-200" }
  Write-Host "Smoke test passed: healthz/ui/models 200" -ForegroundColor Green
} finally {
  if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}
Write-Host "All local verification passed ✅" -ForegroundColor Green