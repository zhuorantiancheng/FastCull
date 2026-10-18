# 项目记忆索引

本目录由 DSH、Codex、ZCode、Claude Code 共用；每条记忆一个文件，本文件只放索引。

格式：- [主题](文件) — 一句话（YYYY-MM-DD，来源）

- [project-overview](project-overview.md) — FastCull 是 Rust + egui 的 RAW/JPEG 选片器，2.7.4，Windows 分发 + macOS 待验收（2026-10-18，DSH）
- [build-and-test](build-and-test.md) — Rust 工具链在 `.agent-temp/2026-10-12-rust/toolchain/`，构建需设 CARGO_HOME/RUSTUP_HOME/TARGET_DIR（2026-10-18，DSH）
- [workspace-acl](workspace-acl.md) — 工作区曾因缺 WRITE_OWNER 导致 pwsh 无法启动，2026-10-18 已用 acl 诊断脚本修复（2026-10-18，DSH）
- [uncommitted-work](uncommitted-work.md) — 工作区存有"载入已有裁切"的 8 个未提交文件，Lua 侧已在 HEAD（2026-10-18，DSH）
- [performance-tuning](performance-tuning.md) — 2026-10-18 实测基线、ColorImage 零拷贝、FastThumb、12 线程三项改动（2026-10-18，DSH）

## 归档（迁移自旧 agent 目录，2026-10-17）

<!-- migrate-agent-memory:2026-10-17 -->

- [photocheck-xmp-feature](_archive/zcode/photocheck-xmp-feature.md) — 来源 zcode，已归档（2026-10-17）
- [rea-mcp-win-env](_archive/zcode/rea-mcp-win-env.md) — 来源 zcode，已归档（2026-10-17）
- [rust-port-plan](_archive/zcode/rust-port-plan.md) — 来源 zcode，已归档（2026-10-17）
- [sony-sdm-exraw-re](_archive/zcode/sony-sdm-exraw-re.md) — 来源 zcode，已归档（2026-10-17）
- [sony-viewer-xmp-rating](_archive/zcode/sony-viewer-xmp-rating.md) — 来源 zcode，已归档（2026-10-17）
- [viewer-raw-speed](_archive/zcode/viewer-raw-speed.md) — 来源 zcode，已归档（2026-10-17）
- [vision-use-flash-subagent](_archive/zcode/vision-use-flash-subagent.md) — 来源 zcode，已归档（2026-10-17）
