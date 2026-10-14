--[[ OpenCuller.lua — 图库 > 增效工具额外命令 > 打开 ARW 选片器

流程(读取方向): 把选中照片在 LR 目录里的评分/颜色写入 .xmp 旁车,
然后由 fastcull.exe 分离启动选片器(不阻塞 Lightroom)。
退出时可自动同步；也可回到 LR 执行「同步选片会话（仅修改过的字段）」取回结果。
]]

local LrApplication = import 'LrApplication'
local LrDialogs = import 'LrDialogs'
local LrTasks = import 'LrTasks'
local LrProgressScope = import 'LrProgressScope'

require 'common'
local Common = require 'common'
local LrUUID = import 'LrUUID'
local sessionId = LrUUID.generateUUID()
local trace = Common.startupTrace(sessionId)
Common.startupMark(trace, 'menu')

LrTasks.startAsyncTask(function()

    Common.startupMark(trace, 'task')

    local catalog = LrApplication.activeCatalog()
    local photos = catalog:getTargetPhotos()
    Common.startupMark(trace, 'selection')
    if #photos == 0 then
        LrDialogs.message('FastCull for ARW',
            '请先在图库里选中要选片的照片\n(可 Ctrl+A 全选当前文件夹)')
        return
    end

    local prefs = Common.getPrefs()
    if not Common.fileExists(prefs.cullerPath) then
        LrDialogs.message('FastCull for ARW', '找不到选片器程序:\n' ..
            prefs.cullerPath .. '\n\n请在插件管理器中修改 FastCull 程序路径。')
        return
    end

    local progress = LrProgressScope { title = '读取 LR 评分并启动选片器…' }

    local raw, formatted = Common.snapshotMetadata(catalog, photos)
    Common.startupMark(trace, 'metadata')
    local picked, skipped = Common.pickArwPhotos(photos, raw)
    Common.log(string.format('[open] 选中 %d 张, RAW/JPEG %d, 不支持 %d, python=%s, culler=%s',
        #photos, #picked, skipped, tostring(Common.getPrefs().pythonPath),
        tostring(Common.getPrefs().cullerPath)))
    if #picked == 0 then
        progress:done()
        LrDialogs.message('FastCull for ARW',
            string.format('选中的 %d 张照片里没有支持的 RAW/JPEG 文件。', #photos))
        return
    end

    -- 生成 path|rating|label|flag 状态文件(每行: 路径|星级|颜色|旗标)
    -- Unix permits these characters in filenames, but the legacy state protocol
    -- cannot encode them. Refuse before creating a session or changing metadata.
    for _, item in ipairs(picked) do
        if item.path:find('[\r\n|]') then
            progress:done()
            LrDialogs.message('FastCull', '照片路径含换行或 |，请先重命名再从 Lightroom 打开：\n' .. item.path)
            return
        end
    end
    -- flag: LR pickStatus 值 1=拾取 / -1=拒绝 / 0=无
    local stateFile = Common.listPath('fastcull_' .. sessionId .. '.txt')
    local isNative = not prefs.cullerPath:lower():match('%.py$')
    local manifest
    if isNative then
        local ok, value = (LrTasks.pcall or pcall)(function()
            Common.catalogKey(catalog) -- verify catalog identity before launching
            return Common.sessionPath(sessionId)
        end)
        if not ok then
            progress:done(); LrDialogs.message('FastCull for ARW', tostring(value)); return
        end
        manifest = value
    end
    Common.startupMark(trace, 'session-path')
    local f = io.open(stateFile, 'w')
    if not f then
        progress:done()
        LrDialogs.message('FastCull for ARW', '无法写入临时文件: ' .. stateFile)
        return
    end
    for _, item in ipairs(picked) do
        local rating = raw[item.photo].rating
        local label = formatted[item.photo].label
        if label == '' then label = nil end
        -- 旗标键为 pickStatus(SDK 4.0+)，旧键名/旧版本防御读取，失败按无旗标
        local pick = raw[item.photo].pickStatus or 0
        f:write(string.format('%s|%s|%s|%s\n',
            item.path,
            rating and tostring(rating) or '',
            label and tostring(label) or '',
            tostring(pick)))
    end
    f:close()
    Common.startupMark(trace, 'state-written')

    -- 把 LR 评分写入旁车 + 分离启动选片器 GUI
    local args = '--open ' .. Common.quote(stateFile)
    if manifest then args = args .. ' --session ' .. Common.quote(manifest) .. ' --session-id ' .. Common.quote(sessionId) end
    Common.startupMark(trace, 'invoke')
    local out, err = Common.runCuller(args, false, trace)
    -- 子进程已持有独立快照，不再依赖这一份 LR 导出文件。
    pcall(function() os.remove(stateFile) end)
    progress:done()
    if not out then
        LrDialogs.message('FastCull for ARW', err or '选片器程序执行失败')
        return
    end
    local sum = Common.parseSummary(out)
    Common.log(string.format('[open] wrote=%d kept=%d error=%d launched=%d',
        sum.wrote or -1, sum.kept or -1, sum.error or -1, sum.launched or -1))
    if (sum.launched or 0) < 1 then
        -- 透出程序错误摘要，便于诊断路径、旁车权限和输入问题。
        local brief = (out or ''):gsub('%s+', ' '):sub(1, 300)
        LrDialogs.message('FastCull for ARW',
            '选片器启动失败。\n\n程序输出:\n' .. brief ..
            '\n\n可手动运行检查:\n' ..
            string.format('"%s" --apply-state <statefile>', prefs.cullerPath))
        return
    end

    if manifest then Common.rememberSession(catalog, sessionId, manifest, picked, raw) end
    Common.startupMark(trace, 'ready')
    if manifest then require('develop').start(catalog,sessionId,manifest,picked) end

    if (sum.pending or 0) > 0 then
        Common.log(string.format('[open] GUI 已启动，后台初始化 %d 张评分', sum.pending))
        return -- 不用成功弹窗挡住刚打开的选片器；后台进度/错误在选片器显示。
    end
    LrDialogs.message('FastCull for ARW',
        string.format('选片器已启动：%d 张（评分已读入 %d，跳过 %d，非 ARW %d）。\n\n' ..
            '原生选片器退出时选择「同步全部并关闭」。\n' ..
            '稍后同步请使用「同步选片会话（仅修改过的字段）」；Python 回退使用「同步选中照片（读取照片元数据）」。',
            #picked, sum.wrote or 0, sum.kept or 0, skipped))
end)
