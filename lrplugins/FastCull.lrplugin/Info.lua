--[[ FastCull for ARW — ARW 内嵌 JPEG 高速选片器 <-> Lightroom Classic 双向集成

原理：直接读取 ARW 内嵌的机内全尺寸 JPEG（约 80ms/张，不触碰 RAW 数据），
选片速度远高于 LR 的完整 RAW 渲染；评分/颜色/旗标经 .xmp 旁车与 LR 双向同步。

菜单: 图库(Library) → 增效工具额外命令(Plug-in Extras)
  · 打开 ARW 选片器        —— 读取方向: 把 LR 目录中的星级/颜色/旗标写入旁车,
                              然后启动 Rust fastcull.exe 选片器(不阻塞 LR)
  · 同步选中照片           —— 同步方向: 把选片器写入旁车的评分/颜色/旗标
                              批量取回 LR 目录

运行: Windows x64 + OpenGL 图形驱动；默认使用插件内置 exe，无需 Python。
     手动选择 .py 时才使用 Python 原型回退。
]]

return {

    VERSION = { major = 2, minor = 9, revision = 0 },

    LrSdkVersion = 9.0,
    LrSdkMinimumVersion = 6.0,

    LrToolkitIdentifier = 'com.zrtc.fastcull-arw',
    LrPluginName = 'FastCull · RAW/JPEG',

    LrPluginInfo = 'PluginInfo.lua',
    LrInitPlugin = 'HotkeyInit.lua',
    LrForceInitPlugin = true,
    LrShutdownPlugin = 'HotkeyStop.lua',
    LrShutdownApp = 'HotkeyStop.lua',

    LrLibraryMenuItems = {
        {
            title = '打开 RAW/JPEG 选片器（Ctrl+Alt+K）(&F)',
            file = 'OpenCuller.lua',
        },
        {
            title = '同步选片会话（仅修改过的字段）',
            file = 'SyncSelected.lua',
        },
        {
            title = '同步选中照片（读取照片元数据）',
            file = 'SyncSidecars.lua',
        },
    },

}
