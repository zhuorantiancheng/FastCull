# DNG 支持与 DxO PureRAW 验证

事实：当前 FastCull 2.7.7 工作区已新增 DNG，独立/Bridge/LR 入口一致放行；有界随机读取 TIFF/SubIFD，按实际像素选择 JPEG，内嵌小 JPEG/RGB8 经 FastThumb 占位，100% 保留 LibRaw 有效尺寸检查及传感器回退。
适用条件：维护 DNG 读取、网格响应、格式过滤及解码级别时；不能把 FastThumb 的低清占位耗时当全尺寸性能。
证据：2026-10-23，`src/dng.rs`、`src/arw.rs`、`src/imaging.rs`、`src/ui.rs`、`docs/DNG.md`。
更新人和时间：Codex，2026-10-23。

事实：DNG 无旁车时读取内嵌 XMP，首次保存以内嵌包为模板建立同名 XMP，保留 DxO/未知字段，后续旁车优先，不改写 DNG；Adobe 宿主读取 DNG 外置旁车未真机验收，LR 使用插件同步接口。
适用条件：评分、曝光、裁切、元数据保存与 Adobe 同步；不要声称旁车等价于修改 DNG 内嵌元数据。
证据：2026-10-23，`src/xmp.rs`、`tests/multiformat.rs`、`docs/FORMATS.md`。
更新人和时间：Codex，2026-10-23。

事实：本机以用户授权的三张 PureRAW 输出副本（74–93 MB）和三种公开 DNG 完成测试，148 项 Rust、18 项 Bridge、58 项 Lua 通过（Lua 另跳过 1）；Clippy 无警告。PureRAW FastThumb 193×129 为 0.089–0.106 ms，200 px 为 35–50 ms，6192×4128 Full 为 73–95 ms，均是预热 CPU 解码、不含显卡上传；源文件哈希复核不变。可执行程序已复制至插件目录，旧分发包未重打包。
适用条件：同一本机本次样片基线，不能推广到全部 DNG/PureRAW 输出，低分辨率预览的其他样片在 Full 可需 1–2 秒传感器解码。
证据：2026-10-23，`docs/dng-validation-2026-10-23.json`、`docs/DNG.md`；可重建日志/样片/截图在 `.agent-temp/2026-10-23-dng/`，运行产物不依赖临时目录。
更新人和时间：Codex，2026-10-23。

事实：Lua 合同测试依赖环境变量 CARGO_TARGET_DIR 定位本次 debug exe；未设置会寻找根 target 并报 WinError 2。沙箱禁止 mklink /J 时需在本任务临时目录补跑目录联接测试；本次完整回归已通过。小于 2 MP 的 JPEG 走 Rust 解码，避免 WIC 固定开销；852×640 样片 Thumb 15 次中位数 8.22→2.87 ms。
适用条件：本机测试和性能复现；工具链变量继续按 build-and-test 条目设置，CARGO_INCREMENTAL=0 可避免沙箱硬链接缓存警告。
证据：2026-10-23，`tests/test_bridge.py`、`src/imaging.rs`、`docs/DNG.md`；本次脚本 `.agent-temp/2026-10-23-dng/build.ps1`。
更新人和时间：Codex，2026-10-23。

## 2026-10-24 修订

事实：新增 RAW 编辑工作区在用户明确保存时同步 DNG 内嵌显影 XMP；上文不改写 DNG 只描述普通选片评分路径。适用条件：DNG 显影保存与 Adobe 读取。证据：docs/RAW-EDITING.md、docs/raw-edit-validation-2026-10-24.json。更新人和时间：Codex，2026-10-24。

## 2026-10-24 Bridge 实际安装脚本修订

事实：工程支持DNG并不代表Bridge已加载新版脚本；本机Bridge 2025/2026 Startup Scripts仍为2.7.4（2026-10-16安装），过滤表达式没有dng，单选DNG会在第52行触发“读取所选照片：请先在Bridge中选中RAW/JPEG照片或包含照片的文件夹”。本次通过install-bridge.ps1 -SkipShellRegistration备份更新到2.7.7，保留exe路径，不改照片或默认文件关联；Bridge完全退出重开后才读取新的启动脚本。
适用条件：用户从Bridge打开DNG但程序尚未启动，或仓库测试通过而实际入口失败时，先比较已安装Startup Scripts的内容/版本，不只检查仓库文件。
证据：2026-10-24，tests/bridge_entry.test.cjs新增FASTCULL_BRIDGE_SCRIPT参数和DNG-only完整open路径回归，旧安装脚本确定复现相同错误，新源码及两份实际安装脚本各19项通过；可重建证据在.agent-temp/2026-10-24-bridge-dng/{installed-before.log,installed-checks.json,Bridge-2025-after.log,Bridge-2026-after.log}。安装脚本有UTF-8 BOM，比较内容需解码后比较；Node harness也先去BOM再去#target。
更新人和时间：Codex，2026-10-24。
