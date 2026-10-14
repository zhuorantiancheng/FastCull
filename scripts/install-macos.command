#!/bin/bash
# Run from the unpacked distribution. Existing files are backed up, never deleted.
set -euo pipefail
package="$(cd "$(dirname "$0")/.." && pwd)"
support="$HOME/Library/Application Support"
app="$HOME/Applications/FastCull.app"
plugin="$support/Adobe/Lightroom/Modules/FastCull.lrplugin"
backup="$support/FastCull/install-backups/$(date +%Y%m%d-%H%M%S)-$$"
if [[ "$(uname -s)" != Darwin ]]; then echo 'This installer requires macOS.'; exit 1; fi
if [[ ! -x "$package/FastCull.app/Contents/MacOS/fastcull" ]]; then
    echo 'Please run this script from an unpacked FastCull macOS package.'; exit 1
fi
if pgrep -x fastcull >/dev/null || pgrep -if 'Adobe (Bridge|Lightroom)' >/dev/null; then
    echo '请先退出 FastCull、Adobe Bridge 和 Lightroom Classic，再运行安装。'; exit 1
fi
mkdir -p "$HOME/Applications" "$(dirname "$plugin")" "$backup"
if [[ -e "$app" ]]; then mv "$app" "$backup/FastCull.app"; fi
ditto "$package/FastCull.app" "$app"
if [[ -e "$plugin" ]]; then mv "$plugin" "$backup/FastCull.lrplugin"; fi
ditto "$package/FastCull.lrplugin" "$plugin"
count=0
for bridge in "$support"/Adobe/Bridge*; do
    [[ -d "$bridge" ]] || continue
    startup="$bridge/Startup Scripts"
    mkdir -p "$startup"
    for name in FastCull.jsx FastCull-path.txt; do
        if [[ -e "$startup/$name" ]]; then
            mkdir -p "$backup/$(basename "$bridge")"
            cp -p "$startup/$name" "$backup/$(basename "$bridge")/$name"
        fi
    done
    cp "$package/bridge/FastCull.jsx" "$startup/FastCull.jsx"
    printf '%s\n' "$app/Contents/MacOS/fastcull" > "$startup/FastCull-path.txt"
    count=$((count + 1))
done
printf '安装完成：%s\nLightroom 插件：%s\nBridge 注册目录：%s 个\n恢复备份：%s\n' "$app" "$plugin" "$count" "$backup"
echo '重启 Adobe 应用并启用插件。Mac 版通过菜单打开，不提供 Windows 的 Ctrl+Alt+K 助手。'
