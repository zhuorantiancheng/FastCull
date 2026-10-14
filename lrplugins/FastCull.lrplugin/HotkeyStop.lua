_G.FastCullHotkeyToken = nil
if _G.FastCullHotkeyAlive then
    pcall(os.remove, _G.FastCullHotkeyAlive)
    _G.FastCullHotkeyAlive = nil
end
