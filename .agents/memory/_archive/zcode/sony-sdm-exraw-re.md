---
name: sony-sdm-exraw-re
description: Sony Imaging Edge「扩展RAW处理(SuperDemosaic/SDM)」完整逆向结论、关键地址与绕过进展
metadata:
  node_type: memory
  type: project
  originSessionId: sess_c2794347-ab02-4df6-aced-eb63499f4e65
---

Sony Imaging Edge 4.1.01（E:\APP\Sony）"扩展 RAW 处理"（内部代号 SuperDemosaic/SDM）逆向结论（2026-10-11，Ghidra 12.1.4 + PyGhidra 3.1.0）：

**判定链（两道门）**：
1. 门1（菜单）：EXIF Model 字符串 + MakerNote 0xb001 数值机型码（a6700=394, α7V/ILCE-7M5=407, α9III/ILCE-9M3=392）。已用等长原位字节补丁绕过（4 处字符串 + 1 处 int16，文件结构零改动；exiftool 重写会导致 Viewer 判损坏，不可用）。
2. 门2（处理跳过）：ARW 解码器（Viewer.exe `FUN_140150f20`@0x140150f20）检查 Sony MakerNote **私有标签 0x789b 和 0x789c** 是否存在——只有受支持机身（α1II/α9III/α7RVI/α7V）才写入。存在时提取约 17 个 int 到 `ZcARW+0x22f98`（+0x22f94 为存在标志）。缺标签 → `FUN_14008b580`@0x14008b580 匹配失败 → 在模型下载之前直接跳过。

**校准数据结构**：tag 0x789b 数组 = 每传感器校准（黑电平/增益/白电平、按 NR 强度插值的端点），由 `FUN_140255750`@0x140255750 读入引擎配置（0x3130 字节缓冲）；`sdmsettings.dat`（C:\ProgramData\Sony\Imaging Edge\OnnxData\，SDMP 魔数 + ver=4，内含按 SDM-ID × ISO 档(log2) 的 0x94 字节校准块，可见 400/800/1600/3200 等 ISO 阶梯）由 `FUN_140253b40` 解析、`FUN_140254470` 查表。

**关键地址**（Viewer.exe, base 0x140000000）：命令分发 FUN_1400804a0；SDM 执行 FUN_1400889c0（0x14008b580 为资格检查调用点 0x417 行附近）；sdm 引擎 FUN_140256410；模型下载 URL `…/imagingedge_DL/sdm?mdl=`@0x1400488d0；反编译输出在 IEDT/.agent-temp/2026-10-11-sony-rea/ghidra-out/。

**绕过路线**：A) 在 ARW 里伪造 tag 0x789b/0x789c（可把全零的 256B tag 0x2003 条目改造为载体，避免重构 IFD），校准值需从真实受支持机身 ARW 借或按 a6700 黑白电平推导；B) 二进制补丁跳过检查（校准仍为零会出垃圾图）。模型本体共 2 个（NR/HiRes 共享，非每机身一个）。Ghidra 工程在 E:\APP\ghidra-ws\viewer_sdm（可复用，秒开）。测试文件在 .agent-temp/2026-10-11-sony-rea/exraw-test/（inplace_spoof.py）。

**Why:** 用户想让 a6700 用上 NR/Hi-Res；跨会话继续此工程时直接从这里的结论出发。
**How to apply:** 继续时先看 [[rea-mcp-win-env]]（工具环境）；下一里程碑=拿到真实 α7V/α9III 样片提取 0x789b 校准值，或反编译 FUN_140253b40 破解 SDMP 完整格式。
