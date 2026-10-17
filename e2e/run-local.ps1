# Local Web E2E runner (mirrors the `e2e` job in .github/workflows/ci.yml).
#
#   Run from the repo root:  & ".\e2e\run-local.ps1"
#
# NOTE: messages are intentionally ASCII-only. Windows PowerShell 5.1 reads .ps1
# files as ANSI unless they carry a UTF-8 BOM, so non-ASCII text in this file
# corrupts the parser. (CI runs the same steps inline on ubuntu, unaffected.)
param(
    [int]$Port = 18080
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$dataDir = Join-Path $env:TEMP "finbook-e2e-local"
if (Test-Path $dataDir) { Remove-Item $dataDir -Recurse -Force }
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null

$env:FINBOOK_REALM      = Join-Path $dataDir "realm.db"
$env:FINBOOK_BOOKS_DIR  = Join-Path $dataDir "books"
$env:FINBOOK_LISTEN     = "127.0.0.1:$Port"
$env:FINWEB_STATIC_DIR  = Join-Path $root "crates/finweb/static"
$env:FINBOOK_ADMIN_USER = "admin"
$env:FINBOOK_ADMIN_PASS = "Admin!2026"

Write-Host "==> starting finweb on 127.0.0.1:$Port"
$log = Join-Path $dataDir "server.log"
$errLog = Join-Path $dataDir "server.err.log"
$proc = Start-Process -FilePath ".\target\debug\finweb.exe" `
    -WorkingDirectory $root -PassThru -NoNewWindow `
    -RedirectStandardOutput $log -RedirectStandardError $errLog

$base = "http://127.0.0.1:$Port"
$up = $false
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 500
    try {
        $r = Invoke-WebRequest -Uri "$base/api/health" -UseBasicParsing -TimeoutSec 2
        if ($r.StatusCode -eq 200) { $up = $true; break }
    } catch { }
}
if (-not $up) {
    Write-Host "::error:: finweb failed to start"
    if (Test-Path $log)    { Get-Content $log -Tail 60 }
    if (Test-Path $errLog) { Get-Content $errLog -Tail 60 }
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    exit 1
}
Write-Host "==> server up"

$env:E2E_BASE_URL = $base
$code = 1
Push-Location (Join-Path $root "e2e")
# npx writes "npm notice ..." to stderr; with ErrorActionPreference=Stop that
# surfaces as NativeCommandError and aborts before any test runs. Playwright's
# real failures come through its exit code, which we read via $LASTEXITCODE.
$ErrorActionPreference = "Continue"
try {
    npx playwright test
    $code = $LASTEXITCODE
} finally {
    $ErrorActionPreference = "Stop"
    Pop-Location
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}

if ($code -ne 0) {
    Write-Host "==> E2E FAILED - server log tail:"
    if (Test-Path $log)    { Get-Content $log -Tail 120 }
    if (Test-Path $errLog) { Get-Content $errLog -Tail 60 }
} else {
    Write-Host "==> E2E PASSED"
}
Write-Host "==> data dir: $dataDir"
exit $code
