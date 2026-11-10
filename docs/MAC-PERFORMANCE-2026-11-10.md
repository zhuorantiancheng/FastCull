# 2026-11-10 Mac ImageIO 缩小解码基准

2026-11-10，同一台 Apple Silicon 电脑（Apple M5 Pro，48 GB，macOS 27.0.1）对照 `main`（dd00771，Rust 全量解码 + 缩放）与本分支（Core Graphics/ImageIO 缩小解码）。两者仅解码路径不同，界面、快捷键、评分、移动及退出同步规则不变。

## 实测结果

以下为中位数，单位 ms。解码每项先预热一次，再测七次；操作系统文件缓存未清空，不代表机械硬盘、网络盘或重启后的冷启动。计时包含文件读取、CPU 解码和缩放，不包含 GUI/纹理上传。

| 样张与操作 | main（Rust 路径） | 本分支（ImageIO） | 耗时减少 |
| --- | ---: | ---: | ---: |
| big.jpg 6000×6000 缩略图 200 px | 93.927 | 35.395 | 62% |
| big.jpg 适屏解码，固定长边 2560 | 105.992 | 85.324 | 20% |
| big.jpg 全尺寸 6000×6000 | 81.679 | 81.925 | 未测得显著变化 |
| small.jpg 1200×1200 缩略图 200 px | 2.867 | 2.964 | 未测得显著变化 |
| small.jpg 适屏解码，固定长边 2560 | 2.196 | 2.267 | 未测得显著变化 |
| sample.arw 快速小图 | 0.090 | 0.089 | 未测得显著变化 |
| sample.arw 缩略图 200 px | 5.395 | 5.519 | 未测得显著变化 |
| sample.arw 适屏解码 | 4.607 | 4.674 | 未测得显著变化 |
| sample.arw 全尺寸 | 57.095 | 55.879 | 未测得显著变化 |

全尺寸路径不经过缩小解码，保持原样；不要把预热命中的延迟当成解码速度。未走 ImageIO 的样张差异在单机运行噪声范围内。

## 门控与未加速的解释

缩小解码仅在满足既有条件时启用：非全尺寸、请求长边不超过原图长边的一半、图像像素数不低于 2 MP、色彩空间为 YCbCr/RGB/Luma。small.jpg（1.44 MP）与样张 ARW 的内嵌预览都低于 2 MP 门槛，保持原 Rust 解码路径；Sony ARW 本来就有独立的小预览，收益不明显，与 2.7.0 的 Windows 记录一致。适屏 2560 对 36 MP 样张只减少约 20%，ImageIO 在该尺寸下仍需处理大部分 DCT 块，收益小于小缩略图。

## 实现

- macOS 缩小 JPEG 解码改用 Core Graphics/ImageIO 的 `CGImageSourceCreateThumbnailAtIndex`，按请求长边设置 `kCGImageSourceThumbnailMaxPixelSize`，并固定 `kCGImageSourceCreateThumbnailFromImageAlways`、关闭缓存与方向变换；接口契约镜像 Windows 的 WIC 路径：任何失败返回 `None`，回退 Rust 解码。
- 位图上下文使用图像自身色彩空间，避免色彩重映射；灰度等非 RGB 来源回退 DeviceRGB。截断或损坏文件通过 `CGImageSourceGetStatus` 检查拒绝，保持严格 Rust 解码为权威路径。
- 审查修订（2026-11-10）：以 2×2 原生探针（首行红、次行蓝）实测位图上下文行序，确认 plain 绘制的缓冲区首行即图像首行，移除了最初多余的 CTM 翻转；原测试样片为纯色图无法检测镜像，新增非对称四象限样片（5120×2048、2048×5120 与 EXIF Orientation=6 变体）的方向回归，覆盖直接入口与生产入口的 200/2560 请求，返回 None 显式失败。

## 同机补充：其他维度的 Mac 实测

2026-11-10 同机补测，覆盖筛选重绘、RAW 传感器解码与真实 DNG：

| 项目 | 结果 | 说明 |
| --- | ---: | --- |
| 筛选侧栏重绘（filter_bench，10000 行） | median 0.017ms，p95 0.020ms | 110 次丢弃前 10 次预热；低于 Windows 记录的 0.037ms；该路径与 JPEG 解码无关，两版本共用 |
| 真实 DNG 传感器解码 Full（7008×4672，DxO 增强，80 MB） | main 1022.8ms / 本分支 1031.7ms | 差异在运行噪声内；传感器解码不经 JPEG 缩小路径 |
| 真实 DNG 预览层级（内嵌最大 JPEG 1024×683，低于 2 MP 门控） | FastThumb/Thumb/Fit 两版本一致 | 该 DNG 无全尺寸内嵌预览，不触发缩小解码 |
| 真实 Sony ARW 全尺寸（24 MP 级私有副本） | main 57.6ms / 本分支 57.6ms | 与上表首轮流数一致 |
| Metal GPU 像素验收 | 通过 | `native_gpu_pixels_match_cpu_scene_and_jpeg_reference` 在真实 Metal 适配器上匹配 CPU 场景与 JPEG 参照 |
| 真实 DNG 元数据往返 | 通过 | 私有副本字节保持，`FASTCULL_DNG_EMBED_FIXTURE` 指向本地副本运行 |

RAW 传感器解码需要 libfastcull_raw.dylib；本机按 `scripts/build-macos.py` 的同一命令链从仓库内 LibRaw 0.22.2 源码构建（SHA-256 校验通过，otool 依赖仅 /usr/lib 系统库）。`bench_raw_pipeline.cpp` 的分阶段计时为 Windows 专用，Mac 未复刻；上表为端到端 Full 耗时，不拆分 open/unpack/process。

## 正确性与界面检查

- 本机 `cargo test --locked` 全绿：125 项库测试（含新增的 ImageIO 尺寸契约、非对称四象限方向回归、EXIF Orientation 处理、损坏/截断回退、重复调用一致性测试，且原有缩略图色彩误差测试在 ImageIO 路径上真实执行）、集成测试 68/25/8/6/3/3/8/8 项全部通过，1 项需私人样张的既有测试跳过；Clippy 所有目标 `-D warnings` 无警告。
- 样张为本地生成（系统壁纸经 sips 转换）与本机照片私有副本，不含私人照片内容；照片目录仅读取。

原始逐次记录见 [macos-performance-2026-11-10.json](macos-performance-2026-11-10.json)。可重复基准为 `examples/decode_bench.rs`：`cargo build --release --locked --example decode_bench` 后运行 `decode_bench <照片路径> 7`；本路径不依赖 libfastcull_raw.dylib。冷启动、Intel 架构与更多机型未测。
