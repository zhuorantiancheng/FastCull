--[[ common.lua — IEDT Bridge 共用工具 ]]

local LrPrefs = import 'LrPrefs'

local Common = {}

-- 默认路径(可在插件管理器设置面板修改)
Common.DEFAULT_PYTHON = 'E:/APP/miniconda3/python.exe'
Common.DEFAULT_VIEWER = [[E:\APP\Sony\Imaging Edge\Viewer.exe]]

function Common.getPrefs()
    local prefs = LrPrefs.prefsForPlugin()
    if not prefs.pythonPath or prefs.pythonPath == '' then
        prefs.pythonPath = Common.DEFAULT_PYTHON
    end
    if not prefs.viewerPath or prefs.viewerPath == '' then
        prefs.viewerPath = Common.DEFAULT_VIEWER
    end
    return prefs
end

-- 插件内置的桥接脚本路径
function Common.bridgePath()
    local LrPathUtils = import 'LrPathUtils'
    return LrPathUtils.child(_PLUGIN.path, 'xmp_bridge.py')
end

-- 临时列表文件路径(纯 ASCII, 避免编码问题; 内容 UTF-8)
-- 注意: LR 沙箱里 os.getenv 不存在, 用 LrPathUtils.getStandardFilePath('temp')
function Common.listPath(name)
    local tmp = nil
    pcall(function()
        local LrPathUtils = import 'LrPathUtils'
        tmp = LrPathUtils.getStandardFilePath('temp')
    end)
    if not tmp or tmp == '' then
        tmp = _PLUGIN.path
    end
    tmp = tmp:gsub('[\\/]+$', '')
    return tmp .. '\\' .. (name or 'iedt_bridge_list.txt')
end

function Common.fileExists(path)
    if not path then return false end
    local f = io.open(path, 'r')
    if f then f:close() return true end
    return false
end

-- 运行 python 桥, 返回 stdout 文本
function Common.runBridge(args)
    if not io.popen then
        return nil, '此 Lightroom 环境不允许 io.popen，无法运行外部脚本'
    end
    local prefs = Common.getPrefs()
    local cmd = string.format('"%s" "%s" %s',
        prefs.pythonPath, Common.bridgePath(), args)
    local pipe = io.popen(cmd)
    if not pipe then
        return nil, '无法启动 Python: ' .. prefs.pythonPath
    end
    local out = pipe:read('*a')
    pipe:close()
    return out, nil
end

-- 从桥输出解析 "RESULT patched=N keep=N ..." 摘要行
function Common.parseSummary(out)
    local result = {}
    if not out then return result end
    for k, v in out:gmatch('(%w+)=(%d+)') do
        if not k:match('^[A-Z]') then result[k] = tonumber(v) end
    end
    return result
end

-- 从选中照片里筛出 ARW, 返回 {photo, path} 列表
function Common.pickArwPhotos(photos)
    local LrApplication = import 'LrApplication'
    local picked = {}
    local skipped = 0
    for _, photo in ipairs(photos) do
        local path = photo:getRawMetadata('path')
        if path and path:lower():match('%.arw$') then
            table.insert(picked, { photo = photo, path = path })
        else
            skipped = skipped + 1
        end
    end
    return picked, skipped
end

-- 从旁车文本提取 xmp:Rating / xmp:Label (属性式或元素式)
function Common.readSidecarValues(arwPath)
    local sidecar = arwPath:gsub('%.%a%a%a$', '') .. '.xmp'
    local f = io.open(sidecar, 'r')
    if not f then return nil, nil end
    local text = f:read('*a')
    f:close()
    local rating = text:match("xmp:Rating%s*=%s*'(%d)'")
        or text:match('xmp:Rating%s*=%s*"(%d)"')
        or text:match('<xmp:Rating>(%d)</xmp:Rating>')
    local label = text:match("xmp:Label%s*=%s*'([^']+)'")
        or text:match('xmp:Label%s*=%s*"([^"]+)"')
        or text:match('<xmp:Label>([^<]+)</xmp:Label>')
    return tonumber(rating), label
end

return Common
