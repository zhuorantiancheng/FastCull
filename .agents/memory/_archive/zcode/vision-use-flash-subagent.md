---
name: vision-use-flash-subagent
description: 用户要求：视觉/读图任务一律改用 GLM-5.3-Flash 子代理，不要用主模型或 4_5v 直读
metadata:
  node_type: memory
  type: feedback
  originSessionId: sess_3aba2f48-0d61-4bc3-923d-f43e01375104
---

2026-10-12 用户明确要求：今后调用视觉能力（读图、截图自检、UI 视觉检查）时，开子代理用 **GLM-5.3-Flash** 模型读图，不要在主会话直接用视觉（`mcp__4_5v__analyze_image` 模型固定、且用户已要求弃用此方式）。

**Why:** 视觉描述占大量 token 且不需要主模型级别的推理，用 Flash 便宜快速；也保持主上下文干净。

**How to apply:** 指定子代理模型的机制是 CreateWorkflow 的 `subagent_model="account:bigmodel-individual-coding-plan/GLM-5.3-Flash"`（调用前需先 Skill 加载 dynamic-workflows），写单 agent 脚本把图片路径/URL 传给它描述并返回结论；本机 Agent 工具本身没有模型参数，勿直接依赖。图片在本地时先让子代理用 Read 读图（Read 支持图片）。
