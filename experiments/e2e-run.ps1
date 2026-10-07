# experiments/e2e-run.ps1 — TSK-505 single-command M3 E2E driver.
#
# Chain: pre-gate (stored Tier2 consent + cloud key, missing => BLOCKED stop,
# no REAPER touched) -> `acrd e2e` (LiveTier2 only, no seeded fallback) ->
# fresh empty REAPER instance -> `-nonewinst` run of e2e-apply.lua (inline
# 42230 renders, FNV records, reverse rollback, null-test) -> .out + file
# assertions -> kill the instance without saving. Whole run must fit in 5 min.
#
# Exit codes: 0 = PASS, 1 = FAIL (assertion or timeout), 2 = BLOCKED stop
# (precondition missing: consent/key, dirty REAPER state, or acrd e2e gate).
# REAPER is expected stopped on entry; the script refuses to run when another
# instance already exists, so user sessions are never touched.

param(
  [uint64]$Seed = 7,
  [string]$Intent = "更亮的混音",
  [string]$OutDir = "experiments/e2e",
  [string]$Reaper = "C:\Program Files\REAPER (x64)\reaper.exe",
  [int]$LuaTimeoutSec = 240
)

$ErrorActionPreference = "Stop"
$sw = [System.Diagnostics.Stopwatch]::StartNew()

function Fail([string]$msg) {
  Write-Host "E2E FAIL: $msg"
  exit 1
}
function Blocked([string]$msg) {
  Write-Host "E2E BLOCKED: $msg"
  exit 2
}

$RepoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $RepoRoot
$OutAbs = Join-Path $RepoRoot $OutDir
$PlanPath = Join-Path $OutAbs "e2e-plan.json"
$LuaPath = Join-Path $OutAbs "e2e-apply.lua"
$OutLog = Join-Path $OutAbs "e2e-apply.out.txt"

# --- 0. Preconditions: REAPER stopped, binary present -----------------------
if (-not (Test-Path $Reaper)) { Fail "reaper not found at $Reaper" }
$existing = Get-Process reaper -ErrorAction SilentlyContinue
if ($existing) { Blocked "reaper already running (pid $($existing.Id)); refusing to touch a live session" }

# --- 1. Pre-gate: stored Tier2 consent + cloud key (else stop, no REAPER) ----
$consentPath = Join-Path $env:APPDATA "SynthLM\consent.json"
if (-not (Test-Path $consentPath)) {
  Blocked "no stored consent at %APPDATA%\SynthLM\consent.json; complete the first-run choice (Tier2) first, then retry (see DEC-010)"
}
try {
  $consent = Get-Content $consentPath -Raw | ConvertFrom-Json
} catch {
  Blocked "consent file unreadable; consent stays fail-closed until a valid Tier2 choice is stored (see DEC-010)"
}
if ($consent.tier -ne "tier2") {
  Blocked "e2e needs a stored Tier2 choice, stored tier is '$($consent.tier)'; switch in settings, then retry (see DEC-010)"
}
$key = $env:OPENCODE_API_KEY
if ([string]::IsNullOrWhiteSpace($key)) { $key = $env:OPENCODE_GO_API_KEY }
if ([string]::IsNullOrWhiteSpace($key)) {
  $dotenv = Join-Path $RepoRoot ".env"
  if (Test-Path $dotenv) {
    $hit = Select-String -Path $dotenv -Pattern "^\s*OPENCODE_(API_KEY|GO_API_KEY)\s*=\s*\S+" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($hit) { $key = "dotenv-present" }
  }
}
if ([string]::IsNullOrWhiteSpace($key)) {
  Blocked "missing API key: set OPENCODE_API_KEY (alias OPENCODE_GO_API_KEY) in .env and restart; e2e stays BLOCKED until configured (see DEC-010)"
}
Write-Host "pre-gate ok: stored tier2 + key present (value never printed)"

# --- 2. acrd e2e (LiveTier2 only) -------------------------------------------
$cargoArgs = @("run", "--quiet", "-p", "synthlm-acrd", "--", "e2e", "--intent", $Intent, "--seed", "$Seed", "--out-dir", $OutDir)
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { Blocked "acrd e2e exited $LASTEXITCODE; see its BLOCKED guidance above (no seeded fallback by design)" }
if (-not (Test-Path $PlanPath)) { Fail "e2e-plan.json missing after acrd e2e" }
if (-not (Test-Path $LuaPath)) { Fail "e2e-apply.lua missing after acrd e2e" }
try {
  $plan = Get-Content $PlanPath -Raw | ConvertFrom-Json
} catch {
  Fail "e2e-plan.json unparsable: $_"
}
if ($plan.backend -ne "live-tier2") { Fail "plan backend is '$($plan.backend)', expected live-tier2" }
if ($plan.candidates.Count -ne 3) { Fail "plan candidates = $($plan.candidates.Count), expected 3" }
foreach ($c in $plan.candidates) {
  if ([string]::IsNullOrWhiteSpace($c.diff_summary_zh)) { Fail "candidate $($c.id) has empty diff sentence" }
}
Write-Host "plan ok: backend=live-tier2 candidates=$($plan.candidates.id -join ', ')"

