[CmdletBinding()]
param(
    [string]$Executable,
    [string[]]$BridgeVersions,
    [switch]$SkipShellRegistration,
    [string]$AdobeUserDirectory = (Join-Path $env:APPDATA 'Adobe')
)
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
if (!$Executable) {
    $Executable = Join-Path $project 'lrplugins/FastCull.lrplugin/fastcull.exe'
    if (!(Test-Path -LiteralPath $Executable -PathType Leaf)) {
        $Executable = Join-Path $project 'FastCull.lrplugin/fastcull.exe'
    }
}
$exe = (Resolve-Path -LiteralPath $Executable).Path
if ([IO.Path]::GetExtension($exe) -ne '.exe') { throw 'Executable 必须是 FastCull exe。' }
if ($exe -match '[%!"]') { throw 'FastCull 程序路径不能包含 %、! 或引号。' }
$source = Join-Path $project 'bridge/FastCull.jsx'
if (!(Test-Path -LiteralPath $source -PathType Leaf)) { throw '缺少 bridge/FastCull.jsx。' }
if (!$BridgeVersions) {
    $BridgeVersions = @(Get-ChildItem -LiteralPath $AdobeUserDirectory -Directory |
        Where-Object Name -Match '^Bridge 20[0-9]{2}$' | Select-Object -ExpandProperty Name)
}
if (!$BridgeVersions.Count) { throw '没有发现 Bridge 用户目录；请指定 -BridgeVersions ''Bridge 2026''。' }
foreach ($version in $BridgeVersions) {
    if ($version -notmatch '^Bridge 20[0-9]{2}$') { throw "无效 Bridge 目录：$version" }
}
$utf8 = [Text.UTF8Encoding]::new($true)
# A private data-file association, never a RAW/JPEG default application change.
if (!$SkipShellRegistration) {
    $classes = 'HKCU:\Software\Classes'
    New-Item -Path "$classes\FastCull.Request" -Force | Out-Null
    Set-Item -LiteralPath "$classes\FastCull.Request" -Value 'FastCull Bridge Request'
    $requestType = New-Item -Path "$classes\FastCull.Request\shell\open\command" -Force
    Set-Item -LiteralPath $requestType.PSPath -Value ('"' + $exe + '" --bridge-list-owned "%1"')
    New-Item -Path "$classes\.fastcullrequest" -Force | Out-Null
    Set-Item -LiteralPath "$classes\.fastcullrequest" -Value 'FastCull.Request'
    # Registry writes alone do not invalidate the desktop Shell's association cache.
    # Retain association support for outstanding requests from older versions.
    if (-not ('FastCull.Install.Shell' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace FastCull.Install {
    public static class Shell {
        [DllImport("shell32.dll")]
        public static extern void SHChangeNotify(int eventId, uint flags, IntPtr item1, IntPtr item2);
    }
}
'@
    }
    [FastCull.Install.Shell]::SHChangeNotify(0x08000000, 0x1000, [IntPtr]::Zero, [IntPtr]::Zero)
}
foreach ($version in $BridgeVersions) {
    $directory = Join-Path (Join-Path $AdobeUserDirectory $version) 'Startup Scripts'
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $script = Join-Path $directory 'FastCull.jsx'
    $config = Join-Path $directory 'FastCull-path.txt'
    # Keep a recoverable copy if upgrading our own files; never change other scripts.
    foreach ($file in @($script, $config)) {
        if (Test-Path -LiteralPath $file -PathType Leaf) {
            $backup = "$file.$([DateTime]::Now.ToString('yyyyMMdd-HHmmss-fffffff')).bak"
            Copy-Item -LiteralPath $file -Destination $backup
        }
    }
    [IO.File]::WriteAllText($script, [IO.File]::ReadAllText($source), $utf8)
    [IO.File]::WriteAllText($config, $exe + [Environment]::NewLine, $utf8)
    Write-Output "已注册 $version ：$script"
}
Write-Output '重新启动 Bridge；若提示新启动脚本，请启用 FastCull。选中支持的 RAW/JPEG（包括 DNG）或包含照片的文件夹后，右键或工具菜单均可打开选片器。'
