---
name: rea-mcp-win-env
description: 本机（zrtc 的 Windows）REA MCP 可用能力与大数据结果的处理技巧
metadata:
  node_type: memory
  type: project
  originSessionId: sess_c2794347-ab02-4df6-aced-eb63499f4e65
---

本机（zrtc，Windows）REA MCP 分析能力（2026-10 确认）：

- 可用：`rea-dotnet-static`（inspect_managed_artifact / members / native_boundaries）、`rea-artifact-graph`。
- **Ghidra 12.1.4 已于 2026-10-11 安装**：`E:\APP\ghidra_12.1.4_PUBLIC`，`GHIDRA_INSTALL_DIR` 已用 setx 设为用户级变量；原生反编译器 decompile.exe 在位，Java（Zulu 25）兼容。注意 MCP 服务进程启动时读取环境变量——设置后需重启 ZCode 才生效；若 REA 仍报 not_configured，先确认该环境变量存在。
- 不可用：Hopper / native-macos（仅 macOS）；IDA 适配器需 REA_IDA_MCP_CONFIG（未配置）。
- `inspect_managed_members` 对中型程序集（约 800 方法）的返回会超 10MB MCP 预算报 resource_constraint；补救：按报错里的 evidence_id 用 `export_evidence_bundle` 导出到 `.agent-temp/<日期>-<任务名>/` 再用 Python 本地解析 JSON。
- 原生 PE 导入/导出/签名补充分析用 pefile：`E:/APP/miniconda3/python.exe -m pip install --target <临时目录>/pylibs pefile`（conda 里默认没有 pefile）；无 Ghidra 时的轻量替代是 capstone + 逐字节 rip-relative 模式扫描（线性 disasm 会失步，抓不到引用）。
- GitHub 大文件直连不稳：用 Python urllib 多镜像（github / ghfast.top / gh-proxy.com）Range 续传循环；注意某镜像可能忽略 Range 头返回全量，追加前要校验 Content-Range。

**Why:** 避免下次 REA 分析重新踩 Ghidra 未配置、大响应被拒的坑。
**How to apply:** Windows 上做二进制分析时直接按上述可用工具组合；需要函数级反编译时先请用户配置 GHIDRA_INSTALL_DIR。此条为单机事实（zrtc），另一台机器需另行探测。
