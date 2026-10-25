-- Apply only changed Develop fields. Never import the entire XMP over the catalog.
local LrApplication=import 'LrApplication'
local LrTasks=import 'LrTasks'
local Edits={}
local codes={'AB','BA','CD','DC','CB','BC','AD','DA'}
local clockwise={6,7,8,5,2,3,4,1}
local cropKeys={'CropLeft','CropTop','CropRight','CropBottom','CropAngle'}
local basicKeys={'contrast','highlights','shadows','whites','blacks','vibrance','saturation'}
local modernBasic={contrast='Contrast2012',highlights='Highlights2012',shadows='Shadows2012',whites='Whites2012',blacks='Blacks2012'}
local function equal(a,b)
    return type(a)=='number' and a==a and math.abs(a-b)<=0.000001
end

local function basicParameter(settings,key)
    if key=='vibrance' then return 'Vibrance',-100,100 end
    if key=='saturation' then return 'Saturation',-100,100 end
    local exposure,reason,legacy=Edits.exposureParameters(settings)
    if not exposure then error(reason) end
    if legacy then
        if key=='contrast' then return 'Contrast',-50,100 end
        error('旧处理版本不能同步高光、阴影、白色或黑色，原处理版本保持不变')
    end
    return assert(modernBasic[key]),-100,100
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

-- Match the XMP editor's process selection. Merely finding Exposure2012 in the
-- SDK dictionary does not mean it is active: old processes can retain both keys.
function Edits.exposureParameters(settings)
    local process=settings.ProcessVersion
    if process~=nil then
        local version=(type(process)=='string' or type(process)=='number') and tonumber(process) or nil
        if not version or version~=version or version==math.huge or version== -math.huge then
            return nil,'无法识别曝光处理版本，原处理版本保持不变'
        end
        if version==5.0 or version==5.7 then return 'Exposure',4,true end
        if version==6.6 or version>=6.7 then return 'Exposure2012',5,false end
        return nil,'此处理版本尚不支持曝光同步，原处理版本保持不变'
    end
    local oldTone,newTone=false,false
    for _,key in ipairs({'Exposure','Contrast','Brightness','Shadows'}) do
        if settings[key]~=nil then oldTone=true end
    end
    for _,key in ipairs({'Exposure2012','Contrast2012','Highlights2012','Shadows2012','Whites2012','Blacks2012'}) do
        if settings[key]~=nil then newTone=true end
    end
    if oldTone and newTone then
        return nil,'未指定处理版本且存在两套亮度参数，无法确定应同步的字段'
    end
    if oldTone then return 'Exposure',4,true end
    return 'Exposure2012',5,false
end

function Edits.apply(photo,row)
    if row.exposure==nil and not row.crop and not row.orientation and not row.whiteBalance and not row.basic then return end
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
    if row.basic then
        for _,name in ipairs(basicKeys) do
            local value=row.basic[name]
            if value~=nil then
                local key,low,high=basicParameter(settings,name)
                if type(value)~='number' or value%1~=0 or value<low or value>high then error('无效的基础调整参数：'..name) end
                if not equal(settings[key],value) or (key=='Contrast' and settings.AutoContrast~=false) then
                    patch[key]=value
                    if key=='Contrast' then patch.AutoContrast=false end
                end
            end
        end
    end
    if row.exposure~=nil then
        local key,limit,legacy=Edits.exposureParameters(settings)
        if not key then error(limit) end
        if settings[key]==nil then error('此照片的处理版本缺少 '..key) end
        if row.exposure~=row.exposure or row.exposure< -limit or row.exposure>limit then
            error('此照片处理版本的曝光范围为 -'..limit..' 到 +'..limit..' EV')
        end
        if not equal(settings[key],row.exposure) or (legacy and settings.AutoExposure~=false) then
            patch[key]=row.exposure
            if legacy then patch.AutoExposure=false end
        end
    end
    if row.whiteBalance then
        local wb=row.whiteBalance
        local changed=settings.WhiteBalance~=wb.WhiteBalance
        if wb.WhiteBalance=='Custom' then
            changed=changed or not equal(settings.Temperature,wb.Temperature) or not equal(settings.Tint,wb.Tint)
        end
        if changed then
            patch.WhiteBalance=wb.WhiteBalance
            if wb.WhiteBalance=='Custom' then
                patch.Temperature=wb.Temperature;patch.Tint=wb.Tint
            end
        end
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
    if row.exposure==nil and not row.crop and not row.orientation and not row.whiteBalance and not row.basic then return end
    local function check()
        local actual=photo:getDevelopSettings()
        if type(actual)~='table' then return false end
        if row.basic then
            for _,name in ipairs(basicKeys) do
                local value=row.basic[name]
                if value~=nil then
                    local key=basicParameter(actual,name)
                    if not equal(actual[key],value) or (key=='Contrast' and actual.AutoContrast~=false) then return false end
                end
            end
        end
        if row.exposure~=nil then
            local key,limit,legacy=Edits.exposureParameters(actual)
            if not key or row.exposure< -limit or row.exposure>limit or
               not equal(actual[key],row.exposure) or (legacy and actual.AutoExposure~=false) then return false end
        end
        if row.whiteBalance then
            local wb=row.whiteBalance
            if actual.WhiteBalance~=wb.WhiteBalance then return false end
            if wb.WhiteBalance=='Custom' and
               (not equal(actual.Temperature,wb.Temperature) or not equal(actual.Tint,wb.Tint)) then return false end
        end
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
    error('LR 尚未确认曝光、基础调整、白平衡、裁切或旋转结果，保留会话供重试（不会重复叠加旋转）')
end
return Edits
