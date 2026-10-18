---
name: rust-port-plan
description: FastCull 最终要用 Rust 重写（Python 只是功能原型），接口协议以 RUST-PLAN.md 为准
metadata:
  node_type: memory
  type: project
  originSessionId: sess_3aba2f48-0d61-4bc3-923d-f43e01375104
---

2026-10-12 用户定下的方向：**Python 版选片器只是功能性验证原型，功能验证通过后用 Rust 重写**（单 exe、零依赖分发、翻片 <10ms）。开发期间要提前规划。

**Why:** Python 启动慢（1-2s 冷启动）、翻片 150-200ms、分发依赖目标机 Python 环境；用户明确要求不绑本机环境（分发场景）。

**How to apply:**
- **开发全程记录在 `IEDT/DEVLOG.md`（2026-10-12 整理，Rust 重构的主输入）**：需求演化、最终 UI/键位/配色规格、12 条踩坑清单（每条含 Rust 对策）、架构、测试体系、已知限制。重构开工先读 DEVLOG 再读 RUST-PLAN。
- 完整规划在 `IEDT/RUST-PLAN.md`（冻结的接口协议 §2：CLI 子命令+RESULT 行、状态文件 path|rating|label|flag 语义、XMP 四字段 patch、ARW 内嵌 JPEG 定位事实；crate 选型：zune-jpeg + fast_image_resize + egui；里程碑 M1 无头核心对拍 → M4 集成分发）。
- Python 原型期间任何接口改动必须同步 RUST-PLAN.md §2；对拍黄金用例 = .agent-temp/2026-10-12-raw-speed/ 的 test_culler.py/test_state.py（测试 ARW 源在 exraw-test/，跨机要带走）。
- Rust 版 Lua 插件端唯一变化：cullerPath 默认指向插件目录内 fastcull.exe。
- **tkinter 线程教训（Rust GUI 同样适用）**：worker 线程绝不直接碰 UI（Python 版曾因 worker 里 root.after_idle 死锁，改 queue + 主线程 30ms 轮询解决）；UI 循环里同步 IO 要限流渐进（每帧 ≤2 项）。
- 关联 [[viewer-raw-speed]]（含 LR SDK 坑清单：prefsForPlugin/getStandardFilePath/pickStatus 等，Rust 期照用）。
