---
name: photocheck-xmp-feature
description: PhotoCheck 已支持 LR/ACR XMP 旁车识别（评分+编辑），实现要点与验证结论
metadata:
  node_type: memory
  type: project
  originSessionId: sess_c2794347-ab02-4df6-aced-eb63499f4e65
---

PhotoCheck（`C:\Users\zrtc\.zcode\workspace\default\PhotoCheck`，用户口头称 "iedt"，本会话工作区为 IEDT 空壳目录）于 2026-10-12 完成 XMP 旁车识别功能：

**功能**：读取 RAW 旁 `.xmp`（Lightroom/ACR/Bridge/ExifTool 写入），提取星级评分、颜色标签、编辑软件判别、是否编辑（与 crd 相机默认值数值对比 + 裁剪/局部蒙版判定）、曝光调整(EV)、编辑强度。核心模块 `src/photocheck/core/xmp.py`，PhotoMetadata 新增 8 字段（rating/color_label/xmp_editor/xmp_edited/xmp_exposure/xmp_adjust_count/xmp_has_crop/xmp_mtime）。

**关键设计**：
- 解析器用 ElementTree，按命名空间 URI 归一前缀，兼容属性式(Adobe)/子元素式(ExifTool)两种 RDF 写法——用户库里 10809 个旁车两种都有，还有约 6000 个纯评分旁车
- 缓存兼容：旧 parquet 缺列 → row.get 回 None；NaN→None 归一化（否则 sync 误判"旁车被删"——真实踩过的坑）
- 增量刷新：缓存存 xmp_mtime，rescan 只重读 mtime 变化的旁车（LR 改评分后 scan 即刷新，不重读 EXIF）
- matplotlib 陷阱×2：必须在 pyplot 前固定 Agg（本机 Tk 运行时损坏）；泛型 "sans-serif" 别名不做 CJK 逐字形回退，需显式家族列表 ["Segoe UI","Microsoft YaHei",...]（模块级设置，函数内覆盖会失效）
- 编辑判定用数值比较（"0.00"≠"0" 字符串比较会误判）

**验证状态**：156 测试全过（含 3 个脱敏真实旁车 fixture）；60 张抽样真值核对 60/60 一致；三编辑器(CR/ExifTool/LR)全识别；三张新图表过 visual-judge 视觉评审。未 commit（用户未要求）。用户库实测规模：16313 ARW、10809 XMP、配对 4232。

**Why:** 跨会话继续 PhotoCheck 开发时避免重踩 matplotlib/parquet 坑，直接知道功能边界。
**How to apply:** 改动都在工作区未提交；下一步若用户要全库数据，直接跑 `uv run python run.py` 或 scan 即可（首次会对已缓存条目批量解析旁车，之后增量）。
