<#
.SYNOPSIS
  Autonomous side-by-side test harness for THOR vs the live mimir.

  End to end, non-destructive to mimir:
    1. build the release binary
    2. export the useful mimir content READ-ONLY to a JSONL snapshot
    3. seed a fresh THOR store from that snapshot
    4. fsck the THOR store (chain + fork + differential auditor)
    5. run a battery of real prompts through `thor courier` and, when mimir is
       present, show mimir's recall for the same prompt right next to it.

  THOR never opens the live mimir DB; only the read-only Python exporter does.
  All data (snapshot + store) lives under -DataDir (default %LOCALAPPDATA%\thor),
  OUTSIDE the repo, so private memories are never committed.

.EXAMPLE
  pwsh thor/tools/run_sidebyside.ps1
  pwsh thor/tools/run_sidebyside.ps1 -Reseed -SkipBuild
#>
[CmdletBinding()]
param(
  [string]$MimirDb  = (Join-Path $env:APPDATA 'mimir\data\mimir.db'),
  [string]$MimirExe = 'C:\Users\<user>\mimir\mimir.exe',
  [string]$DataDir  = (Join-Path $env:LOCALAPPDATA 'thor'),
  [switch]$SkipBuild,
  [switch]$Reseed
)

$ErrorActionPreference = 'Stop'
$repo    = Split-Path -Parent (Split-Path -Parent $PSCommandPath)   # ...\thor
$thorEx = Join-Path $repo 'target\release\thor.exe'
$seedDir = Join-Path $DataDir 'seed'
$snap    = Join-Path $seedDir 'mimir_export.jsonl'
$store   = Join-Path $DataDir 'thor.db'
New-Item -ItemType Directory -Force -Path $seedDir | Out-Null

function Section($t) { Write-Host ""; Write-Host "==== $t ====" -ForegroundColor Cyan }

# Windows PowerShell 5.1 does NOT reliably pipe a string to a native process's
# stdin (`$s | & exe` silently delivers nothing). The courier reads the hook
# JSON from stdin, so we hand it the JSON via a temp file + cmd's `type | exe`,
# which is reliable. (The LIVE Claude Code hook is unaffected: Claude Code pipes
# the JSON to the process stdin at the OS level, exactly like the mimir hook.)
function Invoke-Courier([string]$json) {
  $tmp = [System.IO.Path]::GetTempFileName()
  try {
    [System.IO.File]::WriteAllText($tmp, $json)
    cmd /c "type `"$tmp`" | `"$thorEx`" --db `"$store`" courier"
  } finally {
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
  }
}

# 1. build
if (-not $SkipBuild) {
  Section "build (cargo build --release)"
  Push-Location $repo
  cargo build --release
  Pop-Location
}
if (-not (Test-Path $thorEx)) { throw "thor.exe not found at $thorEx (build first)" }

# 2. export mimir READ-ONLY -> jsonl
Section "export mimir (read-only) -> snapshot"
if (-not (Test-Path $MimirDb)) { throw "mimir DB not found: $MimirDb" }
python (Join-Path $repo 'tools\export_mimir.py') --mimir-db $MimirDb --out $snap

# 3. seed a fresh THOR store
Section "seed THOR store"
if ($Reseed) {
  Remove-Item -Force -ErrorAction SilentlyContinue $store, "$store-wal", "$store-shm"
}
& $thorEx --db $store import $snap

# 4. fsck
Section "fsck THOR store"
& $thorEx --db $store fsck

# 5. battery of prompts through the courier (+ mimir side-by-side)
$prompts = @(
  'how does cross-machine log-shipping sync reconcile',
  'what is the relevance floor in recall',
  'typography rule about em dashes in commits',
  'how does the head-CAS branch-on-miss stay lossless',
  'THOR head-CAS lossless design and the differential auditor',
  'security boundary: what must never go to GitHub',
  'ok thanks'                         # trivial: THOR must stay silent
)

$haveMimir = Test-Path $MimirExe
Section "side-by-side recall (THOR courier vs mimir)"
foreach ($p in $prompts) {
  Write-Host ""
  Write-Host ("PROMPT: {0}" -f $p) -ForegroundColor Yellow
  $json = (@{ prompt = $p; cwd = $repo; session_id = 'harness' } | ConvertTo-Json -Compress)

  Write-Host "--- THOR courier ---" -ForegroundColor Green
  $out = (Invoke-Courier $json) -join "`n"
  if ([string]::IsNullOrWhiteSpace($out)) { Write-Host "(silent - gated or no hits)" } else { Write-Host $out }

  if ($haveMimir) {
    Write-Host "--- mimir recall ---" -ForegroundColor Magenta
    & $MimirExe recall $p -n 3 2>$null
  }
}

Write-Host ""
Write-Host "Done. THOR store: $store  |  snapshot: $snap" -ForegroundColor Cyan
