--[[ common.lua — FastCull for ARW 插件共用工具（分发友好：不绑定特定机器路径） ]]

local LrPrefs = import 'LrPrefs'
local LrPathUtils = import 'LrPathUtils'
local LrFileUtils = import 'LrFileUtils'
local LrTasks = import 'LrTasks'
local LrDate = import 'LrDate'

local Common = {}
Common.isMac = MAC_ENV == true
function Common.quote(value)
    assert(not value:find('[\r\n]') and not value:find(string.char(0), 1, true), '命令参数含换行或 NUL')
    if Common.isMac then return "'" .. value:gsub("'", "'\"'\"'") .. "'" end
    return '"' .. value .. '"'
end
function Common.pathKey(path)
    if Common.isMac then return path end
    return path:gsub('\\','/'):lower()
end

-- SDK wall clock: os.clock measures CPU time and misses waits in io.popen.
function Common.startupTrace(id)
    local ok, now = (LrTasks.pcall or pcall)(LrDate.currentTime)
    return { id = id, started = ok and type(now) == 'number' and now or nil }
end
function Common.startupMark(trace, phase)
    if not trace or not trace.started then return end
    (LrTasks.pcall or pcall)(function()
        local now = LrDate.currentTime()
        Common.log(string.format('[startup] id=%s phase=%s elapsed_ms=%.1f unix_ms=%.0f',
            trace.id, phase, (now - trace.started) * 1000, (now + 978307200) * 1000))
    end)
end

