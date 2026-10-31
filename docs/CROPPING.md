# 裁切与旋转

点击左侧边栏的 **裁切**（位于“筛选”下方），或按 **T**。照片尚未准备好时显示“正在准备裁切”。进入后可在右侧选择比例，直接拖动画面的八个手柄、边框或框内区域。

## 操作

- **自由**：宽高独立调整；在框外拖动可重画裁切框。
- **锁图标**：闭锁时保持比例，开锁时自由改变宽高。预设包含原始比例、1:1，以及 3:2/2:3、4:3/3:4、5:4/4:5、7:5/5:7、16:9/9:16 横竖配对；“横竖互换”可主动切换方向，锁定时拖动角点也会根据鼠标位置自动切换横竖比例。
- **自定义比例**：输入宽和高（允许小数），点击“使用”；填写名称后点击“保存比例预设”。同名保存会更新已有预设，“移除预设”仅移除该预设，不影响已保存的照片。
- **角度**：滑块或数值输入微调 −45°～45°，精度 0.01°；“向左/向右 90°”旋转照片。旋转时自动缩小或平移裁切框，使四角留在原图内，避免黑边。
- **撤销 / 重做**：当前编辑过程内可用；快捷键 Ctrl+Z / Ctrl+Y。文本框正在编辑时保留文本编辑快捷键。
- **重置裁切**：恢复全图和零微调角度；保留照片的 90° 方向设置。
- **应用裁切 / Enter**：保存 XMP，返回浏览。单图、100%、对比、网格和胶片带都使用保存后的裁切范围。
- **取消 / Esc**：丢弃本次草稿，保留进入工具前的裁切。应用后再次进入仍可恢复被裁掉的区域。

编辑期间使用独立工作区，切图、打星和移动照片不会抢占裁切手势。写入失败时显示错误并保留草稿，不把预览当作已保存的结果。预设文件在 `%LOCALAPPDATA%/FastCull/crop-ratios.tsv`，跨重启保留；测试可通过 `FASTCULL_SETTINGS_DIR` 指定私有目录。

## 载入 Lightroom / Camera Raw 已有裁切

- 从 Lightroom Classic 插件打开时，后台直接读取目录中的裁切边界、角度和照片方向；不要求先把 LR 修改保存到 XMP。进入裁切工具时会等待当前照片的目录参数读完，避免从旧裁切开始编辑。
- 从 Bridge 或独立打开时，读取 RAW 同名 XMP 或 JPEG 内嵌 XMP 中已经保存的 ACR/LR 裁切。仅保存在 LR 目录或 ACR 私有数据库、未写入照片元数据的修改，独立打开时无法读取。
- 载入的裁切用于单图、100%、对比、网格和胶片带；进入裁切工具后仍能调整原有裁切。读取本身不写照片，也不登记为需要同步回 LR 的新修改。
- LR 目录参数优先于旧 XMP；明确重置裁切会恢复全图。裁切与曝光独立读取，旧处理版本没有 `Exposure2012` 时仍可载入有效裁切。缺失或无效的目录字段保留原 XMP 对应值。

## XMP 存储和照片移动

裁切保存当前可恢复的编辑参数，不修改 RAW 像素，也不重新压缩 JPEG。XMP 记录的是应用后的裁切状态；当前工具内的撤销栈不作为 Adobe 历史步骤写入旁车。

- RAW（ARW / CR2 / NEF / DNG）：写入同名 `.xmp` 旁车。
- JPG / JPEG：沿用原有的内嵌 XMP 规则。不会创建可能与同名 RAW 共用的 `.xmp`。照片移动时，内嵌元数据随 JPEG 一起移动。
- 使用 `crs:HasCrop`、`CropLeft`、`CropTop`、`CropRight`、`CropBottom`、`CropAngle` 和 `HasSettings`；90° 方向使用绝对 `tiff:Orientation`，JPEG 同时更新已有 EXIF 方向。
- 坐标针对未应用 EXIF 方向的原始像素，范围归一化到 0～1。带角度时存放旋转后的对角端点；八种方向（含镜像）都做对应转换。字段定义参见 [Adobe Camera Raw XMP 命名空间](https://developer.adobe.com/xmp/docs/xmp-namespaces/crs/)，几何转换参考 [crop-corners 作者说明](https://community.adobe.com/questions-675/sdk-computing-the-corners-of-a-crop-rectangle-962108)。
- XMP 写入保留无关字段、曲线和现有命名空间别名，使用临时文件原子替换；损坏 XML、无效参数和写入错误不会覆盖原数据。
- Bridge / 独立模式使用原有照片与旁车组合移动：预检同名冲突，排队执行前再次检查新产生的旁车，旁车先移动、照片最后移动，失败尝试回退并保留恢复日志。
- 移动完成后，裁切保存使用解码/移动线程共享的新路径。连续移到第二个目录后仍可继续编辑，不会在旧目录重建旁车。
- LR 模式退出时选择“同步全部并关闭”，自动同步本次修改的评分、标签、旗标、曝光、裁切和旋转。裁切和方向写入后回读核对，失败保留供重试；同步不会导入整份 XMP 覆盖其他显影设置。实际旋转需要 Lightroom Classic 11.4 或更新版本。详情见 [退出同步说明](LR-SYNC.md)。
- LR 模式仍只建立整理计划，先完成编辑同步，再由 LR 完成实际移动，保留原照片 UUID 和目录记录。独立模式的照片与旁车组合移动规则保持适用。

## 验证与边界

`tests/cropping.rs` 验证独立数值样例、八种方向与不同角度/比例的往返、旋转后四角不越界、XMP 字段保留、重置、损坏保护、目标旁车冲突，以及 RAW/JPEG 连续移动后编辑。UI 测试通过真实 egui 指针事件验证拖动，验证撤销、预设重读、草稿取消和保存失败保留。

原生 GPU 窗口截图使用：

```powershell
python scripts/crop_smoke.py --exe lrplugins/FastCull.lrplugin/fastcull.exe --scratch .agent-temp/<日期>-crop/native-wide
python scripts/crop_smoke.py --exe lrplugins/FastCull.lrplugin/fastcull.exe --scratch .agent-temp/<日期>-crop/native-small --size 800x600 --orientation 6
python scripts/crop_smoke.py --exe lrplugins/FastCull.lrplugin/fastcull.exe --scratch .agent-temp/<日期>-crop/import-xmp --import-source xmp
python scripts/crop_smoke.py --exe lrplugins/FastCull.lrplugin/fastcull.exe --scratch .agent-temp/<日期>-crop/import-lr --import-source lightroom
```

也可传 `--sample <照片路径>`；脚本只编辑私有复制文件，验证原文件哈希未变。截图覆盖自由裁切、带角度的固定比例、90° 旋转、应用、重新进入、网格和 100%。脚本成功仍需检查截图布局。

`--import-source` 使用生成的网格图，分别从手写 Adobe XMP 和模拟 LR 目录基线载入相同的竖幅裁切，并断言首次显示的范围与方向；LR 模式同时验证读取没有把目录裁切写回照片。Lua 模拟和 Rust 测试另覆盖缺少 `HasCrop`、旧曝光协议、目录读取先于照片元数据、明确重置及读取不产生会话修订。

本轮测试不等同于 Camera Raw / Lightroom 真机往返验收；两者的镜头校正、透视校正及完整 RAW 显影不由选片器复现，选片器在原始预览上呈现裁切。最终 Adobe 显影效果应在对应宿主中核对。
