# 发布包与 macOS 支持

事实：2026-10-18 检查了 `dist/public-2.7.4/` 与 macOS 构建链。三个 2.7.4 发布包的 SHA-256 与 `SHA256SUMS.txt` 全部匹配，便携包含 `FastCull.exe`、`fastcull-keys.exe`、`fastcull_raw.dll`、`FastCull.lrplugin`（含 `THIRD-PARTY-NOTICES.txt`）、`bridge/FastCull.jsx`、注册脚本和 LibRaw 完整源码。发现并修复一处发布缺口：`scripts/release.py` 的源码包白名单只列了 Windows 工作流，源码 ZIP 里没有 `docs/MACOS.md`、`scripts/build-macos.py`、`scripts/install-macos.command` 和 `.github/workflows/macos-build.yml`，收到源码包的人无法按 README 构建 macOS 版；现已加入白名单，源码包清单从 80 项增至 97 项。

适用条件：发新版本、修改发布脚本，或在 Mac 上构建前先看本条。

证据：
- 发布包版本一致性由 `scripts/release.py` 强制（`Cargo.toml` 版本 = `FastCull.lrplugin/Info.lua` = `bridge/FastCull.jsx` 的 `api.version`），安装器 `AppVersion={#Version}` 由 `release.py` 传入。
- **当前 `main` 领先已发布的 2.7.4**：包内 `develop.lua` 2738 字节（无裁切导入），源码 3927 字节；包内 exe 也是 2026-10-14 构建。下次发版必须先改版本号（`Cargo.toml`→`Info.lua`→`FastCull.jsx`）、更新 `docs/RELEASE-NOTES.md`，再跑 `scripts/build-release.ps1`（会自动调 `build-native.ps1` + `release.py`）。
- macOS 侧：`aarch64-apple-darwin` 与 `x86_64-apple-darwin` 的 `cargo check --lib --bin fastcull` 均通过（Windows 交叉检查，未链接）；`tests/test_macos.py` 6 项通过；`build-macos.py` 会静态链接 LibRaw、用 `otool -L` 检查无 Homebrew 依赖、ad-hoc 签名并产出 `.zip` + `.sha256`；`install-macos.command` 先备份再安装 app/插件并注册 Bridge。
- macOS 边界（文档已声明）：未在真机验收；无 WIC，缩略图/适屏走 zune 全解码（比 Windows 慢，FastThumb 仍有效）；无 Ctrl+Alt+K 助手；仅 ad-hoc 签名、未公证。

更新人：DSH（deepseek-flash），2026-10-18。

## 修订记录（2026-10-18 发版 2.7.5）

- 版本已在 `Cargo.toml`/`Cargo.lock`、`FastCull.lrplugin/Info.lua`、`bridge/FastCull.jsx`、`installer/FastCull.iss` 统一切到 2.7.5，`docs/RELEASE-NOTES.md` 增加 2.7.5 段落，`docs/VERSION-CONTROL.md` 更正为已配置 `origin`。
- 发现并修复发布阻断缺陷：`scripts/register-integrations.ps1` 的 `$pluginNames` 白名单缺少 `photo_edits.lua`，而 `sync.lua` 会 `require 'photo_edits'`；安装版 LR 插件会因此缺文件。修复后 `tests/test_distribution.py` 9 项通过。
- 已重建 `dist/public-2.7.5/`（安装器、便携包、源码包、SHA256SUMS），三个包哈希自校验通过；安装器 ProductVersion=2.7.5；便携包含 `photo_edits.lua` 与修复后的注册脚本；源码包 97 项含 macOS 构建文件。
- 发版命令：`scripts/build-release.ps1` 需要 `cargo` 在 PATH（本机工具链在 `.agent-temp/2026-10-12-rust/toolchain/cargo`）并先结束常驻的 `fastcull-keys.exe`，否则复制插件程序时报占用。
