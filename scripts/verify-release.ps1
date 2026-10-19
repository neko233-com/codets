<#
.SYNOPSIS
    Verifies a published release by using it.

.DESCRIPTION
    A release is not done when the workflow is green; it is done when its assets have
    been downloaded, checked against the published checksum file and installed. This
    downloads every asset the checksum file lists, verifies each digest, lists each
    archive to confirm it holds the binary and the documents, and installs through
    install.ps1 into a throwaway store that is removed on the way out.

    It needs the network and is not part of `cargo test`; the offline contract lives
    in tests/release_contract.rs. The shell script of the same name covers install.sh,
    so running both covers both installers.

.PARAMETER Version
    The release to verify. Defaults to the newest v* tag.

.EXAMPLE
    .\scripts\verify-release.ps1

    Verifies the newest tagged release.
#>
[CmdletBinding()]
param(
    [string]$Version
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
    [Console]::Error.WriteLine("verify-release: $Message")
    exit 1
}

if (-not $Version) {
    $tag = git tag --list 'v*' --sort=-v:refname | Select-Object -First 1
    if ($tag) { $Version = $tag.TrimStart('v') }
}
if (-not $Version) { Fail 'no v* tag to verify' }

# The system's own tar, by absolute path: a Git shell puts GNU tar first on PATH, and
# that one reads a `C:\...` argument as a remote host rather than a file.
$tar = "$env:SystemRoot\System32\tar.exe"
if (-not (Test-Path $tar)) { $tar = 'tar' }

$suffix = if ($env:OS -eq 'Windows_NT') { '.exe' } else { '' }
$base = "https://github.com/neko233-com/codets/releases/download"
$checksums = "codets-$Version-checksums.txt"

$work = Join-Path ([System.IO.Path]::GetTempPath()) "codets-verify-release-$([guid]::NewGuid().ToString('n'))"
$store = Join-Path ([System.IO.Path]::GetTempPath()) "codets-verify-release-store-$([guid]::NewGuid().ToString('n'))"
New-Item -ItemType Directory -Path $work, $store -Force | Out-Null

try {
    Write-Step "v${Version}: the published checksum file"
    try {
        Invoke-WebRequest -Uri "$base/v$Version/$checksums" -OutFile (Join-Path $work $checksums)
    } catch {
        Fail "cannot download $base/v$Version/$checksums : $($_.Exception.Message)"
    }
    Get-Content (Join-Path $work $checksums) | ForEach-Object { Write-Host "  $_" }

    Write-Step 'every asset it lists, and its digest'
    $entries = @(Get-Content (Join-Path $work $checksums) | Where-Object { $_.Trim() })
    if ($entries.Count -eq 0) { Fail 'the checksum file lists nothing' }
    $assets = @($entries | ForEach-Object { ($_ -split '\s+')[-1] })

    foreach ($asset in $assets) {
        try {
            Invoke-WebRequest -Uri "$base/v$Version/$asset" -OutFile (Join-Path $work $asset)
        } catch {
            Fail "cannot download $asset : $($_.Exception.Message)"
        }
        $expected = @($entries | Where-Object { ($_ -split '\s+')[-1] -eq $asset } |
            ForEach-Object { ($_ -split '\s+')[0] })[0]
        $actual = (Get-FileHash -Path (Join-Path $work $asset) -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { Fail "$asset failed its checksum: expected $expected, got $actual" }

        # What an installer needs is inside: the binary for that platform, and the
        # licence and readme the archive carries alongside it.
        $inner = if ($asset -like '*windows*') { 'codets.exe' } else { 'codets' }
        $listing = & $tar -tzf (Join-Path $work $asset)
        if ($LASTEXITCODE -ne 0) { Fail "cannot list $asset" }
        if (-not @($listing | Where-Object { $_.EndsWith("/$inner") })) { Fail "$asset has no $inner" }
        if (-not @($listing | Where-Object { $_.EndsWith('/README.md') })) { Fail "$asset has no README.md" }
        if (-not @($listing | Where-Object { $_.EndsWith('/LICENSE') })) { Fail "$asset has no LICENSE" }
        Write-Host ("ok   {0,-46} {1}..." -f $asset, $actual.Substring(0, 16))
    }

    Write-Step 'the asset set is the five published platforms'
    # The same list as release::published_triples(); tests/release_contract.rs keeps
    # the workflow honest, and this keeps the published files honest.
    $expectedSet = @(
        "codets-$Version-aarch64-apple-darwin.tar.gz"
        "codets-$Version-aarch64-pc-windows-msvc.tar.gz"
        "codets-$Version-aarch64-unknown-linux-gnu.tar.gz"
        "codets-$Version-x86_64-pc-windows-msvc.tar.gz"
        "codets-$Version-x86_64-unknown-linux-gnu.tar.gz"
    ) | Sort-Object
    $actualSet = $assets | Sort-Object
    if (Compare-Object -ReferenceObject $expectedSet -DifferenceObject $actualSet) {
        Fail "the published assets are not the five platforms: $($actualSet -join ', ')"
    }
    Write-Host "ok   $($assets.Count) assets, macOS is Apple Silicon only"

    Write-Step 'install.ps1, against the real release'
    # The version is pinned so the anonymous releases API is not needed: a shared
    # egress address can exhaust that quota on its own.
    $env:CODETS_VERSION = $Version
    $env:CODETS_HOME = $store
    $shell = (Get-Process -Id $PID).Path
    & $shell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 --level 24
    if ($LASTEXITCODE -ne 0) { Fail "install.ps1 could not install v$Version" }

    foreach ($command in 'codets', 'node', 'npm') {
        if (-not (Test-Path (Join-Path $store "bin/$command$suffix"))) {
            Fail "install.ps1 left no bin/$command$suffix"
        }
    }
    $env:CODETS_HOME = $store
    $reported = & (Join-Path $store "bin/codets$suffix") --version
    if (-not $reported.StartsWith("codets $Version ")) {
        Fail "the installed binary reports '$reported', not codets $Version"
    }
    Write-Host "ok   the installed binary reports: $reported"
    $level = & (Join-Path $store "bin/node$suffix") -p process.version
    if ($level -ne 'v24.0.0') {
        Fail "node reports $level, not v24.0.0 (the level the installer was given)"
    }
} finally {
    # A throwaway store is only throwaway if it goes away.
    Remove-Item -Recurse -Force $work, $store -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "verify-release: v$Version — $($assets.Count) assets verified, install.ps1 installed and ran"
