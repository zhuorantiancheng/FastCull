[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$taskName = (Get-Date -Format 'yyyy-MM-dd') + '-raw-benchmark-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$scratch = Join-Path $project ('.agent-temp/' + $taskName)
New-Item -ItemType Directory -Path "$scratch/bin", "$scratch/tmp" -Force | Out-Null
$env:TEMP = "$scratch/tmp"
$env:TMP = $env:TEMP
$archive = Join-Path $project 'vendor/LibRaw-0.22.2.zip'
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne 'F4448371523F9960D26A131ED9DC21BEE49F967ADBCC4FB6F56B9B47C2BA87C6') {
    throw 'LibRaw source hash mismatch'
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (!$vs) { throw 'Visual Studio C++ Build Tools are required.' }
$dev = Join-Path $vs 'Common7/Tools/VsDevCmd.bat'
foreach ($variant in @('base', 'omp')) {
    Expand-Archive -LiteralPath $archive -DestinationPath "$scratch/$variant"
    $source = "$scratch/$variant/LibRaw-0.22.2"
    # All builds use fresh directories. Remove upstream deletion recipes so no
    # clean/rebuild command can irreversibly remove files through this helper.
    $makefile = Join-Path $source 'Makefile.msvc'
    $text = [IO.File]::ReadAllText($makefile)
    $text = [regex]::Replace($text, '(?m)^\t-?del[^\r\n]*', "`t@rem Deletion disabled in isolated benchmark build")
    [IO.File]::WriteAllText($makefile, $text)
    $omp = if ($variant -eq 'omp') { '/openmp' } else { '' }
    $batch = @"
@echo off
call "$dev" -arch=x64 -host_arch=x64 >nul
if errorlevel 1 exit /b 1
cd /d "$source"
nmake /nologo /f Makefile.msvc "COPT=/EHsc /MP /MT /utf-8 /I. /DWIN32 /O2 /W0 /nologo $omp" lib\libraw_static.lib
if errorlevel 1 exit /b 1
cl /nologo /O2 /EHsc /MT /utf-8 $omp /I. "$project/scripts/bench_raw_pipeline.cpp" lib\libraw_static.lib psapi.lib /Fo"$scratch/bin/raw-$variant.obj" /Fe"$scratch/bin/raw-$variant.exe"
exit /b %errorlevel%
"@
    $batchPath = "$scratch/build-$variant.cmd"
    [IO.File]::WriteAllText($batchPath, $batch, [Text.Encoding]::Default)
    & cmd.exe /d /c $batchPath > "$scratch/build-$variant.log" 2>&1
    if ($LASTEXITCODE) { throw "Build failed; inspect $scratch/build-$variant.log" }
}
Write-Output "Benchmark executables: $scratch/bin"
Write-Output 'Run scripts/evaluate_raw_pipeline.py with --raw-bench, --sample, --runs, --threads, and --output.'
