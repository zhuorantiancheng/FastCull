--[[ CullInIEDT.lua — 图库 > 增效工具额外命令 > 去 IEDT 选片

流程: 把选中照片在 LR 目录里的评分/标签写入 ARW 内嵌 XMP(Viewer 可读),
然后启动 Sony Viewer 并定位到照片所在文件夹。选片打分结束后, 回到 LR
执行"从 IEDT 同步评分"取回结果。
]]

local LrApplication = import 'LrApplication'
local LrDialogs = import 'LrDialogs'
local LrTasks = import 'LrTasks'
local LrProgressScope = import 'LrProgressScope'

require 'common'
local Common = require 'common'

LrTasks.startAsyncTask(function()

    local catalog = LrApplication.activeCatalog()
    local photos = catalog:getTargetPhotos()
    if #photos == 0 then
        LrDialogs.message('IEDT Bridge', '请先在图库里选中照片')
        return
    end

    local prefs = Common.getPrefs()
    if not Common.fileExists(prefs.pythonPath) then
        LrDialogs.message('IEDT Bridge', '找不到 Python:\n' .. prefs.pythonPath ..
            '\n\n请在 文件 > 增效工具管理器 > IEDT Bridge 中修改路径。')
        return
    end
    if not Common.fileExists(prefs.viewerPath) then
        LrDialogs.message('IEDT Bridge', '找不到 Sony Viewer:\n' .. prefs.viewerPath ..
            '\n\n请在插件管理器中修改路径。')
        return
    end

    local progress = LrProgressScope {
        title = '同步评分到 IEDT…',
    }

    local picked, skipped = Common.pickArwPhotos(photos)
    if #picked == 0 then
        progress:done()
        LrDialogs.message('IEDT Bridge',
            string.format('选中的 %d 张照片里没有 ARW 文件。', #photos))
        return
    end

    -- 生成 path|rating|label 列表
    local listFile = Common.listPath('iedt_bridge_fwd.txt')
    local f = io.open(listFile, 'w')
    if not f then
        progress:done()
        LrDialogs.message('IEDT Bridge', '无法写入临时文件: ' .. listFile)
        return
    end
    for _, item in ipairs(picked) do
        local rating = item.photo:getRawMetadata('rating')
        local label = item.photo:getFormattedMetadata('label')
        if label == '' then label = nil end
        f:write(string.format('%s|%s|%s\n',
            item.path,
            rating and tostring(rating) or '',
            label and tostring(label) or ''))
    end
    f:close()

    -- 调 python 桥: 写入 ARW 内嵌 XMP
    local out, err = Common.runBridge(
        string.format('--from-list "%s" --ascii-summary', listFile))
    if not out then
        progress:done()
        LrDialogs.message('IEDT Bridge', err or '桥接脚本执行失败')
        return
    end
    local sum = Common.parseSummary(out)

    -- 启动 Viewer(由 python 处理, 避免中文路径编码问题), 定位到第一张所在文件夹
    local folder = picked[1].path:match('^(.*)\\[^\\]+$')
    local _, err2 = Common.runBridge(string.format(
        '--launch-viewer "%s" --folder "%s"', prefs.viewerPath, folder or ''))

    progress:done()

    LrDialogs.message('IEDT Bridge',
        string.format('已把 %d 张照片的评分同步给 Viewer(跳过 %d, 一致 %d, 非ARW %d)。\n\n' ..
            'Viewer 已启动。选片打分完成后, 回到 Lightroom 执行\n' ..
            '「图库 > 增效工具额外命令 > 从 IEDT 同步评分」。',
            sum.patched or 0, sum.skip or 0, sum.keep or 0, skipped))
end)
