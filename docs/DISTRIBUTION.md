# Windows 分发与 GitHub 发布

## 给使用者的文件

`dist/public-版本/` 包含安装程序、便携 ZIP、干净源码 ZIP 和 `SHA256SUMS.txt`。这些文件可作为 GitHub Release 附件直接上传。独立程序和 Adobe 插件全部内置，不要求用户配置开发环境。

安装器由 Inno Setup 6 构建，使用当前用户目录，Bridge 与 Lightroom Classic 为可选组件。Adobe 注册脚本兼容 Windows 自带的 PowerShell 5.1。Bridge 会匹配已有 `Bridge 20xx` 用户目录；LR 使用标准的用户 Modules 目录。首次启动脚本授权由 Bridge 自己显示。

## 第一次上传 GitHub

1. 解压 `FastCull-Source-版本.zip` 到新的空目录，作为仓库根目录。不要直接上传当前开发工作区，后者含本机日志、测试中间文件和旧分发包。
2. 在 GitHub 创建仓库，上传这个源码目录的内容，保留隐藏目录 `.github` 和 `.cargo`。也可以在该目录使用 `git init`、添加远端后推送。
3. 创建与 `Cargo.toml` 一致的标签，例如 `v2.7.1`。
4. 推送标签会触发 Windows 构建，成功后生成 **草稿 Release**。检查附件与发布说明，确认后在 GitHub 点击 Publish release。
5. 如果使用已在本机生成的安装包，也可手动创建 Release，并上传 `dist/public-版本/` 中的四个文件。

工作流不会自动发布公开 Release；源码中不保存 GitHub 令牌。GitHub Actions 使用仓库提供的短期 `GITHUB_TOKEN` 创建草稿。

## 本地构建

安装 Rust（rustup）、Visual Studio 2022 C++ Build Tools 的“使用 C++ 的桌面开发”、Python 3.11+ 和 Inno Setup 6。建议使用 PowerShell 7 执行开发脚本。

```powershell
python -m pip install lupa pillow
cargo test --locked
cargo build --locked
node --test tests/bridge_entry.test.cjs
python -m unittest discover -s tests -p 'test_*.py'
./scripts/build-release.ps1
```

Python 的 LR 测试使用 `CARGO_TARGET_DIR`（未设置时为 `target`）下的 debug 可执行文件。少量原生 GUI 测试需要交互桌面，不以无桌面 CI 结果代替人工 Adobe 验证。

自定义工具路径：`./scripts/build-release.ps1 -Python <python.exe> -Iscc <ISCC.exe>`。调试便携包可加 `-SkipInstaller`；正式发布不要跳过安装程序。脚本使用 `.agent-temp/日期-任务名/` 保存构建中间文件，成果保存在 `dist/public-版本/`。

版本同步修改：`Cargo.toml`、`Cargo.lock`、`bridge/FastCull.jsx`、`lrplugins/FastCull.lrplugin/Info.lua` 和 Bridge 版本测试。安装器版本从 Cargo 传入。

## 安装与恢复

插件安装记录保存在 `%LOCALAPPDATA%\FastCull\Installer\integrations.json`。已有同名文件先备份；卸载仅对安装器记录且哈希仍匹配的文件清理或恢复。被用户修改的文件保留。恢复备份和卸载记录保留在同目录，便于人工恢复。

安装失败可在关闭 Adobe 后重新运行原目录的安装器。换目录前请先卸载旧安装；安装版与便携版不同时注册。升级中取消组件勾选不会移除已有集成。安装器不删除照片，不重置 Adobe 配置，也不卸载用户自行注册的其他插件。

## 发布前验证

- Rust、Lua SDK 模拟测试、Bridge 脚本测试通过。
- 使用中文和空格路径测试注册、重复升级、冲突备份与卸载预览。
- 检查 ZIP 中没有 `.agent-temp`、真实照片、日志、私人路径、旧版可执行文件。
- 用干净 Windows 10/11 x64 用户实际安装，分别测试独立启动、Bridge 脚本授权、LR 加载、Ctrl+Alt+K、同步和卸载；本机隔离目录测试不能替代这一项。
- 当前安装器未做代码签名。后续正式面向大众发布可增加 Authenticode 签名；签名后重新生成 SHA-256，不能沿用签名前的校验值。

第三方许可证由本地 Cargo 源码缓存和固定哈希的 LibRaw 源码生成。重建原生 DLL 时执行 `scripts/build-native.ps1`；该脚本随二进制发行包提供。
