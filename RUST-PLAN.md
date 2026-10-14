# FastCull for ARW — Rust 重写规划

> 状态：Rust 2.0 已实现并完成开发验收，真实 Lightroom 全流程实机验收待完成。安装见 [README-culler.md](README-culler.md)，实测见 [验收记录](docs/RUST-VALIDATION.md)。
> 日期：2026-10-12

## 1. 背景与目标

Python 版（arw_culler.py）已完成功能性验证的载体，但启动慢（解释器 + import PIL/tkinter ≈ 1-2s 冷启动）、翻片延迟 150-200ms、分发依赖 Python 环境。Rust 版目标：

- **单文件 exe**（`fastcull.exe`，<10MB），不依赖 Python 或额外 VC 运行库，拷进插件目录即完成分发；需 Windows x64 和 OpenGL 3.3 驱动
- 翻片（预取命中）**< 10ms**，冷解码单张 **< 30ms**（zune-jpeg + 多线程）
- 与 Lua 插件端的桥接协议**保持不变**（Lua 只需把脚本路径换成 exe）

## 2. 冻结的接口规范（Rust 版必须逐条兼容）

Python 原型实现是这些协议的参考实现；对拍黄金用例见 §6。

### 2.1 CLI 子命令

```
fastcull.exe <目录或 .ARW 文件...>        # 直接启动 GUI
fastcull.exe --open <statefile>           # LR 插件调用：分离启动 GUI，后台写旁车（2.0.2）
fastcull.exe --apply-state <statefile>    # 仅写旁车，stdout 打 RESULT 行
fastcull.exe --from-state <statefile>     # GUI 从状态文件读照片列表
```

stdout 协议（供 Lua `parseSummary` 解析，`(%w+)=(%d+)`）：

```
RESULT wrote=<n> kept=<n> error=<n>
RESULT launched=1        # 或 launched=0 (原因文本)
```

失败时原因必须以可读文本输出（Lua 端会透出给用户）。

**2.0.2 启动语义修正**：为缩短点击到窗口的等待，`--open` 只校验并交付独立状态快照，立即启动 GUI；写旁车移到 GUI 后台。父调用返回 `RESULT wrote=0 kept=0 error=0 pending=N` 和 `RESULT launched=1`，完成统计/错误在窗口及日志显示。`--apply-state` 仍同步写入并输出实际统计。当前照片初始化完成才可编辑，一次性初始化防止后台覆盖用户评分。公开 CLI 参数、状态字段及 XMP 值语义保持不变。

### 2.2 状态文件（LR → 选片器）

- 路径：`%TEMP%\fastcull_state.txt`（Lua 用 `LrPathUtils.getStandardFilePath('temp')` 生成）
- 编码 UTF-8；每行 `path|rating|label|flag`
- 语义：**空串 = LR 无该值（写清）**；**字段缺失（不足 4 段）= 不改动**
- flag 值：`1`=pick / `-1`=reject / `0`=无（对应 LR `pickStatus`）

### 2.3 XMP 旁车

- 路径：`<ARW 同名>.xmp`，原子写（临时文件 + rename）
- 字段：`xmp:Rating`（0-5，0=清星）、`xmp:Label`（Red/Yellow/Green/Blue/Purple，空=清色）、
  `sr:Flag`（pick|reject，空=清，命名空间 `urn:zrtc:syncratings:1/`）、
  `tiff:Orientation`（1–8 的绝对方向，未旋转不写；以现有旁车方向或 ARW 原始方向为起点叠加旋转）
- **2026-10-12 修正**：原规划把相对旋转 delta 直接映射成绝对 Orientation，仅原图方向=1 时正确。Rust 使用方向组合，转满四次恢复原始方向并落盘；重开读回。CLI/状态文件/属性名均不变。
- **已有旁车只替换这四个属性**（属性式与元素式都要能匹配），其余字节不动——LR 开发设置必须原样保留
- 新建旁车用最小模板（含 xmp/tiff/sr 三个 xmlns）

### 2.4 ARW 解析事实（已实测，a6700 样片）

- 内嵌 JPEG 定位：遍历 TIFF IFD 链（IFD0 → SubIFDs → ExifIFD → next IFD），
  收集所有 `JPEGInterchangeFormat/Length`(0x0201/0x0202) 条目，**取长度最大者**为全尺寸机内 JPEG
  （a6700 实测 @0x84000 4.16MB 6192×4128；不同文件布局不同，如 t04 @0x81000）
- 所有 IFD/JPEG 指针都在文件头 512KB 内；只需 head 读 + jpeg 段读两次 IO
- **内嵌 JPEG 无 EXIF 方向、保持 sensor 横向**；竖拍方向只由 IFD0 Orientation(0x0112) 表达（1/6/8/3）
- EXIF 摘要（HUD 用）：ISO 0x8827、FNumber 0x829d、ExposureTime 0x829a、FocalLength 0x920a（rational，在 ExifIFD）

### 2.5 LR Lua 插件端（不变）

