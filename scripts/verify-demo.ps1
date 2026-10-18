<#
.SYNOPSIS
    Installs the Vite 8 demo with codets and runs its TypeScript.

.DESCRIPTION
    This is the end-to-end check against the real npm registry: it drives `codets npm
    install` in examples\vite8-demo, asserts what the install produced, and then runs
    the demo's own TypeScript with `codets node` — no bundler and no build step.

    It needs the network and is therefore not part of `cargo test`; run it before a
    release alongside the other online checks (`cargo test -- --ignored` with
    CODETS_LIVE_REGISTRY=1). It is also the slower half of the pair: the Vite 8 tree
    is large.

    What it deliberately does **not** do is run Vite's own command line. See the last
    step of its output and the demo's README for why.

.PARAMETER Binary
    The codets binary to drive. Defaults to target\release, then target\debug.

.PARAMETER Jobs
    Worker threads for the download. `codets` installs in parallel; the default here
    is 8.

.EXAMPLE
    .\scripts\verify-demo.ps1

    Uses the release build if there is one.
#>
[CmdletBinding()]
param(
    [string]$Binary,
    [int]$Jobs = 8
)

# Native programs write progress to stderr, which `Stop` would turn into a
# terminating error; every exit code is checked explicitly instead.
$ErrorActionPreference = 'Continue'
Set-Location -Path (Join-Path $PSScriptRoot '..')

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "== $Message"
}

function Fail([string]$Message) {
    [Console]::Error.WriteLine("verify-demo: $Message")
    exit 1
}

$demo = Join-Path (Get-Location) 'examples/vite8-demo'
if (-not (Test-Path $demo)) { Fail "the demo is missing from $demo" }

Write-Step '0/6 the toolchain'
if (-not $Binary) {
    foreach ($candidate in @('target/release/codets.exe', 'target/release/codets', 'target/debug/codets.exe', 'target/debug/codets')) {
        if (Test-Path $candidate) {
            $Binary = (Resolve-Path $candidate).Path
            break
        }
    }
}
if (-not $Binary) {
    Fail 'no codets binary found; run `cargo build --release` first, or pass -Binary'
}
Write-Host "using $Binary"

Write-Step '1/6 a clean starting point'
# Both of these are generated, so the run starts from the same place every time.
Remove-Item -Recurse -Force (Join-Path $demo 'node_modules') -ErrorAction SilentlyContinue
Remove-Item -Force (Join-Path $demo 'package-lock.json') -ErrorAction SilentlyContinue

Push-Location $demo
try {
    Write-Step '2/6 which registry this will use'
    # Merged and reprinted: PowerShell wraps a native program's stderr in an error
    # record, which prints a stack trace where a one-line answer belongs.
    $registry = & $Binary npm config get registry --origin 2>&1
    if ($LASTEXITCODE -ne 0) { Fail 'the toolchain could not report its registry' }
    $registry | ForEach-Object { Write-Host "  $_" }

    Write-Step '3/6 npm install'
    $out = Join-Path ([System.IO.Path]::GetTempPath()) "codets-verify-demo-$([guid]::NewGuid().ToString('n')).out"
    $err = Join-Path ([System.IO.Path]::GetTempPath()) "codets-verify-demo-$([guid]::NewGuid().ToString('n')).err"
    & $Binary npm install --jobs $Jobs > $out 2> $err
    $code = $LASTEXITCODE
    $summary = if (Test-Path $out) { (Get-Content $out -Raw) } else { "" }
    $warnings = @()
    if (Test-Path $err) {
        $warnings = @(Select-String -Path $err -Pattern '^npm: warning:' | Select-Object -ExpandProperty Line)
    }
    Write-Host $summary.Trim()
    Write-Host "warnings: $($warnings.Count) (not asserted empty: a tree this size has ranges no flat installer can satisfy)"
    foreach ($warning in $warnings | Select-Object -First 5) { Write-Host "  $warning" }
    Remove-Item -Force $out, $err -ErrorAction SilentlyContinue
    if ($code -ne 0) { Fail "npm install exited with $code" }

    Write-Step '4/6 what the install produced'
    $viteManifest = Join-Path $demo 'node_modules/vite/package.json'
    if (-not (Test-Path $viteManifest)) { Fail 'node_modules/vite is missing' }
    $vite = (Get-Content $viteManifest -Raw | ConvertFrom-Json).version
    if ($vite -notlike '8.*') { Fail "vite $vite is not the 8.x line the demo pins" }

    $lock = Join-Path $demo 'package-lock.json'
    if (-not (Test-Path $lock)) { Fail 'no package-lock.json was written' }
    $packages = @(Select-String -Path $lock -Pattern '"node_modules/').Count
    if ($packages -eq 0) { Fail 'the lockfile lists no packages' }

    $shim = Join-Path $demo 'node_modules/.bin/vite.cmd'
    if (-not (Test-Path $shim)) { Fail 'node_modules/.bin has no vite shim' }
    Write-Host "vite $vite, $packages packages in the lockfile, vite shim present"

    Write-Step "5/6 the demo's own TypeScript"
    $printed = & $Binary node (Join-Path $demo 'src/summary.ts') 2>&1
    if ($LASTEXITCODE -ne 0) { Fail "running src/summary.ts failed: $printed" }
    $text = ($printed | Out-String).Trim()
    foreach ($line in @('vite@8', 'codets@0.3', 'vite + codets: 2 releases described')) {
        if ($text -notmatch [regex]::Escape($line)) {
            Fail "the program's output is missing `"$line`": $text"
        }
    }
    Write-Host $text

    Write-Step '6/6 what this did not verify'
    Write-Host 'Not run, and not runnable with this toolchain: `vite dev`, `vite build`, `vite preview`.'
    Write-Host "  Vite's CLI spawns esbuild through child_process, which codets does not implement,"
    Write-Host '  and loads Rolldown and lightningcss as native .node modules, which it refuses.'
    Write-Host '  So this verifies installation and TypeScript, not the Vite command line.'
} finally {
    Pop-Location
}

Write-Host ""
Write-Host 'verify-demo: ok'
