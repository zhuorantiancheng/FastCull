# 更高效的 RAW 显影方案评估（2026-10-14）

## 判断

针对 FastCull 当前的 Windows/Rust 实现，优先实测 **近期 RawSpeed 的 Sony 无损分块解码**，同时把 **rawler** 作为 Rust 原生对照。解包速度改善后，再接 GPU 去马赛克、调色与直接显示。完整 GPU 项目可作参考，但本次未发现同时满足“可直接集成、Windows 同机性能已验证、完整复现 Adobe 编辑”的现成引擎。

这是候选研究与一个局部输入路径实测，不是这些引擎已经在本机跑出的性能排名。生产程序与 DLL 未替换。已有三格式基线见 [原性能报告](RAW-PERFORMANCE-2026-10-14.md)，GPU 项目的详细核验见 [候选源码笔记](RAW-GPU-CANDIDATES-2026-10-14.md)。

## 1. 新发现：当前 Sony 样片最该加速的是解包

只读检查 `t01-a6700-ISO5000-original.ARW`：RAW IFD 的 Compression=7，存储尺寸 6656×4608，TileWidth/TileLength=512，共 117 个 tile。存储尺寸包含边缘填充，不是有效照片尺寸。LibRaw 报出的解码器为 `sony_ljpeg_load_raw()`，因此不能把这张样片当成传统有损 ARW2 来推算速度。