- `pickStatus`（SDK v4.0+，1/0/-1）读写均 pcall 防御
- `LrPrefs.prefsForPlugin()`、`LrPathUtils.getStandardFilePath('temp')`、`io.popen(cmd .. ' 2>&1')`
- Rust 默认路径改为插件内 `fastcull.exe`，Python 仅在手动选择 `.py` 回退时探测。
- 同时修复已有 Lua 缺陷：空色/空旗标的清除、`withWriteAccessDo` 写事务、带名字的 `callWithContext`、颜色名小写与显式清色、Windows 管道嵌套引号和唯一状态文件。协议字段不变。

## 3. Rust 技术选型

| 模块 | crate | 理由 |
|---|---|---|
| JPEG 解码 | `zune-jpeg` | 纯 Rust baseline JPEG 解码器（SIMD），已做样片实测；实际速度见验收记录 |
| 缩放 | `fast_image_resize` | SIMD box/bilinear resize |
| GUI | `egui`/`eframe` 0.33 + glow | 即时模式、OpenGL GPU 渲染、单 exe；固定深色主题；方向通过纹理坐标处理 |
| TIFF/IFD 解析 | 手写（~200 行） | 只读 head 512KB，无重量依赖需求；字节序 II/MM 都支持 |
| XMP patch | `quick-xml` 校验 + 原字节范围替换 | 字段固定 4 个，解析命名空间，保留其他内容，损坏文件拒绝覆盖 |
| CLI | `clap`（或手写，子命令就 3 个） | 依赖最小化优先 |
| 分离启动 | `std::process::Command` + `DETACHED_PROCESS`（`windows` crate flag 常量） | 与 Python 版 `CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS` 等效 |

实际线程模型：2–4 个解码 worker，其中一个只接主图；有界结果 channel；UI 线程上传纹理。全尺寸纹理最多 4 张且 300 MiB，缩略图最多 4096 项且 64 MiB。主图预取后续 2 张；超过缓存容量的 6 张循环预取会导致不断驱逐和重复解码，因此已收缩。预算不等于进程总内存上限。

实际源码：`src/arw.rs`、`imaging.rs`、`xmp.rs`、`state.rs`、`cache.rs`、`logger.rs`、`main.rs`、`ui.rs`。以下 UI 子模块拆分保留为后续维护方向，当前统一在 `ui.rs`。

## 4. 模块划分（crate 内）

```
src/
  main.rs        # CLI 分发（--open/--apply-state/--from-state/目录）
  arw.rs         # TIFF/IFD 解析 → JpegRef{offset,len} + Orientation + ExifSummary
  xmp.rs         # Sidecar::read/write（四字段 patch，原子写）
  state.rs       # 状态文件解析/应用（apply_state 语义：零值不新建空旁车）
  launch.rs      # DETACHED 启动自身（--open）
  ui/            # egui 界面
    loupe.rs     # 放大视图（fit/100%/点击切换/拖动平移）
    grid.rs      # 网格视图（6 列 cell：thumb+星旗色+文件名）
    film.rs      # 胶片带（78px cell，当前白框，reject 红叉）
    toolbar.rs   # 底部工具栏（旗标3/旋转2/星5/色5/缩放/导航）
    theme.rs     # LR Classic 配色常量（#1e1e1e/#2b2b2b/#c8c8c8...）
  cache.rs       # 解码池 + LRU + 预取
```

键位与交互规格照抄 Python 版 README（P/X/U、0-5、6-9+点击紫、[ ]、G/D/E、F/点击缩放）。

## 5. 里程碑

M1–M3 已实现。M4 的静态 release、插件默认程序迁移、分发包、Windows C-runtime popen/分离链路和原生 GUI 验收已完成；实际 LR 目录内的往返操作尚未在本次开发中验收，不以 SDK mock 替代实机结论。

- **M1 无头核心**：arw/xmp/state/launch + CLI，跑通 §6 黄金用例（与 Python 输出对拍）
- **M2 GUI 最小**：放大视图 + 翻片 + 打星/颜色/旗标 + 旁车即时写
- **M3 GUI 完整**：网格/胶片带/旋转/100% 平移/EXIF HUD
- **M4 集成分发**：`--open` 链路接 Lua 插件实测、release 构建（opt-level=3, lto）、
  Lua 端 cullerPath 默认探测改为 `fastcull.exe` 优先、更新 README

## 6. 对拍黄金用例（Python 版测试即规格）

`.agent-temp/2026-10-12-raw-speed/` 下（注意该目录不同步，Rust 开发时需重建用例）：

- `test_culler.py`：4 张测试 ARW（t01-t04）解析/方向（竖拍 h>w）/旁车读写全语义（新建/patch/插入/清星/flag/orient）
- `test_state.py`：状态文件 4 段语义、零值跳过、cmd_open、CLI 端到端
- 测试 ARW 源：`exraw-test/`（a6700 ISO5000 竖拍 + 白天 600mm 横拍；跨机需随项目带走）

验收标准：同一输入下，Rust 与 Python 产出的旁车在语义上逐字段一致（Rating/Label/Flag/Orientation + 保留字段不丢失）。

## 7. Python 原型期间的纪律

- 任何接口改动（CLI 参数、状态文件格式、XMP 字段、RESULT 行）**必须同步更新本文档 §2**
- UI 行为规格变化同步 §4/README 键位表
- 不为性能优化 Python 版（除非影响功能验证）
