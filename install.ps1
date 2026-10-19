<#
.SYNOPSIS
    One-line installer for codets, from PowerShell.

.DESCRIPTION
    Downloads the published archive for this machine, checks it against the
    release's checksum file, unpacks the binary and hands it to
    `codets bootstrap`, which owns the store layout — so this script never needs
    to know how a version directory is arranged.

    Windows PowerShell and PowerShell 7 on Windows, Linux and macOS all work: the
    target triple, the archive's binary name and `tar` are all detected rather than
    assumed. That is also what lets the test suite run this script on a Linux CI
    runner, where PowerShell is available and the shell installer is not the only
    thing worth checking.

    Run it from the repository:

        powershell -ExecutionPolicy Bypass -File .\install.ps1

    or without cloning:

        irm https://raw.githubusercontent.com/neko233-com/codets/main/install.ps1 | iex

.PARAMETER Version
    Install a specific release instead of the newest one. `CODETS_VERSION` does
    the same thing.

.PARAMETER BootstrapArgs
    Everything else is passed to `codets bootstrap`:

        .\install.ps1 -Version 0.3.0 --level 22 --no-default

.EXAMPLE
    .\install.ps1

    Installs the newest release and makes it this machine's default toolchain.

.NOTES
    Environment: `CODETS_HOME` (where the store lives), `CODETS_VERSION`,
    `CODETS_DOWNLOAD_BASE` (asset base URL, for a mirror), `CODETS_API_BASE`
    (release API base URL, for a mirror).

    The asset names it reconstructs are a contract with `src/release.rs`:

        <base>/v<version>/codets-<version>-<triple>.tar.gz
        <base>/v<version>/codets-<version>-checksums.txt
#>
[CmdletBinding()]
param(
    [string]$Version,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$BootstrapArgs
)

$ErrorActionPreference = 'Stop'
# Progress rendering costs more than the download on a small archive.
$ProgressPreference = 'SilentlyContinue'

$Repo = 'neko233-com/codets'

# `--help` is answered before anything else: it has to work on a machine with no
# release to install, which is exactly when someone reads it. `return` rather than
# `exit`, so the `irm ... | iex` form does not close the caller's session.
if ($BootstrapArgs -contains '--help' -or $BootstrapArgs -contains '-h') {
    Write-Host 'Usage: install.ps1 [-Version <version>] [<bootstrap arguments>...]'
    Write-Host ''
    Write-Host 'Downloads the published codets release for this platform, verifies its'
    Write-Host 'SHA-256 against the release checksum file, and installs it with'
    Write-Host '`codets bootstrap`. Everything after -Version goes to bootstrap.'
    Write-Host ''
    Write-Host 'Environment: CODETS_VERSION, CODETS_HOME, CODETS_DOWNLOAD_BASE, CODETS_API_BASE.'
    Write-Host 'The full description is the comment header of this file:'
    Write-Host "  Get-Help $PSCommandPath -Full"
    return
}

function Say([string]$Message) {
    Write-Host "codets-install: $Message"
}

# `throw` rather than `exit`: this script also runs as `irm ... | iex`, where
# `exit` would close the caller's session instead of just failing the install.
# A terminating error still leaves a non-zero exit code when run as a script.
function Fail([string]$Message) {
    [Console]::Error.WriteLine("codets-install: $Message")
    throw $Message
}

# Which platform this is, in the words the release assets use.
function Get-Platform {
    # `$IsWindows` only exists in PowerShell 6 and later; the environment variable
    # is set on every Windows, which is what keeps Windows PowerShell 5.1 working.
    if ($env:OS -eq 'Windows_NT') { return 'windows' }
    if ($IsMacOS) { return 'macos' }
    return 'linux'
}

# The target triple, spelled the way the release assets spell it.
function Get-Triple {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    $platform = Get-Platform
    $triple = switch ($platform) {
        'windows' {
            switch ("$arch") {
                'X64' { 'x86_64-pc-windows-msvc' }
                'Arm64' { 'aarch64-pc-windows-msvc' }
            }
        }
        'macos' {
            switch ("$arch") {
                'Arm64' { 'aarch64-apple-darwin' }
                default { throw "no published build for macOS on $arch; codets publishes for Apple Silicon only" }
            }
        }
        'linux' {
            switch ("$arch") {
                'X64' { 'x86_64-unknown-linux-gnu' }
                'Arm64' { 'aarch64-unknown-linux-gnu' }
            }
        }
    }
    if (-not $triple) { throw "no published build for $platform on $arch" }
    return $triple
}

# The name the toolchain binary has on this platform.
function Get-BinaryName {
    if ((Get-Platform) -eq 'windows') { return 'codets.exe' }
    return 'codets'
}

