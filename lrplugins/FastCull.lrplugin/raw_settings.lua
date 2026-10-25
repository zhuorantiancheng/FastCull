-- On-demand ORIGINAL export captures catalog edits without saving the source RAW.
-- Adobe SDK Guide, built-in export keys (pp. 61-68):
-- https://ioconsolerykerprodcdn.azureedge.net/static/installers/lr/sdk/2022/cross_platform/v13/doc/Lightroom%20Classic%20SDK%20Guide_1655133965.pdf
-- Request: id\nsource-UTF8hex\nnew-target-directory-UTF8hex\n
-- Result:  id\nsource-UTF8hex\nOK\tprepared-RAW-UTF8hex\n (or ERROR\terror-UTF8hex).
-- The prepared RAW and all sibling resources belong to the requested source;
-- callers must retain this directory and must save edits back to the SOURCE path.
local LrTasks=import 'LrTasks'
local Common=require 'common'
local RawSettings={}
local running={}
local call=LrTasks.pcall or pcall
local limit=131072

local function hex(value)
    return (value:gsub('.',function(c) return string.format('%02x',string.byte(c)) end))
end
local function read(path, maximum)
    local f=io.open(path,'rb')
    if not f then return nil end
    local text=f:read(maximum+1) or '';f:close()
    if #text>maximum then error('RAW 请求超过长度限制') end
    return text
end
local function absolute(path)
    if type(path)~='string' or path=='' or path:find('[%z\1-\31]') then error('无效的 RAW 缓存路径') end
    local value=path:gsub('\\','/')
    if Common.isMac then
        if value:sub(1,1)~='/' or value:sub(1,2)=='//' then error('RAW 缓存路径必须为本地绝对路径') end
    elseif not value:match('^%a:/') or value:sub(3):find(':',1,true) then
        error('RAW 缓存路径必须为本地绝对路径')
    end
    if value:find('//',1,true) then error('RAW 缓存路径含空目录段') end
    for part in value:gmatch('[^/]+') do
        if part=='.' or part=='..' or (not Common.isMac and part:find('[ .]$')) then
            error('RAW 缓存路径含不安全目录段')
        end
    end
    return (value:gsub('/+$',''))
end
local function same(a,b)
    return Common.pathKey(a)==Common.pathKey(b)
end
local function current(catalog,key,application)
    if Common.catalogKey(catalog)~=key or Common.catalogKey(application.activeCatalog())~=key then
        error('LR 目录已切换，请回到原目录后重试 RAW 编辑')
    end
end
local function manifestActive(manifest,id)
    local f=io.open(manifest,'r')
    if not f then return nil end -- A transient replacement/sharing failure is not closure.
    local line=f:read('*l') or '';f:close()
    local version,actual,status=line:match('^FASTCULL_SESSION_V(%d+)\t([^\t]+)\t([^\t]+)\t%d+\r?$')
    if not version or actual~=id or (status~='active' and status~='closed') then
        error('RAW 编辑的选片会话标识无效')
    end
    return status=='active'
end
local function identity(photo,uuid,path)
    return photo and photo:getRawMetadata('uuid')==uuid and photo:getRawMetadata('path')==path
end
local function selected(picked,path,catalog)
    local uuid
    for _,item in ipairs(picked) do
        if item.path==path then
            local actual=item.photo:getRawMetadata('uuid')
            if type(actual)~='string' or actual=='' or not identity(item.photo,actual,path) then
                error('LR 照片身份或路径已改变，请重新打开选片器')
            end
            if uuid and uuid~=actual then error('同一原片选择了多个 LR 虚拟副本，无法确定 RAW 显影版本') end
            uuid=actual
        end
    end
    if not uuid then error('RAW 请求不属于本次选片会话') end
    local photo=catalog:findPhotoByUuid(uuid)
    if not identity(photo,uuid,path) then
        error('LR 照片身份或路径已改变，请重新打开选片器')
    end
    return photo,uuid
