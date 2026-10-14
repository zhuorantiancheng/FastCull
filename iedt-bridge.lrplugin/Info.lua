--[[

IEDT Bridge — 让 Lightroom Classic 与 Sony Imaging Edge (Viewer) 双向同步评分,
并可直接从 LR 菜单进入 Viewer 进行选片。

菜单: 图库(Library) → 增效工具额外命令(Plug-in Extras)
  · 去 IEDT 选片        —— 把选中照片的 LR 评分写入 ARW, 然后启动 Viewer
  · 从 IEDT 同步评分    —— 把 Viewer 打的评分/标签取回 LR 目录

依赖: Python 3 (默认 E:/APP/miniconda3/python.exe, 可在插件管理器中修改);
     Viewer 路径默认 E:/APP/Sony/Imaging Edge/Viewer.exe。
     文件改动由内置的 xmp_bridge.py 完成(原位改写 ARW 内嵌 XMP, 结构零变动)。

]]

return {

    VERSION = { major=0, minor=2, revision=0, },

    LrSdkVersion = 9.0,
    LrSdkMinimumVersion = 6.0,

    LrToolkitIdentifier = 'com.zrtc.iedt-bridge',
    LrPluginName = 'IEDT Bridge',

    LrPluginInfo = 'PluginInfo.lua',   -- 插件管理器中的设置面板

    LrLibraryMenuItems = {
        {
            title = '去 IEDT 选片',
            file = 'CullInIEDT.lua',
        },
        {
            title = '从 IEDT 同步评分',
            file = 'SyncFromIEDT.lua',
        },
    },

}
