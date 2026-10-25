-- Session-scoped synchronization; all XMP parsing is delegated to Rust.
local LrApplication=import 'LrApplication'
local LrDialogs=import 'LrDialogs'
local LrTasks=import 'LrTasks'
local LrFunctionContext=import 'LrFunctionContext'
local LrProgressScope=import 'LrProgressScope'
local Common=require 'common'
local Edits=require 'photo_edits'
local Sync={}
local call=LrTasks.pcall or pcall

local function synchronize(catalog, selectedPhotos, selectedOnly, onlySessionId, starsOnly, automatic)
        local prefs=Common.getPrefs()
        local owners,pending,issues={},{},{}
        local function issue(owner,message)
            owner.errors=owner.errors+1
            issues[#issues+1]=tostring(message)
            Common.log('[sync] '..tostring(message))
        end
        local function addRows(owner,result)
            owner.result=result
            for _,message in ipairs(result.errors) do issue(owner,message) end
            for _,sourceRow in ipairs(result.rows) do
                local row=sourceRow
                if not starsOnly or row.rating~=nil then
                if starsOnly then row={path=row.path,rating=row.rating} end
                local photos={}
                local ok,err=call(function()
                    if owner.selected then
                        photos=owner.selected[row.path] or {}
                    else
                        if not owner.entry.paths or not owner.entry.paths[row.path] then error('同步记录超出原始会话选区：'..row.path) end
                        local uuids=owner.entry.photos and owner.entry.photos[row.path]
                        if uuids and #uuids>0 then
                            if not catalog.findPhotoByUuid then error('此 SDK 缺少按照片 UUID 查找的接口') end
                            local seen={}
                            for _,uuid in ipairs(uuids) do
                                local photo=catalog:findPhotoByUuid(uuid)
                                if not photo then error('原会话照片已不在此目录中：'..row.path) end
                                if not seen[uuid] then photos[#photos+1]=photo;seen[uuid]=true end
                            end
                        else
                            if not catalog.findPhotoByPath then error('此 SDK 缺少按照片路径查找的接口') end
                            local photo=catalog:findPhotoByPath(row.path)
                            if photo then photos[1]=photo end
                        end
                    end
                    if #photos==0 then error('未找到原会话照片：'..row.path) end
                end)
                if not ok then issue(owner,err)
                else
                    for _,photo in ipairs(photos) do
                        owner.total=owner.total+1
                        pending[#pending+1]={photo=photo,row=row,owner=owner}
                    end
                end
                end
            end
        end
        if selectedOnly then
            local photos=selectedPhotos
            if #photos==0 then LrDialogs.message('FastCull for ARW','请先选择需要手动同步的照片。');return end
            local selected={}
            local LrUUID=import 'LrUUID'
            local list=Common.listPath('fastcull_read_'..LrUUID.generateUUID()..'.txt')
            local f,err=io.open(list,'w')
            if not f then LrDialogs.message('FastCull for ARW',tostring(err));return end
            for _,photo in ipairs(photos) do
                local path=photo:getRawMetadata('path')
                if Common.isSupportedPhoto(path) then
                    if not selected[path] then f:write(path..'\n');selected[path]={} end
                    selected[path][#selected[path]+1]=photo
                end
            end
            f:close()
            local owner={selected=selected,total=0,success=0,errors=0};owners[1]=owner
            local ok,result=call(function()
                local out,runError=Common.runCuller('--read-sidecars '..Common.quote(list),true)
                if not out then error(runError) end
                return Common.parseBridge(out)
            end)
            os.remove(list)
            if ok then addRows(owner,result) else issue(owner,result) end
        else
            local ok,key=call(function() return Common.catalogKey(catalog) end)
            if not ok then LrDialogs.message('FastCull for ARW',tostring(key));return end
            for _,entry in ipairs(prefs.sessions or {}) do
                if entry.catalog==key and (not onlySessionId or entry.id==onlySessionId) and (entry.organizationPending or not (entry.closed and entry.synced==entry.revision)) then
                    local owner={entry=entry,total=0,success=0,errors=0};owners[#owners+1]=owner
                    local readOK,result=call(function()
                        local command
                        if starsOnly then command=string.format('--sync-stars %s %d %d',Common.quote(entry.path),math.max(entry.synced or 0,entry.starsSynced or 0),entry.synced or 0)
                        else command=string.format('--sync-session %s %d %d',Common.quote(entry.path),entry.synced or 0,entry.starsSynced or 0) end
                        local out,runError=Common.runCuller(command,true)
                        if not out then error(runError) end
                        return Common.parseBridge(out,entry.id)
                    end)
                    if readOK then addRows(owner,result) else issue(owner,result) end
                end
            end
            if #owners==0 then
                if not automatic then LrDialogs.message('FastCull for ARW','本目录没有待同步的选片会话。手动读取旁车请使用「同步选中照片（读取照片元数据）」。') end
                return
            end
        end
        if Common.catalogKey(LrApplication.activeCatalog())~=Common.catalogKey(catalog) then
            error('同步期间 LR 目录已切换，原会话保留供重试')
        end
        local completed=0
        if #pending>0 then
            local gateRan=false
            local ok,err=call(function()
                LrFunctionContext.callWithContext('FastCull session sync',function(context)
                    local scope=LrProgressScope {title='同步 FastCull 选片结果…',functionContext=context}
                    catalog:withWriteAccessDo('Sync FastCull decisions',function()
                        gateRan=true
                        for i,item in ipairs(pending) do
                            local row=item.row
                            local saved,message=call(function()
                                if row.rating~=nil then item.photo:setRawMetadata('rating',row.rating) end
                                if row.label~=nil then
                                    local color=row.label:lower()
                                    if color=='red' or color=='yellow' or color=='green' or color=='blue' or color=='purple' then
                                        item.photo:setRawMetadata('colorNameForLabel',color)
                                    else item.photo:setRawMetadata('label',row.label) end
                                end
                                if row.flag~=nil then item.photo:setRawMetadata('pickStatus',row.flag=='pick' and 1 or (row.flag=='reject' and -1 or 0)) end
                                Edits.apply(item.photo,row)
                            end)
                            if saved then item.applied=true
                            else issue(item.owner,row.path..'：部分字段未同步，将保留供重试。'..tostring(message)) end
                            scope:setPortionComplete(i,#pending)
                        end
                    end)
                    scope:done()
                end)
            end)
            if not ok or not gateRan then
                for _,owner in ipairs(owners) do issue(owner,'LR 目录事务未完成：'..tostring(err)) end
            else
                for _,item in ipairs(pending) do
                    if item.applied then
                        local verified,message=call(function() Edits.verify(item.photo,item.row) end)
                        if verified then item.owner.success=item.owner.success+1;completed=completed+1
                        else issue(item.owner,item.row.path..'：'..tostring(message)) end
                    end
                end
            end
        end
        -- Acknowledge exactly the exported snapshot. Newer edits remain pending.
        local sessions=prefs.sessions or {}
        for _,owner in ipairs(owners) do
            owner.editsSynced=owner.entry and owner.result and owner.errors==0 and owner.success==owner.total
            if owner.editsSynced then
                local folderOK,note=call(function() return require('organization').prepare(catalog,owner.entry) end)
                if folderOK then owner.organizationNote=note
                else
                    owner.organizationFailed=true;owner.entry.organizationPending=true
                    issue(owner,'文件整理未完成，将保留供重试：'..tostring(note))
                end
            end
            if owner.editsSynced then
                if starsOnly then
                    -- WB and basic adjustments are non-star revisions too. Never acknowledge
                    -- it (or other Develop edits) when only stars were applied.
                    for _,entry in ipairs(sessions) do
                        if entry.id==owner.entry.id and entry.catalog==owner.entry.catalog then
                            entry.starsSynced=owner.result.revision
                            entry.revision=owner.result.revision;entry.closed=owner.result.status=='closed'
                            if owner.result.pendingOtherFields==0 then entry.synced=owner.result.revision end
                        end
                    end
                    if not owner.organizationFailed then os.remove(owner.entry.path:gsub('%.txt$','.stars-request')) end
                else
                for _,entry in ipairs(sessions) do
                    if entry.id==owner.entry.id and entry.catalog==owner.entry.catalog then
                        entry.synced=owner.result.revision;entry.revision=owner.result.revision;entry.closed=owner.result.status=='closed'
                    end
                end
                if not owner.organizationFailed then os.remove(owner.entry.path:gsub('%.txt$','.stars-request')) end
                end
            end
        end
        local waiting={}
        for _,entry in ipairs(sessions) do
            -- Closed GUI sessions cannot produce another revision. Retain only
            -- unacknowledged/active sessions in LR preferences; keep disk receipts.
            if entry.organizationPending or not (entry.closed and entry.synced==entry.revision) then waiting[#waiting+1]=entry end
        end
        prefs.sessions=waiting
        local message=string.format('已同步 %d 张照片%s。',completed,starsOnly and '本次修改的星级' or (selectedOnly and '的旁车字段' or '在选片会话中修改的字段'))
        if #issues>0 then message=message..string.format('\n%d 项读取或写入失败，未确认的会话可再次同步。\n%s',#issues,issues[1])
        elseif completed==0 then message=message..' 当前没有新增修改。' end
        local hasOrganizationNote=false
        for _,owner in ipairs(owners) do
            if owner.organizationNote then message=message..'\n\n'..owner.organizationNote;hasOrganizationNote=true end
        end
        Common.log('[sync] '..message)
        if (not starsOnly and not automatic) or #issues>0 or hasOrganizationNote then LrDialogs.message('FastCull for ARW',message) end
end

-- SDK tasks yield during CLI execution and Develop readback. Serialize the whole
-- snapshot/apply/ack cycle so an older task cannot overwrite a newer snapshot.
local queue,running={},false
function Sync.run(selectedOnly, onlySessionId, starsOnly, automatic)
    local catalog=LrApplication.activeCatalog()
    queue[#queue+1]={catalog=catalog,key=Common.catalogKey(catalog),
        photos=selectedOnly and catalog:getTargetPhotos() or nil,
        selectedOnly=selectedOnly,id=onlySessionId,starsOnly=starsOnly,automatic=automatic}
    if running then return end
    running=true
    LrTasks.startAsyncTask(function()
        while #queue>0 do
            local job=table.remove(queue,1)
            local ok,err=call(function()
                if Common.catalogKey(LrApplication.activeCatalog())~=job.key then
                    error('同步前 LR 目录已切换，原会话保留供重试')
                end
                synchronize(job.catalog,job.photos,job.selectedOnly,job.id,job.starsOnly,job.automatic)
            end)
            if not ok then
                -- A reporting failure must not strand every subsequent request.
                call(function()
                    Common.log('[sync] '..tostring(err))
                    LrDialogs.message('FastCull for ARW','同步未完成，原会话保留供重试。\n'..tostring(err))
                end)
            end
        end
        running=false
    end)
end
return Sync
