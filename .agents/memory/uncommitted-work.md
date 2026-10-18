# 未提交改动：载入已有裁切（2.7.5 候选）

事实：截至 2026-10-18，Git 工作区有 8 个已跟踪文件未提交，内容是"载入 Lightroom 目录 / XMP 中已有裁切与方向"：`src/ui.rs`（+157 行，develop_pending / receive_develop / 裁切等待逻辑）、`src/exposure.rs`（9 列基线读取与测试）、`tests/cropping.rs`、`tests/organizing.rs`、`tests/test_bridge.py`、`scripts/crop_smoke.py`（`--import-source`）、`README.md`、`docs/CROPPING.md`、`docs/LR-SYNC.md`；另有未跟踪的 `AGENTS.md`、`.agents/`（项目记忆协议，2026-10-17 加入）。

适用条件：准备提交这批改动、继续 2.7.4 之后的开发或发布新版本时。Lua 侧 `develop.lua` 的 9 列基线格式已在 `ad32984`（2026-10-14 23:34）提交，Rust 侧读取与测试仍只在工作区——单独拉取 HEAD 的 Rust 代码无法完整消费新格式。

证据：`git diff --stat`（8 files, +280/-9）；`git log -3`（main 最新 `9869800`，2026-10-16 Bridge 启动修复）；验证记录 `.agent-temp/2026-10-14-import-crop/`（137 Rust + 59 Lua 测试通过、Clippy 无警告、release 构建成功、接触表截图 `native-lr-contact.png` / `native-xmp-contact.png`）。本轮未运行测试，未提交任何改动。

更新人：DSH（deepseek-flash），2026-10-18。
