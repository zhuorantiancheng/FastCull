-- Preserve photo UUIDs/history/virtual copies. Handoff uses collections, never disk moves or reimport.
local Common=require 'common'
local Organization={}
local function key(path) return Common.pathKey(path) end
local function parent(path)
    if Common.isMac then return path:match('^(.*)/[^/]+$') end
    return path:match('^(.*)[/\\][^/\\]+$')
end
local function leaf(path)
    if Common.isMac then return path:match('[^/]+$') or path end
    return path:match('[^/\\]+$') or path
end
local function gate(catalog,name,action)
    local ran=false
    catalog:withWriteAccessDo(name,function() ran=true;action() end)
    if not ran then error('LR 文件整理事务未执行') end
end

function Organization.parse(out,id)
    local result={moves={}}
    local seen,targets={},{}
    for line in out:gmatch('[^\r\n]+') do
        if line:match('^ORGANIZATION\t') then
            local found,revision,status=line:match('^ORGANIZATION\t([^\t]+)\t(%d+)\t([^\t]+)$')
            if not found or found~=id or result.id or (status~='active' and status~='closed') then error('文件整理会话标识不匹配') end
            result.id=found;result.revision=tonumber(revision);result.status=status
        elseif line:match('^MOVE\t') then
            local from,to=line:match('^MOVE\t([^\t]+)\t([^\t]+)$')
            if not from then error('无效的文件整理记录') end
            from=Common.hexDecode(from);to=Common.hexDecode(to)
            if seen[key(from)] or targets[key(to)] or leaf(from)~=leaf(to) or not parent(to) then error('文件整理记录重名或改变了文件名') end
            seen[key(from)]=true;targets[key(to)]=true
            result.moves[#result.moves+1]={from=from,to=to}
        elseif not line:match('^RESULT moves=%d+$') then error('未知的文件整理输出') end
    end
    if not result.id or tonumber(out:match('RESULT moves=(%d+)'))~=#result.moves then error('文件整理输出不完整') end
    return result
end

function Organization.prepare(catalog,entry)
    -- Recheck catalog ownership before creating any collection.
    if Common.catalogKey(catalog)~=entry.catalog then error('文件整理目录身份不匹配') end
    if not Common.fileExists(entry.path:gsub('%.txt$','.folders')) then
        if entry.organizationPending then error('待整理记录文件不存在，保留原会话供恢复') end
        return nil
    end
    local out,err=Common.runCuller('--read-folders '..Common.quote(entry.path),true)
    if not out then error(err) end
    local plan=Organization.parse(out,entry.id)
    if #plan.moves==0 then entry.organizationPending=false;return nil end
    if plan.status~='closed' then entry.organizationPending=true;return nil end
    local grouped,order,total={},{},0
    for _,move in ipairs(plan.moves) do
        if not entry.paths or not entry.paths[move.from] then error('整理记录超出原始会话：'..move.from) end
        local photos={}
        local uuids=entry.photos and entry.photos[move.from]
        if uuids and #uuids>0 then
            if not catalog.findPhotoByUuid then error('此 SDK 缺少照片 UUID 查找接口') end
            local seen={}
            for _,uuid in ipairs(uuids) do
                local photo=catalog:findPhotoByUuid(uuid)
                if not photo then error('原会话照片已不在此目录中：'..move.from) end
                if not seen[uuid] then photos[#photos+1]=photo;seen[uuid]=true end
            end
        else
            local photo=catalog:findPhotoByPath(move.from)
            if not photo then error('找不到原会话照片：'..move.from) end
            photos[1]=photo
        end
        for _,photo in ipairs(photos) do
            local current=photo:getRawMetadata('path')
            if not current then error('无法取得原照片路径：'..move.from) end
            if key(current)~=key(move.to) then
                if key(current)~=key(move.from) then error('原照片已移动到其他位置，请核对计划：'..current) end
                local destination=parent(move.to)
                if not grouped[destination] then grouped[destination]={};order[#order+1]=destination end
                grouped[destination][#grouped[destination]+1]=photo;total=total+1
            end
        end
    end
    if total==0 then entry.organizationPending=false;return '文件整理计划已完成，原照片身份保持。' end
    if not catalog.createCollectionSet or not catalog.createCollection then error('此 SDK 缺少文件整理集合接口') end
    table.sort(order)
    local set,collections
    collections={}
    -- SDK-created collections cannot be used until the creation gate commits.
    gate(catalog,'Create FastCull organization groups',function()
        set=catalog:createCollectionSet('FastCull 待整理 · '..entry.id,nil,true)
        if not set then error('无法创建待整理集合组') end
        for n,destination in ipairs(order) do
            local name=string.format('r%d · %02d · %s',plan.revision,n,leaf(destination))
            local collection=catalog:createCollection(name,set,true)
            if not collection then error('无法创建目标文件夹分组') end
            collections[n]=collection
        end
    end)
    gate(catalog,'Populate FastCull organization groups',function()
        for n,destination in ipairs(order) do collections[n]:addPhotos(grouped[destination]) end
    end)
    entry.organizationPending=true;entry.organizationRevision=plan.revision
    entry.organizationGroups={}
    for n,destination in ipairs(order) do
        entry.organizationGroups[n]={destination=destination,count=#grouped[destination],collection=collections[n].localIdentifier}
    end
    if catalog.setActiveSources then catalog:setActiveSources({collections[1]}) end
    if catalog.setSelectedPhotos then catalog:setSelectedPhotos(grouped[order[1]][1],grouped[order[1]]) end
    return string.format('文件整理：%d 张照片，%d 个目标文件夹。已在 LR「集合」面板建立「FastCull 待整理」分组。\n第一组目标：%s\n请在 LR 文件夹面板添加该目标文件夹，将当前选中的照片拖入。再次执行「同步选片会话」可核对完成情况并定位下一组。\n文件尚未移动，原照片的编辑历史、集合关系和虚拟副本均保留。',total,#order,order[1])
end
return Organization
