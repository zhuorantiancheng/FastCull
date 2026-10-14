# FastCull for ARW 开发记录（Python 原型全程）

> 后续 Rust 版已提供 2.7.1 Windows 分发包（2026-10-14）：安装器可选 Bridge/LR 注册，另有便携包与干净源码包。当前入口见 [README.md](README.md)、[分发说明](docs/DISTRIBUTION.md) 和 [本次验证记录](docs/DISTRIBUTION-VALIDATION-2.7.1.md)。下文保留原始 Python 开发记录。

> 2026-10-11 ~ 10-12，单日冲刺完成。本文档面向 Rust 重构：需求演化、最终规格、
> 全部踩坑与对策。架构规划另见 [RUST-PLAN.md](RUST-PLAN.md)（接口协议以其 §2 为准）。

## 目录

1. [项目概览与时间线](#1-项目概览与时间线)
2. [背景机制研究](#2-背景机制研究)
3. [需求演化全记录](#3-需求演化全记录)
4. [最终功能规格](#4-最终功能规格)
5. [踩坑清单（Rust 版必读）](#5-踩坑清单rust-版必读)
6. [最终架构](#6-最终架构)
7. [测试体系](#7-测试体系)
8. [已知限制与 Rust 期注意事项](#8-已知限制与-rust-期注意事项)
9. [文件清单](#9-文件清单)

---

## 1. 项目概览与时间线

**目标**：Sony ARW 高速选片器 + Lightroom Classic 双向集成。核心机制是读 ARW 内嵌的
机内全尺寸 JPEG（约 79ms/张）做浏览显示，不触碰 37MB RAW sensor 数据；评分经
.xmp 旁车与 LR 双向同步。

| 时间 | 里程碑 |
|---|---|
| 10-11（前一会话） | Sony Viewer SDM 逆向（Ghidra 工程 viewer_sdm）、iedt-bridge 插件、XMP 桥接工具 |
| 10-12 上午 | 机制研究：ARW 结构实测、Viewer.exe 逆向（libjpeg-turbo/OpenJPEGFileAtFirst）、79ms vs 1300ms 基准 |
| 10-12 午后 | 选片器 MVP（tkinter）→ LR 插件（读取/同步）→ 旗标/旋转/网格/胶片带 LR 风格 UI |
| 10-12 下午 | 实机调试马拉松（8 个生产 bug 逐一暴露修复）、日志系统、Rust 规划 |

## 2. 背景机制研究

### 2.1 ARW 文件结构（a6700 实测，t01-t04 样片）

TIFF 结构（小端 II），所有 IFD/JPEG 指针都在文件头 512KB 内：

```
IFD0 @0x8
├─ JPEGInterchangeFormat=196770/len 341,856   → 1616×1080 预览 JPEG
├─ SubIFDs → RAW strip（6656×4608 14bit Sony Lossless, ~37.4MB, offset 4,702,208）
├─ ExifIFD @0x11c0（MakerNote 38KB + 全部拍摄参数）
└─ next → IFD @0xaa50（160×120 缩略图）→ next → IFD @0x21bee
    └─ JPEGInterchangeFormat=540,672/len 4,160,231  → 6192×4128 全尺寸机内 JPEG ★
```

要点：
- **取全尺寸 JPEG 的规则：遍历全部 IFD，收集 0x0201/0x0202 条目，取长度最大者**。
  不同文件布局不同（t04 在 0x81000、2.57MB）。
- **内嵌 JPEG 无 EXIF、保持 sensor 横向**；竖拍方向只由 IFD0 Orientation(0x0112)
  表达（值 1/6/8/3），显示时需 transpose。
- 用字节扫描找 FFD8/FFD9 会误报——Sony Lossless 压缩 RAW 里大量伪 SOI 序列，必须走 IFD。
- EXIF 摘要字段：ISO 0x8827(H)、FNumber 0x829d(rational)、ExposureTime 0x829a(rational)、
  FocalLength 0x920a(rational)、Model 0x0110(ascii)，都在 ExifIFD。

### 2.2 Sony Viewer 为什么快（逆向证据）

- Viewer.exe 静态链接 **libjpeg-turbo 2.1.5.1**（版本字符串 @0x140594478 + 完整错误表）
- 渲染走 GDI+ `GdipCreateBitmapFromScan0`+`GdipDrawImageI`
- 注册了配置项 `OpenJPEGFileAtFirst/OpenTIFFFileAtFirst/OpenHEIFFileAtFirst`
  （注册函数 FUN_1400292a0/140029870）——"优先以 JPEG 打开"是官方设计
- RAW 解码器 FUN_140150f20 只被编辑/导出/SDM/放大路径调用

### 2.3 性能基准（同机同文件）

| 路径 | 耗时 | 读取量 |
|---|---|---|
| 内嵌 JPEG 解码（PIL/libjpeg-turbo） | 79 ms | 4.2 MB |
| LibRaw 全 RAW 解码+demosaic | ~1300 ms | 37.4 MB |

（LR/ACR 还要在此之上做色彩/矫正/降噪并建预览缓存，首开更慢。）

## 3. 需求演化全记录

### 阶段 1：选片器 MVP
- **需求**："根据这个机制给 LR 和 ACR 写一个选片插件，选片阶段只读全尺寸 jpg，可以打星和加色"
- **决策**：ACR 无插件接口 → 写标准 XMP 旁车即 ACR/Bridge 的集成方式；LR 走
  "独立选片器写旁车 + LR 端小插件同步目录"（业界 FastRawViewer 模式）。
- **实现**：arw_culler.py（tkinter）+ XmpSidecar（只 patch Rating/Label）。

### 阶段 2：LR 直接打开 + 双向
- **需求**："在 LR 里能直接打开选片器，有读取和同步功能"
- **实现**：OpenCuller.lua（LR 评分→%TEMP% 状态文件→python `--open` 写旁车+DETACHED
  分离启动 GUI）+ SyncSelected.lua（旁车→目录批量写）。
- 照抄 iedt-bridge 已验证的桥接模式（io.popen→Popen、path|rating|label 管道分隔状态文件）。

### 阶段 3：LR 原生风格
- **需求**：附 LR Loupe 截图，"操作逻辑和 UI 尽可能和 LR 一样"
- **实现**：深色配色、底部居中工具栏（旗标3/旋转2/星5/色5/缩放/导航）、胶片带、
  网格视图、LR 键位（P/X/U、0-5、6-9、[ ]、G/D/E、F）。
- **决策**（用户未及时答复按推荐执行）：旗标经自定义 `sr:Flag` 属性桥接
  （XMP 无旗标标准字段）；Survey 对比视图未做。

### 阶段 4：实机调试马拉松（8 个生产 bug）

按暴露顺序（每个都在 §5 详述）：
1. `prefsForApp` 不存在 → `prefsForPlugin`
2. `os.getenv` 沙箱禁用 → `LrPathUtils.getStandardFilePath('temp')`
3. `pick` 键 Unknown → 官方手册确认键名 `pickStatus`
4. "选片器启动失败"无原因 → python 输出透出 + stdout reconfigure
5. 用户提出**分发需求**（不绑本机路径）→ 插件目录自包含 + Python 自动探测
6. 打开后卡死 → tkinter 跨线程 after_idle 死锁（消息队列架构修复）
7. 主图仍空白 → 单 worker FIFO 解码排队（当前张插队修复）
8. 还是空白 → **画布创建后未 pack**（1×1 不可见，真凶）

### 阶段 5：日志系统
- **需求**："加上日志系统，崩溃了知道哪里出问题"
- **实现**：`%LOCALAPPDATA%\FastCull\logs\` 统一目录；GUI 崩溃弹窗+写堆栈、
  headless 只写日志（弹窗=挂死的教训）；tkinter 回调异常钩子；渲染打点
  （canvas 尺寸——正是它定位了 bug 8）。
- 开发中还引入了"视觉读图用 GLM-5.3-Flash 子代理"的工作方式。

### 阶段 6：Rust 规划
- **用户方向**："Python 只做功能验证，之后用 Rust 重构，开发时提前规划"
- 产出 RUST-PLAN.md（冻结接口协议 + crate 选型 + 里程碑）+ 本文档。

## 4. 最终功能规格

### 4.1 窗口布局（自上而下）

```
┌────────────────────────────────────────────────┐
│ 24 / 340   DSC01335.ARW      ISO400 f/6.3 1/400s 350mm │ ← 信息条 (#2b2b2b)
├────────────────────────────────────────────────┤
│                                                │
│              放大视图画布 (#1e1e1e)              │ ← fit 居中 / 100% 可拖动
│                                                │
├────────────────────────────────────────────────┤
│   ⚑ ○ ⊗ │ ↺ ↻ │ ★★★★★ │ ■■■■■ │ F │ ◀ ▶      │ ← 工具栏(居中)
├────────────────────────────────────────────────┤
│ ◀ [thumb][thumb][thumb][thumb][thumb] ▶        │ ← 胶片带(高86px)
└────────────────────────────────────────────────┘
```

- 网格视图（G）：6 列 × cell 180×224；cell = thumb + 旗标符号 + 星串 + 色块 + 文件名，
  当前照片白框 2px
- 胶片带：格宽 78px、thumb 64px；当前项白框；reject 项画红叉两道；
  箭头按钮 = 跳 4 张

### 4.2 配色（LR Classic 深色）

| 用途 | 值 |
|---|---|
| 画布背景 | `#1e1e1e` |
| 面板（信息条/工具栏/胶片带） | `#2b2b2b` |
| 分隔线 | `#3f3f3f` |
| 文字 | `#c8c8c8`，高亮 `#ececec` |
| 星实/空 | `#e8e8e8` / `#5a5a5a` |
| 选中框 | `#e8e8e8` |
| reject | `#c94f4f` |
| 标签色（LR 柔和色） | 红 `#ce4b42` 黄 `#e2b937` 绿 `#77ab4f` 蓝 `#5a79a5` 紫 `#a27fa5` |

### 4.3 键位（与 LR 一致）

| 键 | 功能 | 键 | 功能 |
|---|---|---|---|
| P / X / U | 旗标 拾取/拒绝/清除 | 0-5 | 星级（0=清星） |
| 6/7/8/9 | 红黄绿蓝（同键取消；**紫无快捷键是 LR 原生设定**，点击色块） | [ / ] | 左/右旋转 |
| → / 空格 / PgDn | 下一张 | ← / PgUp | 上一张 |
| G | 网格视图 | D / E / 回车 / 双击 | 放大视图 |
| F / 点击图片 | fit ↔ 100%（100% 下拖动/方向键平移） | Home / End | 首/末张 |
| L | 打开日志目录 | Q / Esc | 退出 |

### 4.4 数据语义（Rust 必须逐条兼容，详见 RUST-PLAN §2）

- **评分写入即时原子落盘**（tmp + os.replace）
- 状态文件 `path|rating|label|flag`：**空串=写清，字段缺失=不动**；flag 1/-1/0
- XMP 四字段 patch：`xmp:Rating`(0-5)、`xmp:Label`(5 色英文名)、
  `sr:Flag`(pick|reject，命名空间 `urn:zrtc:syncratings:1/`)、
  `tiff:Orientation`（Python 原型写旋转 delta；Rust 2.0 修正为叠加原始方向的绝对方向，见 §10，未旋转不写）
- LR→选片器方向："LR 目录是当前真相"（会覆盖旁车旧值；零评分+无旁车时不新建空文件）
- 旗标桥接：LR `pickStatus`(1/0/-1) ↔ `sr:Flag` ↔ 选片器 flag 三态

### 4.5 性能/缓存参数（Python 版实测值，Rust 可调）

| 参数 | 值 | 说明 |
|---|---|---|
| 全尺寸图 LRU | 4 张 | ~300MB 内存上限 |
| 缩略图缓存 | 4096 | thumb200(网格) + thumb64(胶片带) |
| 预取窗口 | 6 张（向后） | 翻页命中时 ~100ms 显示 |
| 解码线程 | 1（Python 版限制） | **Rust 应做优先队列**：当前张插队（见坑 7） |
| 渐进解析 | 胶片带每帧 ≤2 项、网格 ≤8 项 | 防主线程长 IO |
| 轮询间隔 | 30ms（worker→UI 消息队列） | Rust 用 channel + 请求重绘即可 |

## 5. 踩坑清单（Rust 版必读）

每条：现象 → 根因 → Python 修复 → **Rust 对策**。

### 坑 1：`LrPrefs.prefsForApp` 不存在
- 现象：`common.lua:17: attempt to call field 'prefsForApp' (a nil value)`
- 根因：记忆混淆，正确 API 是 `LrPrefs.prefsForPlugin()`（可无参调用）
- **Rust**：Lua 端照抄现实现即可（FastCull.lrplugin/common.lua 是修复后的参考）。

### 坑 2：LR 沙箱禁 `os.getenv`
- 现象：`common.lua:29: attempt to call field 'getenv' (a nil value)`
- 根因：LR 的 os 表是受限子集
- 修复：`LrPathUtils.getStandardFilePath('temp')`（pcall + fallback `_PLUGIN.path`）；
  localAppData 同理（日志目录）
- **Rust**：同上，Lua 端不变；Rust 侧自己用系统 API 取目录。

### 坑 3：旗标键名是 `pickStatus` 不是 `pick`
- 现象：`Unknown key: "pick"`
- 根因：社区笔记以讹传讹；官方手册（lrc.mcor.dev/modules/LrPhoto.html）：
  `getRawMetadata('pickStatus')` / `setRawMetadata('pickStatus', 1|0|-1)`，SDK v4.0+
- 修复：改键名 + 双向 pcall 防御（读失败按 0，写失败计数并在结果对话框说明）
- **Rust**：照抄；pcall 防御保留（兼容旧 LR）。

### 坑 4：python 失败原因被吞
- 现象：LR 弹"选片器启动失败，请手动运行"，无原因
- 根因：python 输出只被 parseSummary 摘要化，失败时不透出
- 修复：`runCuller` 加 `2>&1`；失败对话框显示输出前 300 字符；
  python stdout `reconfigure(utf-8, errors=replace)`（防中文文件名在 cp936 管道崩溃）
- **Rust**：exe 的 stderr 合并进 stdout 输出；所有错误信息必须可读且进 stdout。

### 坑 5：分发不能绑机器
- 需求：用户明确"要分发到别的电脑"
- 修复：arw_culler.py 副本放进插件目录（defaultCullerPath 优先插件内）；
  Python 探测链 `py -3` → `python` → 本机兜底；命令构造规则：路径含 `\` 加引号、
  `py -3` 这类命令原样拼接；PIL 延迟导入（headless 不依赖）+ 缺失时明确提示安装命令
- **Rust**：单 exe 天然解决——Lua 端 cullerPath 默认指向插件内 `fastcull.exe`，
  不再需要 Python 探测逻辑（可留作回退）。

### 坑 6：tkinter 跨线程调用死锁
- 现象：窗口打开后直接卡死（无响应）
- 根因：worker 线程解码完成后直接 `root.after_idle(...)`；tkinter 非线程安全，
  Windows 上跨线程碰 Tcl 解释器即死锁
- 修复：`queue.Queue` + 主线程 30ms 轮询统一刷新
- **Rust**：天然无此问题，但纪律相同——**worker 只发消息，UI 只在主线程**。
  用 channel（`std::sync::mpsc`/egui 的 request_repaint）。

### 坑 7：解码任务排队饿死当前张
- 现象：340 张大库，主图空白数秒，胶片带预取缩略图反而先出
- 根因：单 worker FIFO，翻页时当前张解码排在 6 个预取任务后
- 修复：当前张提交前 cancel 排队中的预取 futures + `pending.clear()`
  （被取消任务 finally 不执行的次生坑）
- **Rust**：**架构级解决**——解码调度用优先队列（优先级=|idx-当前idx|），
  翻页即重排；多 worker（≤4）时按优先级取任务。

### 坑 8：画布创建后未 pack（真凶，最难找）
- 现象：主图永远空白但无任何报错；胶片带缩略图正常
- 根因：`_build_ui` 创建 loupe 画布后没有 pack，布局代码只写在视图切换函数里，
  而默认视图从不触发切换 → 画布 1×1 从未显示（深灰色是背景面板）
- 定位：靠渲染打点日志 `loupe drawn … canvas=1x1` 一眼看出
- 修复：build 时 pack 默认视图
- **Rust**：egui 无显式 pack（每帧声明布局），天然免疫；但**教训通用**：
  默认状态的控件路径必须验证可见，测试断言要查**实际布局尺寸**而非对象存在。

### 坑 9：tkinter 回调异常静默
- 根因：tkinter 把回调异常交给 `report_callback_exception`（默认打 stderr），
  不经过 `sys.excepthook`；分离进程 stderr=DEVNULL → 彻底无声
- 修复：`root.report_callback_exception = 写日志`
- **Rust**：panic 默认有 hook，UI 线程 panic 要 catch（egui 下 `catch_unwind` 包 run）
  并写日志。

### 坑 10：无人值守路径弹窗=挂死
- 现象：`--apply-state` 遇输入文件不存在 → 崩溃钩子弹 tk 对话框等人点 →
  LR 的分离调用链里永远没人点 → 进程挂死（实测复现）
- 修复：崩溃钩子分 interactive（GUI 弹窗）/headless（只写日志+stdout+`os._exit(1)`）；
  且 `os._exit` 前必须 `logging.shutdown()`（否则堆栈缓冲不落盘——也实测确认）
- **Rust**：headless 子命令出错直接 eprintln + exit code，绝不交互。

### 坑 11：XMP patch 细节
- 已有 LR 旁车必须只替换目标属性（属性式 `xmp:Rating="3"` 与元素式
  `<xmp:Rating>3</xmp:Rating>` 都要匹配），`crs:*` 开发设置逐字节保留
- rating=0 写 `"0"`（LR 的清星表示）；label/flag 空串写 `""`（=清除）；
  新建旁车用最小模板（xmlns: xmp/tiff/sr）
- 原子写：tmp 文件 + rename
- **Rust**：字符串/正则处理同语义；建议对拍 Python 输出（§7 黄金用例）

### 坑 12：工程环境坑（开发效率类）
- Git Bash 下 sed/heredoc 多层转义吃反斜杠（`\\`→空、`\n`→真换行），改批量文件
  **一律用 Python 脚本按行/按锚点做**
- lupa 迭代 Lua table 产生 keys 而非 values（用 `.items()`）
- 本会话 Bash 工具在 shell 退出时清理子进程树——**分离进程存活验证必须在
  单条命令内轮询完成**
- exiftool 重写 ARW 会破坏文件（Viewer 判损坏）——早期结论，XMP 一律走旁车

### 观察（非坑但要知道）
- LR `getTargetPhotos()` 返回顺序=用户选中顺序：用户从第 24 张开始选中时，
  选片器起点就是第 24 张（不是第 1 张）。行为合理，勿当 bug 修。
- 用户照片库 340 张时首开冷解码明显慢于测试库（60 张 SSD）——磁盘是第一瓶颈，
  Rust 版预取并发 + 更快 JPEG 解码（zune-jpeg）直接受益。

## 6. 最终架构

```
Lightroom Classic
  └─ FastCull.lrplugin（Lua，5 文件 + arw_culler.py 副本）
       ├─ OpenCuller.lua     读取方向: 选中照片 → %TEMP%\fastcull_state.txt
       │                     (path|rating|label|flag) → io.popen: python --open
       ├─ SyncSelected.lua   同步方向: 读旁车 → withWriteAccess 批量写目录
       │                     (rating/colorNameForLabel/pickStatus)
       ├─ common.lua         prefs(prefsForPlugin)/路径(getStandardFilePath)/
       │                     python 探测/runCuller(popen 2>&1)/日志(pcall 全包)
       ├─ PluginInfo.lua     设置面板（pythonPath/cullerPath 可改）
       └─ info.lua           LrLibraryMenuItems 双菜单

arw_culler.py（选片器，单文件）
  ├─ ArwInfo        TIFF/IFD 解析 → jpeg_off/len + orientation + exif 摘要
  ├─ XmpSidecar     read(3 元组)/write(4 字段 patch，原子)
  ├─ decode_arw     内嵌 JPEG → PIL Image（transpose orientation）
  ├─ CullerApp      tkinter UI（loupe/grid/filmstrip/toolbar）
  │    ├─ 解码: ThreadPoolExecutor(1) + LRU + 预取(6) + 当前张插队
  │    └─ 线程: worker→queue→主线程 30ms 轮询刷新
  ├─ state 协议     parse_state_file/apply_state（零值不新建空旁车）
  ├─ launch         --open: apply 后 DETACHED_PROCESS 分离启动自身 --from-state
  └─ 日志           %LOCALAPPDATA%\FastCull\logs\，GUI/headless 分裂钩子，
                    tkinter 回调钩子，渲染/解码打点
```

数据流：LR 目录 --(状态文件)--> 旁车 <--(即时读写)-- 选片器 --(RESULT 行)--> LR 弹窗

## 7. 测试体系

| 测试 | 验证 | Rust 对拍价值 |
|---|---|---|
| test_culler.py | 4 张样片解析/方向（竖拍 h>w）/XMP 全语义（新建/patch/插入/清星/flag/orient/保留 crs） | ★★★ 协议黄金用例 |
| test_state.py | 状态文件 4 段语义/零值跳过/cmd_open/CLI 端到端 | ★★★ |
| lua_logic_test.py | lupa 模拟 LR SDK 跑 SyncSelected（rating/label/pickStatus 写入） | ★ |
| lua_open_test.py | lupa 模拟跑 OpenCuller（状态文件内容/命令行/摘要） | ★ |
| stress_test.py | 60 张快速连翻+网格+跳转；断言**主图 item>0 且画布宽>100**、限时 | ★★ Rust 版照抄断言思路 |
| gui_smoke.py | GUI 操作全流程 + 截图 + 旁车终态校验 | ★ |
| repro_popen.py | cmd.exe 管道全链路（launched=1 + 分离进程存活） | ★★（Rust 换 exe 后重写） |

**测试资产**：`.agent-temp/2026-10-11-sony-rea/exraw-test/` 4 张 ARW
（t01-t03 同片竖拍 ISO5000、t04 横拍 600mm 不同布局）。**该目录不同步**，
Rust 开发前先确认存在/复制到正式位置。视觉读图验证用 GLM-5.3-Flash 子代理
（工作方式见项目记忆）。

## 8. 已知限制与 Rust 期注意事项

1. **性能**：Python 冷启动 1-2s；翻片未命中预取时 100-500ms（磁盘解码）。
   Rust 目标：冷启动 <100ms、命中 <10ms、未命中 <30ms。
2. **未实现**：Survey/对比视图（N）、播放、多选、修改日期、双屏。需求出现时先补协议再实现。
3. **iGPU 内存**：全尺寸 LRU=4 张 ≈ 300MB。Rust 用 GPU 纹理时注意上限。
4. **LR 端协议冻结**：CLI 子命令/RESULT 行/状态文件/XMP 字段都不能变
   （RUST-PLAN §2），Lua 端只换目标 exe。
5. **横竖屏照片方向**：以 ARW orientation 为准 transpose；旋转功能叠加用户 delta
   后写 `tiff:Orientation`（只影响 ACR/Bridge，LR 目录方向不自动应用——文档已注明）。
6. **Lua 与 exe 的编码**：状态文件 UTF-8；exe stdout 的 RESULT 行保持 ASCII 数字；
   错误信息可含中文（Lua 端按字节透出）。

## 9. 文件清单

```
IEDT/
├─ arw_culler.py                     选片器主程序（开发主副本）
├─ README-culler.md                  使用说明（含分发）
├─ RUST-PLAN.md                      Rust 迁移规划（接口协议冻结）
├─ DEVLOG.md                         本文档
├─ xmp_bridge.py                     （早期）Viewer↔XMP 桥
├─ iedt-bridge.lrplugin/             （早期）LR↔Viewer 桥，坑 1/2 已同步修复
├─ lrplugins/FastCull.lrplugin/      ★交付物：插件目录（自包含，拷走即分发）
│   ├─ info.lua / PluginInfo.lua / common.lua
│   ├─ OpenCuller.lua / SyncSelected.lua
│   └─ arw_culler.py                 主副本的同步副本
└─ .zcode/memory/                    项目记忆副本
```

日志（运行时生成）：`%LOCALAPPDATA%\FastCull\logs\fastcull_app_*.log`（每次启动一份，
保留 20）与 `fastcull_plugin_YYYYMMDD.log`（按天）。

## 10. Rust 2.0 重构（2026-10-12）

Rust 版已实现，交付 `lrplugins/FastCull.lrplugin/fastcull.exe` 和 `dist/FastCull-Rust-2.0.0-win64.zip`。构建入口 `scripts/build.ps1`，依赖锁定在 `Cargo.lock`，正式使用说明更新为 [README-culler.md](README-culler.md)，原 Python README 备份在 `backups/`。

实现覆盖主图、100% 缩放与平移、六列网格、可见胶片带、LR 键位、星级/标签/旗标、旋转和 EXIF。ARW 解析支持 II/MM、SubIFDs 数组、乱序 JPEG 标签、IFD 环路和范围检查。只读嵌入 JPEG，不写 ARW。

性能路径采用 zune-jpeg、SIMD 缩略图缩放、2–4 个后台线程、主图保留线程、导航时重排队列、GPU 纹理方向变换。全图缓存同时受数量与字节预算约束，预取收缩为后续 2 张，防止缓存不足时无限重复预取。

额外修复原型问题：

- 旋转增量不再直接写成绝对方向；对 ARW/已有旁车方向进行组合，四次恢复并能重开读回。
- 空 rating 字段显式清星，空色和空旗标可清除 LR 旧状态；字段缺失仍保持。
- XMP 新字段补齐命名空间，兼容单引号和前缀别名；其他字节保持，损坏 XML/非 UTF-8 拒绝覆盖。
- Lua 使用 SDK 的 `withWriteAccessDo` 和有名字的 `callWithContext`；颜色写入用小写色名，清色写空 label。
- exe 路径不再触发 Python 检查，旧默认路径可迁移；管道对中文/空格程序及状态路径做了实测。
- LR 状态文件唯一命名，分离 GUI 持有独立快照，不受后一轮打开覆盖影响。
- 固定深色主题，避免系统主题覆盖；首次主图日志记录真实画布与图像尺寸。

开发验收：14 项 Rust 回归、5 项 Lua/SDK mock 回归；4 张样片解析与 XMP 对拍；真实 GPU 窗口截图；340 项列表、180 次跳转/网格切换；Windows C runtime popen → exe → 分离 GUI，包括中文、空格和父状态文件覆盖。另修复工具栏焦点截走快捷键和空格同时触发按钮的问题。实际 LR 全流程还需实机验收。具体速度、内存、截图和局限统一记录在 [Rust 验收记录](docs/RUST-VALIDATION.md)，规划目标不作为已达成的性能宣称。

### 2.0.1：实际旁车的缺失命名空间兼容修复

用户在 LR 打开时遇到 `invalid XML attributes: ... duplicated attribute`。读取实际旁车发现原文件没有重复属性，但 Python 原型曾插入 `sr:Flag` 而未补 `xmlns:sr`。Rust 依据命名空间 URI 识别字段时漏识别旧属性，随后同时新增声明与同名 `sr:Flag`，导致输出重复、原子写前校验失败、GUI 未启动。复现在两张实际旁车副本和最小回归中均命中相同错误。

修复：仅为命名空间未绑定的冻结字段（`xmp:Rating/Label`、`sr:Flag`、`tiff:Orientation`）识别旧前缀，在所属 rdf:Description 上补声明，更新原值；已绑定的前缀不重解释。只修改命名空间声明和目标字段。真正的重复属性继续拒绝写入。

验证：17 项 Rust 回归和 5 项 Lua 回归通过，Clippy 无警告。两张实际 RAW/XMP 的副本通过 `--apply-state` 与 `--open` 原生窗口验证，严格 XML 校验通过，其他字节保持；核验照片目录中的原始 RAW/旁车均未改动。正式程序已更新，新增分发包 `dist/FastCull-Rust-2.0.1-win64.zip`。本次中间文件位于 `.agent-temp/2026-10-12-xmp-duplicates/`，实际照片副本不进入分发包。

### 2.0.2：启动等待与 100% 翻片修复

用户报告点击打开到 GUI 出现过慢，且要求 100% 下左右键仍选上一张/下一张。最小真实键盘事件测试先复现：右箭头只改变 pan、不改变 current；修复后左右键在所有缩放模式下翻片，保持 100% 并重置平移，鼠标拖动仍可平移。

启动测试发现两个等待：`--open` 在窗口前顺序写完所有旁车；即便子 GUI 标准输入输出设为 null，仍继承了父进程其他标准管道句柄，导致 C runtime `io.popen` 等待 GUI 退出才读完。修复后清除这些父标准句柄的可继承位，启动管道及时返回；窗口后台以 Session 一次性初始化各照片，解码前确认当前照片初始化完成。后台与解码线程共享 OnceLock，评分编辑不会被稍后的初始化覆盖，重复 RAW 条目按 LR 列表最后值初始化一次。关闭窗口也会完成剩余 handoff。

Lua 使用两次批量 SDK 调用读 path/rating/pickStatus 和 label，支持失败/旧环境回退；不再逐张重复调用四个元数据 getter，也不弹成功提示挡住窗口。后台初始化的进度和失败数在选片器显示。升级需重新载入 Lua 插件模块一次。

340 项同机测试（只测 cmd/exe/GUI，排除 LR 导出时间）：2.0.1 首图约 1139ms、管道返回约 7650ms；首个 2.0.2 候选首帧回调约 412ms、首图约 609ms、管道返回约 196ms。具体日志与对比在 `.agent-temp/2026-10-12-startup/`；性能脚本为 `scripts/startup_probe.py`，原生脚本也使用真实键盘事件验收 100% 翻片。验证记录见 `docs/RUST-VALIDATION.md`，正式 exe 及分发包更新至 2.0.2。


### 2.0.3：高优先级改进

增加缩略图/适屏/100% 三级加载，适屏优先读取中等内嵌 JPEG，100% 使用完整像素。当前图优先、按方向预取、请求代次淘汰旧结果，非阻塞有界邮箱避免旧任务占满管道；最多两路全尺寸解码。导航输入先于大纹理转换，缓存不足时优先保留邻近预览，避免远预取挤掉下一张。

图像与元数据独立加载。损坏 XMP 不阻止看图，各图像等级的失败互不阻断，小 JPEG 失败可回退完整 JPEG；R 重试可恢复修好的旁车。失败初始化允许重试，成功初始化不重复覆盖后续评分。先新增回归观察损坏旁车导致正常 JPEG 加载失败，再实现分离并验证通过。

新增持久会话清单与每字段修改版本。Rust 统一读取 XMP，Lua 删除模式匹配路径。按原目录、原照片 UUID/路径及会话选区同步，仅写编辑过的评分/颜色/旗标；已确认快照不重放，活动会话后续修改仍待下一轮。读取/部分写入失败保留待同步状态；关闭且完全同步的会话移出偏好队列，磁盘清单保留。旧 CLI/状态语义保持兼容，手动选区同步保留为单独菜单。

30 项 Rust 与 15 项 Lua/实际 Rust 后端回归通过，Clippy 无警告，四张样片黄金对拍、原生 GPU 截图、340 项混合适屏/100% 快速跳转、Windows popen 与会话关闭导出验证通过。100% 首次大图约 0.11 秒、命中缓存约 1ms（到绘制帧，非显示器呈现时间）。详情与边界见 `docs/RUST-VALIDATION.md`、`docs/performance-2.0.3.json`。正式 exe/zip 为 2.0.3；临时验收数据在 `.agent-temp/2026-10-12-high-priority/`，不依赖它运行。真实 LR 目录仍需实机验收。


### 2.0.4：LR SDK 会话目录参数修复

用户在 LR 打开时收到 `[string common.lua]:258: 无法找到本地会话目录`。原因是 Lua 使用了 SDK 不支持的 `localAppData` 标准目录参数；原 mock 对所有参数都返回测试路径，掩盖了错误。将 mock 限制为文档中的合法参数后，原生打开与后台启动两项入口回归立即失败，复现相同目录错误。

改用 `appData`，取不到路径或目录创建失败时回退 `appPrefs`；使用 `createAllDirectories` 并检查布尔返回和错误原因。插件日志同样修复，Rust 日志仍在 LOCALAPPDATA。现有会话仍按偏好中保存的完整路径读取，不迁移或删除。新增目录存在/缺失/创建失败、禁用 os.getenv、日志落盘与 SDK 入口到真实 Rust GUI 的回归。正式版本升为 2.0.4，图像引擎没有逻辑变更。临时验收目录 `.agent-temp/2026-10-12-session-path/`。

验证结果：30 项 Rust 与 21 项 Lua/SDK/可选窗口回归全部通过。已确认SDK目录生成、实际 Rust 窗口启动、关闭会话、切换选区后仅同步最后编辑照片的三类字段；插件日志也能实际落盘。未操作真实 LR 目录，需重新载入插件后实机复测。

### 2.0.5：Windows 启动句柄隔离

LR 报 `clear inherited stdio: 拒绝访问 (os error 5)`。旧版启动前修改标准流的继承标志，遇到不能修改的句柄就退出。`scripts/launch_probe.py` 在真实 Win32 进程中给标准输入传入伪句柄，复现完全相同的 error 5；另一个标准流表以外的管道写句柄复现旧版等 GUI 关闭才返回的问题（6384.6ms）。

改用 `CreateProcessW` 且禁止句柄继承，GUI 标准流为空，不再改动调用方句柄。可执行路径和 UTF-16 参数分别处理，保留环境/工作目录；关闭创建返回的两个句柄，快照启动失败清理、成功由子进程清理。没有改动图像引擎和会话同步协议。版本更新为 2.0.5。

32 项 Rust、21 项 Lua/SDK/实际窗口回归通过，Clippy 无告警。正式 exe 在中文/空格路径下通过拒绝修改标准流、隐藏管道、普通管道三项原生回归，返回 126.0/124.1/127.7ms，实际绘制和关闭会话均成功。详见 `docs/RUST-VALIDATION.md`；临时复现和验收在 `.agent-temp/2026-10-12-launch-handles/`。真实 LR 入口仍待用户重新加载后确认。

340 项 `_wpopen` 启动、后台初始化与最后编辑保留验证通过。单次同环境对照旧版返回 172.8ms/首图 446.2ms，新版返回 161.4ms/首图 421.9ms；首轮新版首图出现过 1528.7ms 波动，未清系统缓存，不能作为冷启动保证。验收脚本还修复了结果文件存在但未写完的读取竞态。原始数字归档到 `docs/launch-validation-2.0.5.json`。

### 2.0.6：真实 LR 单张五秒等待的计时与候选修正

用户确认单张仍等约五秒。真实日志显示 LR 外部调用至 Rust 启动进程首次日志为 4889–5888ms，随后 GUI 入口至首帧 711ms；原日志无法区分调用前、最早程序入口、控制台访问和日志初始化。

移除非必需的入口控制台编码调用；Rust 自带的控制台 Unicode 和管道 UTF-8 处理保持。加入 Lua SDK 墙钟和 Rust 最早入口/图形/字体等阶段计时，用 `scripts/analyze_startup.py` 按会话关联，并对旧版整秒记录报告范围。计时失败不阻断打开，不修改图像引擎、会话协议或评分行为。

32 项 Rust、24 项 Lua/SDK/GUI、2 项日志分析回归通过；Clippy 无告警，中文错误管道编码正常。三种正式 release 原生管道启动均通过，返回 122.8/118.0/130.6ms。尚不能宣称真实 LR 五秒等待已消失，需要用户重新载入并点击一次取得新日志。中间文件位于 `.agent-temp/2026-10-12-startup-stages/`。

### 2.0.7：消除 LR 阻塞管道读取并请求前台焦点

用户复测 2.0.6 仍等五秒且窗口在后台。新日志排除控制台候选：元数据 23.1ms、popen 35.9ms，读取阻塞 5850.9ms；外部调用至 Rust main 5792ms，之后 GUI 图形初始化 562ms、字体 6.8ms。

原生命令改用 SDK `LrTasks.execute`（协作任务等待），通过唯一临时输出文件返回结果、清理及错误状态。避免 `io.popen:read` 阻塞 LR 主线程消息处理。启动器向 GUI 传递前台权限，GUI 设置 active 并在可见帧一次性请求焦点。

32 项 Rust 与 28 项 Lua/SDK/原生窗口/日志回归、Clippy 通过；原生 SDK 回归走实际 Common.runCuller、Windows shell 重定向和真实 GUI，并验证会话同步。正式额外管道继承回归返回 131.0ms。真实 LR 加速和前台效果仍等用户重新载入本版复测。临时测试、2.0.5/2.0.6 真实计时报告在 `.agent-temp/2026-10-12-startup-stages/`。

### 2.0.8：原生前台核验

真实 LR 已确认 2.0.7 加速，340 张菜单到首帧 475ms、首图 572ms，但仍在后台。GUI 改用实际 HWND 和 Windows 前台核验，窗口可见后有限激活；必要时短暂连接原发起程序的输入线程并立即断开。最多三次、仅限启动 1.5s，已前台或用户切换应用即停止。

35 项 Rust、28 项 Lua/SDK/GUI/日志回归、Clippy 和正式额外管道测试通过。用户复测确认自动前台正常；对应 340 张菜单到首帧 489ms、首图 591ms，native foreground=true，本次无需激活兜底。日志归档 `docs/focus-validation-2.0.8.json`，临时目录 `.agent-temp/2026-10-12-focus-window/`。真实启动和前台验证通过，完整 LR 同步仍保留验证边界。

### 2.1.0：后台筛选、曝光快调、双片对比与关闭自动星级同步

用户确认的筛选范围全部实现，使用同字段或、跨字段且和稳定原始索引。相机、镜头、拍摄时间及曝光参数从有界 TIFF 头部后台索引，侧栏首次打开才启动，不延长菜单前的全量读取。

曝光四按钮按⅓/1 EV调整，从窗口启动后的 SDK 导出取得实际 LR 曝光值，使用无损旁车与逐字段 V2 修订保存。GPU 在机内 JPEG 上近似预览；只同步 Exposure2012，保护其他显影设置。双片支持独立/联动适配、100%、滚轮缩放和拖动平移，保护两个当前大图，保持左右键选片与原会话身份。

关闭确认包含同步星级并关闭、直接关闭、取消。确认后先退出 GUI，再提交星级请求，LR 按原会话自动同步；星级确认独立于其他字段，失败保留供重试，已自动同步的星级不会在会话同步时重复写入。兼容旧 V1 会话。

46 项 Rust、32 项 Lua/SDK/GUI/日志回归及 Clippy 通过；13 帧实际窗口与最小窗口布局视觉检查、曝光像素检查通过。曝光基线仅读取完整记录，完成后停止轮询。340 张隔离启动返回 171.5ms、首图 428.1ms，不含 LR SDK 导出，不作为冷启动保证。完整真实 LR 新功能验收等待用户复测。详见 `docs/RUST-VALIDATION.md` 和 `docs/features-validation-2.1.0.json`；中间文件在 `.agent-temp/2026-10-12-photo-filters/` 与 `.agent-temp/2026-10-13-photo-qa/`。

### 2.1.1：需求核对、组合交互修正与简洁 UI

针对用户要求逐项核对既定功能。修正同帧打星/翻片事件顺序、对比活动照片切换、对比键盘平移与联动、纯星级关闭会话完成判定；保留未同步曝光/旗标/标签。自动同步成功直接返回，失败才弹提示。工具栏分为评分行与查看/曝光行，整组换行，将旋转/重试/日志放入更多菜单，减少重复提示及常驻调试文字。

85 项回归与 Clippy 通过，补充菜单输入隔离、筛选＋对比＋曝光＋星级组合及最小窗口高度检查。一次未记录原因的曝光保存失败随后未复现，200 次定向与 15 轮组合复测通过，保持保存错误提示和原值保护。核对表 `docs/UI-ACCEPTANCE.md`，新原生截图及启动数据 `docs/features-validation-2.1.1.json`；真实 LR 完整新功能仍待用户重新载入后确认，中间文件在 `.agent-temp/2026-10-13-ui-audit/`。

## 2026-10-13 — 2.1.2：关闭确认框与对比相对联动

- 加大关闭框字号和留白：标题 24px、正文 18px、说明 14px；16px 按钮文字、40px 按钮高度、22px 内边距，常规与 640×480 原生截图检查通过。
- 对比联动保留各自的主体锚点和倍率差，施加共同倍率和图像比例平移增量；开启开关不跳位，边缘共同限制；换候选不重置参考图。滚轮、拖动、上下键、F、双击和放大按钮统一处理。
- 回归先复现旧逻辑覆盖另一照片的倍率和位置，修正后 89 项回归及 Clippy 通过；两种实际窗口验证不同主体偏移在缩放、移动后保留，取消联动后只移动活动图。
- 分发包 `dist/FastCull-Rust-2.1.2-win64.zip`，汇总 `docs/features-validation-2.1.2.json`；原生窗口与 SDK mock 不替代真实 Lightroom 目录验收。
- 临时文件 `.agent-temp/2026-10-13-relative-link/`，正式成果不依赖临时目录。

## 2026-10-13 — 2.2.0：文件夹树与整理计划

用户确认完整保留 LR 编辑历史、集合关系和虚拟副本，因此移动使用整理计划，实际移动交由 LR 原生完成。

- 多文件夹自动展开文件树，支持目录/后代筛选、搜索、本次加载计数、创建实际目录、Ctrl/Shift/Ctrl+A 多选、拖放安排、查看和取消计划。
- 原始照片路径、缓存和会话索引不变；计划独立原子保存，重名和损坏记录保护。
- 有计划时关闭框明确同步修改及整理分组；没有计划时保持只自动同步星级。退出后先同步照片字段，再创建 LR 待整理集合并选中第一组原对象；按原 UUID 核对完成情况并定位下一组。
- 整理失败独立于字段确认进度，保留计划及收据；重试不重复曝光历史。新集合提交后才在下一事务加入照片。
- 108 项回归及 Clippy 通过；两个正式窗口各 19 帧已检查，实际创建目录、保存两张计划，RAW 哈希保持。小窗口像素验证修正侧栏误采样，补充两个回归并保持原误差阈值。
- 说明 `docs/FOLDERS.md`，证据 `docs/folders-validation-2.2.0.json`，安装包 `dist/FastCull-Rust-2.2.0-win64.zip`。真实 LR 的分组定位和原生移动仍待用户复测；中间文件 `.agent-temp/2026-10-13-folders/`。

## 2026-10-13 — 2.2.1：Adobe Bridge 注册与快捷入口

- 新增 ExtendScript 启动插件：Bridge 照片右键及工具菜单可打开所选 ARW，混合选择仅保留有效 ARW，保持顺序并去重。启动只导出路径，既有 XMP 不应用默认零评分；大批选择通过清单避免 Windows 命令行长度限制。
- Rust Bridge 原生助手校验并保存私有快照，复用已验证的无句柄继承分离进程与前台机制；GUI 直接读取 XMP，曝光基线采用旁车值。成功启动后清理请求，失败保留供诊断。
- 已注册本机 Bridge 2025 / 2026 用户启动脚本目录；现用 Bridge 2026 为 16.0.8.10。安装器支持源码/发布包布局，更新仅备份并替换自身文件，不改宿主快捷键或启动脚本授权偏好。
- LR 打开菜单添加 (&F) 助记键。Bridge 使用照片右键入口；Windows Shift+F10 尚未在当前 Bridge 确认。公开接口没有直接脚本快捷键注册属性，LR SDK 没有照片右键菜单扩展，本次不声称新增全局 Ctrl 快捷键。
- 无 LR 会话的关闭框改为 XMP 保存说明、直接关闭/取消，避免与 LR 自动同步混淆。Bridge 实际移动仍使用宿主原生功能，LR 整理计划继续保留已有范围。
- 118 项回归（68 Rust、44 Python 含私有 GUI 与安装器、6 ExtendScript 合同）和 Clippy 通过；Bridge 分离启动链 13 帧验证、5000 张列表测试及关闭框视觉检查完成。真实 Bridge 启用后的菜单点击仍待用户验收。
- 说明 `docs/BRIDGE.md`，证据 `docs/bridge-validation-2.2.1.json`，包 `dist/FastCull-Rust-2.2.1-win64.zip`；中间文件 `.agent-temp/2026-10-13-bridge-entry/`，正式交付不依赖该目录。

## 2026-10-13 — 2.2.2：Bridge 原生 Syntax error

- 用户在实际调用 Bridge 打开选片器时收到 Syntax error。直接在 Bridge 16.0.8.10 引擎中复现 api.quote 正则错误 8；原始 NUL 字符类转义为触发点，Node 合同测试接受该表达式，说明宿主模拟覆盖不足。
- 改为逐字符拒绝六类命令危险字符；保留中文、空格、括号和 &。明确 cmd.exe /d /v:off /s /c 执行器与嵌套引用，错误包含执行阶段、编号和行号。
- Bridge 内热加载修正文件，原生 quote、六种拒绝输入、两个菜单、实际 Rust 命令调用均通过。实际右键回调使用三张私有 Sony 样片，经请求与原生分离进程完成 GUI 五帧、评级写入和自动关闭；真实选区、照片未改动。
- 120 项回归（68 Rust、44 Python、8 脚本合同）及 Clippy 通过。正式证据 `docs/bridge-validation-2.2.2.json`，包 `dist/FastCull-Rust-2.2.2-win64.zip`；临时原生探针和私有样片 `.agent-temp/2026-10-13-bridge-syntax/`。

## 2026-10-13 — 2.2.3：Bridge 多文件夹加载

- 回归测试复现只选文件夹时菜单被禁用；脚本跳过容器，Rust 校验也仅接受 ARW。两端改为接受目录根路径，递归扫描放到分离后的 Rust 进程，Bridge 不遍历目录、不读取照片元数据。
- 支持单个/多个文件夹、ARW 与文件夹混选和子文件夹；按选择顺序处理根路径，目录内按路径排序遍历。父子目录重叠、单独文件与目录重叠去重；后代链接/重解析点不递归，显式链接根路径可加载。空目录和访问错误明确报告。
- 多个照片目录自动显示既有文件树，不增加新的控件。目录扫描仍先于窗口创建；大目录耗时不能视为已经解决。Bridge 的实际移动继续使用宿主原生功能。
- 125 项回归（72 Rust、44 Python、9 脚本合同）及 Clippy 通过。真实 Bridge File/Folder 对象混选与右键回调使用私有样片成功，四个入口最终加载三张照片；五帧 GUI 与文件树视觉检查完成。原始 ARW 内容未变。
- 本机 2025/2026 脚本和项目 exe 已更新。正式证据 `docs/bridge-validation-2.2.3.json`、界面 `docs/images/rust-2.2.3-bridge-folders.png`、包 `dist/FastCull-Rust-2.2.3-win64.zip`。可重建探针和样片放在 `.agent-temp/2026-10-13-bridge-folders/`，正式交付不依赖该目录。

## 2026-10-13 — 2.3.0：Bridge 连续与批量移动

- 仅 Bridge 模式启用实际文件移动，LR 继续保存整理计划。支持拖到文件夹、右键目标、移动弹窗、目标路径粘贴；Ctrl/Shift/Ctrl+A 多选。M／移到上次位置复用目标，默认加入队列后下一张，支持前一批执行期间继续提交其他照片。
- RAW 与四种 XMP/ACR 命名方式同行，目标冲突不覆盖。每张照片独立提交，失败尝试回退本张并停止本批余项；完成项准确更新位置。记录保留原路径、目标与完成/回退状态，不承诺整批断电原子性，也不自动重放恢复。
- 解码、元数据与移动共享每照片路径锁；评分在排队照片上禁用，完成后写到新位置，图像索引和对比锚点保持不变。文件树更新数量及新位置，新建空目录不改变 Bridge 当前筛选；关闭等待队列，错误保留窗口。
- 优化批量准备，每个源目录只列举/canonicalize 一次；排队执行前仅检查具体文件，发现新旁车才重新列举。跨盘使用临时复制与不覆盖发布，源文件删除失败不会伪报成功。
- 134 项回归（81 Rust、44 Python、9 脚本合同）与 Clippy 通过。实际 Bridge 原生菜单回调到私有 GUI 21 帧，以及 E→C 跨卷整理 21 帧通过；两个窗口尺寸视觉/曝光/对比核验完成，RAW 哈希未变。未修改真实照片或选区。
- 已更新本机 Bridge 2025/2026 注册文件与项目 exe。正式记录 `docs/bridge-validation-2.3.0.json`，包 `dist/FastCull-Rust-2.3.0-win64.zip`。可重建测试位于 `.agent-temp/2026-10-13-bridge-moves/`；正式包不依赖该目录。

## 2026-10-13 — 2.3.1：混合选择与文件树反馈

- 回归复现原生入口遇到 JPG 等格式就拒绝整个选区；Bridge 清单和直接 exe 入口改为跳过非 ARW，全部不支持才提示没有支持的 ARW。LR/Bridge 脚本的既有过滤增加 JPG、CR3、XMP、视频混选覆盖；不写入跳过文件。
- 文件树使用独立滚动条空间，数量统一右对齐，名称与计数放在同一行。拖到有效目标显示移动光标和蓝边框，源文件夹、排队照片和不可用模式显示禁止光标和红边框；无效释放不提交。拖拽照片时关闭树内容拖动滚屏，保留滑轨与滚轮；队列状态仅在请求/事件发生时复制。
- 138 项回归（85 Rust、44 Python、9 脚本合同）与 Clippy 通过。284 张私有样片混选四种不支持格式，常规/最小原生窗口各 5 帧；直接 exe 混选另 5 帧。数量清晰、边框和平台光标输出正确，跳过文件内容未变，全部不支持的两种原生入口均返回明确错误。未修改用户真实照片。
- 正式记录 `docs/tree-validation-2.3.1.json`、截图 `docs/images/rust-2.3.1-folder-tree.png` 与小窗口图、包 `dist/FastCull-Rust-2.3.1-win64.zip`。本机注册脚本与项目 exe 更新；本轮未重复真实 Bridge 菜单回调或跨卷移动，其既有证据保留在 2.3.0 记录中。临时样片/日志 `.agent-temp/2026-10-13-tree-feedback/`；正式交付不依赖该目录。

## 2026-10-13 — 2.4.0：CR2、NEF 与 JPG/JPEG

- 按用户最新范围只启用 ARW、CR2、NEF、JPG/JPEG，其他 RAW 延后。Bridge、LR 打开、LR 手动同步和独立入口共用支持范围；混选跳过不支持文件，原有文件树与拖拽反馈保留。
- CR2/NEF 使用 LibRaw 0.22.2 原生适配层；最大内嵌预览按像素选择，100% 有完整预览时直接解码 JPEG，不足时后台解码 RAW。发现包含边缘的传感器区与相机输出区不同，造成多余完整解码；使用厂商裁剪、EXIF 输出尺寸及旧 NEF 的受限边缘容差修正。两份正式样片全图均走 JPEG 路径，不做插值放大。
- JPG 标准内嵌 XMP 原子更新，已有 EXIF 方向与旋转保持一致，压缩图像数据不变；扩展/歧义/损坏 XMP 保留并拒绝编辑。RAW/JPG 同名移动不会误搬 RAW 旁车；LR 手动同步实际读取 JPEG 内嵌评分。
- 146 项回归（91 Rust、45 Python、10 脚本合同）及 Clippy 通过；正式窗口验证混合选择、JPG 最小窗口和低预览 CR2 的全图回退，各 5 帧。样片 CR2 5616×3744、NEF 4256×2832；本机单次原始像素解码约 68/29ms，低预览的完整 RAW 解码另计，不代表任意照片或冷启动性能。
- 插件提供静态 CRT 原生 DLL、固定 SHA256 上游源码包及重建脚本，不依赖 Python 或临时工具链运行。正式记录 `docs/formats-validation-2.4.0.json`、说明 `docs/FORMATS.md`、包 `dist/FastCull-Rust-2.4.0-win64.zip`。临时探针/样片 `.agent-temp/2026-10-13-multiformat/`。当前窗口占用时原 exe 同目录改名保存，再安装新 exe，旧副本不纳入分发；未修改用户真实照片。
# 2026-10-13 · 2.5.0 独立 Windows 版与 Bridge 启动

- Bridge 旧日志中，请求生成到 Rust 入口约 5.8 秒，进入 Rust 后首张照片约 0.4 秒。新增私有 `.fastcullrequest` 文件关联，经 ExtendScript `File.execute()` 直接启动；保留未注册安装的旧命令入口。
- 原生窗口先显示，照片和文件夹列表后台扫描。无参数启动显示照片/文件夹选择页，支持多选、拖入与打开新窗口。独立模式复用实际移动队列；LR 的整理计划模式保持原约束。
- 发布独立 Windows ZIP，包含 exe、原生 RAW DLL、许可及源码；提供当前用户安装和快捷方式脚本。
- ARW 100% 查看同样经过原生有效图像尺寸校验；CR2/NEF 根据每份文件的元数据及实际 JPEG 头判断尺寸，支持不同机型的尺寸差异。格式范围仍为 ARW、CR2、NEF、JPG/JPEG/JPE。
- 验证记录见 `docs/startup-validation-2.5.0.json`；测试照片和截图中间文件位于 `.agent-temp/2026-10-13-launcher/`。

## 2026-10-13 · 2.5.1 宿主快捷键与相机评分

- 按用户指定统一设为 Ctrl+Alt+K，调用 Bridge / LR 既有打开入口，保留选区、评分及同步语义。
- 新增原生 `fastcull-keys.exe`，仅在对应宿主主窗口前台注册 Windows 热键；无键盘钩子、无 Windows 开机启动项。MOD_NOREPEAT、800ms 去重和 1.5 秒请求过期保护防止重复/延迟误触发。
- Bridge 使用 scheduleTask，LR 使用 LrTasks；支持热加载取消和退出心跳回收。实机发现 Bridge File.execute 启动快捷方式未保留助手参数，改为直接启动支持无参数 Bridge 模式的助手；已验证助手持续运行、一次请求分派一次回调。LR 插件初始化时启动助手。
- 验证脚本和原生 Bridge 探针位于 `.agent-temp/2026-10-13-hotkeys/`，用户重新载入 LR 后的真实按键仍需现场确认。
- 修复相机评分漏读：RAW 原先仅读取 XMP 旁车，新增有界 IFD0 Rating / ApplicationNotes XMP 读取，JPEG 支持 EXIF Rating 回退。旁车或 JPEG XMP 的显式评分（含 0）优先；LR 目录清零会生成必要覆盖，读取不改 RAW。已有曝光旁车未包含评分时仍可读相机评分。
- 96 项 Rust、48 项 Python/Lua（含真实原生窗口私有照片往返）、14 项 Bridge 脚本测试通过，Clippy 通过。新增回归先确认缺失星级失败，再验证大小端、内嵌 XMP 优先级、异常偏移、清零及 RAW 字节不变。尚未使用用户这批相机原片做最终实测。
- 2026-10-13，2.5.2：底部缩略图栏改为可拖动上边缘调整大小，缩略图尺寸与可见数量自适应；高度限制随窗口变化，给主图保留空间。滚轮仅在栏内切换当前筛选结果，复用对比参考图避让和照片选择逻辑；拖拽、弹窗、修饰键不抢占，触控板小增量累计后切图。新增真实 egui 拖动与滚轮事件测试；49 项主程序回归、14 项 Bridge 脚本测试及 Clippy 通过。原生窗口布局验证中间文件位于 `.agent-temp/2026-10-13-filmstrip/`。
- 2026-10-13，2.5.3：100% 切图不再清零视图位置。单图偏移改为旋转后图像尺寸的比例，拖动与键盘平移统一换算、限制边界；切换分辨率、横竖构图和 DPI 不复用旧图像素偏移。对比切图保留当前侧原有缩放和相对偏移，参考侧不变。新增尺寸/方向/DPI 回归，更新方向键和对比导航用例；原生截图测试增加切图往返保留非零锚点的断言。临时验证文件位于 `.agent-temp/2026-10-13-zoom-anchor/`。
- 2026-10-13，2.6.0：单图点击、对比双击按实际绘制图像矩形计算归一化落点，100% 将点击位置居中，并延续切图相对位置保持。模式入口统一为 E 单图、G 网格、C 对比，按钮显示按键；输入框/菜单保留自身按键。默认展开筛选，文件夹与筛选改为左侧互斥活动栏，Ctrl+F 保留；右上角两行显示文件名与拍摄参数，悬停显示相机/镜头/拍摄时间。精简筛选说明，缩略图宽度在窄侧栏余量下限制以避免溢出。51 项主程序回归、14 项 Bridge 脚本测试与 Clippy 通过；原生单图、网格、对比、关闭确认及 1280×850 / 800×600 / 640×480 窗口截图验证，临时文件位于 `.agent-temp/2026-10-13-layout/`。
- 2026-10-13，2.6.1：重新整理视觉样式，深灰分层背景与淡蓝选中态、手绘矢量导航图标、统一圆角及无重边框按钮；正文 14px、常规交互行高 26px，头部/侧栏/工具栏增加留白。筛选保留星级默认展开，其余按需展开；搜索提示移入输入框，重置无条件时禁用。缩略图柔化背景与选中描边，窄窗口工具栏自动压缩间距，避免拥挤画布。交互回归、Clippy 与三种窗口原生截图复核，临时文件 `.agent-temp/2026-10-13-polish/`。

## 2026-10-14 · 2.7.0 性能优化

- 保持 2.6.1 的布局和交互，优化照片列表加载与字体读取的启动时序；窗口创建本身约 240ms，首图中位时间由 535ms 降至 308ms（本机三次原生 GUI 对照，不含宿主菜单时间）。
- 新增 Windows WIC 缩小 JPEG 解码，保留 Rust 回退；按实际画布像素加余量选择适屏尺寸，跳过不透明照片的多余 alpha 运算。CR2/NEF/JPEG 缩略图样本耗时减少约 64%～66%，固定 2560 适屏减少约 26%～42%。
- 切图复用仍相关的在途和待接收结果，跨切换去重；重试取消旧结果。元数据首次探测合并，索引改用有序队列；筛选只统计展开分组，数据变化时失效。
- 300ms 停留后使用后台线程预热当前全尺寸图，保留主图专用线程。预热命中时 100% 到绘制帧中位时间约 1ms；未命中仍需解码和上传。全尺寸解码本身没有显著提速，分辨率校验和传感器回退保持原样。
- 107 项 Rust、14 项 Bridge 脚本、48 项 Python/Lua（默认 1 项可选跳过，随后单独启用原生往返通过）、Clippy、180 次快速切换通过；检查真实 ARW/CR2/NEF 全尺寸内容哈希、缩图颜色、三种窗口尺寸和单图/网格/对比/关闭截图。
- 正式实测和限制见 `docs/PERFORMANCE-2.7.0.md`、`docs/performance-2.7.0.json`；发布包位于 `dist/`。可重建中间文件统一在 `.agent-temp/2026-10-14-performance/`，正式包不依赖该目录。
