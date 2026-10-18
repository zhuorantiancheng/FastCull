---
name: viewer-raw-speed
description: Imaging Edge Viewer 加载 RAW 快的机制结论（内嵌 JPEG 路径）与逆向/基准证据
metadata:
  node_type: memory
  type: project
  originSessionId: sess_3aba2f48-0d61-4bc3-923d-f43e01375104
---

Imaging Edge Viewer（4.1.01）浏览加载 ARW 快的机制（2026-10-12 验证）：

- **ARW 内嵌三层图像**（a6700 实测 t01 样片）：160×120 缩略图、1616×1080 预览（IFD0 JPEGInterchangeFormat）、**6192×4128 全尺寸机内渲染 JPEG（4.16MB，第二个 IFD 链 @0x84000）**；RAW strip 为 37.4MB 14bit Sony Lossless（6656×4608）。扫描出的 256×256"JPEG"是压缩 RAW 字节误报。
- **Viewer.exe 静态链接 libjpeg-turbo 2.1.5.1**（字符串 'libjpeg-turbo version 2.1.5.1 (build 20230601)'@0x140594478 + 完整 libjpeg 错误表字符串），渲染走 GDI+ `GdipCreateBitmapFromScan0`+`GdipDrawImageI`。
- **官方配置项 'OpenJPEGFileAtFirst' / 'OpenTIFFFileAtFirst' / 'OpenHEIFFileAtFirst'**（注册于 FUN_1400292a0/FUN_140029870，L"宽字符"全局设置表）——"优先以 JPEG 打开文件"是产品设计。
- RAW 解码器 FUN_140150f20 的调用链全部来自编辑/导出/SDM/放大等重操作（经 FUN_140147180 包装，上层是 FUN_14007d4c0 等命令执行层）。
- **基准**（本机 zrtc）：内嵌 JPEG 解码 79ms vs LibRaw 全 RAW 解码+demosaic ~1300ms（16×），IO 量 4.2MB vs 37.4MB（9×）。
- LjCore.dll = "sony_lajiao" 远程控制 SDK（Remote 用）；sonyhevd.dll = HEVC 解码，与浏览显示无关。

**Why:** 解释 IEDT 快于 LR/ACR 的根因是"浏览显示走机内 JPEG，LR/ACR 必须自渲染全管线"；后续涉及 Viewer 显示/性能问题可直接引用。
**How to apply:** 相关逆向继续用 [[sony-sdm-exraw-re]] 的 Ghidra 工程（E:\APP\ghidra-ws\viewer_sdm，注意工程实际在 viewer_sdm\viewer_sdm.gpr，pyghidra 用 open_project+program_context，headless analyzeHeadless.bat 不带 PyGhidra 不能跑 .py）。基准脚本在 IEDT/.agent-temp/2026-10-12-raw-speed/。

**主图空白真凶（2026-10-12 终版定位）**：`_build_ui` 创建 loupe 画布后**没有 pack**（只有 `_enter_grid/_enter_loupe` 切换时才 pack，默认视图没人调）→ 画布 1x1 从未显示，画面上深灰是背景面板；测试 `find_all()>0` 断言了画布对象而非可见性（盲区）。教训：①tkinter 控件"创建≠显示"，默认视图的控件必须在 build 时布局；②可见性断言要查 `winfo_width()>100`；③tkinter 回调异常默认静默（分离进程 stderr 也丢），必须挂 `root.report_callback_exception` 进日志——本轮靠 loupe drawn 打点日志（canvas=1x1）才定位。

**解码队列教训（2026-10-12 实机：340 张大库）**：单 worker FIFO 池里当前张解码排在预取队尾 → 等数秒。修复=当前张提交前 cancel 排队中的预取 futures + pending.clear()。Rust 版要做成优先队列（按距当前 idx 距离）。

**日志系统（2026-10-12 加）**：统一 `%LOCALAPPDATA%\FastCull\logs\`；选片器 fastcull_app_*.log（每启动一份保留 20，GUI 崩溃钩子弹窗告知路径、headless 崩溃只写日志+stdout 后 os._exit——**无人值守路径绝不能弹窗**，曾因弹窗挂死 headless；os._exit 前必须 logging.shutdown() 刷新缓冲）；Lua 端 fastcull_plugin_YYYYMMDD.log（Common.log 全 pcall；取目录用 getStandardFilePath('localAppData')+LrFileUtils.createDirectory）；选片器 L 键开日志目录。

**LR SDK 坑（2026-10-12 实机踩中，逐个暴露）**：①取插件偏好设置的正确 API 是 `LrPrefs.prefsForPlugin()`（可无参调用），`prefsForApp` 不存在；②LR Lua 沙箱里 **`os.getenv` 不存在**（os 表受限），取临时目录要用 `LrPathUtils.getStandardFilePath('temp')`（pcall 包裹 + fallback `_PLUGIN.path`）；③`io.popen`/`io.open` 可用但应先存在性检查；④**旗标的 rawMetadata 键名是 `pickStatus`（SDK v4.0+），不是 `pick`**（社区笔记的 'pick' 是错的，报 Unknown key；官方手册 lrc.mcor.dev/modules/LrPhoto.html 确认）：读 `photo:getRawMetadata('pickStatus')`、写 `photo:setRawMetadata('pickStatus', 1|0|-1)`（1=pick，0=无，-1=reject，写在 withWriteAccess 内）。仍用 pcall 防御旧版本。

**已交付选片工具 v2**（2026-10-12，LR Classic 风格 UI）：IEDT/arw_culler.py（tkinter：放大/网格视图、底部工具栏[旗标/旋转/星/5色/缩放/导航]、胶片带；键位 P/X/U、0-5、6-9+点击紫、[ ]旋转、G/D/E、F/点击缩放。XMP 桥接：xmp:Rating/xmp:Label 标准 + sr:Flag=pick|reject 旗标 + tiff:Orientation 旋转。竖拍靠 ARW orientation transpose，内嵌 JPEG 无 EXIF 方向）+ IEDT/lrplugins/FastCull.lrplugin（2026-10-12 定名 "FastCull for ARW"，identifier com.zrtc.fastcull-arw，**分发自包含**：arw_culler.py 副本在插件目录内、Python 自动探测 py -3→python→miniconda（用户要求不绑本机路径）、缺 Pillow 时 --open 回报 launched=0 带安装提示、cmd_open 对 stdout reconfigure(utf-8,replace) 防中文文件名崩溃、OpenCuller 失败对话框透出 python 输出摘要。OpenCuller.lua：LR 评分+旗标经 %TEMP% 状态文件 path|rating|label|flag 落旁车后 python `--open` 分离启动 GUI；SyncSelected.lua：旁车→目录批量同步含 pickStatus 旗标。label/flag 空串=清除、字段缺失=不动。cmd.exe 管道链路复现测试通过）。测试：无头/GUI 冒烟截图自检/lupa 模拟 SDK/真实分离存活全过。注意本会话 Bash 工具会在 shell 退出时清理子进程树，分离进程存活验证须在命令内轮询完成。用法见 IEDT/README-culler.md。关联 [[sony-viewer-xmp-rating]]。
