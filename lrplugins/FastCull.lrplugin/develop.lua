-- Runs after native GUI launch; never delays the first window with per-photo SDK calls.
local LrTasks=import 'LrTasks'
local LrApplication=import 'LrApplication'
local Common=require 'common'
local Edits=require 'photo_edits'
local Develop={}
local call=LrTasks.pcall or pcall
local function hex(s) return (s:gsub('.',function(c) return string.format('%02x',string.byte(c)) end)) end
local function number(value, low, high)
    if type(value)=='number' and value==value and value>=low and value<=high then
        return tostring(math.floor(value*1000000+0.5))
    end
    return '-'
end
local function baseline(settings)
    local key,limit=Edits.exposureParameters(settings)
    local fields={key and number(settings[key],-limit,limit) or '-','-','-','-','-','-','-','-'}
    for n,code in ipairs({'AB','BA','CD','DC','CB','BC','AD','DA'}) do
        if settings.orientation==code then fields[2]=tostring(n) end
    end
    -- getDevelopSettings may omit HasCrop. Complete crop edges still describe
    -- the catalog crop; explicit false always means reset, even with stale edges.
    if settings.HasCrop==false then
        fields[3]='0';fields[4]='0';fields[5]='0'
        fields[6]='1000000';fields[7]='1000000';fields[8]='0'
    else
        local complete=true
        for n,key in ipairs({'CropLeft','CropTop','CropRight','CropBottom'}) do
            fields[n+3]=number(settings[key],0,1)
            if fields[n+3]=='-' then complete=false end
        end
        fields[8]=number(settings.CropAngle or 0,-45,45)
        if complete and fields[8]~='-' then fields[3]='1' end
    end
    -- White balance is one atomic field. Never substitute a made-up As Shot
    -- temperature, and keep the legacy row when the SDK has no editable WB.
    if settings.WhiteBalance=='As Shot' then
        fields[9]=hex('As Shot');fields[10]='-';fields[11]='-'
    elseif settings.WhiteBalance=='Custom' then
        local temperature,tint=settings.Temperature,settings.Tint
        if type(temperature)=='number' and temperature%1==0 and temperature>=2000 and temperature<=50000 and
           type(tint)=='number' and tint%1==0 and tint>= -150 and tint<=150 then
            fields[9]=hex('Custom');fields[10]=string.format('%.0f',temperature);fields[11]=string.format('%.0f',tint)
        end
    end
    return table.concat(fields,'\t')
end
function Develop.start(catalog, id, manifest, picked)
    LrTasks.startAsyncTask(function()
        local f,err=io.open(manifest:gsub('%.txt$','.exposure'),'w')
        if f then
            for n,item in ipairs(picked) do
                local ok,value=call(function()
                    local settings=item.photo:getDevelopSettings()
                    if type(settings)~='table' then error('无法读取 LR 显影设置') end
                    return baseline(settings)
                end)
                f:write(hex(item.path)..'\t'..(ok and tostring(value) or '-')..'\n');f:flush()
                if not ok then Common.log('[exposure] '..item.path..': '..tostring(value)) end
                if LrTasks.yield and n%8==0 then LrTasks.yield() end
            end
            f:write('END\n');f:close()
        else Common.log('[exposure] '..tostring(err)) end
    end)
    -- Watch immediately instead of waiting for every photo's Develop baseline.
    -- Test SDKs without a scheduler do not install a long-lived watcher.
    if not LrTasks.sleep then return end
    require('raw_settings').start(catalog,id,manifest,picked)
    LrTasks.startAsyncTask(function()
        local request=manifest:gsub('%.txt$','.stars-request')
        local closedPolls=0
        while true do
            LrTasks.sleep(0.5)
            local receipt=io.open(request,'r')
            if receipt then
                local requested=receipt:read('*l');local mode=receipt:read('*l');receipt:close()
                if requested~=id then Common.log('[auto-sync] 会话 ID 不匹配');return end
                if mode and mode~='all' then Common.log('[auto-sync] 无效的关闭同步方式，保留收据供核对');return end
                if Common.catalogKey(LrApplication.activeCatalog())~=Common.catalogKey(catalog) then
                    Common.log('[auto-sync] 当前目录已切换，保留原会话供稍后同步');return
                end
                require('sync').run(false,id,mode~='all',true)
                return
            end
            local m=io.open(manifest,'r')
            if m then
                local header=m:read('*l') or '';m:close()
                if header:match('\tclosed\t') then closedPolls=closedPolls+1 else closedPolls=0 end
                -- Receipt is committed only after the native GUI has exited.
                if closedPolls>=20 then return end
            end
        end
    end)
end
return Develop
