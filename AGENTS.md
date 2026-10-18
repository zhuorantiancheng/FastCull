<!-- study-root-pointer:start -->
## Study 库根约定（必须遵守）

本工作区位于 `Study` 同步库内。会话开始时先读取并遵守库根 `AGENTS.md`：从当前目录向上找到名为 `Study` 的库根目录（本机已知位置 `E:\documents\Study\AGENTS.md`），把它作为本文件的上级约定叠加执行。库根文件包含跨机器路径规则、`.agent-temp/` 临时目录约定和 `.agents/memory/` 项目记忆协议。
<!-- study-root-pointer:end -->

# AGENTS.md

<!-- agent-memory-protocol:start -->
## 项目记忆（跨 agent 共享）

- 本项目记忆统一放在 `.agents/memory/`，入口是 `MEMORY.md`；任何 agent 进入本项目先读索引。
- 新事实、决定、坑写 `.agents/memory/<主题>.md`，同时更新 `MEMORY.md`；修正既有条目时保留原内容并追加修订记录。
- 条目格式：一句事实 + 适用条件 + 证据（日期、路径）+ 更新人（agent 名）和时间。密钥只记位置，不记值。
- 不要新建 `.dsh/memory/`、`.Codex/memory/`、`.zcode/memory/`、`.claude/memory/` 副本。
<!-- agent-memory-protocol:end -->

