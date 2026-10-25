# DNG 支持与实测（2026-10-23）

2026-10-24 补充：新增的手动 **RAW 编辑** 工作区在明确保存时，会通过 Adobe 安全更新接口把曝光/白平衡等显影 XMP 同步到 DNG 内部，RAW 压缩图像数据保持不变。下文“DNG 文件本身不改写”描述的是普通选片评分路径；编辑工作区的存储与宿主依赖见 [RAW 编辑说明](RAW-EDITING.md)。

独立打开、拖入文件/文件夹、Bridge 和 Lightroom 插件均已放行 `.dng`，扩展名不区分大小写。当前工作区的 `lrplugins/FastCull.lrplugin/fastcull.exe` 已更新，保留相邻的 `fastcull_raw.dll`；历史版本压缩包不包含本次改动。Bridge 已安装的脚本需重新安装，Lightroom 插件需重新载入。

## 读取与性能策略

- 依据 TIFF 索引随机读取 IFD/SubIFD，不扫描 RAW 像素区，也不把整个 DNG 读入内存。支持大小端和位于文件后部的目录；限制目录数量、元数据读取量和 JPEG 范围。
- 预览按照实际 JPEG 宽高选择，避免把压缩文件长度当作分辨率。RAW 传感器条带、深度图和掩膜不作为照片预览。
- 网格、胶片带先显示内嵌小 JPEG 或有界 RGB8 缩略图，再升级为 200 像素预览。RGB 图要求单条带、交错 RGB8、每轴不超过 1024；位深或布局不符时交给原生解码器。
- 适屏使用最大照片预览；100% 通过 LibRaw 比较预览和有效图像尺寸，预览不足才解码传感器，不把小预览放大冒充原始分辨率。缓存和后台工作线程沿用现有实现。
- 小于 2 MP 的 JPEG 直接使用 Rust 解码，避免 Windows 缩放接口的固定开销。对 iPhone 样片导出的 852×640 JPEG，15 次预热后的缩略图解码中位数为 8.22 → 2.87 ms；此阈值是实测折中，不保证所有机器相同。

## DxO PureRAW 实测

经用户授权，从指定目录复制三张 DNG 到私有测试目录，测试前后核对 SHA-256。原始目录只读，评分、颜色标签、旗标、旋转和 XMP 保存均在副本上执行。

Windows release 构建，系统文件缓存已预热，每个级别先预热 1 次再测 7 次，表内为中位数。计时包含文件读取、CPU 解码和缩放，不包含 GUI/GPU 纹理上传；不同级别的清晰度不同，不能把小图解码时间当作全尺寸速度。

| PureRAW 样片 | 文件大小 | 首屏小图 193×129 | 200 px 缩略图 | 适屏 2560 px | 100% 6192×4128 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 20261018-DSC03309.dng | 73.82 MB | 0.092 ms | 35.181 ms | 78.100 ms | 73.466 ms |
| 20261018-DSC03486.dng | 84.81 MB | 0.089 ms | 45.375 ms | 90.691 ms | 84.331 ms |
| 20261019-DSC03708.dng | 93.10 MB | 0.106 ms | 49.617 ms | 94.296 ms | 95.450 ms |

三张样片均可直接读取内嵌预览。首次索引读取为 0.375–0.387 ms；原生入口首次调用为 6.57–7.23 ms，后者含 DLL 初始化，不能视为稳定的逐张解析差值。200 px、适屏和全尺寸 JPEG 解码耗时总体接近原生基线，主要改善是首屏小图和避免重复的原生元数据解析，并非全尺寸解码数十倍提速。

真实窗口已完成适屏、100% 左右切图、网格、评分、标签、拒绝和旋转测试，生成 5 张 GPU 截图，逐项查看适屏、100%、网格和旋转后画面；无黑图、花屏或界面错误。结果为 `first_draw=true`、`photos=3`、`fast_thumbs=3`、`error=None`。全部源照片和用于解码测试的初始副本哈希保持不变。

## 元数据与验证范围

