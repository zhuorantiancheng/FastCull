[CmdletBinding()]
param([string]$Destination)
$ErrorActionPreference = 'Stop'
$source = Split-Path $PSScriptRoot -Parent
if (!(Test-Path -LiteralPath (Join-Path $source 'FastCull.exe'))) { throw '请先完整解压 Windows 独立版，再运行安装。' }
if (!$Destination) { $Destination = Join-Path $env:LOCALAPPDATA 'Programs/FastCull/2.7.0' }
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
$destinationPath = (Resolve-Path -LiteralPath $Destination).Path
if ($destinationPath -ne (Resolve-Path -LiteralPath $source).Path) {
    foreach ($item in Get-ChildItem -LiteralPath $source) {
        Copy-Item -LiteralPath $item.FullName -Destination $destinationPath -Recurse -Force
    }
}
$shell = New-Object -ComObject WScript.Shell
foreach ($folder in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))) {
    $shortcut = $shell.CreateShortcut((Join-Path $folder 'FastCull.lnk'))
    $shortcut.TargetPath = Join-Path $destinationPath 'FastCull.exe'
    $shortcut.WorkingDirectory = $destinationPath
    $shortcut.Description = 'FastCull RAW / JPEG 快速选片器'
    $shortcut.Save()
}
Write-Output "已安装到 $destinationPath，并创建桌面和开始菜单快捷方式。"
