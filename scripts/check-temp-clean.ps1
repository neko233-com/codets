<#
.SYNOPSIS
    Finds the scratch directories a test run leaves in the temp directory.

.DESCRIPTION
    Every test and script in this repository puts its scratch state in the system
    temp directory under a `codets-` prefix, and a full test run must leave none of
    it behind. This is the check `scripts/pre-release` runs, and the check to run
    after `cargo test`.

    Run it when no tests are running: a test that is still in flight looks exactly
    like a leak.

.PARAMETER Fix
    Remove what was found instead of only reporting it.

.PARAMETER TempRoot
    Where to look. Defaults to the system temp directory.

.EXAMPLE
    .\scripts\check-temp-clean.ps1

    Reports leftovers and exits 1 if there are any.

.EXAMPLE
    .\scripts\check-temp-clean.ps1 -Fix

    Removes them and exits 0.
#>
[CmdletBinding()]
param(
    [switch]$Fix,
    [string]$TempRoot
)

$ErrorActionPreference = 'Stop'
if (-not $TempRoot) { $TempRoot = [System.IO.Path]::GetTempPath() }
if (-not (Test-Path $TempRoot)) {
    Write-Error "check-temp-clean: $TempRoot does not exist"
    exit 2
}

function Get-Leftovers {
    @(Get-ChildItem -Path $TempRoot -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'codets-*' })
}

$leftovers = Get-Leftovers
if ($leftovers.Count -eq 0) {
    Write-Host "check-temp-clean: clean, no codets-* directory in $TempRoot"
    exit 0
}

Write-Host "check-temp-clean: found $($leftovers.Count) leftover director$(if ($leftovers.Count -eq 1) { 'y' } else { 'ies' }) in $TempRoot"
foreach ($item in $leftovers) {
    $bytes = ($item | Get-ChildItem -Recurse -File -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
    if (-not $bytes) { $bytes = 0 }
    Write-Host ("  {0}  {1:N1} KiB  {2:yyyy-MM-dd HH:mm:ss}" -f $item.Name, ($bytes / 1KB), $item.LastWriteTime)
}

if (-not $Fix) {
    Write-Host "check-temp-clean: run with -Fix to remove them"
    exit 1
}

foreach ($item in $leftovers) {
    Remove-Item -Path $item.FullName -Recurse -Force -ErrorAction SilentlyContinue
}
$remaining = Get-Leftovers
if ($remaining.Count -gt 0) {
    Write-Error "check-temp-clean: $($remaining.Count) could not be removed; a running test may still hold them open"
    exit 1
}
Write-Host "check-temp-clean: removed $($leftovers.Count)"
exit 0
