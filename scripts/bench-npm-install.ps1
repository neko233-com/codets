# Parallel install throughput, measured end to end.
#
# The point of this script is a reproducible number rather than a claim: it
# builds a local registry of real gzipped tarballs, installs the same project
# once with one worker and once with many, and reports both wall clocks. The
# unpacking is CPU-bound gzip work, so the measurement shows actual parallel
# throughput instead of network luck.
#
# Usage:
#   pwsh -File scripts/bench-npm-install.ps1
#   pwsh -File scripts/bench-npm-install.ps1 -Packages 48 -FillerKb 2000 -Jobs 8

param(
    [int]$Packages = 24,
    [int]$FillerKb = 1500,
    [int]$Jobs = 8
)

$ErrorActionPreference = 'Stop'

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("codets-bench-" + [Guid]::NewGuid().ToString("N").Substring(0, 8))
$registry = Join-Path $root 'registry'
$project = Join-Path $root 'project'
New-Item -ItemType Directory -Path $registry, $project | Out-Null

# The binaries live next to this script's parent, either in debug or release.
$repo = Split-Path -Parent $PSScriptRoot
$npm = Join-Path $repo 'target\release\npm.exe'
if (-not (Test-Path $npm)) { $npm = Join-Path $repo 'target\debug\npm.exe' }
if (-not (Test-Path $npm)) { throw "build the toolchain first: cargo build --release" }
$node = Join-Path (Split-Path -Parent $npm) 'node.exe'

Write-Host "building a registry of $Packages packages ($FillerKb KiB of filler each)"

$dependencies = @()
for ($i = 1; $i -le $Packages; $i++) {
    $name = "bench-pkg-$i"
    $version = '1.0.0'
    $dependencies += '"' + $name + '": "^1.0.0"'

    $stage = Join-Path $root "stage\$name\package"
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    Set-Content -Path (Join-Path $stage 'package.json') -Value ('{ "name": "' + $name + '", "version": "' + $version + '", "main": "index.js" }')
    Set-Content -Path (Join-Path $stage 'index.js') -Value ("module.exports = '$name';")
    # Compressible filler: gzip inflation is the CPU work being parallelised.
    $filler = ('codets filler line for ' + $name + "`n") * [int]($FillerKb * 1024 / 30)
    Set-Content -Path (Join-Path $stage 'data.txt') -Value $filler

    $target = Join-Path $registry "$name\$version.tgz"
    New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
    & tar -czf $target -C (Join-Path $root "stage\$name") package

    $packument = '{ "name": "' + $name + '", "versions": { "' + $version + '": { "dist": { "tarball": "' + $name + '-' + $version + '.tgz" } } } }'
    Set-Content -Path (Join-Path $registry "$name.json") -Value $packument
}

Set-Content -Path (Join-Path $project 'package.json') -Value ('{ "name": "bench", "version": "1.0.0", "dependencies": { ' + ($dependencies -join ', ') + ' } }')

function Measure-Install([int]$workers, [string]$label) {
    $dir = Join-Path $root ("run-" + $workers)
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    Copy-Item (Join-Path $project 'package.json') $dir
    $start = Get-Date
    # npm resolves `package.json` from the working directory, so run it there.
    Push-Location $dir
    try {
        $output = & $npm install --registry $registry --jobs $workers --clean 2>&1 | Out-String
    }
    finally {
        Pop-Location
    }
    $elapsed = (Get-Date) - $start
    $output.Trim() | Write-Host
    [pscustomobject]@{
        Label     = $label
        Workers   = $workers
        WallClock = [math]::Round($elapsed.TotalSeconds, 3)
    }
}

$serial = Measure-Install 1 'serial'
$parallel = Measure-Install $Jobs ("parallel x" + $Jobs)

Write-Host ""
Write-Host "packages: $Packages"
Write-Host ("serial   ({0} worker) : {1}s" -f $serial.Workers, $serial.WallClock)
Write-Host ("parallel ({0} workers): {1}s" -f $parallel.Workers, $parallel.WallClock)
if ($parallel.WallClock -gt 0) {
    Write-Host ("speed-up: {0}x" -f [math]::Round($serial.WallClock / $parallel.WallClock, 2))
}

# The installed tree must be identical either way, which is what makes the
# timing comparison meaningful.
$serialFiles = (Get-ChildItem (Join-Path $root 'run-1\node_modules') -Recurse -File).Count
$parallelFiles = (Get-ChildItem (Join-Path $root ("run-" + $Jobs + '\node_modules')) -Recurse -File).Count
Write-Host "installed files: serial=$serialFiles parallel=$parallelFiles"
if ($serialFiles -ne $parallelFiles) { throw "the two runs produced different trees" }

# A JavaScript throughput baseline, so interpreter work has a number to move.
# This is single-threaded by design: JavaScript execution is single-threaded.
Write-Host ""
$script = @'
let total = 0;
const start = Date.now();
for (let i = 0; i < 200000; i++) {
  total += (i % 7) * 2;
}
const elapsed = Date.now() - start;
console.log(`js baseline: 200000 loop iterations in ${elapsed}ms (total ${total})`);
'@
Set-Content -Path (Join-Path $root 'bench.js') -Value $script
& $node (Join-Path $root 'bench.js')

Remove-Item -Recurse -Force $root
