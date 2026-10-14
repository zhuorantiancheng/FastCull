-- Apply only changed Develop fields. Never import the entire XMP over the catalog.
local LrApplication=import 'LrApplication'
local LrTasks=import 'LrTasks'
local Edits={}
local codes={'AB','BA','CD','DC','CB','BC','AD','DA'}
local clockwise={6,7,8,5,2,3,4,1}
local cropKeys={'CropLeft','CropTop','CropRight','CropBottom','CropAngle'}
local function equal(a,b)
    return type(a)=='number' and a==a and math.abs(a-b)<=0.000001
end
local function cropValues(crop)
    if crop.HasCrop then return crop end
    return {HasCrop=false,CropLeft=0,CropTop=0,CropRight=1,CropBottom=1,CropAngle=0}
end
local function orientation(settings)
    local value=settings and settings.orientation
    for n,code in ipairs(codes) do if value==code then return n end end
    error('未取得有效的 LR 照片方向，保留修改供重试')
end

function Edits.apply(photo,row)
    if row.exposure==nil and not row.crop and not row.orientation then return end
    local settings=photo:getDevelopSettings()
    if type(settings)~='table' then error('无法读取 LR 显影设置') end
    local turns=0
    if row.orientation then
        local current=orientation(settings)
        local o=current
        while o~=row.orientation and turns<4 do o=clockwise[o];turns=turns+1 end
        if turns==4 then error('LR 与 XMP 的镜像状态不同，无法仅通过旋转同步；修改保留供重试') end
        if turns>0 then
            -- Before 11.4 these methods could rotate unrelated selected photos.
            -- https://community.adobe.com/bug-reports-674/p-sdk-photo-rotateleft-and-rotateright-can-rotate-all-photos-in-the-catalog-663142
            local version=LrApplication.versionTable and LrApplication.versionTable()
            if not version or not (version.major>11 or (version.major==11 and version.minor>=4)) then
                error('安全自动同步旋转需要 Lightroom Classic 11.4 或更高版本')
            end
            if not photo.rotateRight then error('LR 缺少照片旋转接口') end
        end
    end
    local patch={}
    if row.exposure~=nil then
        if settings.Exposure2012==nil then error('此照片的处理版本不支持 Exposure2012') end
        if not equal(settings.Exposure2012,row.exposure) then patch.Exposure2012=row.exposure end
    end
    if row.crop then
        local desired=cropValues(row.crop)
        local changed=turns>0 or (settings.HasCrop~=nil and settings.HasCrop~=desired.HasCrop)
        for _,key in ipairs(cropKeys) do
            if not equal(settings[key],desired[key]) then changed=true end
        end
        if changed then
            patch.HasCrop=desired.HasCrop
            for _,key in ipairs(cropKeys) do patch[key]=desired[key] end
            -- Avoid a previous LR aspect ratio overriding the saved rectangle.
            patch.CropConstrainAspectRatio=false
        end
    end
    -- Rotate from the CURRENT catalog direction to the absolute target. Retrying
    -- after any partial failure never blindly adds the same quarter turns again.
    for _=1,turns do photo:rotateRight() end
    if next(patch) then photo:applyDevelopSettings(patch,'FastCull 选片修改') end
end

function Edits.verify(photo,row)
    if row.exposure==nil and not row.crop and not row.orientation then return end
    local function check()
        local actual=photo:getDevelopSettings()
        if type(actual)~='table' then return false end
        if row.exposure~=nil and not equal(actual.Exposure2012,row.exposure) then return false end
        if row.orientation and orientation(actual)~=row.orientation then return false end
        if row.crop then
            local expected=cropValues(row.crop)
            for _,key in ipairs(cropKeys) do
                if type(actual[key])~='number' or actual[key]~=actual[key] or math.abs(actual[key]-expected[key])>0.00001 then return false end
            end
        end
        return true
    end
    -- Read back outside the write gate. LR can defer Develop updates until commit.
    for attempt=1,21 do
        if check() then return end
        if not LrTasks.sleep or attempt==21 then break end
        LrTasks.sleep(0.1)
    end
    error('LR 尚未确认曝光、裁切或旋转结果，保留会话供重试（不会重复叠加旋转）')
end
return Edits
