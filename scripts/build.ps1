param([switch]$Test, [switch]$Check)
$ErrorActionPreference = 'Stop'
$env:CARGO_BUILD_TARGET = 'x86_64-pc-windows-msvc'
$project = Split-Path $PSScriptRoot -Parent
Set-Location $project
$scratch = Join-Path $project '.agent-temp/2026-10-12-rust'
New-Item -ItemType Directory -Force "$scratch/tmp" | Out-Null
$env:TEMP = "$scratch/tmp"
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = "$scratch/target"
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargoCommand) { $cargo = $cargoCommand.Source } else {
    $env:CARGO_HOME = "$scratch/toolchain/cargo"
    $env:RUSTUP_HOME = "$scratch/toolchain/rustup"
    $cargo = "$env:CARGO_HOME/bin/cargo.exe"
    if (!(Test-Path $cargo)) { throw '请先安装 Rust 1.90+ 和 Visual Studio C++ Build Tools，或将 cargo 加入 PATH。' }
    $env:PATH = "$env:CARGO_HOME/bin;$env:PATH"
}
if (!$Check) {
    & "$PSScriptRoot/build-native.ps1"
    if ($LASTEXITCODE) {exit $LASTEXITCODE}
}
if ($Test) { & $cargo test --locked; if ($LASTEXITCODE) {exit $LASTEXITCODE}; exit 0 }
if ($Check) { & $cargo check --locked; exit $LASTEXITCODE }
& $cargo build --release --locked
if ($LASTEXITCODE) {exit $LASTEXITCODE}
Copy-Item -LiteralPath "$env:CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/fastcull.exe" -Destination 'lrplugins/FastCull.lrplugin/fastcull.exe' -Force
Copy-Item -LiteralPath "$env:CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/fastcull-keys.exe" -Destination 'lrplugins/FastCull.lrplugin/fastcull-keys.exe' -Force
Write-Host '已构建并复制至 lrplugins/FastCull.lrplugin/fastcull.exe'
