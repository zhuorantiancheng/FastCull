---
name: sony-viewer-xmp-rating
description: Sony Viewer 评分存于 ARW 内嵌 XMP(tag 0x02BC)，LR 旁车桥接工具已完成
metadata:
  node_type: memory
  type: project
  originSessionId: sess_c2794347-ab02-4df6-aced-eb63499f4e65
---

Sony Imaging Edge Viewer 的"评价"（星级+颜色标签）机制（2026-10-12 逆向确认）：

- Viewer.exe/Edit.exe **静态链接完整 Adobe XMP SDK**（XMPFiles::GetXMP/PutXMP），有 SetRating/RemoveRating/SetLabelRed/SortByRating 完整 UI。
- 评分存放位置：**ARW 内嵌 XMP 包**（TIFF IFD0 tag 0x02BC，固定 4096 字节，位于文件头 ~0x156，含大量空白 padding）——不是旁车文件。相机直出文件浏览后会被 Viewer 初始化为 `<xmp:Rating>0</xmp:Rating>`。
- LR/ACR 写旁车 `.xmp`，两边互不读取 → 需要桥接。

**桥接工具**：`E:\documents\Study\codes\IEDT\xmp_bridge.py`（副本在 .agent-temp/2026-10-11-sony-rea/）。读旁车评分+Label → 按 Viewer 模板重建内嵌包，**总字节长度恒定**（padding 补齐），只读写文件头 4KB 区域，TIFF 结构零变动。支持 --dry-run/--apply、文件夹递归、幂等（已一致跳过）。已验证：评分/绿标写入、exiftool 读回、改旁车后重跑增量更新。

**限制**：图像编辑（crs develop 参数）**无法桥接**——Viewer 不解释 XMP 开发设置，只有评分和颜色标签可传递。

**Why:** 用户要让 Sony iedt 读取 LR/ACR 的 XMP 修改；此桥接是评分部分的完整方案。
**反向桥接（--reverse, 2026-10-12 已加）**：ARW 内嵌评分/标签 → 旁车（已有旁车只改 Rating/Label 字段不动 develop 设置；无旁车创建最小旁车）。已在用户真实"3星_优选"文件夹验证（3★Red/1★Green 正确生成）。Viewer 还自带"发送到已注册程序"（RegisteredPrograms/SendApplication），可注册 Adobe 程序右键直发。完整工作流：Viewer 初筛打星 → --reverse → LR "从文件读取元数据" → 星级过滤精修导出；LR 改分后 --apply 回同步。注意一次只跑一个方向。

**How to apply:** 用户在 Viewer 确认显示后，可对整个照片库跑 `python xmp_bridge.py --apply "D:\pictures\照片\RAW"`（建议先 --dry-run）。相关：[[sony-sdm-exraw-re]]、[[photocheck-xmp-feature]]（PhotoCheck 侧同主题实现）。


**LR 插件（2026-10-12 已构建, 待用户在 LR 内实测）**：`E:\documents\Study\codes\IEDT\iedt-bridge.lrplugin\`（Info.lua 注册"图库→增效工具额外命令"两个菜单：去 IEDT 选片 / 从 IEDT 同步评分；common.lua 工具层；PluginInfo.lua 设置面板；捆绑 xmp_bridge.py）。桥新增 `--from-list path|rating|label`、`--reverse-from-list`、`--launch-viewer EXE --folder DIR`（Python 处理中文路径, 绕开 cmd 编码）、`--ascii-summary`（供 Lua 解析 "RESULT patched=N ..."）。环境：LR Classic 15.6 @ E:\APP\Adobe Lightroom Classic；目录 D:\pictures\照片\Lr\Lr.lrcat。Lua 已过 lupa 语法检查; LR 内交互未经实测, 首次安装若有菜单/绑定问题需小修。