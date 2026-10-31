[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference='Stop'
$global:LASTEXITCODE=0
$project=Split-Path $PSScriptRoot -Parent
$scratch=Join-Path $project ('.agent-temp/'+(Get-Date -Format 'yyyy-MM-dd')+'-native-build')
$archive=Join-Path $project 'vendor/LibRaw-0.22.2.zip'
$plugin=Join-Path $project 'lrplugins/FastCull.lrplugin'
if (!(Test-Path -LiteralPath $plugin)) {$plugin=Join-Path $project 'FastCull.lrplugin'}
if (!(Test-Path -LiteralPath $plugin) -and (Test-Path -LiteralPath (Join-Path $project 'FastCull.exe'))) {$plugin=$project}
$destination=Join-Path $plugin 'fastcull_raw.dll'
if ($OutputDirectory) {
    New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
    $destination=Join-Path $OutputDirectory 'fastcull_raw.dll'
}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne 'F4448371523F9960D26A131ED9DC21BEE49F967ADBCC4FB6F56B9B47C2BA87C6') {throw 'LibRaw source hash mismatch'}
$inputs=@("$project/native/raw_bridge.cpp","$project/native/jpeg_scaled.cpp","$project/native/white_balance.h","$project/native/white_balance_table.inc",$PSCommandPath)
if (Test-Path -LiteralPath $destination) {
    $built=(Get-Item -LiteralPath $destination).LastWriteTimeUtc
    if (($inputs | Where-Object { (Get-Item -LiteralPath $_).LastWriteTimeUtc -ge $built }).Count -eq 0) {return}
}
New-Item -ItemType Directory -Force "$scratch/tmp" | Out-Null
$env:TEMP="$scratch/tmp";$env:TMP=$env:TEMP
if (!(Test-Path "$scratch/LibRaw-0.22.2/Makefile.msvc")) {Expand-Archive -LiteralPath $archive -DestinationPath $scratch}
$vswhere=Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$vs=& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (!$vs) {throw '需要 Visual Studio C++ Build Tools'}
$dev=Join-Path $vs 'Common7/Tools/VsDevCmd.bat'
$force=if (Test-Path "$scratch/utf8-mt.flags") {''} else {'/A'}
$script=@"
@echo off
call "$dev" -arch=x64 -host_arch=x64 >nul
if errorlevel 1 exit /b 1
cd /d "$scratch\LibRaw-0.22.2"
nmake /nologo $force /f Makefile.msvc "COPT=/EHsc /MP /MT /utf-8 /I. /DWIN32 /O2 /W0 /nologo" lib\libraw_static.lib
if errorlevel 1 exit /b 1
cl /nologo /std:c++17 /O2 /EHsc /MT /utf-8 /LD /I. "$project\native\raw_bridge.cpp" "$project\native\jpeg_scaled.cpp" lib\libraw_static.lib ole32.lib windowscodecs.lib /Fo"$scratch\\" /link /OUT:"$scratch\fastcull_raw.dll" /IMPLIB:"$scratch\fastcull_raw.lib"
exit /b %errorlevel%
"@
$batch=Join-Path $scratch 'build.cmd'
[IO.File]::WriteAllText($batch,$script,[Text.Encoding]::Default)
& cmd.exe /d /c $batch
if ($LASTEXITCODE) {exit $LASTEXITCODE}
[IO.File]::WriteAllText("$scratch/utf8-mt.flags",'LibRaw 0.22.2 /MT /utf-8')
Copy-Item -LiteralPath "$scratch/fastcull_raw.dll" -Destination $destination -Force
Write-Output '已构建 LibRaw 原生适配层（静态 CRT）。'
