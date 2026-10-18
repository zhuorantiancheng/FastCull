# 性能基线、已做优化与实测方法

事实：2026-10-18 对本机（i7-13700HX / 24 逻辑处理器）做了一轮解码性能实测并落地三项改动，均已通过 141 项 Rust 测试、Clippy 和 GUI 冒烟：

1. Full 路径 `RGBA → egui::ColorImage` 改为零拷贝：`src/cache.rs` 的 `color_image()` 在长度、容量、4 字节对齐都成立时直接移交 `Vec<Color32>`，否则回退逐像素构造。25.6 MP 转换实测 27.1 ms → 0。
2. 新增 `imaging::Level::FastThumb`：网格/胶片带先用相机内嵌小图（Sony TIFF 的 160×120，解码 < 1 ms）占位，200 px 到达后原位替换；只对 `thumbnail.offset != preview.offset` 的源启用，JPEG 与 LibRaw 源自动跳过。`src/ui.rs` 用独立的 `fast_thumbs` 纹理缓存（8 MiB）。
3. 解码线程从 `clamp(2, 4)` 改为 `clamp(2, 12)`：缩略图批量吞吐实测 8 线程约为 4 线程的 1.9 倍，12 线程再快 25～35%，16 以上饱和；`eligible()` 仍只让前两个 worker 做 Full，且 worker 0 不接 FastThumb（专职当前图 fit/full）。

适用条件：继续做性能优化、回归性能数字，或调整 `cache.rs` 的线程与纹理策略时先看这条。

证据（相对项目根）：
- 实测报告与复现脚本：`.agent-temp/2026-10-18-perf/PERF-FINDINGS.md`、`build.ps1`、`cull_probe.rs.txt`（临时目录，不随 Seafile 同步，另一台机器需按报告重建）。
- 关键基线：ARW Thumb/Fit/Full = 7.2 / 7.0 / 89.3 ms；CR2 = 31.9 / 75.1 / 75.7 ms；NEF = 18.6 / 66.3 / 39.2 ms；JPG = 35.5 / 75.3 / 80.2 ms。
- GUI 冒烟结果字段：`result.txt` 的 `thumbs=` 与 `fast_thumbs=` 计数可用于确认小图路径是否生效。
- 复现坑：`build.ps1` 的 `-Action` 默认值曾是 `bench`，无参数调用会构建 example 而不是 exe；现默认 `release`。

未做、留待后续：纹理按可见区域上传（100% 主线程 102 MB 上传）、非 Sony 格式的 DCT 缩放（需引入 libjpeg-turbo）、`fast_image_resize` 换 Box 或交 GPU。

更新人：DSH（deepseek-flash），2026-10-18。
