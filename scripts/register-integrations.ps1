# Compatible with the PowerShell supplied by Windows 10/11 (no extra runtime).
[CmdletBinding()]
param(
    [ValidateSet('Install','Uninstall')][string]$Mode = 'Install',
    [string]$InstallRoot,
    [string]$AdobeDirectory = (Join-Path $env:APPDATA 'Adobe'),
    [string]$StateDirectory = (Join-Path $env:LOCALAPPDATA 'FastCull/Installer'),
    [switch]$Bridge, [switch]$Lightroom, [switch]$SkipRegistry, [switch]$Preview
)
$ErrorActionPreference = 'Stop'
if (!$InstallRoot) { $InstallRoot = Split-Path $PSScriptRoot -Parent }
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot).TrimEnd('\','/')
$AdobeDirectory = [IO.Path]::GetFullPath($AdobeDirectory).TrimEnd('\','/')
$StateDirectory = [IO.Path]::GetFullPath($StateDirectory).TrimEnd('\','/')
$manifest = Join-Path $StateDirectory 'integrations.json'
$registryKeys = @('Software\Classes\.fastcullrequest', 'Software\Classes\FastCull.Request\shell\open\command')
$names = @('FastCull.jsx','FastCull-path.txt','FastCull-shell.txt')
$pluginNames = @('fastcull.exe','fastcull-keys.exe','fastcull_raw.dll','Info.lua','common.lua','develop.lua','photo_edits.lua','HotkeyInit.lua','HotkeyStop.lua','OpenCuller.lua','organization.lua','PluginInfo.lua','sync.lua','SyncSelected.lua','SyncSidecars.lua','LICENSE','THIRD-PARTY-NOTICES.txt')
function Hash([string]$Path) {
    $stream=[IO.File]::OpenRead($Path); $hasher=[Security.Cryptography.SHA256]::Create()
    try { [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-','') }
    finally { $stream.Dispose(); $hasher.Dispose() }
}
function Allowed([string]$Path) {
    $pathFull = [IO.Path]::GetFullPath($Path)
    $lr = Join-Path $AdobeDirectory 'Lightroom/Modules/FastCull.lrplugin'
    if ([IO.Path]::GetDirectoryName($pathFull) -eq $lr -and $pluginNames -contains [IO.Path]::GetFileName($pathFull)) { return $true }
    foreach ($dir in @(Get-ChildItem -LiteralPath $AdobeDirectory -Directory -ErrorAction SilentlyContinue)) {
        if ($dir.Name -match '^Bridge 20[0-9]{2}$' -and [IO.Path]::GetDirectoryName($pathFull) -eq (Join-Path $dir.FullName 'Startup Scripts') -and $names -contains [IO.Path]::GetFileName($pathFull)) { return $true }
    }
    return $false
}
$state = [pscustomobject]@{schema=1; root=$InstallRoot; adobe=$AdobeDirectory; files=@(); registry=@()}
if (Test-Path -LiteralPath $manifest) {
    $saved = Get-Content -LiteralPath $manifest -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($saved.schema -ne 1) { throw '无法识别安装记录版本。' }
    if (@($saved.files).Count -or @($saved.registry).Count) {
        if ($Mode -eq 'Uninstall' -and ($saved.root -ne $InstallRoot -or $saved.adobe -ne $AdobeDirectory)) {
            Write-Output '集成由另一安装位置管理，保留其注册。'; exit 0
        }
        if ($saved.root -ne $InstallRoot -or $saved.adobe -ne $AdobeDirectory) { throw '安装记录与目标目录不一致；保留原记录，请使用原安装位置升级或卸载。' }
        $state = $saved
    }
}
function Save-State {
    New-Item -ItemType Directory -Force $StateDirectory | Out-Null
    $json = $state | ConvertTo-Json -Depth 8
    [IO.File]::WriteAllText($manifest + '.new', $json, [Text.UTF8Encoding]::new($false))
    if (Test-Path -LiteralPath $manifest) { [IO.File]::Replace($manifest+'.new',$manifest,$manifest+'.previous') }
    else { [IO.File]::Move($manifest+'.new',$manifest) }
}
if ($Mode -eq 'Uninstall') {
    foreach ($entry in @($state.files)) {
        if (!(Allowed $entry.path)) { throw '安装记录包含不属于 FastCull 的路径，停止清理。' }
        if (!(Test-Path -LiteralPath $entry.path) -or (Hash $entry.path) -ne $entry.sha256) {
            Write-Output "保留已修改或已移除的文件：$($entry.path)"; continue
        }
        if ($entry.backup) {
            if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($entry.backup)) -ne (Join-Path $StateDirectory 'backups')) { throw '无效恢复备份路径' }
            if (!(Test-Path -LiteralPath $entry.backup)) { throw '原始备份缺失，保留现有文件' }
            if (!$Preview) { Copy-Item -LiteralPath $entry.backup -Destination $entry.path -Force }
            Write-Output "恢复安装前文件：$($entry.path)"
        } else {
            # Exact, receipt-owned files only; never recurse or touch photos.
            if (!$Preview) { Remove-Item -LiteralPath $entry.path }
            Write-Output "移除安装器创建的文件：$($entry.path)"
        }
    }
    if (!$SkipRegistry) {
        foreach ($entry in @($state.registry)) {
            if ($registryKeys -notcontains $entry.key) { throw '无效注册表恢复记录' }
            $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($entry.key, !$Preview)
            if ($key) {
                try {
                    if ($key.GetValue('') -ceq $entry.installed -and !$Preview) {
                        if ($entry.existed) { $key.SetValue('',[string]$entry.original) }
                        else { $key.DeleteValue('', $false) }
                    }
                } finally { $key.Dispose() }
            }
        }
    }
    # Keep the receipt and original backups for recovery; no recursive removal.
    if (!$Preview) {
        if (Test-Path -LiteralPath $manifest) {
            Copy-Item -LiteralPath $manifest -Destination (Join-Path $StateDirectory ('uninstalled-'+[guid]::NewGuid().ToString()+'.json'))
        }
        $state.files=@(); $state.registry=@(); Save-State
    }
    exit 0
}
$exe = Join-Path $InstallRoot 'FastCull.exe'
if (!(Test-Path -LiteralPath $exe) -or $exe -match '[%!"\r\n]') { throw '程序不存在或安装路径含不支持的命令字符（%、!、引号）。' }
$targets = @()
if ($Lightroom) {
    foreach ($name in $pluginNames) {
        $source = Join-Path (Join-Path $InstallRoot 'FastCull.lrplugin') $name
        if (!(Test-Path -LiteralPath $source)) { throw "插件不完整：$name" }
        $targets += [pscustomobject]@{path=(Join-Path (Join-Path $AdobeDirectory 'Lightroom/Modules/FastCull.lrplugin') $name); source=$source; content=$null}
    }
}
if ($Bridge) {
    $source = Join-Path $InstallRoot 'bridge/FastCull.jsx'
    if (!(Test-Path -LiteralPath $source)) { throw 'Bridge 脚本缺失' }
    $found = @(Get-ChildItem -LiteralPath $AdobeDirectory -Directory -ErrorAction SilentlyContinue | Where-Object Name -Match '^Bridge 20[0-9]{2}$')
    if (!$found.Count) { Write-Output '未发现 Bridge 用户目录。先启动一次 Bridge，再重新运行安装程序并勾选 Bridge。' }
    foreach ($dir in $found) {
        $dest = Join-Path $dir.FullName 'Startup Scripts'
        $targets += [pscustomobject]@{path=(Join-Path $dest 'FastCull.jsx');source=$source;content=$null}
        $targets += [pscustomobject]@{path=(Join-Path $dest 'FastCull-path.txt');source=$null;content=($exe+[Environment]::NewLine)}
        if (!$SkipRegistry) { $targets += [pscustomobject]@{path=(Join-Path $dest 'FastCull-shell.txt');source=$null;content='FastCull.Request'} }
    }
}
foreach ($target in $targets) {
    if (!(Allowed $target.path)) { throw '注册目标不在 Adobe 插件目录中' }
    if ($Preview) { Write-Output "注册：$($target.path)"; continue }
    $entry = @($state.files | Where-Object path -EQ $target.path) | Select-Object -First 1
    if (!$entry) {
        $backup = $null
        if (Test-Path -LiteralPath $target.path) {
            $backupDir=Join-Path $StateDirectory 'backups'; New-Item -ItemType Directory -Force $backupDir | Out-Null
            $backup=Join-Path $backupDir ([guid]::NewGuid().ToString()+'.original')
            Copy-Item -LiteralPath $target.path -Destination $backup
        }
        $entry=[pscustomobject]@{path=$target.path;backup=$backup;sha256=''}
        $state.files=@($state.files)+@($entry)
    }
    New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($target.path)) | Out-Null
    if ($target.source) { $entry.sha256=Hash $target.source }
    else {
        $bytes=[Text.UTF8Encoding]::new($true).GetPreamble()+[Text.Encoding]::UTF8.GetBytes($target.content)
        $hasher=[Security.Cryptography.SHA256]::Create()
        try {$entry.sha256=[BitConverter]::ToString($hasher.ComputeHash($bytes)).Replace('-','')} finally {$hasher.Dispose()}
    }
    Save-State
    if ($target.source) { Copy-Item -LiteralPath $target.source -Destination $target.path -Force }
    else { [IO.File]::WriteAllText($target.path,$target.content,[Text.UTF8Encoding]::new($true)) }
    Write-Output "已注册：$($target.path)"
}
if ($Bridge -and !$SkipRegistry -and !$Preview) {
    $values=@('FastCull.Request',('"'+$exe+'" --bridge-list-owned "%1"'))
    for ($i=0;$i -lt $registryKeys.Count;$i++) {
        $path=$registryKeys[$i]; $key=[Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($path)
        try {
            $entry=@($state.registry | Where-Object key -EQ $path) | Select-Object -First 1
            if (!$entry) {
                $old=$key.GetValue(''); $entry=[pscustomobject]@{key=$path;existed=($null -ne $old);original=$old;installed=$values[$i]}
                $state.registry=@($state.registry)+@($entry)
            }
            $entry.installed=$values[$i]; Save-State; $key.SetValue('',$values[$i])
        } finally {$key.Dispose()}
    }
}
Write-Output '完成。重新启动 Adobe 应用；Bridge 首次提示 FastCull 启动脚本时请选择启用。'
