# GPU RAW 显影候选核验（2026-10-14）

目标是为独立 Windows 选片器寻找更快的 RAW 显影路线；输入只有 RAW 与编辑参数文件。本次是公开源码和一手测试记录核验，没有编译、执行这些项目，没有替换 IEDT 生产代码。已有本机基线见 [RAW 性能报告](RAW-PERFORMANCE-2026-10-14.md)。

## 结论

**优先自行验证“快速解包 + GPU 显影 + GPU 直接显示”的分阶段架构；下列两个项目都不适合直接当成已验证的 ACR 兼容引擎。** Ceyx 有较完整的实现与上游性能记录，但本项目源码授权不明确；seecull 可参考 XMP 与选片架构，其被搜索到的 GPU 高速路线已经撤回，不能拿计划数字当实际性能。证据分别见下面源码与记录。

## 1. Ceyx：技术实现值得研究，暂不直接引入

核验版本：[`f05458d0a73ac03dda8248020f7b49efa54d40c9`](https://github.com/jhangyu/ceyx/tree/f05458d0a73ac03dda8248020f7b49efa54d40c9)。

- **确有实现。** 通用 RAW 使用 LibRaw 解包，可优先调用 RawSpeed3；GPU 部分用 Halide AOT 完成去马赛克和颜色渲染。前端实际设置 `LIBRAW_RAWSPEEDV3_USE`，Windows 路径使用宽字符 `open_file`，不是只写在路线图中的接口。[LibRaw 前端源码](https://github.com/jhangyu/ceyx/blob/f05458d0a73ac03dda8248020f7b49efa54d40c9/native/src/pipeline/libraw_frontend.cpp)
- **已公布性能，但不能换算为本机收益。** 作者记录 M3 Ultra 上 Sony ARW 的 FFI 端到端为 72.9–76.6 ms；24 MP 无损 DNG 温态为 160–171 ms，DNG 冷启动应用示例为 291 ms。它们分别使用不同输入与路径，不是 IEDT 的 A6700 样片在 Windows 上的同条件结果。[上游性能记录](https://github.com/jhangyu/ceyx/blob/f05458d0a73ac03dda8248020f7b49efa54d40c9/README.md#performance)
- **Windows 的证据要拆开。** README 宣称 Windows Vulkan 已验证，但 Windows CI 自己明确说明没有 Vulkan GPU/ICD，只能验证编译链接，不能运行解码和 PSNR 门槛。本次没有获得 Windows 端到端性能数据，因此不能以 CI 通过替代本机实测。[Windows 工作流](https://github.com/jhangyu/ceyx/blob/f05458d0a73ac03dda8248020f7b49efa54d40c9/.github/workflows/windows_build.yml)
- **不同格式仍受 CPU 解包限制。** 作者给出的 X-T5 RAF 解包为 1231 ms，解释为该机型变体未被其固定 RawSpeed3 版本支持、回退到 LibRaw；不能把 GPU 渲染耗时当成任意 RAW 的整体耗时。[格式差异说明](https://github.com/jhangyu/ceyx/blob/f05458d0a73ac03dda8248020f7b49efa54d40c9/README.md#stage-level-and-per-format-detail)
- **未找到项目自身明确的开放使用许可。** 根目录没有通用 LICENSE；`plugin/LICENSE` 仅说明 private/unpublished，并列举第三方依赖的授权索引。它不构成对本项目全部源码的明确复用授权。应先取得作者明确许可，再考虑复制、集成或分发；依赖的 MIT/LGPL 不自动覆盖作者自己的代码。[plugin/LICENSE](https://github.com/jhangyu/ceyx/blob/f05458d0a73ac03dda8248020f7b49efa54d40c9/plugin/LICENSE) · [当前目录树](https://github.com/jhangyu/ceyx/tree/f05458d0a73ac03dda8248020f7b49efa54d40c9)
- **DNG SDK 对照不是 Lightroom 编辑复现。** `dng_xmp_stub.cpp` 中 `Parse` 为空操作、多个读取接口返回 false；至少该构建用的 stub 不具备 XMP 显影解释能力。现有 DNG SDK 算术一致性门槛不能作为 ACR 的曝光、高光阴影、局部蒙版等编辑一致性的证明。[XMP stub 源码](https://github.com/jhangyu/ceyx/blob/f05458d0a73ac03dda8248020f7b49efa54d40c9/native/src/pipeline/dng_xmp_stub.cpp)

判断：可借鉴架构，性能前景值得实验；目前不是可以直接交付的独立 Adobe 编辑兼容引擎。可靠性：实现存在为高置信，Windows 实际时延未验证，本项目源码复用许可未解决。

## 2. seecull：适合参考选片与 XMP，不能作为高速 GPU 引擎采购/集成依据

核验版本：[`ecd14ed7007794e07160d015527b6807988de9b5`](https://github.com/gyng/seecull/tree/ecd14ed7007794e07160d015527b6807988de9b5)。

- **250–400 ms 是估算。** Plan 007 将 RawSpeed 解包与 GPU 管线组合后的 40 MP X-Trans 总时间标为 estimated；同时明确实验 crate 已从工作树回滚。当前 `sc-raw` 源码保留 `rawspeed branch removed — plan 007 dropped` 注释，实际默认解析为 LibRaw 或 rawler。[计划](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/docs/plans/007-rawspeed-gpu.md) · [当前解码分派源码](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/crates/sc-raw/src/lib.rs)
- **GPU 实验数字有严格边界。** 41 MP 的 bilinear 97 ms、AHD-light 100 ms、markesteijn-lite 148 ms 使用合成像素面，Windows 11/DX12，温态 5 次取最佳，包含上传与回读；没有给出具体 GPU 型号，运行波动约 20–30%。进程首次 GPU 初始化约 455 ms。它们不是真实 RAW 从读盘到显示的延迟。[实验记录](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/docs/experiments/001-gpu-demosaic.md)
- **120 倍不能用作等画质加速结论。** 上述实验用合成 markesteijn-lite 对比真实 RAF 的完整 LibRaw markesteijn-3，且 lite 缺少后续色差混叠抑制；文档明确真实样片的算法等价性尚未验证。相同 bilinear 算法的并行 CPU 与 GPU 比较收益只是约 30%。[实验条件与剩余工作](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/docs/experiments/001-gpu-demosaic.md)
- **Windows 与许可证条件较清楚。** 项目提供 Windows 安装形式，工作区许可为 MIT OR Apache-2.0；CI 配置包含 Windows，但存在 CI 配置不等于本次已复跑通过，更不证明已撤回的 GPU 管线可用。[README](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/README.md) · [Cargo.toml](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/Cargo.toml) · [CI](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/.github/workflows/ci.yml)
- **XMP 兼容主要是字段往返。** 作者明确说明自己的预览与 Lightroom 渲染不同，预览会应用于缓存 JPEG 或通用 LibRaw 结果；不能因此承诺复现 Adobe 的编辑画面。它适合作为星级、标签、编辑字段往返和缓存设计参考。[项目限制与 FAQ](https://github.com/gyng/seecull/blob/ecd14ed7007794e07160d015527b6807988de9b5/README.md#limitations)

判断：XMP/交互参考价值高，GPU 引擎候选优先级低；“250–400 ms 已可用”这一主张不成立。

## 对 IEDT 的实验建议

以下是工程建议，不是已经测得的结果。

1. 保留现有 LibRaw 回退，先把解包、去马赛克、调色、纹理上传、首帧展示计时分开。以现有三张样片和更多真实机型作同输入对照，不能跨机器搬用上游倍数。
2. GPU 原型先以现有解包出的 CFA 数据为输入，测双线性或明确选定的算法；将初始化与稳态分开，尽可能让调色和显示共用 GPU 纹理，避免显影后完整回读 CPU。
3. 适屏预览与 100% 对焦检查分开验收。降低去马赛克质量得到的速度收益必须同时检查摩尔纹、细线、彩边和锐度。
4. ACR/LR 编辑兼容单独建测试集。性能测试不能替代效果一致性测试；上述两个候选都不能解决完整 Adobe 渲染复现问题。

本次只新增本研究笔记，未生成本地下载代码或临时文件。
