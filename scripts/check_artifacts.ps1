# check_artifacts.ps1 - scans for stale build artifacts/test leftovers (report only, no deletion; deletion needs manual confirmation)
$p = "C:\Users\Administrator.DESKTOP-EGNE9ND\Desktop\freebuff-2api\成品\Freebuff-2API"
Write-Host "== Old installers (desktop/dist) ==" -ForegroundColor Cyan
Get-ChildItem "$p\desktop\dist" -Filter *.exe -ErrorAction SilentlyContinue | Where-Object { $_.Name -notmatch '0.10.0' } | Select-Object Name,@{n='MB';e={[math]::Round($_.Length/1MB,1)}} | Format-Table -AutoSize
Write-Host "== win-unpacked (local debug artifacts) ==" -ForegroundColor Cyan
if (Test-Path "$p\desktop\win-unpacked") { $sz = (Get-ChildItem "$p\desktop\win-unpacked" -Recurse -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum; "win-unpacked totals {0} MB" -f [math]::Round($sz/1MB,1) }
Write-Host "== data/ leftover test databases ==" -ForegroundColor Cyan
Get-ChildItem "$p\data" -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -match 'mock-demo|rev_|freebuff2api2|e2e' } | Select-Object Name,@{n='KB';e={[math]::Round($_.Length/1KB,1)}} | Format-Table -AutoSize
Write-Host "== Large promo video files (>1.5MB) ==" -ForegroundColor Cyan
Get-ChildItem "$p\宣传视频" -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Extension -match 'mp4|png|jpg' -and $_.Length -gt 1.5MB } | Select-Object Name,@{n='MB';e={[math]::Round($_.Length/1MB,1)}} | Format-Table -AutoSize
Write-Host "== Notes =="; Write-Host "The above is just a listing (safe, read-only). Confirm manually before deleting, or move files to _archive_ to observe." -ForegroundColor Yellow