DNG 内嵌 XMP 用于读取评分、色标、曝光、裁切和方向；外置旁车优先。首次编辑以内嵌 XMP 为模板建立同名 `.xmp`，保留 DxO 和其他未知字段，随后采用既有原子旁车保存机制。DNG 文件本身不改写。文件移动携带旁车。Lightroom 插件使用既有目录同步接口；Bridge/ACR 是否采用 DNG 外置旁车未做宿主真机验收，不能等同于修改内嵌 XMP。

148 项 Rust 测试通过，含三张 PureRAW 输出、Apple iPhone 6s Plus、Pentax K-x 和 Leica M9 DNG；覆盖小 JPEG、RGB8 缩略图、内嵌预览不足时的传感器回退、大小端、远处 SubIFD、循环和越界、位深检查、内嵌 XMP、评分与裁切、文件移动。Bridge 18 项测试通过；Lightroom Lua 模拟接口测试 58 项通过、1 项跳过。Clippy 所有 target 无警告。模拟接口不代表运行了 Lightroom/Bridge 宿主；当前没有 macOS 真机验收。

没有足够大的预览时，100% 需要传感器解码，iPhone/Leica 样片可耗时约 1–2 秒；不适用上表 PureRAW 的 73–95 ms。JPEG XL 等特殊 DNG 编码是否可全尺寸解码受现有 LibRaw 构建能力限制，未声称覆盖全部 DNG 版本和生成软件。

数值、哈希及验证记录见 [dng-validation-2026-10-23.json](dng-validation-2026-10-23.json)。可重建日志、样片副本、构建缓存和截图位于 `.agent-temp/2026-10-23-dng/`；交付程序不依赖该目录。

协议参考：[Adobe DNG 文档入口](https://helpx.adobe.com/camera-raw/desktop/dng-and-file-formats/digital-negative.html)、[LibRaw API](https://github.com/LibRaw/LibRaw/blob/master/doc/API-CXX.html)。公开验证样片来自 [raw.pixls.us](https://raw.pixls.us/)，逐份 SHA-256 和 CC0 来源记录见验证 JSON。


## 2026-10-24 最新修订：本地 RAW 窗口与内嵌 XMP

本节说明当前本地实现，替代本文开头同日较早补充中的 Adobe 安全更新流程。前文的 2026-10-23 性能表、测试数量及旁车策略作为历史记录保留；不能将它们当作当前独立 RAW 窗口的耗时或最终验收结果。

当前 **RAW 编辑** 在独立进程窗口运行，通过 LibRaw 解码及相机/DNG 色彩矩阵生成 sRGB 预览，不启动 Photoshop、Camera Raw 或其他 Adobe 软件。曝光与白平衡在本地 RAW 阶段处理，七项基础调整由 GPU 显示。窗口先呈现内嵌快速预览，再后台加载最多 1600 像素的适屏显影；全尺寸需明确请求。曝光/WB 使用 250 ms 去抖和过期处理取消，成功解包的当前照片可复用传感器缓冲。

DNG 的明确显影保存先写 XMP 旁车，再调用本地 `dng::write_xmp`：验证 Classic TIFF 目录和偏移，将新 XMP 与 IFD0 追加到同目录临时副本，最后原子替换原文件。已有 RAW 条带/瓦片数据不重新编码；DNG 元数据、文件大小和文件哈希会改变。BigTIFF、超过经典 TIFF 偏移范围、损坏目录和只读文件会拒绝内嵌更新。若旁车成功而内嵌失败，窗口明确提示并允许重试；普通选片评分仍沿用旁车保存路径。

与 LR/ACR 的互通以 XMP 字段为边界。本地引擎保留 DCP Look、Adobe 曲线、蒙版及其他未知设置，但不重现这些算法；本地色温/Tint 及灰点吸管估计也不与 Adobe 标度精确等价。即使参数相同，当前本地画面也可能与 LR/ACR 不同。完整操作和保存说明见 [RAW 编辑说明](RAW-EDITING.md)。

当前本地独立窗口的验证记录使用 [raw-window-validation-2026-10-24.json](raw-window-validation-2026-10-24.json)。此前 [raw-edit-validation-2026-10-24.json](raw-edit-validation-2026-10-24.json) 属于 Photoshop/Camera Raw 后端的历史验证，不证明当前本地后端的颜色、性能或测试覆盖。
