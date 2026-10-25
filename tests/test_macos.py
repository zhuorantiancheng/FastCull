"""Portable checks for Mac packaging metadata and LR's POSIX command boundary."""
import importlib.util
from pathlib import Path
import shlex
import unittest
from lupa import LuaRuntime

ROOT = Path(__file__).resolve().parents[1]


class MacTests(unittest.TestCase):
    def test_lightroom_manifest_uses_exact_case(self):
        names = {path.name for path in (ROOT / 'lrplugins/FastCull.lrplugin').iterdir()}
        self.assertIn('Info.lua', names)
        self.assertNotIn('info.lua', names)

    def setUp(self):
        self.lua = LuaRuntime(unpack_returned_tuples=True)
        self.lua.globals().SOURCE = str(ROOT / "lrplugins/FastCull.lrplugin/common.lua")
        self.lua.execute('''
            MAC_ENV=true
            _PLUGIN={path="/Users/测试 O'Brien/$photos/FastCull.lrplugin"}
            PREFS={}
            local modules={
                LrPrefs={prefsForPlugin=function() return PREFS end},
                LrPathUtils={child=function(a,b) return a..'/'..b end,
                    getStandardFilePath=function() return '/Users/测试/Library/Caches' end},
                LrFileUtils={}, LrTasks={}, LrDate={}
            }
            import=function(name) return assert(modules[name]) end
            Common=dofile(SOURCE)
        ''')

    def test_posix_quotes_preserve_special_paths(self):
        for path in ["/Users/O'Brien/中文 space", '/tmp/$HOME `whoami` "quote" &;!', '/tmp/a\\b']:
            self.assertEqual(shlex.split(self.lua.globals().Common.quote(path)), [path])
        for path in ['a\nb', 'a\rb', 'a\0b']:
            with self.assertRaises(Exception):
                self.lua.globals().Common.quote(path)

    def test_app_default_and_relocated_plugin(self):
        expected = "/Users/测试 O'Brien/$photos/FastCull.lrplugin/FastCull.app/Contents/MacOS/fastcull"
        self.assertEqual(self.lua.eval('Common.defaultCullerPath()'), expected)
        self.lua.execute("PREFS.cullerPath='/old/FastCull.lrplugin/FastCull.app/Contents/MacOS/fastcull'; Common.fileExists=function() return true end")
        self.assertEqual(self.lua.eval('Common.getPrefs().cullerPath'), expected)
        self.lua.execute("PREFS.cullerPath='/custom/fastcull'")
        self.assertEqual(self.lua.eval('Common.getPrefs().cullerPath'), '/custom/fastcull')

    def test_case_sensitive_catalog_and_unix_temp_paths(self):
        common = self.lua.globals().Common
        self.assertNotEqual(common.pathKey('/Photos/A.ARW'), common.pathKey('/Photos/a.ARW'))
        self.assertEqual(common.pathKey('/Photos/a\\b.ARW'), '/Photos/a\\b.ARW')
        self.assertEqual(common.listPath('state.txt'), '/Users/测试/Library/Caches/state.txt')

    def test_native_sdk_command_is_not_wrapped_in_cmd_quotes(self):
        self.lua.execute('''
            PREFS.cullerPath='/custom/fastcull'
            Common.log=function() end
            Common.listPath=function() return "/tmp/O'Brien/$output.txt" end
            local original=import
            import=function(name)
                if name=='LrUUID' then return {generateUUID=function() return 'id' end} end
                return original(name)
            end
            import('LrTasks').execute=function(cmd) COMMAND=cmd; return 7 end
            io.open=function() return nil end
            os.remove=function() end
        ''')
        self.lua.globals().INPUT_PATH = '/tmp/中文 "file.txt'
        self.lua.execute("Common.runCuller('--open '..Common.quote(INPUT_PATH),true)")
        argv = shlex.split(self.lua.globals().COMMAND)
        self.assertEqual(argv, ['/custom/fastcull', '--open', '/tmp/中文 "file.txt', '>', "/tmp/O'Brien/$output.txt", '2>&1'])

    def test_bundle_plist(self):
        spec = importlib.util.spec_from_file_location('build_macos', ROOT / 'scripts/build-macos.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        data = module.info_plist('2.7.4')
        self.assertEqual(data['CFBundleExecutable'], 'fastcull')
        self.assertEqual(data['CFBundlePackageType'], 'APPL')
        self.assertTrue(data['NSHighResolutionCapable'])
        self.assertEqual(data['LSMinimumSystemVersion'], '11.0')

    def test_macos_rejects_windows_python_fallback(self):
        self.lua.execute("PREFS.cullerPath='/custom/old.py'; PREFS.pythonPath='python3'")
        output, error = self.lua.globals().Common.runCuller('--open fixture', False)
        self.assertIsNone(output)
        self.assertIn('仅支持 Windows', error)


if __name__ == '__main__':
    unittest.main()
