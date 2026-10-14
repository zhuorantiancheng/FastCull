--[[ SyncFromIEDT.lua — 图库 > 增效工具额外命令 > 从 IEDT 同步评分

流程: 读取选中照片 ARW 内嵌 XMP 里 Viewer 打的评分/标签 -> 写旁车 ->
直接更新 LR 目录(评分 + 颜色标签), 无需手动"从文件读取元数据"。
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
        LrDialogs.message('IEDT Bridge', '请先在图库里选中要同步的照片\n(可 Ctrl+A 全选当前文件夹)')
        return
    end

    local prefs = Common.getPrefs()
    if not Common.fileExists(prefs.pythonPath) then
        LrDialogs.message('IEDT Bridge', '找不到 Python:\n' .. prefs.pythonPath)
        return
    end

    local progress = LrProgressScope {
        title = '从 IEDT 同步评分…',
    }

    local picked, skipped = Common.pickArwPhotos(photos)
    if #picked == 0 then
        progress:done()
        LrDialogs.message('IEDT Bridge', '选中的照片里没有 ARW 文件。')
        return
    end

    -- 列表文件(每行一个 path)
    local listFile = Common.listPath('iedt_bridge_rev.txt')
    local f = io.open(listFile, 'w')
    if not f then
        progress:done()
        LrDialogs.message('IEDT Bridge', '无法写入临时文件: ' .. listFile)
        return
    end
    for _, item in ipairs(picked) do
        f:write(item.path .. '\n')
    end
    f:close()

    -- 调 python 桥: ARW 内嵌 -> 旁车
    local out, err = Common.runBridge(
        string.format('--reverse-from-list "%s" --ascii-summary', listFile))
    if not out then
        progress:done()
        LrDialogs.message('IEDT Bridge', err or '桥接脚本执行失败')
        return
    end
    local sum = Common.parseSummary(out)

    -- 旁车 -> LR 目录(评分 + 标签)
    local imported = 0
    catalog:withWriteAccessDo('从 IEDT 同步评分', function()
        for _, item in ipairs(picked) do
            local rating, label = Common.readSidecarValues(item.path)
            if rating then
                item.photo:setRawMetadata('rating', rating)
                imported = imported + 1
            end
            if label then
                item.photo:setRawMetadata('label', label)
            end
        end
    end)

    progress:done()

    LrDialogs.message('IEDT Bridge',
        string.format('同步完成:\n  更新目录评分 %d 张\n  旁车新建/更新 %d / 一致 %d\n  跳过 %d, 非ARW %d',
            imported, (sum.created or 0) + (sum.updated or 0), sum.keep or 0,
            sum.skip or 0, skipped))
end)
