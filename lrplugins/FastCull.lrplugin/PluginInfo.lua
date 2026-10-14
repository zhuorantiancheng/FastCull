--[[ PluginInfo.lua — 插件管理器里的设置面板 ]]

local LrView = import 'LrView'
local LrPrefs = import 'LrPrefs'

local bind = LrView.bind

return {

    sectionsForBottom = function(f, _)
        local prefs = LrPrefs.prefsForPlugin()
        return {
            {
                title = 'FastCull RAW/JPEG 设置',

                f:row {
                    bind_to_object = prefs,
                    spacing = f:control_spacing(),
                    f:static_text {
                        title = 'Python 回退:',
                        alignment = 'right',
                        width = LrView.share 'label',
                    },
                    f:edit_field {
                        value = bind 'pythonPath',
                        width_in_chars = 50,
                        tooltip = 'python.exe 完整路径, 或 "py -3" 这类命令(留空则自动探测)',
                        immediate = true,
                    },
                },

                f:row {
                    bind_to_object = prefs,
                    spacing = f:control_spacing(),
                    f:static_text {
                        title = '选片器程序:',
                        alignment = 'right',
                        width = LrView.share 'label',
                    },
                    f:edit_field {
                        value = bind 'cullerPath',
                        width_in_chars = 50,
                        tooltip = 'FastCull 可执行文件完整路径（默认使用插件内置程序）；Windows 可填写 .py 使用旧版回退',
                        immediate = true,
                    },
                },

                f:row {
                    f:static_text {
                        title = '说明：默认使用插件内置 FastCull（Rust），请安装与 Windows / macOS 系统及处理器匹配的版本，无需 Python。' ..
                            '旧 Python 原型保留用于回退；选择 .py 时才需要 Python + Pillow。' ..
                            'RAW 优先内嵌预览，JPG 直接读取原图；RAW 写旁车，JPG 写内嵌 XMP。',
                        wrap = true,
                    },
                },
            },
        }
    end,

}
