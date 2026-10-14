[CmdletBinding()]
param([string]$Python = 'python', [string]$Iscc, [switch]$SkipInstaller)
$ErrorActionPreference = 'Stop'
$env:CARGO_BUILD_TARGET = 'x86_64-pc-windows-msvc'
$project = Split-Path $PSScriptRoot -Parent
Set-Location $project
if (!(Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Install Rust 1.90.0 and Visual Studio C++ Build Tools first.' }
$scratch = Join-Path $project ('.agent-temp/'+(Get-Date -Format 'yyyy-MM-dd')+'-release-build')
New-Item -ItemType Directory -Force "$scratch/tmp" | Out-Null
$env:TEMP="$scratch/tmp"; $env:TMP=$env:TEMP
if (!$env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR=Join-Path $scratch 'target' }
& "$PSScriptRoot/build-native.ps1"
if ($LASTEXITCODE) { throw 'Native build failed' }
& cargo build --release --locked
if ($LASTEXITCODE) { throw 'Rust build failed' }
foreach ($name in @('fastcull.exe','fastcull-keys.exe')) {
    Copy-Item -LiteralPath "$env:CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/$name" -Destination "$project/lrplugins/FastCull.lrplugin/$name" -Force
}
$arguments=@("$PSScriptRoot/release.py")
if ($Iscc) { $arguments+=@('--iscc',$Iscc) }
if ($SkipInstaller) { $arguments+='--skip-installer' }
& $Python @arguments
if ($LASTEXITCODE) { throw 'Packaging failed' }
