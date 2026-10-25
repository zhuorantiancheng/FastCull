# RAW 编辑与 Adobe 配方同步

事实：Windows 的按需 RAW 编辑工作区支持 ARW/NEF/DNG，调用本机 Photoshop/Camera Raw，经完整 XMP/相机配置文件显影后转换 sRGB；曝光/白平衡可编辑，普通选片不启动 Adobe。运行需已激活 Photoshop/Camera Raw 和 PowerShell 7，不依赖 Python/ExifTool；脚本内嵌 exe。
适用条件：要求与 LR/ACR 配方一致的手动显影；不能把普通选片的内嵌 JPEG/LibRaw 近似预览称作 Adobe 完整显影，macOS 此功能尚未实现。
证据：2026-10-24，`src/raw_editor.rs`、`src/adobe_raw.rs`、`bridge/FastCullRawRender.jsx`、`scripts/adobe-raw-render.ps1`、`docs/RAW-EDITING.md`。
更新人和时间：Codex，2026-10-24。

事实：LR 进入 RAW 编辑时按需 ORIGINAL 导出完整未落盘配方及相对资源，保存仅合并完整 CRS 显影范围并保留原片非显影元数据；源片无明确方向时可带入 LR 的方向。保存 ARW/NEF 只写旁车，DNG 另同步内嵌 XMP，部分失败可重试。XMP/RAW 外部冲突拒绝覆盖；LR 白平衡原子修订使用向后兼容 V4 协议和读回确认。旧 PV 5.0/5.7 用 Exposure±4 并关闭 AutoExposure，现代版本 Exposure2012±5，保留 ProcessVersion。
适用条件：维护保存、会话同步和资源文件；LR 原生宿主完整导出/同步尚未真机端到端验收，严格 SDK 模拟不能替代它。DNG 文件哈希会因显影元数据保存改变，但压缩图像数据不重编码。
证据：2026-10-24，`src/xmp.rs`、`src/handoff.rs`、`lrplugins/FastCull.lrplugin/raw_settings.lua`、`develop.lua`、`photo_edits.lua`、`tests/raw_develop.rs`、`tests/test_raw_settings.py`、`docs/raw-edit-validation-2026-10-24.json`。
更新人和时间：Codex，2026-10-24。

事实：最终184项Rust、18项Bridge、69项LR同步模拟、12项完整配方导出模拟、9项分发验证通过，另跳过1项可选GUI；Clippy全target无警告。真实ARW/NEF/PureRAW DNG release窗口完成调整/全尺寸/保存，生成12张GPU截图；2560px重新显影单次约2.4–3.2秒，相同配方缓存约32–52毫秒，DNG6192×4128约5.5秒（Photoshop预热，含PNG解码，不含纹理上传）。用户原片三张最终SHA不变，DNG保存副本的三段压缩图像哈希不变。新exe位于插件目录，历史安装包未重建。
适用条件：仅本机、这些照片、此版本验证；Photoshop冷启动可能数十秒。构建日志/副本/截图在`.agent-temp/2026-10-23-raw-edit/`，运行不依赖它。源码/安装白名单必须携带raw_settings.lua与内嵌用adobe-raw-render.ps1/JSX。
证据：2026-10-24，`docs/raw-edit-validation-2026-10-24.json`、`scripts/release.py`、`scripts/register-integrations.ps1`。
更新人和时间：Codex，2026-10-24。


## 2026-10-24 最新修订：本地 LibRaw、完整基本调节与独立窗口

事实：用户明确要求不用 Adobe 软件显影；当前 RAW 编辑已切换为本地 LibRaw 0.22.2 相机/DNG 矩阵到 sRGB，曝光/WB 在传感器阶段处理，对比度/高光/阴影/白色/黑色/自然饱和度/饱和度七项由 GPU 处理，并有灰点白平衡估计。此前 Adobe 后端说明与 raw-edit-validation 报告仅为历史，不能描述当前运行程序。
适用条件：ARW/NEF/DNG，Windows 当前构建；XMP 配方完整保留且字段可互读，但不重现 Adobe DCP Look、曲线、蒙版和专有 Tone；本地 Tint 标度与 Adobe 不严格等价，不宣称像素一致。
证据：2026-10-24，`src/local_raw.rs`、`src/raw_editor.rs`、`src/raw_preview.rs`、`src/raw_sample.rs`、`native/raw_bridge.cpp`、`docs/RAW-EDITING.md`。
更新人和时间：Codex，2026-10-24。

