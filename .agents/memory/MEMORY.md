# 项目记忆索引

本目录由 DSH、Codex、ZCode、Claude Code 共用；每条记忆一个文件，本文件只放索引。

格式：- [主题](文件) — 一句话（YYYY-MM-DD，来源）

- [dng-support](dng-support.md) — DNG 快速预览、RGB8 小图、内嵌 XMP 与三张 DxO PureRAW 实测，148 项 Rust 通过（2026-10-23，Codex）
- [raw-editing](raw-editing.md) — Adobe RAW 工作区、完整 LR 配方、曝光/WB、DNG 内嵌保存、184 项 Rust 与真实三格式窗口验证（2026-10-24，Codex）
- [project-overview](project-overview.md) — FastCull 是 Rust + egui 的 RAW/JPEG 选片器，2.7.4，Windows 分发 + macOS 待验收（2026-10-18，DSH）
- [build-and-test](build-and-test.md) — Rust 工具链在 `.agent-temp/2026-10-12-rust/toolchain/`，构建需设 CARGO_HOME/RUSTUP_HOME/TARGET_DIR（2026-10-18，DSH）
- [workspace-acl](workspace-acl.md) — 工作区曾因缺 WRITE_OWNER 导致 pwsh 无法启动，2026-10-18 已用 acl 诊断脚本修复（2026-10-18，DSH）
- [uncommitted-work](uncommitted-work.md) — 原“载入已有裁切”的未提交文件已于 2026-10-18 在 fae9d98 提交（2026-10-18，DSH）
- [performance-tuning](performance-tuning.md) — 2026-10-18 实测基线、ColorImage 零拷贝、FastThumb、12 线程三项改动（2026-10-18，DSH）
- [release-and-macos](release-and-macos.md) — 2.7.4 包校验通过但落后源码；源码包补 macOS 构建文件；Apple 双架构 cross-check 通过（2026-10-18，DSH）

## 归档（迁移自旧 agent 目录，2026-10-17）

<!-- migrate-agent-memory:2026-10-17 -->

- [photocheck-xmp-feature](_archive/zcode/photocheck-xmp-feature.md) — 来源 zcode，已归档（2026-10-17）
- [rea-mcp-win-env](_archive/zcode/rea-mcp-win-env.md) — 来源 zcode，已归档（2026-10-17）
- [rust-port-plan](_archive/zcode/rust-port-plan.md) — 来源 zcode，已归档（2026-10-17）
- [sony-sdm-exraw-re](_archive/zcode/sony-sdm-exraw-re.md) — 来源 zcode，已归档（2026-10-17）
- [sony-viewer-xmp-rating](_archive/zcode/sony-viewer-xmp-rating.md) — 来源 zcode，已归档（2026-10-17）
- [viewer-raw-speed](_archive/zcode/viewer-raw-speed.md) — 来源 zcode，已归档（2026-10-17）
- [vision-use-flash-subagent](_archive/zcode/vision-use-flash-subagent.md) — 来源 zcode，已归档（2026-10-17）

- [dng-support 补记](dng-support.md#2026-10-24-bridge-实际安装脚本修订) — 本机 Bridge 2025/2026 旧 2.7.4 启动脚本拒绝 DNG；已备份更新为 2.7.7，需重启 Bridge；实际安装脚本各 19 项通过（2026-10-24，Codex）

- [raw-editing 最新修订](raw-editing.md#2026-10-24-最新修订本地-libraw完整基本调节与独立窗口) — 已替换Adobe后端：本地LibRaw/GPU七项调节、灰点吸管、独立窗口、DNG本地内嵌；214项Rust和6组真实窗口验证，现有插件目录exe/DLL已更新；上方Adobe条目为历史（2026-10-24，Codex）
