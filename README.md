# FastCull

轻量 RAW/JPEG 选片器，使用 Rust 开发，可独立运行，也可从 Adobe Bridge 和 Lightroom Classic 打开。2.8.0 提供 Windows 安装与注册包，以及 macOS Apple Silicon / Intel 独立构建包；Mac 桌面与 Adobe 宿主的人工验收范围见下文。

A fast RAW/JPEG photo culler with local RAW development, Adobe Bridge and Lightroom Classic integration. Windows installer and separate Apple Silicon / Intel macOS packages.

macOS 的构建、安装和验证范围见 [macOS 说明](docs/MACOS.md)。下述安装包和快捷键说明针对 Windows。

**2.8.0 新增独立 RAW 编辑窗口**：支持 ARW/NEF/DNG，使用本地 LibRaw 显影和 sRGB 预览，编辑曝光、白平衡及七项基础调整，支持灰点吸管估计并保存 XMP。无需安装或启动 Adobe 软件，主选片窗口可继续操作。本地色彩模型与 LR/ACR 的像素结果存在差异，DCP Look、Adobe 曲线及蒙版等保留在 XMP 中；用法、性能策略和验证边界见 [RAW 编辑说明](docs/RAW-EDITING.md)。2.7.x 安装包不包含这套新增功能。

## 下载与安装