end
local function prepare(catalog,key,id,manifest,picked,text)
    local LrApplication=import 'LrApplication'
    local LrPathUtils=import 'LrPathUtils'
    local LrFileUtils=import 'LrFileUtils'
    local requestId,sourceHex,targetHex=text:gsub('\r\n','\n'):match('^([^\n]+)\n([^\n]+)\n([^\n]+)\n?$')
    if requestId~=id or not sourceHex or not targetHex then error('RAW 请求的会话标识或字段无效') end
    local source=Common.hexDecode(sourceHex)
    local target=absolute(Common.hexDecode(targetHex))
    local extension=source:lower():match('%.([%w]+)$')
    if extension~='arw' and extension~='nef' and extension~='dng' then error('RAW 编辑目前仅支持 ARW、NEF 和 DNG') end
    current(catalog,key,LrApplication)
    if not manifestActive(manifest,id) then error('RAW 编辑会话已关闭') end
    if not same(absolute(Common.sessionPath(id)),absolute(manifest)) then error('RAW 请求的会话路径无效') end
    local photo,uuid=selected(picked,source,catalog)
    local root=absolute(LrPathUtils.child(LrPathUtils.parent(LrPathUtils.parent(manifest)),'raw-cache'))
    local parent=absolute(LrPathUtils.parent(target))
    local leaf=target:match('([^/]+)$')
    local prefix='lr-'..id..'-'
    local nonce=leaf and leaf:sub(#prefix+1)
    if not same(parent,root) or not leaf or leaf:sub(1,#prefix)~=prefix or
       not nonce or #nonce<8 or #nonce>128 or nonce:find('[^%w-]') then
        error('RAW 导出目标必须为本会话 raw-cache 下的全新私有目录')
    end
    if LrFileUtils.exists(target) then error('RAW 导出目标已存在，请使用新的请求目录') end
    local made,reason=LrFileUtils.createAllDirectories(target)
    if made~=true then error('无法创建 RAW 导出目录：'..tostring(reason)) end
    -- Recheck after SDK calls that can yield, before invoking the exporter.
    current(catalog,key,LrApplication)
    if not identity(photo,uuid,source) then error('导出前 LR 照片身份已改变') end
    local LrExportSession=import 'LrExportSession' -- No import or export for normal culling.
    local session=LrExportSession {
        photosToExport={photo},
        exportSettings={
            LR_format='ORIGINAL',
            LR_export_destinationType='specificFolder',
            LR_export_destinationPathPrefix=target,
            LR_export_useSubfolder=false,
            LR_collisionHandling='skip',
            LR_renamingTokensOn=false,
            LR_extensionCase='lowercase',
            LR_reimportExportedPhoto=false,
            LR_minimizeEmbeddedMetadata=false,
            LR_embeddedMetadataOption='all',
            LR_metadata_keywordOptions='lightroomHierarchical',
            LR_removeLocationMetadata=false,
            LR_removeFaceMetadata=false,
            LR_size_doConstrain=false,
            LR_outputSharpeningOn=false,
            LR_useWatermark=false,
            LR_export_postProcessing='doNothing',
        },
    }
    local output,count=nil,0
    for _,rendition in session:renditions() do
        count=count+1
        if count~=1 or not identity(rendition.photo,uuid,source) then error('LR 导出返回了非请求照片') end
        local ok,value=rendition:waitForRender()
        if ok~=true then error('LR 原始文件导出失败：'..tostring(value)) end
        output=absolute(value)
    end
    current(catalog,key,LrApplication)
    if count~=1 or not output then error('LR 未返回请求照片的导出结果') end
    if not identity(photo,uuid,source) or not identity(catalog:findPhotoByUuid(uuid),uuid,source) then
        error('导出期间 LR 照片身份已改变')
    end
    local name=source:match('([^/\\]+)$'):gsub('%.[^%.]+$','')..'.'..extension
    local expected=absolute(LrPathUtils.child(target,name))
    if not same(output,expected) or same(output,source) or LrFileUtils.exists(output)~='file' then
        error('LR 导出结果路径无效，未接受该文件')
    end
    local sidecar=(output:gsub('%.[^%.]+$','.xmp'))
    if extension~='dng' and LrFileUtils.exists(sidecar)~='file' then
        error('LR 未导出当前显影设置的 XMP 旁车，请重试')
    end
    -- Keep the WHOLE controlled export directory, including .acr/relative assets.
    local marker,err=io.open(LrPathUtils.child(target,'.fastcull-adobe-export'),'wb')
    if not marker then error('无法标记 RAW 私有导出目录：'..tostring(err)) end
    marker:close()
    return output
end
local function respond(manifest,id,text,ok,value)
    local source=text:gsub('\r\n','\n'):match('^[^\n]+\n([%x]+)\n') or '~'
    local request=manifest:gsub('%.txt$','.raw-request')
    -- A newer GUI request owns the result mailbox; do not overwrite its response.
    if read(request,limit)~=text then return end
    local path=manifest:gsub('%.txt$','.raw-result')
    local temporary=path..'.tmp'
    local f,err=io.open(temporary,'wb')
    if not f then error('无法写入 RAW 响应：'..tostring(err)) end
    f:write(id..'\n'..source..'\n'..(ok and 'OK' or 'ERROR')..'\t'..hex(tostring(value))..'\n')
    f:close()
    -- Only our fixed small mailbox file is replaced; exported assets stay intact.
    os.remove(path)
    local moved,reason=os.rename(temporary,path)
    if not moved then error('无法提交 RAW 响应：'..tostring(reason)) end
end

function RawSettings.start(catalog,id,manifest,picked)
    if not LrTasks.sleep then return end
    if type(id)~='string' or #id==0 or #id>128 or id:find('[^%w-]') or
       type(manifest)~='string' or not manifest:match('%.txt$') then error('无效的 RAW 选片会话') end
    local token=Common.pathKey(manifest)
    if running[token] then return end
    running[token]=true
    local key=Common.catalogKey(catalog)
    LrTasks.startAsyncTask(function()
        local success,message=call(function()
            local previous
            while true do
                LrTasks.sleep(0.25)
                local active=manifestActive(manifest,id)
                if active==false then return end
                if active then
                    local text=read(manifest:gsub('%.txt$','.raw-request'),limit)
                    if text and text~=previous then
                        previous=text
                        local ok,value=call(function() return prepare(catalog,key,id,manifest,picked,text) end)
                        respond(manifest,id,text,ok,value)
                    end
                end
            end
        end)
        running[token]=nil
        if not success then Common.log('[raw-settings] '..tostring(message)) end
    end)
end
return RawSettings