事实：RAW 编辑使用独立进程原生窗口，主窗口不切换工作区；读取、显影、PNG 解码和保存均在后台。先内嵌快预览，再 1600px 适屏显影，显式请求全尺寸；曝光/WB 250ms 去抖、最新请求和取消，单图 unpack 缓存复用；本地取消约115ms返回。主进程按序接收每次保存的独立回执，仅合并明确修改字段，LR 会话关闭等待子窗口回执；跨进程 LR 配方邮箱加锁。
适用条件：同一照片重复打开会提示，主窗口评分等其它修改不被旧回执覆盖；RAW/XMP 被外部改动会阻止旧草稿保存，需要重新加载；真实 LR 宿主端到端未验收。
证据：2026-10-24，`src/raw_window.rs`、`src/raw_gate.rs`、`src/ui.rs`、`src/session.rs`、`src/handoff.rs`、`docs/raw-window-validation-2026-10-24.json`。
更新人和时间：Codex，2026-10-24。

事实：DNG 显影保存改为纯本地 atomic embed：同目录临时副本保留旧载荷位置，追加 XMP700/新IFD0 后原子替换；Classic TIFF 有界验证，损坏、BigTIFF、>32bit、只读文件拒绝，部分失败可重试。ARW/NEF 仅写旁车。
适用条件：明确点击保存会改变 DNG 元数据与文件 SHA，不重编码图像；普通选片评分仍用旁车。三张当前用户 DxO 输出仅只读拷贝测试，写入私有副本，实时原片测试前后 SHA 一致；其中两张在本次22时验收之前21:04已有元数据变化，与10/23历史整文件SHA不同，但三张图像载荷哈希与历史相同。
证据：2026-10-24，`src/dng.rs`、`src/raw_editor.rs`、`docs/raw-window-validation-2026-10-24.json`。
更新人和时间：Codex，2026-10-24。

事实：214项Rust通过（另2项忽略，其中真实DNG测试单独通过）、Bridge19、LR同步74通过/1可选GUI跳过、完整配方导出12、分发9通过，Clippy全target无警告；最终release完成三格式加三张当前DxO副本共6组独立窗口保存、24张GPU截图，主窗口均可网格/切图，未新增Adobe进程，关闭编辑器后主窗口保持运行；原生24组参数往返一致。程序与新DLL已复制到 `lrplugins/FastCull.lrplugin/`，现有LR偏好与Bridge入口指向此目录，LR插件需重新载入才能使用新Lua同步逻辑。
适用条件：性能仅这些照片、本机release与OS缓存；内嵌快预览16–103ms不含进程启动与纹理上传；不得将先前debug慢速基准当作发布版性能。交付程序不依赖 `.agent-temp/2026-10-24-raw-window/`；历史安装包未重新发布。
证据：2026-10-24，`docs/raw-window-validation-2026-10-24.json`、`docs/RAW-EDITING.md`，EXE SHA `05e56b084b31213d60eacf17f338723c5be302f22ad6242883192884d01d9809`，DLL SHA `029e0892508cc421d579cbf897bfe99379b1fb3054cd1e975c18ba822effeda5`。
更新人和时间：Codex，2026-10-24。

事实：七项基础控制已逐项经过真实GPU截图回归，旧饱和度加法公式在 Saturation=-100/Vibrance=+100 时通道差64，修复为乘法后通道差0，七项各自+80均改变像素，中性基线逐像素一致。
适用条件：本地GPU算法验证，不表示Adobe算法等价；临时harness直接引用正式raw_preview.rs，11张截图，测试窗口退出0。
证据：2026-10-24，`src/raw_preview.rs`、`docs/raw-window-validation-2026-10-24.json`，临时GPU证据位于`.agent-temp/2026-10-24-raw-window/backend/gpu/screenshots/`。
更新人和时间：Codex，2026-10-24。