在本仓库的 [Releases](https://github.com/zhuorantiancheng/FastCull/releases) 页面下载：

- **FastCull-Setup-版本-windows-x64.exe**：推荐。安装程序可同时注册 Bridge 和 Lightroom Classic 插件。
- **FastCull-Portable-版本-windows-x64.zip**：免安装，解压到固定目录后运行 `FastCull.exe`；需要 Adobe 集成时运行 `register-adobe.cmd`。
- `SHA256SUMS.txt`：下载文件的 SHA-256 校验值。`FastCull-Source-版本.zip` 是供开发者使用的源码。
- macOS 分别下载 `FastCull-版本-macos-arm64.zip`（Apple Silicon）或 `FastCull-版本-macos-x86_64.zip`（Intel），各自有 `.sha256`。包内 `scripts/install-macos.command` 可安装应用并注册 Bridge/LR；仅独立使用可直接复制应用。Mac 包仅做 ad-hoc 签名，未做 Apple 公证。

需要 Windows 10/11 x64，以及支持 OpenGL 3.3 的显卡驱动。用户无需安装 Rust、Python、LibRaw、PowerShell 7 或 Imaging Edge。当前安装包未做代码签名，Windows 可能显示“未知发布者”；请核对下载来源及校验值。

安装前退出 FastCull、Bridge 和 Lightroom Classic。安装程序默认仅安装给当前 Windows 用户，不需要管理员权限。安装界面目前为英文，选片器及使用说明为中文。

### Adobe Bridge

先至少启动过一次 Bridge，让它建立用户配置目录。安装时保留 **Register / update Adobe Bridge integration** 勾选。安装程序会为检测到的 `Bridge 20xx` 用户目录注册脚本。重新打开 Bridge，允许 FastCull 启动脚本；也可在“编辑 → 首选项 → 启动脚本”中启用 FastCull 后重启。

选中照片或文件夹，在右键菜单中选择 FastCull，或按 **Ctrl + Alt + K**。快捷键仅在宿主应用位于前台时生效。首次安装 Bridge 或升级到新年份版本后，重新运行 FastCull 安装程序注册即可。

### Lightroom Classic

安装时保留 **Register / update Lightroom Classic integration** 勾选，插件会安装到 `%APPDATA%\Adobe\Lightroom\Modules\FastCull.lrplugin`。重启 Lightroom Classic，在“文件 → 增效工具管理器”确认 FastCull 已启用。如果未自动显示，点击“添加”并选择上述目录。

选中照片，按 **Ctrl + Alt + K**，或使用“图库 → 增效工具额外命令”中的 FastCull 入口。LR 的原生照片右键菜单没有开放可自由扩展的插件接口，因此使用菜单及快捷键。仅支持 Lightroom Classic，不支持云端 Lightroom。

已有手动安装版用户应先在增效工具管理器移除旧的 FastCull 注册，再使用新目录，避免同一插件重复注册。旧的内置程序路径会自动迁移；用户手动指定的自定义程序路径仍保留。

## 使用

独立打开程序后选择照片或文件夹，或将文件/文件夹拖入窗口。支持 **Sony ARW、Canon CR2、Nikon NEF、DNG、JPG/JPEG**；自动跳过其他类型。DNG 优先按 TIFF 索引直接读取内嵌 JPEG，网格选小预览、适屏选大预览，避免普通浏览时解码传感器数据；无可用 JPEG 时回退 LibRaw。预览尺寸由机型和拍摄设置决定，不保证与 RAW 传感器分辨率相同，100% 查看会检查有效输出尺寸。详见 [DNG 支持与性能](docs/DNG.md)。

- **E / G / C**：单图、网格、对比。左右方向键切图；缩略图栏内滚轮快速切图。
- 支持星级、旗标、颜色及拍摄元数据筛选；支持曝光快速调整和双图独立/联动查看。
- 100% 查看支持点击位置放大，切图保留相对查看位置。
- **T / 裁切**：自由与固定比例裁切、自定义比例预设、角度微调及 90° 旋转；编辑结果保存在 XMP，移动照片时一起携带。详见 [裁切说明](docs/CROPPING.md)。
- **RAW 编辑**：在独立窗口调整曝光、白平衡、对比度、高光、阴影、白色、黑色、自然饱和度和饱和度；先显示快速预览，再后台加载适屏显影，全尺寸按需加载。灰点吸管给出可微调的估计值，保存后可通过 XMP 与 LR/ACR 交换参数。
- 载入已有裁切：LR 插件直接读取目录中的裁切与旋转（无需预先保存 XMP）；Bridge/独立打开读取 RAW 旁车或 JPEG 内嵌 XMP 中已保存的裁切。
- 从 LR 打开后选择 **同步全部并关闭**，自动同步本次修改的星级、色标、旗标、曝光、白平衡、基础调整、裁切和旋转；失败保留重试，其他显影设置不变。详见 [退出同步说明](docs/LR-SYNC.md) 及 [RAW 编辑说明](docs/RAW-EDITING.md)。
- 退出时可确认同步评分。Bridge/独立版使用 RAW 旁车 XMP 或 JPEG 内嵌 XMP；LR 会话可将修改同步回目录。
- Bridge/独立版可实际移动照片；LR 使用整理计划，由 LR 原生完成文件移动，以保留编辑历史、集合和虚拟副本。

主选片窗口的快捷曝光预览基于内嵌 JPEG；独立 RAW 窗口使用 LibRaw 与本地 GPU 色调模型。保存编辑不重新编码 RAW 图像数据；DNG 的显影保存会更新其内嵌 XMP 及文件哈希。

## 升级和卸载

关闭 Adobe 应用及 FastCull，运行新版安装程序并使用原目录。未勾选某个集成组件不代表移除已有注册；要移除本安装器管理的集成，请卸载 FastCull。

安装版在 Windows“已安装的应用”中卸载；便携版先运行 `unregister-adobe.cmd`，然后自行处理程序目录。卸载只清理仍属于本次安装的插件文件和注册项，并尝试恢复安装前的文件；已被用户修改的插件文件会保留。照片、XMP、用户会话及恢复备份不清理。

安装版和便携版共用当前用户的集成记录，同一用户请只保留一个注册来源。切换发行方式前先卸载/取消旧注册。便携版注册后不要移动所在目录；需要移动时先取消注册，移动后重新注册。

## 构建与发布

源码、归档分支与日常提交方式见 [Git 版本管理](docs/VERSION-CONTROL.md)。安装包和编译缓存不纳入 Git。

见 [Windows 分发与 GitHub 发布](docs/DISTRIBUTION.md)。开发环境需要 Rust 1.90.0、Visual Studio C++ Build Tools、Python 3.11+ 和 Inno Setup 6；这些不是最终用户运行依赖。

FastCull 源码按 [MIT](LICENSE) 许可发布。第三方依赖遵守各自许可证，发行包提供 `THIRD-PARTY-NOTICES.txt`。LibRaw 按 CDDL 1.0 选项链接，发行包附上完整上游源码及适配层重建脚本。