-- Lightroom supports appData/appPrefs, not the Windows localAppData selector.
-- Persist sessions outside temp/plugin install folders; validate SDK error returns.
local function storageDirectory(name)
    local call = LrTasks.pcall or pcall
    local failures = {}
    for _, selector in ipairs({ 'appData', 'appPrefs' }) do
        local found, base = call(function() return LrPathUtils.getStandardFilePath(selector) end)
        if found and type(base) == 'string' and base ~= '' then
            local dir = LrPathUtils.child(LrPathUtils.child(base, 'FastCull'), name)
            local ran, created, reason = call(function() return LrFileUtils.createAllDirectories(dir) end)
            if ran and created == true then return dir end
            failures[#failures + 1] = dir .. '：' .. tostring(ran and reason or created)
        else
            failures[#failures + 1] = selector .. '：' .. tostring(base or 'SDK 未返回目录')
        end
    end
    error('无法创建 FastCull 持久数据目录：\n' .. table.concat(failures, '\n'))
end

-- 可选旧 Python 原型只探测 PATH，不绑定开发机器。
Common.PYTHON_CANDIDATES = {
    'py -3',
    'python',
}

-- 插件内置的 Rust 选片器（分发时随插件目录走）
function Common.bundledCullerPath()
    if Common.isMac then
        return LrPathUtils.child(_PLUGIN.path, 'FastCull.app/Contents/MacOS/fastcull')
    end
    return LrPathUtils.child(_PLUGIN.path, 'fastcull.exe')
end

-- 默认优先 Rust exe；未构建时允许使用 Python 原型回退。
function Common.defaultCullerPath()
    local bundled = Common.bundledCullerPath()
    -- The old Tk prototype is Windows-only and is not included in Mac packages.
    if Common.isMac then return bundled end
    if Common.fileExists(bundled) then
        return bundled
    end
    local dir = LrPathUtils.parent(LrPathUtils.parent(_PLUGIN.path))
    local python = LrPathUtils.child(_PLUGIN.path, 'arw_culler.py')
    if Common.fileExists(python) then return python end
    return LrPathUtils.child(dir, 'arw_culler.py')
end

-- 探测可用的 Python 命令（返回 nil 表示都没找到）
function Common.detectPython()
    if not io.popen then return nil end
    for _, exe in ipairs(Common.PYTHON_CANDIDATES) do
        local cmd = exe
        if exe:find('\\', 1, true) then cmd = '"' .. exe .. '"' end
        local pipe = io.popen(cmd .. ' --version 2>&1')
        if pipe then
            local out = pipe:read('*a') or ''
            pipe:close()
            if out:find('Python') then
                return exe
            end
        end
    end
    return nil
end

function Common.getPrefs()
    local prefs = LrPrefs.prefsForPlugin()
    local bundled = Common.bundledCullerPath()
    -- 升级旧的默认 Python 路径；显式自定义程序路径继续保留。
    if not prefs.cullerPath or prefs.cullerPath == '' or
       (Common.isMac and prefs.cullerPath:match('/FastCull%.lrplugin/FastCull%.app/Contents/MacOS/fastcull$') and Common.fileExists(bundled)) or
       (prefs.cullerPath:lower():match('[\\/]fastcull%.lrplugin[\\/]fastcull%.exe$') and Common.fileExists(bundled)) or
       (prefs.cullerPath:match('[\\/]arw_culler%.py$') and
        (prefs.cullerPath == LrPathUtils.child(_PLUGIN.path, 'arw_culler.py') or
         prefs.cullerPath == LrPathUtils.child(LrPathUtils.parent(LrPathUtils.parent(_PLUGIN.path)), 'arw_culler.py')) and
        Common.fileExists(bundled)) then
        prefs.cullerPath = Common.defaultCullerPath()
    end
    if prefs.cullerPath:lower():match('%.py$') and
       (not prefs.pythonPath or prefs.pythonPath == '') then
        prefs.pythonPath = Common.detectPython()
    end
    return prefs
end

-- ---------------- 日志 ----------------
-- 插件日志在 SDK appData/FastCull/logs（失败回退 appPrefs）。
-- Rust 程序日志仍在 %LOCALAPPDATA%/FastCull/logs。
-- 所有调用 pcall 包裹：日志失败绝不影响主流程
local logDir = nil
local function ensureLogDir()
    if logDir ~= nil then return logDir end
    logDir = false
    local ok, dir = (LrTasks.pcall or pcall)(function() return storageDirectory('logs') end)
    if ok then logDir = dir end
    return logDir
end

function Common.log(msg)
    (LrTasks.pcall or pcall)(function()
        local d = ensureLogDir()
        if not d then return end
        local day = ''
        pcall(function() day = os.date('%Y%m%d') end)
        local ts = ''
        pcall(function() ts = os.date('%H:%M:%S') end)
        local f = io.open(LrPathUtils.child(d, 'fastcull_plugin_' .. day .. '.log'), 'a')
        if f then
            f:write(string.format('%s %s\n', ts, msg))
            f:close()
        end
    end)
end

-- 临时状态文件路径(文件名纯 ASCII, 内容 UTF-8)
-- 注意: LR 沙箱里 os.getenv 不存在, 用 LrPathUtils.getStandardFilePath('temp')
function Common.listPath(name)
    local tmp = nil
    pcall(function()
        tmp = LrPathUtils.getStandardFilePath('temp')
    end)
    if not tmp or tmp == '' then
        tmp = _PLUGIN.path
    end
    tmp = tmp:gsub('[\\/]+$', '')
    return LrPathUtils.child(tmp, name or 'fastcull_state.txt')
end

function Common.fileExists(path)
    if not path then return false end
    local f = io.open(path, 'r')
    if f then f:close() return true end
    return false
end

-- 构造运行选片器的命令串：路径含 \ 时加引号，'py -3' 这类命令原样拼接
local function pythonCmd(exe, script, args)
    if exe:find('\\', 1, true) or exe:find('/', 1, true) then
        return string.format('"%s" "%s" %s', exe, script, args)
    end
    return string.format('%s "%s" %s', exe, script, args)
end

-- 运行选片器脚本, 返回 stdout 文本
function Common.runCuller(args, nativeOnly, trace)
    local prefs = Common.getPrefs()
    local program = prefs.cullerPath
    if nativeOnly and program:lower():match('%.py$') then program = Common.bundledCullerPath() end
    local isPython = program:lower():match('%.py$')
    if Common.isMac and isPython then
        return nil, 'Mac 版请使用 FastCull.app/Contents/MacOS/fastcull；旧版 Python 原型仅支持 Windows。'
    end
    if isPython and (not prefs.pythonPath or prefs.pythonPath == '') then
        return nil, '未找到 Python。请安装 Python 3(或 py 启动器)，\n' ..
            '或在 文件 > 增效工具管理器 > FastCull for ARW 中手动填写 python.exe 路径。'
    end
    local cmd = isPython and pythonCmd(prefs.pythonPath, program, args)
        or (Common.quote(program) .. ' ' .. args)
    Common.log('[run] ' .. cmd)
    if not isPython then
        if not LrTasks.execute then return nil, '此 Lightroom 环境未提供 LrTasks.execute' end
        local outputFile = Common.listPath('fastcull_output_' .. (import 'LrUUID').generateUUID() .. '.txt')
        -- SDK execute yields the LR task while the child runs. A blocking
        -- io.popen read stops LR's UI message processing and delays GUI startup.
        Common.startupMark(trace, 'execute-start')
        local ran, code = (LrTasks.pcall or pcall)(function()
            local command = cmd .. ' > ' .. Common.quote(outputFile) .. ' 2>&1'
            if not Common.isMac then command = '"' .. command .. '"' end
            return LrTasks.execute(command)
        end)
        Common.startupMark(trace, 'execute-return')
        local out
        local f = io.open(outputFile, 'r')
        if f then out = f:read('*a'); f:close() end
        pcall(function() os.remove(outputFile) end)
        Common.startupMark(trace, 'file-read')
        if not ran then return nil, '选片器执行失败：' .. tostring(code) end
        if code ~= 0 or not out then
            return nil, '选片器执行失败（退出码 ' .. tostring(code) .. '）：\n' .. (out or '未收到程序输出')
        end
        Common.log('[run] out: ' .. out:gsub('%s+', ' '):sub(1, 500))
        return out, nil
    end
    if not io.popen then return nil, '此 Lightroom 环境不允许 io.popen，无法运行 Python 回退' end
    -- Windows cmd /c 会去掉第一层引号，外层再包一对，兼容程序/参数中的空格。
    local pipe = io.popen('"' .. cmd .. ' 2>&1"')
    Common.startupMark(trace, 'popen-return')
    if not pipe then
        Common.log('[run] popen 失败')
        return nil, '无法启动选片器: ' .. program
    end
    local out = pipe:read('*a')
    Common.startupMark(trace, 'read-return')
    pipe:close()
    Common.startupMark(trace, 'pipe-closed')
    Common.log('[run] out: ' .. (out or ''):gsub('%s+', ' '):sub(1, 500))
    return out, nil
end

-- 从脚本输出解析 "RESULT k=v ..." 摘要
function Common.parseSummary(out)
    local result = {}
    if not out then return result end
    for k, v in out:gmatch('(%w+)=(%d+)') do
        result[k] = tonumber(v)
    end
    return result
end

-- 从选中照片里筛出 ARW, 返回 {photo, path} 列表
function Common.snapshotMetadata(catalog, photos)
    local raw, formatted
    local call = LrTasks.pcall or pcall
    if catalog.batchGetRawMetadata then
        local ok, values = call(function()
            return catalog:batchGetRawMetadata(photos, { 'path', 'rating', 'pickStatus', 'uuid' })
        end)
        if ok then raw = values end
    end
    if catalog.batchGetFormattedMetadata then
        local ok, values = call(function()
            return catalog:batchGetFormattedMetadata(photos, { 'label' })
        end)
        if ok then formatted = values end
    end
    raw = raw or {}; formatted = formatted or {}
    for _, photo in ipairs(photos) do
        if not raw[photo] then
            local ok, flag = call(function() return photo:getRawMetadata('pickStatus') end)
            local uuidOk, uuid = call(function() return photo:getRawMetadata('uuid') end)
            raw[photo] = { uuid = uuidOk and uuid or nil, path = photo:getRawMetadata('path'),
                rating = photo:getRawMetadata('rating'), pickStatus = ok and flag or 0 }
        end
        if not formatted[photo] then
            formatted[photo] = { label = photo:getFormattedMetadata('label') }
        end
    end
    return raw, formatted
end

function Common.isSupportedPhoto(path)
    local extensions={arw=true,cr2=true,nef=true,dng=true,jpg=true,jpeg=true,jpe=true}
    return path and extensions[path:lower():match('%.([%w]+)$')] == true
end
function Common.pickArwPhotos(photos, metadata)
    -- Keep the legacy function name for plugin compatibility.
    local picked = {}
    local skipped = 0
    for _, photo in ipairs(photos) do
        local path = metadata and metadata[photo] and metadata[photo].path
        if not metadata then path = photo:getRawMetadata('path') end
        if Common.isSupportedPhoto(path) then
            picked[#picked + 1] = { photo = photo, path = path }
        else
            skipped = skipped + 1
        end
    end
    return picked, skipped
end

-- 从旁车文本提取 xmp:Rating / xmp:Label / sr:Flag (属性式或元素式)
-- All sidecars are read by the Rust namespace-aware parser.
function Common.hexDecode(token)
    if token == '~' then return '' end
    if #token == 0 or #token % 2 ~= 0 or token:find('[^%x]') then error('无效的桥接字段编码') end
    return (token:gsub('%x%x', function(v) return string.char(tonumber(v,16)) end))
end
function Common.parseBridge(out, expectedId)
    if not out then error('未收到 Rust 同步结果') end
    local result = {rows={}, errors={}}
    for line in out:gmatch('[^\r\n]+') do
        if line:match('^SESSION\t') then
            local id,status,revision=line:match('^SESSION\t([^\t]+)\t([^\t]+)\t(%d+)$')
            if not id or (status~='active' and status~='closed' and status~='standalone') then error('无效的会话标识') end
            if expectedId and id~=expectedId then error('同步会话 ID 不匹配') end
            if result.id then error('重复的会话标识') end
            result.id=id;result.status=status;result.revision=tonumber(revision)
        elseif line:match('^ROW\t') or line:match('^ROW_V3\t') or line:match('^ROW_V4\t') or line:match('^ROW_V5\t') then
            local fields={}
            for field in (line..'\t'):gmatch('(.-)\t') do fields[#fields+1]=field end
            local v5=fields[1]=='ROW_V5'
            local v4=fields[1]=='ROW_V4' or v5
            local v3=fields[1]=='ROW_V3' or v4
            if (v5 and #fields~=23) or (v4 and not v5 and #fields~=16) or (v3 and not v4 and #fields~=13) or (not v3 and #fields~=5 and #fields~=6) then error('无效的同步记录') end
            local path,rating,label,flag=fields[2],fields[3],fields[4],fields[5]
            local exposure=fields[6]
            if exposure=='-' then exposure=nil end
            local r=rating~='-' and tonumber(rating) or nil
            if rating~='-' and (not r or r<0 or r>5 or r%1~=0) then error('无效的星级') end
            local l=label~='-' and Common.hexDecode(label) or nil
            local f=flag~='-' and Common.hexDecode(flag) or nil
            if f~=nil and f~='' and f~='pick' and f~='reject' then error('无效的旗标') end
            local e=exposure and tonumber(exposure) or nil
            if exposure and (not e or e~=e or e < -5 or e > 5) then error('无效的曝光参数') end
            local row={path=Common.hexDecode(path),rating=r,label=l,flag=f,exposure=e}
            if v3 then
                if fields[7]~='-' then
                    local o=tonumber(fields[7])
                    if not o or o%1~=0 or o<1 or o>8 then error('无效的旋转方向') end
                    row.orientation=o
                end
                if fields[8]=='-' then
                    for i=9,13 do if fields[i]~='-' then error('不完整的裁切记录') end end
                else
                    if fields[8]~='0' and fields[8]~='1' then error('无效的裁切开关') end
                    local crop={HasCrop=fields[8]=='1'}
                    for i,key in ipairs({'CropLeft','CropTop','CropRight','CropBottom','CropAngle'}) do
                        local n=tonumber(fields[i+8])
                        local lo,hi=0,1;if key=='CropAngle' then lo,hi=-45,45 end
                        if not n or n~=n or n<lo or n>hi then error('无效的裁切参数：'..key) end
                        crop[key]=n
                    end
                    if crop.HasCrop and crop.CropLeft==crop.CropRight and crop.CropTop==crop.CropBottom then error('裁切范围为空') end
                    row.crop=crop
                end
            end
            if v4 then
                if fields[14]=='-' then
                    if fields[15]~='-' or fields[16]~='-' then error('不完整的白平衡记录') end
                else
                    local mode=Common.hexDecode(fields[14])
                    local wb={WhiteBalance=mode}
                    if mode=='As Shot' then
                        if fields[15]~='-' or fields[16]~='-' then error('拍摄白平衡不应包含自定义参数') end
                    elseif mode=='Custom' then
                        local temperature,tint=tonumber(fields[15]),tonumber(fields[16])
                        if not temperature or temperature%1~=0 or temperature<2000 or temperature>50000 or
                           not tint or tint%1~=0 or tint< -150 or tint>150 then error('无效的自定义白平衡参数') end
                        wb.Temperature=temperature;wb.Tint=tint
                    else error('不支持的白平衡模式') end
                    row.whiteBalance=wb
                end
            end
            if v5 then
                local basic={}
                for i,key in ipairs({'contrast','highlights','shadows','whites','blacks','vibrance','saturation'}) do
                    local token=fields[16+i]
                    if token~='-' then
                        local value=tonumber(token)
                        if not token:match('^[-+]?%d+$') or not value or value%1~=0 or value< -100 or value>100 then error('无效的基础调整参数：'..key) end
                        basic[key]=value
                    end
                end
                if next(basic) then row.basic=basic end
            end
            result.rows[#result.rows+1]=row
        elseif line:match('^ROW_ERROR\t') then
            local path,message=line:match('^ROW_ERROR\t([^\t]+)\t([^\t]+)$')
            if not path then error('无效的错误记录') end
            result.errors[#result.errors+1]=Common.hexDecode(path)..'：'..Common.hexDecode(message)
        end
    end
    local summary=Common.parseSummary(out)
    result.pendingOtherFields=tonumber(out:match('RESULT pending_other_fields=(%d+)'))
    if not result.id or summary.rows~=#result.rows or summary.error~=#result.errors then error('Rust 同步输出不完整：'..out:sub(1,300)) end
    return result
end
function Common.catalogKey(catalog)
    return Common.pathKey(catalog:getPath())
end
function Common.sessionPath(id)
    local dir=storageDirectory('sessions')
    return LrPathUtils.child(dir,'session_'..id..'.txt')
end
function Common.rememberSession(catalog,id,path,picked,raw)
    local prefs=Common.getPrefs()
    local entries=prefs.sessions or {}
    local photos={}
    local paths={}
    for _,item in ipairs(picked) do
        paths[item.path]=true
        local uuid=raw[item.photo].uuid
        if uuid then
            photos[item.path]=photos[item.path] or {}
            photos[item.path][#photos[item.path]+1]=uuid
        end
    end
    entries[#entries+1]={id=id,path=path,catalog=Common.catalogKey(catalog),synced=0,photos=photos,paths=paths}
    prefs.sessions=entries
end
return Common
