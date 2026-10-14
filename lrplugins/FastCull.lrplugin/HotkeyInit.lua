local Tasks = import 'LrTasks'
local Paths = import 'LrPathUtils'
local Files = import 'LrFileUtils'
local Date = import 'LrDate'
local Common = require 'common'
-- Windows helper uses Win32 foreground hooks. macOS uses the LR menu entry.
if Common.isMac then return end
local token = {}
_G.FastCullHotkeyToken = token
Tasks.startAsyncTask(function()
    local ok, reason = Tasks.pcall(function()
        local directory = Paths.child(Paths.parent(Common.listPath('hotkey.txt')), 'hotkeys-lightroom')
        Files.createAllDirectories(directory)
        local request = Paths.child(directory, 'request')
        local alive = Paths.child(directory, 'alive')
        _G.FastCullHotkeyAlive = alive
        local function heartbeat()
            local f = assert(io.open(alive, 'w')); f:write('active'); f:close()
        end
        os.remove(request) -- Only our stale command, never a photo.
        heartbeat()
        local exe = Paths.child(_PLUGIN.path, 'fastcull-keys.exe')
        local command = '""' .. exe .. '" --start lightroom "' .. directory .. '""'
        assert(Tasks.execute(command) == 0, '无法启动快捷键助手')
        local ticks = 0
        while _G.FastCullHotkeyToken == token do
            local f = io.open(request, 'r')
            if f then
                local stamp = tonumber(f:read('*a')); f:close(); os.remove(request)
                local age = (Date.currentTime() + 978307200) * 1000 - (stamp or 0)
                if age >= 0 and age < 1500 then
                    dofile(Paths.child(_PLUGIN.path, 'OpenCuller.lua'))
                end
            end
            ticks = ticks + 1
            if ticks % 20 == 0 then heartbeat() end
            Tasks.sleep(0.25)
        end
    end)
    if not ok then Common.log('[hotkey] ' .. tostring(reason)) end
end)
