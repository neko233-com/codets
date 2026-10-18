<#
.SYNOPSIS
    The local gate a release has to pass before its tag is pushed.

.DESCRIPTION
    GitHub Actions minutes are limited and the release matrix runs exactly once, on
    a tag, so everything that can be checked locally is checked here first:

      1. the working tree is clean, because a release is built from a commit
      2. `cargo test`, offline
      3. `cargo build --release --locked`, the command the workflow runs
      4. nothing the test suite left behind in the temp directory
      5. the version in Cargo.toml has a CHANGELOG section, which is what becomes
         the release notes

    The online checks are deliberately not here: `cargo test -- --ignored` with
    CODETS_LIVE_REGISTRY=1, and scripts\verify-demo.ps1. Both need the network and
    are run by hand.

    The script only reads the tree and writes to target\, so running it twice is the
    same as running it once.

.PARAMETER AllowDirty
    Do not require a clean working tree. For a dry run while work is in flight.

.PARAMETER SkipChangelog
    Do not require a CHANGELOG section for the current version. For a dry run while
    the entry is still being written; never for a real tag.

.EXAMPLE
    .\scripts\pre-release.ps1

    The gate. Exits non-zero at the first step that fails.

.EXAMPLE
    .\scripts\pre-release.ps1 -AllowDirty -SkipChangelog

    Runs the tests and the release build on a tree that is not ready to tag.
#>
[CmdletBinding()]
param(
    [switch]$AllowDirty,
    [switch]$SkipChangelog
)

# Native programs — cargo, git — write progress to stderr, which `Stop` would turn
# into a terminating error mid-script. Every one of their exit codes is checked
# explicitly below instead, so stderr stays ordinary output.
$ErrorActionPreference = 'Continue'
Set-Location -Path (Join-Path $PSScriptRoot '..')

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "== $Message"
}

function Fail([string]$Message) {
    [Console]::Error.WriteLine("pre-release: $Message")
    exit 1
}

# `cargo` is not on PATH when the rustup proxy scripts are missing, which happens on
# a machine where only the toolchains are installed.
function Find-Cargo {
    $onPath = Get-Command cargo -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }

    $candidates = @()
    $proxy = Join-Path $HOME '.cargo/bin/cargo.exe'
    if (Test-Path $proxy) { $candidates += $proxy }
    $toolchains = Join-Path $HOME '.rustup/toolchains'
    if (Test-Path $toolchains) {
        # The stable toolchain first, so the gate runs what CI runs.
        $candidates += @(Get-ChildItem -Path $toolchains -Filter 'cargo.exe' -Recurse -Depth 3 -ErrorAction SilentlyContinue |
            Sort-Object -Property @{ Expression = { if ($_.FullName -like '*stable*') { 0 } else { 1 } } } |
            Select-Object -ExpandProperty FullName)
    }
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) { return $candidate }
    }
    return $null
}

$cargo = Find-Cargo
if (-not $cargo) {
    Fail "cannot find cargo; see AGENTS.md for this machine's toolchain layout"
}
Write-Host "pre-release: using $cargo"

Write-Step '1/5 the working tree'
if ($AllowDirty) {
    Write-Host 'skipped (-AllowDirty)'
} else {
    $dirty = @(git status --porcelain)
    if ($LASTEXITCODE -ne 0) { Fail 'git status failed; is this a git working tree?' }
    if ($dirty.Count -gt 0) {
        $dirty | ForEach-Object { Write-Host "  $_" }
        Fail 'the tree has uncommitted changes; commit them, or pass -AllowDirty'
    }
    Write-Host 'clean'
}

Write-Step '2/5 cargo test'
& $cargo test --quiet
if ($LASTEXITCODE -ne 0) { Fail 'the test suite failed' }

Write-Step '3/5 cargo build --release --locked'
& $cargo build --release --locked
if ($LASTEXITCODE -ne 0) {
    Fail 'the release build failed; on Windows, check the LIB note in AGENTS.md'
}

Write-Step '4/5 temp leftovers'
& (Join-Path $PSScriptRoot 'check-temp-clean.ps1')
if ($LASTEXITCODE -ne 0) {
    Fail 'the test run left scratch behind; run check-temp-clean.ps1 -Fix once nothing else is running'
}

Write-Step '5/5 version and changelog'
$version = (Select-String -Path Cargo.toml -Pattern '^version *= *"(.*)"').Matches[0].Groups[1].Value
if (-not $version) { Fail 'cannot read the version from Cargo.toml' }
# The newest section is the one that describes this release, so it has to be this
# version's. An [Unreleased] section above it means the tree is not ready — and a
# version whose section is no longer newest has already shipped, which is the other
# way re-tagging goes wrong.
$newest = (Select-String -Path CHANGELOG.md -Pattern '^## ' | Select-Object -First 1).Line
# Keep a Changelog writes `## [0.3.0] - 2026-10-19`, so the version is what is inside
# the brackets and the rest of the line is not part of the comparison.
$newestVersion = if ($newest -match '^## \[([^\]]*)\]') { $Matches[1] } else { '' }
if ($newestVersion -eq $version) {
    Write-Host "Cargo.toml says $version, and CHANGELOG.md's newest section is [$newestVersion]"
} elseif ($SkipChangelog) {
    Write-Host "warning: CHANGELOG.md's newest section is `"$newest`", not [$version] (-SkipChangelog)"
} else {
    Fail "CHANGELOG.md's newest section is `"$newest`" but Cargo.toml says $version. Turn [Unreleased] into [$version], or bump the version: the release notes come from that section."
}

Write-Host ""
Write-Host 'pre-release: ready.'
Write-Host '  the tag has to point at a commit on the default branch, and CI checks that.'
Write-Host "  next:  git tag v$version; git push origin v$version"