# The archive tool: Windows has shipped `tar.exe` since Windows 10, every other
# platform ships `tar`.
#
# On Windows it is taken by absolute path rather than from `PATH`, because `PATH` is
# not a reliable place to find it there: Git for Windows ships its own `tar` in
# `/usr/bin`, which comes first for anyone working in a Git shell, and GNU tar reads a
# `C:\...` argument as a remote host — it fails with "Cannot connect to C: resolve
# failed" instead of unpacking. The system's `tar.exe` is bsdtar and takes Windows
# paths natively.
function Get-TarProgram {
    if ((Get-Platform) -eq 'windows') {
        if ($env:SystemRoot) {
            $system = Join-Path $env:SystemRoot 'System32\tar.exe'
            if (Test-Path $system) { return $system }
        }
        return 'tar.exe'
    }
    return 'tar'
}

function Get-LatestVersion {
    $api = if ($env:CODETS_API_BASE) { $env:CODETS_API_BASE } else { "https://api.github.com/repos/$Repo" }
    $release = Invoke-RestMethod -Uri "$($api.TrimEnd('/'))/releases/latest"
    return $release.tag_name
}

# Runs a native program and returns its exit code, with its output on the console.
# Native programs write progress to stderr, which `Stop` — kept on for the cmdlets —
# would turn into a terminating error; the exit code is what matters, and every
# caller checks it.
function Invoke-Native([string]$Program, [string[]]$Arguments) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Program @Arguments | Out-Host
        return $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
}

$version = if ($Version) { $Version } elseif ($env:CODETS_VERSION) { $env:CODETS_VERSION } else { $null }
if (-not $version) {
    if ($env:CODETS_DOWNLOAD_BASE -and -not $env:CODETS_API_BASE) {
        Fail "CODETS_DOWNLOAD_BASE is set, so the newest version cannot be looked up; set CODETS_VERSION as well"
    }
    $version = Get-LatestVersion
    # An anonymous caller of the releases API is rate-limited hard — a shared egress
    # address can exhaust the quota on its own — so the message names the way out.
    if (-not $version) {
        Fail "cannot determine the newest release from the releases API (it may be rate-limiting an anonymous caller); set CODETS_VERSION to install a specific release"
    }
}
$version = $version.TrimStart('v')

$triple = Get-Triple
$base = if ($env:CODETS_DOWNLOAD_BASE) { $env:CODETS_DOWNLOAD_BASE } else { "https://github.com/$Repo/releases/download" }
$releaseUrl = "$($base.TrimEnd('/'))/v$version"
$asset = "codets-$version-$triple.tar.gz"

Say "installing codets $version for $triple"

$work = Join-Path ([System.IO.Path]::GetTempPath()) "codets-install-$([guid]::NewGuid().ToString('n'))"
New-Item -ItemType Directory -Path $work -Force | Out-Null

try {
    Say "downloading $asset"
    $archive = Join-Path $work $asset
    try {
        Invoke-WebRequest -Uri "$releaseUrl/$asset" -OutFile $archive
    } catch {
        Fail "cannot download $releaseUrl/$asset : $($_.Exception.Message)"
    }

    # A missing checksum file is tolerated, the way `codets upgrade` tolerates it;
    # a mismatch never is. Fetching and checking are separate statements so that a
    # checksum failure cannot be mistaken for a missing file.
    $checksums = Join-Path $work 'checksums.txt'
    $fetched = $true
    try {
        Invoke-WebRequest -Uri "$releaseUrl/codets-$version-checksums.txt" -OutFile $checksums
    } catch {
        $fetched = $false
    }

    if (-not $fetched) {
        Say "warning: no checksum file for $version to check the download against"
    } else {
        # The asset name is escaped: release file names contain dots.
        $pattern = "^([0-9a-fA-F]+)\s+\*?$([regex]::Escape($asset))$"
        $line = Select-String -Path $checksums -Pattern $pattern | Select-Object -First 1
        if (-not $line) {
            Say "warning: the checksum file does not list $asset; installing unverified"
        } else {
            $expected = $line.Matches[0].Groups[1].Value.ToLowerInvariant()
            $actual = (Get-FileHash -Path $archive -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actual -ne $expected) {
                Fail "$asset failed its checksum: expected sha256 $expected, got $actual"
            }
            Say "checksum matches"
        }
    }

    # Unpack with the platform's own archive tool.
    if ((Invoke-Native (Get-TarProgram) @('-xzf', $archive, '-C', $work)) -ne 0) {
        Fail "cannot unpack $asset"
    }

    $binaryName = Get-BinaryName
    $binary = Get-ChildItem -Path $work -Recurse -File -Filter $binaryName |
        Select-Object -First 1 -ExpandProperty FullName
    if (-not $binary) { Fail "$asset does not contain $binaryName" }

    $code = Invoke-Native $binary (@('bootstrap') + $BootstrapArgs)
    if ($code -ne 0) { exit $code }

    Say "add the bin directory to PATH:"
    Invoke-Native $binary @('env') | Out-Null
} finally {
    Remove-Item -Path $work -Recurse -Force -ErrorAction SilentlyContinue
}
