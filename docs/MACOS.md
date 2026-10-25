# macOS 适配与验证

2.8.0 分别构建 Apple Silicon（arm64）和 Intel（x86_64）包，部署目标为 macOS 11+。发布前检查两个架构的 Rust 编译、在对应 GitHub Mac runner 上运行测试、构建并验证原生库和应用包；最低系统版本、Mac 桌面及 Adobe 宿主操作仍须人工验收，不能用编译成功代替。

## 已接入的功能

- 独立窗口、原生照片／文件夹选择、窗口拖入照片和文件夹。
- 系统中文字体和 Retina 窗口配置。
- RAW 适配层使用 `libfastcull_raw.dylib`，通过 UTF-8 路径调用 LibRaw；Windows 保留 UTF-16 接口和 DLL。
- ARW/NEF/DNG 本地 RAW 编辑在独立进程窗口运行，读取 XMP 并显示 sRGB，主窗口可继续操作；不通过 Adobe 软件显影。曝光、WB、七项基础调节及兼容范围见 [RAW 编辑说明](RAW-EDITING.md)。
- 多个 RAW 编辑窗口从同一 LR 会话读取配方时，使用跨进程文件锁串行访问邮箱；取消可中断等待，进程退出自动释放锁。
- 筛选、多选、裁切撤销使用 Command；裁切重做支持 Command+Shift+Z，也保留 Command+Y。
- 设置和日志保存在 `~/Library/Application Support/FastCull/`；“打开日志”使用 Finder。
- Bridge 菜单启动、Lightroom Classic 菜单启动和原有会话同步流程。Mac 路径按 POSIX 规则引用，保留大小写及反斜杠。

Mac 版不包含 Windows 的 Ctrl+Alt+K 前台快捷键助手，请使用 Bridge 右键／工具菜单和 Lightroom 的“图库 → 增效工具额外命令”。Windows 的 WIC 缩小 JPEG 加速也不用于 Mac，Mac 目前使用已有 Rust JPEG 解码和缩放路径；性能需要实测。

旧版 Adobe 文本交接协议无法表示含换行的文件名；LR 交接还不能表示路径中的 `|`。插件会提示先重命名，避免将路径拆成错误记录。Mac 不提供旧 Python 原型回退。

## 在 Mac 构建

准备 Xcode Command Line Tools、Rust 1.90.0 和 Python 3.11+。最终用户运行应用无需安装这些工具。

在源码根目录执行：

```sh
python3 scripts/build-macos.py
```

脚本按当前 Mac 架构构建，校验仓库内 LibRaw 0.22.2 源码散列，静态链接 LibRaw，运行 Rust 测试，检查原生库依赖，并生成：

```text
dist/macos/FastCull-版本-macos-arm64.zip
dist/macos/FastCull-版本-macos-arm64.zip.sha256
```

Intel 输出后缀为 `x86_64`。全部构建中间文件放在 `.agent-temp/<日期>-macos-<时间>-<架构>/`，最终压缩包不依赖该目录。

GitHub Actions 的 **macOS build** 工作流提供两个架构的独立构建和测试任务，可手动触发，也会响应版本标签。构建成功后在工作流 Artifacts 下载，工作流不会自动发布正式 Release。Runner 名称依照 [GitHub 官方列表](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)配置。

应用结构按 [Apple Bundle 文档](https://developer.apple.com/library/archive/documentation/CoreFoundation/Conceptual/CFBundles/BundleTypes/BundleTypes.html)组织，可执行文件在 `Contents/MacOS`，原生库在 `Contents/Frameworks`。包内提供依赖许可和 LibRaw 完整源码；只重建原生适配层可在解包目录运行 `python3 scripts/build-macos.py --native-only`，输出到 `dist/macos-native-<架构>/`。修改已签名应用中的库后需要重新签名应用。

## 安装和 Adobe 注册

1. 退出 FastCull、Bridge 和 Lightroom Classic。
2. 解压与本机处理器匹配的包。仅独立使用时可将 `FastCull.app` 拖到“应用程序”。
3. 需要 Adobe 集成时运行解包目录内的 `scripts/install-macos.command`。脚本将应用安装到 `~/Applications/FastCull.app`，将插件安装到 `~/Library/Application Support/Adobe/Lightroom/Modules/FastCull.lrplugin`，并为已存在的 Bridge 用户目录注册启动脚本。
4. 重启 Adobe 应用。Bridge 中允许 FastCull 启动脚本；LR 的增效工具管理器中确认启用 FastCull，未自动显示时手动添加上述插件目录。

安装脚本会保存被替换的应用、插件及 Bridge 配置到 `~/Library/Application Support/FastCull/install-backups/`，不删除旧版备份。升级前关闭宿主；第一次安装 Bridge 或升级年份后，可重新运行注册脚本。LR 插件内带独立的应用副本，移动独立版不会影响插件副本；Bridge 注册的独立应用位置需要保持固定。

当前构建仅做 ad-hoc 签名，尚无 Apple Developer ID 签名和公证。下载包可能受到 Gatekeeper 限制；请核实来源，通过 macOS 的“隐私与安全性”按系统提示允许打开，不要全局关闭系统安全保护。签名验证通过不等于通过 Apple 公证。

## 验证边界

2.8.0 的 Windows 本地 RAW 验证记录见 [raw-window-validation-2026-10-24.json](raw-window-validation-2026-10-24.json)，其中的 Windows 耗时不能用于估计 Mac 性能。Apple Silicon / Intel 在 Windows 上交叉执行的 `cargo check` 仅验证 Rust 编译，不完成 Apple 链接或 Mac 界面验证；发布所用两个 Mac 架构的实际构建和测试状态，以同一版本标签的 GitHub Actions 与 Release 为准。

自动检查覆盖跨平台 Rust 逻辑、Bridge 模拟宿主中的启动请求、LR 命令引用和配置迁移、应用包信息。Mac 构建脚本还检查 dylib 加载、UTF-8 路径、新本地显影 ABI，以及无私人照片依赖的合成 DNG 显影与参数往返；这些检查不代替真实 ARW/NEF/DNG 相机覆盖、GPU 界面和 Adobe 宿主验收。

工作流还会自动打开实际 RAW 编辑窗口，以合成 DNG 完成适屏显影、曝光/WB/七项调整、全尺寸和保存，检查 XMP 与回执，并生成四张 GPU 窗口截图。截图和日志在每个架构的 `validation` Artifact 中供核对；该自动流程不代替 Finder 双击、手动交互、多窗口及 Adobe 宿主验收。

首次对外发布前，需在 Apple Silicon 和 Intel Mac 上逐项验收：

- Finder 双击启动、中文显示、Retina 清晰度、原生选择对话框、取消选择与拖入。
- 使用中文、空格、单引号和 `$` 的文件夹，打开 ARW／CR2／NEF／JPEG；比较缩略图、适屏与 100% 解码、方向及元数据。
- Command 多选、Command+F、裁切与撤销／重做；重启后自定义比例仍在。
- Bridge 启动后窗口位于前台，菜单和照片清单正确。
- Lightroom 启动不阻塞，退出同步星级、色标、旗标、曝光、裁切和旋转，其他显影参数不变。
- APFS 大小写敏感卷、外置磁盘、同名冲突和只读目录；确认文件整理不覆盖照片。
- 在未安装开发工具的 Mac 上运行发行包，确认不依赖构建机或 Homebrew 目录。

本次实现未在 Mac 桌面和 Adobe 宿主上完成上述人工验收，发行包需经 Mac 构建工作流成功后才可交付使用。