# --- 3. Fresh empty REAPER instance + run Lua --------------------------------
Remove-Item $OutLog -ErrorAction SilentlyContinue
Remove-Item (Join-Path $OutAbs "e2e-render-*.wav") -ErrorAction SilentlyContinue
$proc = Start-Process -FilePath $Reaper -ArgumentList "-new" -PassThru
Start-Sleep -Seconds 8
if ($proc.HasExited) { Fail "reaper -new exited early" }
& $Reaper -nonewinst $LuaPath
$deadline = [DateTime]::UtcNow.AddSeconds($LuaTimeoutSec)
$done = $false
while ([DateTime]::UtcNow -lt $deadline) {
  if (Test-Path $OutLog) {
    $tail = Get-Content $OutLog -Raw
    if ($tail -match "e2e_apply_ok=true" -or $tail -match "FATAL=") { $done = $true; break }
  }
  Start-Sleep -Seconds 2
}
$logText = ""
if (Test-Path $OutLog) { $logText = Get-Content $OutLog -Raw }

try {
  if (-not $done) { Fail "lua timed out after ${LuaTimeoutSec}s (.out without e2e_apply_ok=true/FATAL=)" }
  if ($logText -match "FATAL=") { Fail "lua reported FATAL; see e2e-apply.out.txt" }
  if ($logText -notmatch "tracks_before=0") { Fail "not an empty project (tracks_before!=0); refusing to certify" }
  foreach ($c in $plan.candidates) {
    $id = [regex]::Escape($c.id)
    if ($logText -notmatch "applied\|$id\|") { Fail ".out missing applied|$($c.id)" }
    if ($logText -notmatch "render\|$id\|fnv=[0-9a-f]{8}") { Fail ".out missing render fnv for $($c.id)" }
    if ($logText -notmatch "undo\|$id") { Fail ".out missing undo|$($c.id)" }
  }
  if ($logText -notmatch "render\|baseline_a\|fnv=[0-9a-f]{8}") { Fail ".out missing baseline_a render fnv" }
  if ($logText -notmatch "render\|baseline_b\|fnv=[0-9a-f]{8}") { Fail ".out missing baseline_b render fnv" }
  if ($logText -notmatch "settleset\|stable=(true|false)") { Fail ".out missing settleset record" }
  if ($logText -notmatch "undone_blocks=\d+") { Fail ".out missing undone_blocks count" }
  if ($logText -notmatch "explicit_restore=(true|false)") { Fail ".out missing explicit_restore record" }
  if ($logText -notmatch "render\|restored_a\|fnv=[0-9a-f]{8}") { Fail ".out missing restored_a render fnv" }
  if ($logText -notmatch "render\|restored_b\|fnv=[0-9a-f]{8}") { Fail ".out missing restored_b render fnv" }
  if ($logText -notmatch "nulltest\|ref=[0-9a-f]{8}\|restored=[0-9a-f]{8}\|match=true") { Fail "render null-test did not match" }
  if ($logText -notmatch "restored_all=true") { Fail "parameter restore assert failed" }
  if ($logText -notmatch "tracks_zero=true") { Fail "temp track leaked" }
  if ($logText -notmatch "e2e_apply_ok=true") { Fail ".out missing e2e_apply_ok=true" }
  foreach ($name in $plan.renders) {
    $p = Join-Path $OutAbs $name
    if (-not (Test-Path $p)) { Fail "render file missing: $name" }
    if ((Get-Item $p).Length -le 0) { Fail "render file empty: $name" }
  }
  Write-Host "lua ok: 3 applied + renders + nulltest match + restored_all + tracks_zero"
} finally {
  if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
  Start-Sleep -Seconds 2
  $leftover = Get-Process reaper -ErrorAction SilentlyContinue
  if ($leftover) { Fail "reaper still alive after stop" }
  Write-Host "reaper closed without saving"
}

$sw.Stop()
Write-Host ("elapsed: {0:N1}s" -f $sw.Elapsed.TotalSeconds)
if ($sw.Elapsed.TotalMinutes -gt 5) { Fail "over the 5-minute budget" }
Write-Host "E2E PASS"
exit 0
