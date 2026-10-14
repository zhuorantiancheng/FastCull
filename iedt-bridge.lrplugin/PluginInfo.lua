--[[ PluginInfo.lua — 插件管理器里的设置面板 ]]

local LrView = import 'LrView'
local LrPrefs = import 'LrPrefs'

local bind = LrView.bind

return {

    sectionsForBottom = function(f, _)
        local prefs = LrPrefs.prefsForPlugin()
        return {
            {
                title = 'IEDT Bridge 设置',

                f:row {
                    spacing = f:control_spacing(),
                    f:static_text {
                        title = 'Sony Viewer 路径:',
                        alignment = 'right',
                        width = LrView.share 'label',
                    },
                    f:edit_field {
                        value = bind 'viewerPath',
                        width_in_chars = 50,
                        tooltip = 'Imaging Edge Viewer.exe 完整路径',
                        immediate = true,
                    },
                },

                f:row {
                    spacing = f:control_spacing(),
                    f:static_text {
                        title = 'Python 路径:',
                        alignment = 'right',
                        width = LrView.share 'label',
                    },
                    f:edit_field {
                        value = bind 'pythonPath',
                        width_in_chars = 50,
                        tooltip = 'Python 3 完整路径(用于运行内置 xmp_bridge.py)',
                        immediate = true,
                    },
                },

                f:row {
                    f:static_text {
                        title = '说明: 评分改动由内置 xmp_bridge.py 原位写入 ARW 内嵌 XMP(等长替换), 不改变 RAW 结构。',
                        wrap = true,
                    },
                },
            },
        }
    end,

}
