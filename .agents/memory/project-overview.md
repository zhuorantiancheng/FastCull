# FastCull 项目概况

事实：本项目是 FastCull，Rust + egui 0.33 的 RAW/JPEG 快速选片器（Cargo.toml `name = "fastcull"`，当前版本 2.7.4）。核心机制是只读 RAW 内嵌的全尺寸 JPEG 做浏览，评分/色标/旗标/曝光/裁切写入 XMP 旁车（RAW 同名 .xmp 或 JPEG 内嵌 XMP），不重新编码 RAW 像素。入口有三类：独立 exe（direct）、Lightroom Classic 插件（Lua）和 Adobe Bridge 脚本（ExtendScript + 助手进程）。

适用条件：接手或修改本项目、需要判断技术栈、入口和分发范围时先读本条。

证据：`Cargo.toml`、`README.md`、`README-culler.md`、`DEVLOG.md`（Python 原型记录）、`RUST-PLAN.md`、`docs/` 目录。2026-10-18 由 DSH 通读确认。

补充结构（2026-10-18 核对）：
- `src/` 29 个 .rs，`src/ui.rs` 约 5054 行是 GUI 核心（App/Photo/裁切编辑器/对比视图），其余按领域拆分：`arw.rs`、`jpeg.rs`、`imaging.rs`、`cache.rs`、`xmp.rs`、`crop.rs`、`exposure.rs`、`session.rs`、`handoff.rs`、`organizing.rs`、`relocating.rs`、`native_raw.rs`。
- `native/`：LibRaw 适配层 `raw_bridge.cpp`、JPEG 缩放 `jpeg_scaled.cpp`。
- `lrplugins/FastCull.lrplugin/`：LR 插件（Lua）与随插件程序；`bridge/FastCull.jsx` + `scripts/install-bridge.ps1`：Bridge 入口；`installer/FastCull.iss`：Inno Setup。
- 支持格式：Sony ARW、Canon CR2、Nikon NEF、JPG/JPEG；macOS 有适配源码与构建脚本，待真机验收（`docs/MACOS.md`）。
- 验证体系：`tests/` 下 Rust 集成测试 + Python/Lua 合同测试 + `docs/*-validation-*.json` 证据文件，正式发布走 `scripts/release.py`。

更新人：DSH（deepseek-flash），2026-10-18。