本机 LibRaw 0.22.2 源码的该函数逐块读取、解压，没有块级 OpenMP 循环；只给整个库加 `/openmp` 不会使这个函数自动并行。[上游对应源码](https://github.com/LibRaw/LibRaw/blob/0.22.2/src/decoders/decoders_libraw_dcrdefs.cpp)

新增输入对照：i7-13700HX、相同 OpenMP 4 线程库、同一真实 Sony 样片、半尺寸显影、相机白平衡、关闭自动亮度、sRGB 8-bit。每种输入一次预热、七次测量，操作系统文件缓存为热态。

| 输入路径 | 打开/准备输入中位数 | 解包中位数 | RGB 就绪中位数 |
|---|---:|---:|---:|
| `open_file` | 0.64 ms | 516.67 ms | 594.87 ms |
| 完整读入内存后 `open_buffer` | 15.80 ms | 477.25 ms | 575.44 ms |
| 只读内存映射后 `open_buffer` | 0.49 ms | 515.89 ms | 596.61 ms |

RGB 就绪时间包含每次读取/映射、解包、半尺寸处理和输出缓冲区生成；不包含校验、释放、GPU 上传、呈现和 Adobe 编辑。各阶段中位数不能直接相加。24 次输出的完整 64-bit FNV-1a 校验均一致，这是输出一致性的校验值检查，不是逐字节数学证明。

**仅调整输入方式仍需约 0.58–0.60 秒，没有数量级改善。** 这次各模式按组顺序运行且只有七次测量，约 3% 的组间差异不足以认定稳定收益。该测试排查输入路径，不证明冷读盘、其他格式或 UI 的表现。

原始数据与源文件哈希：[输入对照 JSON](raw-input-comparison-2026-10-14.json)；可复用探针：[bench_raw_input.cpp](../scripts/bench_raw_input.cpp)。编译需 MSVC Release `/O2 /EHsc /MT /utf-8 /openmp`、LibRaw 0.22.2 头文件及相同配置静态库；调用形式为 `raw-input.exe <RAW> <file|buffer|mmap> half 7 4`。库的隔离构建入口为 [build-raw-benchmark.ps1](../scripts/build-raw-benchmark.ps1)，该脚本现有探针列表不自动包含本次新增输入探针。

## 2. 候选排序

### 第一候选：近期 RawSpeed + GPU 显影

近期 RawSpeed 的 `ArwDecoder::DecodeLJpeg` 有针对 Compression=7 的入口，并用 OpenMP 并行处理 tile。这与当前 117 个独立压缩块的样片直接相关；传统 ARW2 另有按行并行路径。[LJPEG 分块实现](https://github.com/darktable-org/rawspeed/blob/develop/src/librawspeed/decoders/ArwDecoder.cpp) · [ARW2 实现](https://github.com/darktable-org/rawspeed/blob/develop/src/librawspeed/decompressors/SonyArw2Decompressor.cpp)

这里的收益是有源码依据的待验证假设，不能用“117 个块”推导 117 倍加速。需要实际测线程数、解码精度、裁剪/黑电平信息、内存占用和尾延迟。

**不能仅给当前 LibRaw 打开 RawSpeed3 开关。** 有两处需要适配：

1. LibRaw 的 RawSpeed3 接入说明固定到旧提交 `de70ef5fbc62cde91009c8cff7a206272abe631e`；该提交的 Sony 解码器只接受无压缩与 32767 压缩，没有上述 Compression=7 路径。[接入说明](https://github.com/LibRaw/LibRaw/blob/master/RawSpeed3/README.md) · [固定版本 Sony 源码](https://github.com/darktable-org/rawspeed/blob/de70ef5fbc62cde91009c8cff7a206272abe631e/src/librawspeed/decoders/ArwDecoder.cpp)
2. 本机使用的 LibRaw 0.22.2 没给 `sony_ljpeg_load_raw` 设置 `LIBRAW_DECODER_TRYRAWSPEED3`，而解包分派检查这个标志。因此仅换入 DLL/设置开关仍可能完全没走新引擎。[decoder_info](https://github.com/LibRaw/LibRaw/blob/0.22.2/src/utils/decoder_info.cpp) · [unpack 分派](https://github.com/LibRaw/LibRaw/blob/0.22.2/src/decoders/unpack.cpp)

建议先直接调用近期 RawSpeed 的小型 C ABI 探针，固定测试提交，显式启用并验证 OpenMP，再决定包装进应用。保留 LibRaw 为不支持格式的回退。RawSpeed 只输出 RAW 像素与必要信息，不负责去马赛克、白平衡、颜色渲染或 Adobe 编辑；属于 LGPL 组件，集成时按实际文件与分发方式核对要求。[功能边界](https://github.com/darktable-org/rawspeed/blob/develop/README.rst)

### 第二候选：rawler + GPU 显影

rawler 是 Rust RAW 解码与元数据库。其 Sony LJPEG 路径使用 `decompress_strips_fn`，该辅助函数通过 Rayon 的 `par_chunks_mut` 并行处理条带。比“换 Rust 就会快”更具体的依据，是它确实给这类数据安排了并行工作。[Sony 解码源码](https://github.com/dnglab/dnglab/blob/main/rawler/src/decoders/arw.rs) · [并行辅助函数](https://github.com/dnglab/dnglab/blob/main/rawler/src/decompressors/mod.rs)

它与现有 Rust 项目语言一致，但不能因此免除兼容性和画质验证；本次没有在同一 A6700 文件上测过它，不能宣称比 RawSpeed 更快。当前 crate 声明 LGPL-2.1、最低 Rust 1.89，项目自身声明 Rust 1.90，工具链版本门槛在声明层面吻合。[Cargo 配置](https://github.com/dnglab/dnglab/blob/main/rawler/Cargo.toml)

### 完整 GPU 方案与参考项目

| 方案 | 已核实的价值 | 对当前任务的限制 |
|---|---|---|
| Ceyx | LibRaw/RawSpeed3 前端 + Halide GPU 管线；作者公布 M3 Ultra Sony ARW 72.9–76.6 ms | 非本机/同样片成绩；Windows CI 只构建；自身复用许可未明确；XMP stub 不实现编辑读取 |
| vkdt | Vulkan 显影节点图，图像留在 GPU 直接显示；可选 RawSpeed/rawler 输入 | 是完整应用，移植成本需评估；不能直接当作 Windows SDK；部分源码有不同许可证 |
| Fastvideo SDK | 商业 CUDA 显影；公开 GPU 与磁盘管线基准 | 限 NVIDIA；65 MP 约 60 fps 是 GPU 内处理条件，不能套成 Sony ARW 文件打开延迟；需确认 ARW 解包与编辑兼容 |
| seecull | Rust 选片、缓存、XMP 字段往返可参考 | RawSpeed/GPU 计划已撤回；250–400 ms 为估算；合成像素实验不是真实 RAW 首帧成绩 |

Ceyx、seecull 的固定提交、测试条件与源码出处见 [详细核验](RAW-GPU-CANDIDATES-2026-10-14.md)。其他来源：[vkdt README](https://github.com/hanatos/vkdt) · [Fastvideo GPU RAW Processor](https://www.fastcompression.com/products/gpu-raw-processor.htm)。厂商的“画质可比 ACR/LR”表述不等于兼容 Adobe 编辑参数。

## 3. 对选片器的实现判断

推荐的数据流：**RAW → 并行解包 → CFA 缓存 → GPU 去马赛克/调色 → 显示纹理**。现有界面使用 eframe/glow，因此 GPU 计算接口还要评估与 OpenGL 显示纹理的共享方式；不能默认换成 wgpu 后没有纹理拷贝或后端迁移成本。

只替换去马赛克解决不了当前约 0.5 秒的首次解包。解包成功后缓存 CFA 和已显影预览，调整参数时复用 CFA；顺序切图通过有上限的邻片预取提升命中率。未预取的随机跳转必须另测，不能以缓存命中成绩承诺所有操作都无延迟。

适屏预览可研究面向输出分辨率的显影，100% 检查再提供高质量结果。若降低分辨率或更换算法，需同时检查摩尔纹、细节、彩边和色彩，不能仅比较毫秒数。RAW 内嵌 JPEG 通常不是当前 Adobe 编辑后的画面；它可以提供快速首帧，但不能计为“编辑效果已加载”。

建议下一轮只做一个同输入对照：LibRaw / 近期 RawSpeed / rawler，先比较解包，测 1/4/8 线程、冷暖启动、P50/P95 与有效像素一致性。择优后才接 GPU。当前不承诺 50 ms 或 100 ms 的真实 RAW 首帧；可把这些作为验收预算，必须区分缓存命中和未命中。

Adobe 效果是另一项验收：XMP 中同名数值不足以规定高光/阴影、配置文件、局部蒙版与 AI 处理的完整渲染算法。本次候选没有证据证明完整复现 ACR/LR；快的 RAW 渲染可以推进，Adobe 兼容程度需要逐项定义和参考图验证。

## 交付与范围

正式成果是本报告、GPU 候选笔记、输入对照 JSON 和 C++ 探针。临时编译、下载的源码文本及短测结果在项目 `.agent-temp/2026-10-14-raw-research/`，未自动删除。报告阅读和结果核查不依赖该临时目录；重新执行测试需要原 RAW 样片与按说明构建的探针。未编译、运行候选项目，也未修改生产显影管线。
