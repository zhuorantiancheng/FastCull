"""Exercise the real LR hotkey script without a running Adobe catalog."""
import unittest
from pathlib import Path
from lupa import LuaRuntime

ROOT=Path(__file__).resolve().parents[1]
class HotkeyTests(unittest.TestCase):
    def run_case(self,age):
        lua=LuaRuntime(unpack_returned_tuples=True)
        lua.execute('''
            calls=0; deleted=0; ticks=0; writes=0; pending=nil
            _PLUGIN={path='C:/插件/FastCull.lrplugin'}
            local modules={
              LrTasks={pcall=pcall,startAsyncTask=function(f) task=f end,
                execute=function(cmd) command=cmd; pending=tostring(2000000-age); return 0 end,
                sleep=function() ticks=ticks+1; if ticks==3 then FastCullHotkeyToken=nil end end},
              LrPathUtils={child=function(a,b)return a..'/'..b end,parent=function(a)return a:match('^(.*)/[^/]+$')end},
              LrFileUtils={createAllDirectories=function()return true end},
              LrDate={currentTime=function()return 2000-978307200 end}}
            import=function(name)return modules[name]end
            require=function() return {listPath=function(name)return 'C:/用户/会话/'..name end,log=function(s)error(s)end} end
            os.remove=function()pending=nil;deleted=deleted+1 end
            io.open=function(path,mode)
              if mode=='w' then return {write=function()writes=writes+1 end,close=function()end} end
              if pending then return {read=function()return pending end,close=function()end} end
            end
            dofile=function(path) assert(path=='C:/插件/FastCull.lrplugin/OpenCuller.lua'); calls=calls+1 end
        ''')
        lua.globals().age=age
        lua.execute((ROOT/'lrplugins/FastCull.lrplugin/HotkeyInit.lua').read_text(encoding='utf-8'))
        lua.execute('task()')
        return lua
    def test_fresh_request_uses_lr_entry_once_with_quoted_unicode_paths(self):
        lua=self.run_case(100)
        self.assertEqual(lua.globals().calls,1)
        self.assertEqual(lua.globals().ticks,3)
        self.assertIn('--start lightroom "C:/用户/会话/hotkeys-lightroom"',lua.globals().command)
    def test_stale_and_future_requests_never_open_a_photo(self):
        for age in [5000,-2000]:
            self.assertEqual(self.run_case(age).globals().calls,0)
    def test_shutdown_cancels_the_listener(self):
        lua=LuaRuntime();lua.execute('FastCullHotkeyToken={}')
        lua.execute((ROOT/'lrplugins/FastCull.lrplugin/HotkeyStop.lua').read_text())
        self.assertIsNone(lua.globals().FastCullHotkeyToken)
