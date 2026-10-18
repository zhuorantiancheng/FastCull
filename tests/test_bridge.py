"""Run Lua against a strict SDK-shaped mock. Not a replacement for real Lightroom QA.

Usage: PYTHONPATH=<directory containing lupa> python -m unittest discover -s tests -p test_bridge.py
"""
import tempfile
import subprocess
import os
import time
import struct
import unittest
from pathlib import Path
from lupa import LuaRuntime

ROOT = Path(__file__).resolve().parents[1]
PLUGIN = ROOT / "lrplugins/FastCull.lrplugin"

class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="fastcull-lua-")
        self.addCleanup(self.tmp.cleanup)
        self.lua = LuaRuntime(unpack_returned_tuples=True)
        self.lua.globals().PLUGIN_DIR = str(PLUGIN)
        self.lua.globals().SCRATCH = self.tmp.name
        self.lua.globals().BACKEND = self.backend
        self.lua.globals().MAKE_DIRS = self.make_dirs
        self.lua.globals().CLOCK = lambda: time.time() - 978307200
        self.lua.execute(r'''
            REGISTERED={}; UUIDS={}; WRITE_OWNERS={}; PREFS = {}; MESSAGES = {}; WRITES = {}; PHOTOS = {}; COMMANDS = {}; DEVELOP_WRITES = {}
            _PLUGIN = { path = PLUGIN_DIR }
            local modules = {
                LrPrefs = { prefsForPlugin = function() return PREFS end },
                LrFileUtils = { createAllDirectories = function(path) return MAKE_DIRS(path) end },
                LrPathUtils = {
                    child = function(a,b) return a .. '/' .. b end,
                    parent = function(p) return p:match('^(.*)[/\\][^/\\]+$') end,
                    getStandardFilePath = function(which) return SDK_PATHS[which] end,
                },
                LrDialogs = { message = function(a,b) MESSAGES[#MESSAGES+1] = b end },
                LrTasks = { startAsyncTask = function(f) f() end },
                LrDate = { currentTime = function() return CLOCK() end },
                LrUUID = { generateUUID = function() return 'test-unique-id' end },
                LrProgressScope = setmetatable({}, { __call = function()
                    return { done = function() end, setPortionComplete = function() end }
                end }),
                LrFunctionContext = { callWithContext = function(name, f)
                    assert(type(name)=='string'); f({})
                end },
            }
            CATALOG = {
                getTargetPhotos = function() return PHOTOS end,
                getPath = function() return "catalog.lrcat" end,
                findPhotoByPath = function(self,path) return REGISTERED[path] end,
                findPhotoByUuid = function(self,uuid) return UUIDS[uuid] end,
                withWriteAccessDo = function(self, name, f)
                    assert(type(name)=='string'); IN_GATE = true; f(); IN_GATE = false
                end,
            }
            SDK_VERSION={major=14,minor=0};ROTATIONS={};PATCHES={}
            modules.LrApplication = { activeCatalog = function() return CATALOG end, versionTable=function() return SDK_VERSION end }
            import = function(name) assert(modules[name], name); return modules[name] end
            SDK_PATHS={home=SCRATCH,temp=SCRATCH,desktop=SCRATCH,appPrefs=SCRATCH..'/sdk/prefs',pictures=SCRATCH,documents=SCRATCH,appData=SCRATCH..'/sdk/data'}
            package.path = PLUGIN_DIR .. '/?.lua;' .. package.path
            function addPhoto(path, rating, label, flag, uuid)
                local develop={Exposure2012=0.1,Contrast2012=23,orientation='AB',CropLeft=0,CropTop=0,CropRight=1,CropBottom=1,CropAngle=0}
                PHOTOS[#PHOTOS+1] = {
                    getRawMetadata = function(self,k)
                        if k=='path' then return path elseif k=='rating' then return rating
                        elseif k=='pickStatus' then return flag elseif k=='uuid' then return uuid end
                    end,
                    getFormattedMetadata = function(self,k) if k=='label' then return label end end,
                    getDevelopSettings = function() return develop end,
                    rotateRight=function(self)
                        assert(IN_GATE,'rotate outside gate')
                        local right={AB='BC',BC='CD',CD='DA',DA='AB',BA='AD',AD='DC',DC='CB',CB='BA'}
                        develop.orientation=assert(right[develop.orientation]);ROTATIONS[#ROTATIONS+1]=self
                    end,
                    applyDevelopSettings = function(self,settings)
                        assert(IN_GATE,'develop write outside gate')
                        assert(settings.Contrast2012==nil)
                        DEVELOP_WRITES[#DEVELOP_WRITES+1]=settings.Exposure2012
                        PATCHES[#PATCHES+1]=settings
                        for k,v in pairs(settings) do develop[k]=v end
                    end,
                    setRawMetadata = function(self,k,v)
                        assert(IN_GATE, 'metadata write outside gate')
                        WRITES[#WRITES+1] = {k,v}
                        WRITE_OWNERS[#WRITE_OWNERS+1]=self
                    end,
                }
                REGISTERED[path]=PHOTOS[#PHOTOS]
                if uuid then UUIDS[uuid]=PHOTOS[#PHOTOS] end
            end
            Common = require 'common'
            Common.runCuller = function(args) return BACKEND(args), nil end
        ''')

    def make_dirs(self, path):
        target=Path(path).resolve()
        if not target.is_relative_to(Path(self.tmp.name).resolve()):
            return False, 'test path outside scratch'
        try:
            existed=target.is_dir()
            target.mkdir(parents=True,exist_ok=True)
            return True, not existed
        except OSError as error:
            return False, str(error)

    def backend(self, args):
        target=Path(os.environ.get('CARGO_TARGET_DIR', ROOT/'target'))
        exe=target/'x86_64-pc-windows-msvc/debug/fastcull.exe'
        import re
        argv=re.findall(r'"([^"]*)"|([^ ]+)',args)
        argv=[a or b for a,b in argv]
        env=os.environ.copy();env['FASTCULL_LOG_DIR']=str(Path(self.tmp.name)/'logs')
        env.update(getattr(self,'backend_env',{}))
        env.pop('FASTCULL_STRESS',None)
        return subprocess.check_output([str(exe),*argv],env=env).decode('utf-8')

    def sdk_execute(self, command):
        # Use the actual Windows C-runtime shell quoting expected by the SDK,
        # including its output-file redirection. Only generated fixtures are used.
        import ctypes
        env={'FASTCULL_LOG_DIR':str(Path(self.tmp.name)/'logs'),
             **getattr(self,'backend_env',{})}
        saved={key:os.environ.get(key) for key in env}
        try:
            os.environ.update(env)
            crt=ctypes.CDLL('ucrtbase')
            crt._wsystem.argtypes=[ctypes.c_wchar_p]
            return crt._wsystem(command)
        finally:
            for key,value in saved.items():
                if value is None: os.environ.pop(key,None)
                else: os.environ[key]=value

    def script(self, name):
        self.lua.execute((PLUGIN / name).read_text(encoding="utf-8"))

    def test_clear_and_aliases(self):
        raw = Path(self.tmp.name) / "clear.ARW"
        raw.with_suffix(".xmp").write_text('''<rdf:Description
            xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
            xmlns:m='http://ns.adobe.com/xap/1.0/' xmlns:s="urn:zrtc:syncratings:1/"
            m:Rating='0' m:Label='' s:Flag=''/>''', encoding="utf-8")
        self.lua.globals().RAW = str(raw)
        self.lua.execute("addPhoto(RAW,5,'Red',1)")
        self.script("SyncSidecars.lua")
        writes = [tuple(v.values()) for _, v in self.lua.globals().WRITES.items()]
        self.assertEqual(writes, [("rating", 0), ("label", ""), ("pickStatus", 0)])

    def test_missing_fields_do_not_clear_and_color_is_lowercase(self):
        raw = Path(self.tmp.name) / "color.ARW"
        raw.with_suffix(".xmp").write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Label="Blue"/>', encoding="utf-8")
        self.lua.globals().RAW = str(raw)
        self.lua.execute("addPhoto(RAW,5,'Red',1)")
        self.script("SyncSidecars.lua")
        writes = [tuple(v.values()) for _, v in self.lua.globals().WRITES.items()]
        self.assertEqual(writes, [("colorNameForLabel", "blue")])

    def test_native_open_never_probes_python_and_preserves_state(self):
        self.lua.execute("package.loaded.common=nil; Common=require 'common'")
        self.lua.globals().PROGRAM = str(PLUGIN / "fastcull.exe")
        self.lua.globals().RAW = str(Path(self.tmp.name) / "照片 空格.ARW")
        self.lua.execute(r'''
            PREFS.cullerPath = PROGRAM
            Common.fileExists = function(path) return path==PROGRAM end
            io.popen = function() error('native path used io.popen') end
            local tasks=import 'LrTasks'
            tasks.execute = function(command)
                COMMANDS[#COMMANDS+1] = command
                local state = command:match('%-%-open "([^"]+)"')
                local f=assert(io.open(state,'r')); EXPORTED = f:read('*a'); f:close()
                local output=assert(command:match(' > "([^"]+)"'))
                local f=assert(io.open(output,'w'))
                f:write('RESULT wrote=1 kept=0 error=0\nRESULT launched=1\n');f:close()
                return 0
            end
            addPhoto(RAW,4,'Purple',-1)
            addPhoto(SCRATCH..'/unsupported.JPG',1,'Red',0)
            addPhoto(SCRATCH..'/unsupported.CR3',2,'Blue',0)
            addPhoto(SCRATCH..'/Canon.CR2',2,'Blue',0)
            addPhoto(SCRATCH..'/Nikon.NEF',3,'Green',1)
            addPhoto(SCRATCH..'/unsupported.xmp',0,'',0)
        ''')
        self.script("OpenCuller.lua")
        self.assertEqual(self.lua.globals().EXPORTED, self.lua.globals().RAW + "|4|Purple|-1\n" + self.tmp.name + "/unsupported.JPG|1|Red|0\n" + self.tmp.name + "/Canon.CR2|2|Blue|0\n" + self.tmp.name + "/Nikon.NEF|3|Green|1\n")
        command = self.lua.globals().COMMANDS[1]
        self.assertTrue(command.startswith('""'))
        self.assertTrue(command.endswith('2>&1"'))
        self.assertNotIn("python", command.lower())

    def test_default_path_migrates_old_bundled_python(self):
        self.lua.execute("PREFS.cullerPath = PLUGIN_DIR .. '/arw_culler.py'; Common.fileExists = function() return true end")
        self.assertTrue(self.lua.eval("Common.getPrefs().cullerPath").endswith("fastcull.exe"))

    def test_relocated_bundled_exe_migrates_to_current_plugin(self):
        self.lua.execute(r"PREFS.cullerPath = 'D:/old/FastCull.lrplugin/fastcull.exe'; Common.fileExists = function() return true end")
        self.assertEqual(self.lua.eval("Common.getPrefs().cullerPath"), str(PLUGIN)+'/fastcull.exe')

    def test_custom_exe_is_preserved(self):
        self.lua.execute("PREFS.cullerPath = 'D:/custom/fastcull.exe'; Common.fileExists = function() return true end")
        self.assertEqual(self.lua.eval("Common.getPrefs().cullerPath"), 'D:/custom/fastcull.exe')

    def test_jpeg_manual_sync_reads_embedded_xmp_without_raw_sidecar(self):
        path=Path(self.tmp.name)/'JPG 照片.JPG'
        path.write_bytes((ROOT/'tests/fixtures/tiny.jpg').read_bytes())
        state=Path(self.tmp.name)/'jpeg-state.txt';state.write_text(str(path)+'|4|Blue|1\n',encoding='utf-8')
        result=self.backend('--apply-state "'+str(state)+'"')
        self.assertIn('wrote=1',result)
        self.assertFalse(path.with_suffix('.xmp').exists())
        self.lua.globals().JPEG=str(path)
        self.lua.execute("addPhoto(JPEG,0,'',0)")
        self.script('SyncSidecars.lua')
        writes=[tuple(v.values()) for _,v in self.lua.globals().WRITES.items()]
        self.assertEqual(writes,[('rating',4),('colorNameForLabel','blue'),('pickStatus',1)])

    def test_native_sdk_execute_avoids_blocking_popen_and_cleans_output(self):
        self.lua.execute("package.loaded.common=nil; Common=require 'common'")
        self.lua.globals().PROGRAM=str(PLUGIN/'fastcull.exe')
        self.lua.execute(r'''
            PREFS.cullerPath=PROGRAM;Common.fileExists=function() return true end
            io.popen=function() error('blocking io.popen used for native launch') end
            local tasks=import 'LrTasks'
            tasks.execute=function(command)
                COMMANDS[#COMMANDS+1]=command
                OUTPUT_PATH=assert(command:match(' > "([^"]+)"'))
                local f=assert(io.open(OUTPUT_PATH,'w'))
                f:write('RESULT launched=1 pending=1\n');f:close()
                return 0
            end
            addPhoto('fixture.ARW',0,'',0)
        ''')
        self.script('OpenCuller.lua')
        self.assertEqual(len(self.lua.globals().COMMANDS),1)
        self.assertEqual(len(self.lua.globals().MESSAGES),0)
        self.assertFalse(Path(self.lua.globals().OUTPUT_PATH).exists())

    def test_native_sdk_execute_failure_without_output_reports_status(self):
        self.lua.execute("package.loaded.common=nil; Common=require 'common'")
        self.lua.globals().PROGRAM=str(PLUGIN/'fastcull.exe')
        self.lua.execute(r'''
            PREFS.cullerPath=PROGRAM;Common.fileExists=function() return true end
            io.popen=function() error('blocking io.popen used for native launch') end
            local tasks=import 'LrTasks';tasks.execute=function() return 7 end
        ''')
        out,error=self.lua.eval('Common.runCuller')('--help')
        self.assertIsNone(out)
        self.assertIn('7',error)

    def test_lua_sources_compile(self):
        for path in PLUGIN.glob("*.lua"):
            result = self.lua.eval("load")(path.read_text(encoding="utf-8"))
            self.assertFalse(isinstance(result, tuple), path.name)

    def test_batch_snapshot_does_not_call_per_photo_metadata(self):
        self.lua.execute(r'''
            PHOTOS={{}, {}}
            local function unexpected() error('per-photo metadata call') end
            for _,p in ipairs(PHOTOS) do p.getRawMetadata=unexpected;p.getFormattedMetadata=unexpected end
            RAW_CALLS=0;FORMATTED_CALLS=0
            CATALOG.batchGetRawMetadata=function(self,photos,keys)
                RAW_CALLS=RAW_CALLS+1;local values={}
                for i,p in ipairs(photos) do values[p]={path='photo'..i..'.ARW',rating=i,pickStatus=-1} end
                return values
            end
            CATALOG.batchGetFormattedMetadata=function(self,photos,keys)
                FORMATTED_CALLS=FORMATTED_CALLS+1;local values={}
                for _,p in ipairs(photos) do values[p]={label='Blue'} end
                return values
            end
            RAW,FORMATTED=Common.snapshotMetadata(CATALOG,PHOTOS)
            assert(RAW[PHOTOS[2]].rating==2 and FORMATTED[PHOTOS[1]].label=='Blue')
            assert(#Common.pickArwPhotos(PHOTOS,RAW)==2)
        ''')
        self.assertEqual(self.lua.globals().RAW_CALLS,1)
        self.assertEqual(self.lua.globals().FORMATTED_CALLS,1)

    def test_pending_launch_has_no_blocking_success_dialog(self):
        self.lua.globals().PROGRAM=str(PLUGIN/'fastcull.exe')
        self.lua.execute(r'''
            PREFS.cullerPath=PROGRAM;Common.fileExists=function() return true end
            Common.runCuller=function() return 'RESULT wrote=0 kept=0 error=0 pending=1\nRESULT launched=1',nil end
            addPhoto('test.ARW',4,'Purple',-1)
        ''')
        self.script('OpenCuller.lua')
        self.assertEqual(len(self.lua.globals().MESSAGES),0)

    def test_startup_trace_records_sdk_wait_and_output_read(self):
        self.lua.execute("package.loaded.common=nil; Common=require 'common'")
        self.lua.globals().PROGRAM=str(PLUGIN/'fastcull.exe')
        self.lua.execute(r'''
            CLOCK_VALUE=1000; local date=import 'LrDate'
            date.currentTime=function() return CLOCK_VALUE end
            TRACE_LOGS={};Common.log=function(line) TRACE_LOGS[#TRACE_LOGS+1]=line end
            PREFS.cullerPath=PROGRAM;Common.fileExists=function() return true end
            io.popen=function() error('native used blocking popen') end
            local tasks=import 'LrTasks'
            tasks.execute=function(command)
                CLOCK_VALUE=CLOCK_VALUE+5
                local path=assert(command:match(' > "([^"]+)"'))
                local f=assert(io.open(path,'w'))
                f:write('RESULT wrote=0 kept=0 error=0 pending=1\nRESULT launched=1');f:close()
                return 0
            end
            addPhoto('fixture.ARW',4,'Purple',-1,'original-uuid')
        ''')
        self.script('OpenCuller.lua')
        logs='\n'.join(v for _,v in self.lua.globals().TRACE_LOGS.items())
        self.assertRegex(logs,r'phase=execute-return elapsed_ms=5000\.0')
        self.assertRegex(logs,r'phase=file-read elapsed_ms=5000\.0')
        self.assertRegex(logs,r'phase=ready elapsed_ms=5000\.0')
        self.assertEqual(len(self.lua.globals().MESSAGES),0)
        self.assertEqual(self.lua.globals().PREFS.sessions[1].photos['fixture.ARW'][1],'original-uuid')

    def test_startup_trace_includes_async_task_queue_time(self):
        self.lua.globals().PROGRAM=str(PLUGIN/'fastcull.exe')
        self.lua.execute(r'''
            CLOCK_VALUE=1000;local date=import 'LrDate'
            date.currentTime=function() return CLOCK_VALUE end
            TRACE_LOGS={};Common.log=function(line) TRACE_LOGS[#TRACE_LOGS+1]=line end
            local tasks=import 'LrTasks'
            tasks.startAsyncTask=function(f) CLOCK_VALUE=CLOCK_VALUE+2;f() end
            PREFS.cullerPath=PROGRAM;Common.fileExists=function() return true end
            Common.runCuller=function() return 'RESULT launched=1 pending=1',nil end
            addPhoto('fixture.ARW',0,'',0)
        ''')
        self.script('OpenCuller.lua')
        logs='\n'.join(v for _,v in self.lua.globals().TRACE_LOGS.items())
        self.assertRegex(logs,r'phase=menu elapsed_ms=0\.0')
        self.assertRegex(logs,r'phase=task elapsed_ms=2000\.0')

    def test_startup_clock_failure_does_not_prevent_launch(self):
        self.lua.globals().PROGRAM=str(PLUGIN/'fastcull.exe')
        self.lua.execute(r'''
            local date=import 'LrDate';date.currentTime=function() error('clock unavailable') end
            PREFS.cullerPath=PROGRAM;Common.fileExists=function() return true end
            Common.runCuller=function() return 'RESULT launched=1 pending=1',nil end
            addPhoto('fixture.ARW',0,'',0)
        ''')
        self.script('OpenCuller.lua')
        self.assertEqual(len(self.lua.globals().MESSAGES),0)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),1)

    def manifest(self, raw, *, revision=1, marks=(1,0,0), closed=True):
        path=Path(self.tmp.name)/'session.txt'
        version={3:1,4:2,6:3}[len(marks)]
        path.write_text(f'FASTCULL_SESSION_V{version}\ttest-id\t'+('closed' if closed else 'active')+f'\t{revision}\n'
                        +str(raw).encode().hex()+'\t'+'\t'.join(map(str,marks))+'\n',encoding='utf-8')
        self.lua.globals().RAW=str(raw);self.lua.globals().MANIFEST=str(path)
        self.lua.execute("PREFS.sessions={{id='test-id',path=MANIFEST,catalog='catalog.lrcat',synced=0,photos={},paths={[RAW]=true}}}")
        return path

    def folder_fixture(self, *, closed=True, marks=(1,0,0)):
        raw=Path(self.tmp.name)/'原始 照片.ARW'
        raw.write_bytes(b'original raw')
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="4"/>',encoding='utf-8')
        manifest=self.manifest(raw,closed=closed,marks=marks)
        dest=Path(self.tmp.name)/'精选';dest.mkdir()
        target=dest/raw.name
        manifest.with_suffix('.folders').write_text('FASTCULL_FOLDERS_V1\ttest-id\t1\nMOVE\t'+str(raw).encode().hex()+'\t'+str(target).encode().hex()+'\n',encoding='utf-8')
        self.lua.globals().TARGET=str(target)
        self.lua.execute(r'''
            addPhoto(RAW,1,'Red',1,'original-uuid')
            ORIGINAL=PHOTOS[1]
            PREFS.sessions[1].photos={[RAW]={'original-uuid'}}
            CURRENT_PATH=RAW
            local get=ORIGINAL.getRawMetadata
            ORIGINAL.getRawMetadata=function(self,k) if k=='path' then return CURRENT_PATH end;return get(self,k) end
            SETS={};COLLECTIONS={};GATE_NUMBER=0;NEXT_COLLECTION=0
            local write=CATALOG.withWriteAccessDo
            CATALOG.withWriteAccessDo=function(self,name,f) GATE_NUMBER=GATE_NUMBER+1;return write(self,name,f) end
            CATALOG.createCollectionSet=function(self,name,parent,prior)
                assert(IN_GATE and prior);if not SETS[name] then SETS[name]={name=name} end;return SETS[name]
            end
            CATALOG.createCollection=function(self,name,parent,prior)
                assert(IN_GATE and prior);local key=parent.name..'/'..name
                if not COLLECTIONS[key] then
                    NEXT_COLLECTION=NEXT_COLLECTION+1
                    local c={localIdentifier=NEXT_COLLECTION,createdGate=GATE_NUMBER,members={},history='preserved'}
                    c.addPhotos=function(self,photos)
                        assert(IN_GATE and GATE_NUMBER>self.createdGate,'collection used before creation gate committed')
                        for _,photo in ipairs(photos) do self.members[photo]=true end
                    end
                    COLLECTIONS[key]=c
                end
                return COLLECTIONS[key]
            end
            CATALOG.setActiveSources=function(self,sources) ACTIVE_SOURCES=sources end
            CATALOG.setSelectedPhotos=function(self,active,photos) ACTIVE_PHOTO=active;SELECTED_PHOTOS=photos end
            CATALOG.addPhoto=function() error('organization must not reimport photos') end
        ''')
        return raw,manifest,target

    def edits_fixture(self, marks=(1,1,1,1,1,1), *, enabled=True, orientation=6):
        raw=Path(self.tmp.name)/'裁切 照片.ARW'
        raw.with_suffix('.xmp').write_text(f'''<rdf:Description
            xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
            xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:sr="urn:zrtc:syncratings:1/"
            xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
            xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
            xmp:Rating="4" xmp:Label="Blue" sr:Flag="reject" crs:Exposure2012="0.433333"
            crs:HasCrop="{str(enabled)}" crs:CropLeft="0.1" crs:CropTop="0.2"
            crs:CropRight="0.8" crs:CropBottom="0.9" crs:CropAngle="3.25"
            tiff:Orientation="{orientation}"/>''',encoding='utf-8')
        manifest=self.manifest(raw,marks=marks)
        self.lua.execute("addPhoto(RAW,1,'Red',1,'original-id');ORIGINAL=PHOTOS[1];PREFS.sessions[1].photos={[RAW]={'original-id'}}")
        request=manifest.with_suffix('.stars-request');request.write_text('test-id\nall\n')
        return raw,manifest,request

    def test_exit_watcher_syncs_all_six_fields_without_folder_plan(self):
        _,_,request=self.edits_fixture()
        self.lua.execute("PHOTOS={};local tasks=import 'LrTasks';tasks.sleep=function() end;require('develop').start(CATALOG,'test-id',MANIFEST,{})")
        self.assertEqual([tuple(v.values()) for _,v in self.lua.globals().WRITES.items()],[('rating',4),('colorNameForLabel','blue'),('pickStatus',-1)])
        settings=self.lua.eval('ORIGINAL:getDevelopSettings()')
        self.assertEqual(settings.orientation,'BC')
        self.assertAlmostEqual(settings.CropLeft,.1)
        self.assertAlmostEqual(settings.CropAngle,3.25)
        self.assertAlmostEqual(settings.Exposure2012,.433333)
        self.assertEqual(settings.Contrast2012,23)
        self.assertEqual(len(self.lua.globals().ROTATIONS),1)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.assertFalse(request.exists())
        self.assertEqual(len(self.lua.globals().MESSAGES),0)

    def test_crop_and_orientation_only_leave_ratings_and_exposure_alone(self):
        self.edits_fixture((0,0,0,0,1,1))
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().WRITES),0)
        self.assertEqual(len(self.lua.globals().DEVELOP_WRITES),0)
        self.assertAlmostEqual(self.lua.eval('ORIGINAL:getDevelopSettings().Exposure2012'),.1)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)

    def test_crop_reset_and_orientation_only_are_supported(self):
        self.edits_fixture((0,0,0,0,1,0),enabled=False)
        self.script('SyncSelected.lua')
        settings=self.lua.eval('ORIGINAL:getDevelopSettings()')
        self.assertEqual([settings[k] for k in ('CropLeft','CropTop','CropRight','CropBottom','CropAngle')],[0,0,1,1,0])
        self.assertEqual(len(self.lua.globals().ROTATIONS),0)
        self.edits_fixture((0,0,0,0,0,1))
        self.lua.execute('PATCHES={}')
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().PATCHES),0)
        self.assertEqual(len(self.lua.globals().ROTATIONS),1)

    def test_partial_failure_retry_does_not_rotate_twice_and_cleans_receipt(self):
        _,_,request=self.edits_fixture((0,0,0,0,1,1))
        self.lua.execute("APPLY=ORIGINAL.applyDevelopSettings;ORIGINAL.applyDevelopSettings=function() error('crop write failed') end")
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.assertTrue(request.exists())
        self.assertEqual(len(self.lua.globals().ROTATIONS),1)
        self.lua.execute('ORIGINAL.applyDevelopSettings=APPLY')
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().ROTATIONS),1)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.assertFalse(request.exists())

    def test_silent_develop_failure_remains_pending_until_readback_matches(self):
        _,_,request=self.edits_fixture((0,0,0,0,1,1))
        self.lua.execute('APPLY=ORIGINAL.applyDevelopSettings;ORIGINAL.applyDevelopSettings=function() end')
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.assertTrue(request.exists())
        self.lua.execute('ORIGINAL.applyDevelopSettings=APPLY')
        self.script('SyncSelected.lua')
        self.assertFalse(request.exists())
        self.assertEqual(len(self.lua.globals().ROTATIONS),1)

    def test_unsafe_old_lr_rotation_is_refused_without_acknowledgement(self):
        _,_,request=self.edits_fixture((0,0,0,0,0,1))
        self.lua.execute('SDK_VERSION={major=11,minor=3}')
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().ROTATIONS),0)
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.assertTrue(request.exists())
        self.assertIn('11.4',self.lua.globals().MESSAGES[1])

    def test_old_stars_sync_does_not_acknowledge_crop_or_orientation(self):
        self.edits_fixture()
        self.lua.execute("require('sync').run(false,'test-id',true,true)")
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.assertEqual(len(self.lua.globals().ROTATIONS),0)
        self.assertEqual(len(self.lua.globals().PATCHES),0)
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().WRITES),3)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)

    def test_absolute_rotation_is_idempotent_for_all_eight_orientations(self):
        self.lua.execute('''
            addPhoto('orientation.ARW',0,'',0)
            local codes={'AB','BA','CD','DC','CB','BC','AD','DA'}
            local right={6,7,8,5,2,3,4,1}
            local photo=PHOTOS[1];local edits=require 'photo_edits'
            for start=1,8 do
                photo:getDevelopSettings().orientation=codes[start]
                local row={orientation=right[start]}
                IN_GATE=true;edits.apply(photo,row);IN_GATE=false;edits.verify(photo,row)
                local n=#ROTATIONS
                IN_GATE=true;edits.apply(photo,row);IN_GATE=false;edits.verify(photo,row)
                assert(#ROTATIONS==n,'retry added a rotation')
                assert(photo:getDevelopSettings().orientation==codes[right[start]])
            end
        ''')
        self.assertEqual(len(self.lua.globals().ROTATIONS),8)

    def test_v3_parser_rejects_incomplete_and_nonfinite_crop(self):
        self.lua.execute('''
            local row='ROW_V3\\t61\\t-\\t-\\t-\\t-\\t6\\t1\\t0.1\\t0.2\\t0.8\\t0.9\\t3.25'
            local good=Common.parseBridge('SESSION\\ttest-id\\tclosed\\t1\\n'..row..'\\nRESULT rows=1 error=0\\n')
            assert(good.rows[1].orientation==6 and good.rows[1].exposure==nil)
            for _,bad in ipairs({row:gsub('3.25$','-'),row:gsub('3.25$','nan'),row:gsub('3.25$','46'),row:gsub('\\t6\\t','\\t9\\t')}) do
                assert(not pcall(Common.parseBridge,'SESSION\\ttest-id\\tclosed\\t1\\n'..bad..'\\nRESULT rows=1 error=0\\n'))
            end
        ''')

    def test_concurrent_sync_requests_serialize_snapshots_and_keep_later_edits(self):
        raw,manifest,_=self.edits_fixture((0,0,0,1,0,0))
        manifest.write_text(manifest.read_text().replace('closed','active'))
        self.lua.execute('''
            TASKS={};READS=0
            local tasks=import 'LrTasks'
            tasks.startAsyncTask=function(f) TASKS[#TASKS+1]=coroutine.create(f) end
            local read=Common.runCuller
            Common.runCuller=function(...)
                READS=READS+1;local out,err=read(...)
                if READS==1 then coroutine.yield('snapshot') end
                return out,err
            end
            require('sync').run(false,'test-id',false,true)
            assert(coroutine.resume(TASKS[1]))
            require('sync').run(false,'test-id',false,true)
            assert(#TASKS==1,'overlapping sync task was started')
            assert(#PATCHES==0)
        ''')
        manifest.write_text(manifest.read_text().replace('active\t1','closed\t2').replace('\t0\t0\t0\t1\t0\t0','\t0\t0\t0\t2\t0\t0'))
        xmp=raw.with_suffix('.xmp');xmp.write_text(xmp.read_text().replace('0.433333','0.766667'))
        self.lua.execute("local ok,err=coroutine.resume(TASKS[1]);assert(ok,err);assert(coroutine.status(TASKS[1])=='dead')")
        self.assertEqual(self.lua.globals().READS,2)
        self.assertAlmostEqual(self.lua.eval('ORIGINAL:getDevelopSettings().Exposure2012'),.766667)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.assertEqual(len(self.lua.globals().MESSAGES),0)

    def test_queued_manual_read_keeps_original_selection(self):
        raw,_,_=self.edits_fixture()
        self.lua.execute('''
            TASKS={};local tasks=import 'LrTasks'
            tasks.startAsyncTask=function(f) TASKS[#TASKS+1]=f end
            require('sync').run(true)
            addPhoto('unrelated.ARW',0,'',0,'unrelated')
            PHOTOS={PHOTOS[2]}
            TASKS[1]()
            assert(#WRITE_OWNERS==3)
            for _,photo in ipairs(WRITE_OWNERS) do assert(photo==ORIGINAL) end
        ''')
        self.assertEqual(len(self.lua.globals().ROTATIONS),1)

    def test_queue_recovers_after_unexpected_failure_and_catalog_switch(self):
        self.edits_fixture((0,0,0,1,0,0))
        self.lua.execute('''
            local prefs=Common.getPrefs
            Common.getPrefs=function() error('unexpected prefs failure') end
            require('sync').run(false,'test-id',false,true)
            Common.getPrefs=prefs
            TASKS={};local tasks=import 'LrTasks'
            tasks.startAsyncTask=function(f) TASKS[#TASKS+1]=f end
            require('sync').run(false,'test-id',false,true)
            local app=import 'LrApplication'
            app.activeCatalog=function() return {getPath=function() return 'other.lrcat' end} end
            TASKS[1]()
            assert(#PATCHES==0 and PREFS.sessions[1].synced==0)
            app.activeCatalog=function() return CATALOG end
            require('sync').run(false,'test-id',false,true)
            TASKS[2]()
        ''')
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.assertEqual(len(self.lua.globals().PATCHES),1)

    def test_session_opened_during_organization_handoff_is_not_lost(self):
        self.folder_fixture()
        self.lua.execute('''
            package.loaded.organization={prepare=function(catalog,entry)
                -- Opening another culler can append a session while the
                -- collection write gate yields to other SDK tasks.
                Common.rememberSession(catalog,'new-session','new.txt',{}, {})
                entry.organizationPending=true
                return 'organization pending'
            end}
        ''')
        self.script('SyncSelected.lua')
        entries=self.lua.globals().PREFS.sessions
        self.assertEqual(len(entries),2)
        self.assertEqual(entries[2].id,'new-session')
        self.assertTrue(entries[1].organizationPending)

    def test_retry_after_transaction_failure_does_not_duplicate_develop_writes(self):
        _,_,request=self.edits_fixture()
        self.lua.execute('''
            GATE=CATALOG.withWriteAccessDo
            CATALOG.withWriteAccessDo=function(self,name,f) GATE(self,name,f);error('commit response lost') end
        ''')
        self.script('SyncSelected.lua')
        self.assertTrue(request.exists())
        self.assertEqual(len(self.lua.globals().PATCHES),1)
        self.lua.execute('CATALOG.withWriteAccessDo=GATE')
        self.script('SyncSelected.lua')
        self.assertFalse(request.exists())
        self.assertEqual(len(self.lua.globals().PATCHES),1)
        self.assertEqual(len(self.lua.globals().ROTATIONS),1)

    def test_silent_exposure_failure_is_not_acknowledged(self):
        _,_,request=self.edits_fixture((0,0,0,1,0,0))
        self.lua.execute('APPLY=ORIGINAL.applyDevelopSettings;ORIGINAL.applyDevelopSettings=function() end')
        self.script('SyncSelected.lua')
        self.assertTrue(request.exists())
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.lua.execute('ORIGINAL.applyDevelopSettings=APPLY')
        self.script('SyncSelected.lua')
        self.assertFalse(request.exists())

    def test_exit_watcher_runs_before_large_baseline_finishes(self):
        _,_,request=self.edits_fixture((0,0,0,1,0,0))
        self.lua.execute('''
            TASKS={};local tasks=import 'LrTasks'
            tasks.startAsyncTask=function(f) TASKS[#TASKS+1]=f end
            tasks.sleep=function() end
            require('develop').start(CATALOG,'test-id',MANIFEST,{{path=RAW,photo=ORIGINAL}})
            assert(#TASKS==2,'watcher was deferred behind baseline export')
            TASKS[2]()
            assert(#TASKS==3,'exit sync was not scheduled independently')
            TASKS[3]()
        ''')
        self.assertFalse(request.exists())
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)

    def test_folder_handoff_keeps_uuid_and_waits_for_native_lr_move(self):
        raw,manifest,target=self.folder_fixture()
        self.lua.execute("PHOTOS={};require('sync').run(false,'test-id',false,true)")
        entry=self.lua.globals().PREFS.sessions[1]
        self.assertTrue(entry.organizationPending)
        self.assertEqual(entry.synced,1)
        self.assertEqual(self.lua.globals().NEXT_COLLECTION,1)
        self.assertTrue(self.lua.eval('ACTIVE_PHOTO==ORIGINAL and SELECTED_PHOTOS[1]==ORIGINAL'))
        self.assertEqual(raw.read_bytes(),b'original raw');self.assertFalse(target.exists())
        self.assertIn(str(target.parent),self.lua.globals().MESSAGES[1])
        self.lua.execute("require('sync').run(false,'test-id',false,true)")
        self.assertEqual(self.lua.globals().NEXT_COLLECTION,1)
        # Lightroom changes the original object's path itself; no removal/reimport.
        self.lua.execute("CURRENT_PATH=TARGET;require('sync').run(false,'test-id',false,true)")
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.assertTrue(self.lua.eval("UUIDS['original-uuid']==ORIGINAL"))

    def test_folder_handoff_includes_original_virtual_copy_objects(self):
        raw,manifest,target=self.folder_fixture()
        self.lua.execute("addPhoto(RAW,2,'Blue',0,'virtual-copy');VIRTUAL=PHOTOS[2];PREFS.sessions[1].photos[RAW][2]='virtual-copy'")
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().SELECTED_PHOTOS),2)
        self.assertTrue(self.lua.eval("SELECTED_PHOTOS[1]==ORIGINAL and SELECTED_PHOTOS[2]==VIRTUAL"))
        self.assertEqual(raw.read_bytes(),b'original raw');self.assertFalse(target.exists())

    def test_failed_collection_gate_keeps_receipt_and_plan_for_retry(self):
        raw,manifest,target=self.folder_fixture()
        manifest.write_text('FASTCULL_SESSION_V2\ttest-id\tclosed\t1\n'+str(raw).encode().hex()+'\t1\t0\t0\t1\n')
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="4" crs:Exposure2012="0.433333"/>')
        request=manifest.with_suffix('.stars-request');request.write_text('test-id\nall\n')
        self.lua.execute("ORIGINAL_CREATE=CATALOG.createCollection;CATALOG.createCollection=function() error('collection gate failed') end;require('sync').run(false,'test-id',false,true)")
        self.assertTrue(request.exists())
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,1)
        self.assertTrue(self.lua.globals().PREFS.sessions[1].organizationPending)
        self.assertIn('保留供重试',self.lua.globals().MESSAGES[1])
        self.assertEqual(raw.read_bytes(),b'original raw');self.assertFalse(target.exists())
        self.lua.execute("CATALOG.createCollection=ORIGINAL_CREATE;require('sync').run(false,'test-id',false,true)")
        self.assertEqual(len(self.lua.globals().WRITES),1)
        self.assertEqual(len(self.lua.globals().DEVELOP_WRITES),1)
        self.assertFalse(request.exists())

    def test_collection_gate_that_never_runs_keeps_organization_pending(self):
        raw,manifest,target=self.folder_fixture(marks=(0,0,0))
        self.lua.execute("CATALOG.withWriteAccessDo=function() end")
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,1)
        self.assertTrue(self.lua.globals().PREFS.sessions[1].organizationPending)
        self.assertEqual(self.lua.globals().NEXT_COLLECTION,0)
        self.assertFalse(target.exists())

    def test_active_folder_plan_is_deferred_until_gui_exit(self):
        self.folder_fixture(closed=False)
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().NEXT_COLLECTION,0)
        self.assertTrue(self.lua.globals().PREFS.sessions[1].organizationPending)

    def test_folder_plan_identity_and_unexpected_path_block_handoff(self):
        raw,manifest,target=self.folder_fixture()
        plan=manifest.with_suffix('.folders')
        original=plan.read_text();plan.write_text(original.replace('test-id','wrong-id'))
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().NEXT_COLLECTION,0)
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,1)
        self.assertTrue(self.lua.globals().PREFS.sessions[1].organizationPending)
        plan.write_text(original)
        self.lua.execute("CURRENT_PATH=RAW..'.elsewhere'")
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().NEXT_COLLECTION,0)
        self.assertFalse(target.exists())

    def test_exit_all_mode_syncs_edits_before_preparing_folder_groups(self):
        raw,manifest,target=self.folder_fixture(marks=(1,1,1))
        manifest.write_text('FASTCULL_SESSION_V2\ttest-id\tclosed\t1\n'+str(raw).encode().hex()+'\t1\t1\t1\t1\n')
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:sr="urn:zrtc:syncratings:1/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="4" xmp:Label="Blue" sr:Flag="reject" crs:Exposure2012="0.433333"/>')
        request=manifest.with_suffix('.stars-request');request.write_text('test-id\nall\n')
        self.lua.execute("local make=CATALOG.createCollection;CATALOG.createCollection=function(self,...) assert(#WRITES==3 and #DEVELOP_WRITES==1,'handoff precedes edit sync');return make(self,...) end;local tasks=import 'LrTasks';tasks.sleep=function() end;require('develop').start(CATALOG,'test-id',MANIFEST,{})")
        self.assertAlmostEqual(self.lua.globals().DEVELOP_WRITES[1],.433333)
        self.assertFalse(request.exists())
        self.assertEqual(self.lua.globals().NEXT_COLLECTION,1)
        self.assertTrue(self.lua.globals().PREFS.sessions[1].organizationPending)

    def test_session_ignores_current_selection_and_preserves_untouched_fields(self):
        raw=Path(self.tmp.name)/'原始 照片.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="4" xmp:Label="Blue" sr:Flag="reject"/>',encoding='utf-8')
        self.manifest(raw)
        self.lua.execute("addPhoto(RAW,1,'Red',1); PHOTOS={}")
        self.script('SyncSelected.lua')
        self.assertEqual([tuple(v.values()) for _,v in self.lua.globals().WRITES.items()],[('rating',4)])
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().WRITES),1)

    def test_active_session_acknowledges_snapshot_and_later_edits_remain_pending(self):
        raw=Path(self.tmp.name)/'active.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="4" xmp:Label=""/>',encoding='utf-8')
        path=self.manifest(raw,closed=False)
        self.lua.execute("addPhoto(RAW,1,'Red',1)")
        self.script('SyncSelected.lua')
        text=path.read_text().replace('active\t1','active\t2').replace('\t1\t0\t0','\t1\t2\t0')
        path.write_text(text,encoding='utf-8')
        self.script('SyncSelected.lua')
        self.assertEqual([tuple(v.values()) for _,v in self.lua.globals().WRITES.items()],[('rating',4),('label','')])
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,2)

    def test_exposure_sync_patches_only_exposure_and_retains_failed_receipt(self):
        raw=Path(self.tmp.name)/'exposure.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.433333" crs:Contrast2012="23"/>',encoding='utf-8')
        manifest=self.manifest(raw,marks=(0,0,0))
        manifest.write_text(manifest.read_text().replace('SESSION_V1','SESSION_V2').rstrip()+'\t1\n',encoding='utf-8')
        self.lua.execute("addPhoto(RAW,1,'Red',1);DEVELOP={};SETTINGS={Exposure2012=0.1,Contrast2012=23};PHOTOS[1].getDevelopSettings=function() return SETTINGS end;PHOTOS[1].applyDevelopSettings=function(self,s,name) assert(IN_GATE);assert(s.Contrast2012==nil);DEVELOP[#DEVELOP+1]=s.Exposure2012;SETTINGS.Exposure2012=s.Exposure2012 end")
        self.script('SyncSelected.lua')
        self.assertAlmostEqual(self.lua.globals().DEVELOP[1],.433333)
        self.assertEqual(len(self.lua.globals().WRITES),0)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.manifest(raw,marks=(0,0,0))
        # Restore a fresh V2 receipt, then force SDK write failure.
        manifest.write_text('FASTCULL_SESSION_V2\ttest-id\tclosed\t1\n'+str(raw).encode().hex()+'\t0\t0\t0\t1\n',encoding='utf-8')
        self.lua.execute("SETTINGS.Exposure2012=0.1;PHOTOS[1].applyDevelopSettings=function() error('write failed') end")
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)

    def test_close_auto_stars_ignores_selection_and_never_acknowledges_other_fields(self):
        raw=Path(self.tmp.name)/'original.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="5" xmp:Label="Blue" sr:Flag="reject"/>',encoding='utf-8')
        manifest=self.manifest(raw,marks=(1,1,1))
        request=manifest.with_suffix('.stars-request');request.write_text('test-id\n',encoding='utf-8')
        self.lua.execute("addPhoto(RAW,1,'Red',1,'original-id');PREFS.sessions[1].photos={[RAW]={'original-id'}};PHOTOS={}")
        self.lua.execute("require('sync').run(false,'test-id',true)")
        self.assertEqual([tuple(v.values()) for _,v in self.lua.globals().WRITES.items()],[('rating',5)])
        self.assertFalse(request.exists())
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.script('SyncSelected.lua')
        self.assertEqual([tuple(v.values()) for _,v in self.lua.globals().WRITES.items()][-2:],[('colorNameForLabel','blue'),('pickStatus',-1)])

    def test_star_only_closed_session_is_removed_after_automatic_sync(self):
        raw=Path(self.tmp.name)/'completed-stars.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="4"/>',encoding='utf-8')
        manifest=self.manifest(raw);manifest.with_suffix('.stars-request').write_text('test-id\n')
        self.lua.execute("addPhoto(RAW,1,'Red',1);require('sync').run(false,'test-id',true)")
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)
        self.assertFalse(manifest.with_suffix('.stars-request').exists())
        self.assertEqual(len(self.lua.globals().MESSAGES),0,'Successful automatic synchronization should not add another confirmation')

    def test_develop_baseline_and_native_close_watcher_are_deferred_and_session_scoped(self):
        raw=Path(self.tmp.name)/'watch.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="4"/>',encoding='utf-8')
        manifest=self.manifest(raw)
        self.lua.execute("addPhoto(RAW,1,'Red',1);PHOTOS[1].getDevelopSettings=function() return {Exposure2012=-0.2} end;TASKS={};local tasks=import 'LrTasks';tasks.startAsyncTask=function(f) TASKS[#TASKS+1]=f end;tasks.sleep=function() end;require('develop').start(CATALOG,'test-id',MANIFEST,{{path=RAW,photo=PHOTOS[1]}})")
        self.assertFalse(manifest.with_suffix('.exposure').exists())
        manifest.with_suffix('.stars-request').write_text('test-id\n',encoding='utf-8')
        self.lua.execute('TASKS[1]()')
        self.assertEqual(manifest.with_suffix('.exposure').read_text().splitlines()[0].split('\t')[1], '-200000')
        self.assertEqual(len(self.lua.globals().WRITES),0)
        self.lua.execute('TASKS[2]()')
        self.assertEqual(len(self.lua.globals().WRITES),0)
        self.lua.execute('TASKS[3]()')
        self.assertEqual(len(self.lua.globals().WRITES),1)

    def test_unresolved_exposure_is_disabled_instead_of_assuming_zero(self):
        raw=Path(self.tmp.name)/'unresolved.ARW'
        manifest=self.manifest(raw)
        self.lua.execute("addPhoto(RAW,1,'Red',1);PHOTOS[1].getDevelopSettings=function() return {Exposure2012=-999999} end;require('develop').start(CATALOG,'test-id',MANIFEST,{{path=RAW,photo=PHOTOS[1]}})")
        baseline=manifest.with_suffix('.exposure').read_text()
        self.assertEqual(baseline.splitlines()[0].split('\t')[1], '-')
        self.assertTrue(baseline.endswith('END\n'))
        self.assertEqual(len(self.lua.globals().WRITES),0)

    def test_catalog_crop_and_orientation_are_read_without_writing_photo_or_catalog(self):
        raw=Path(self.tmp.name)/'catalog-crop.ARW'
        manifest=self.manifest(raw)
        self.lua.execute("addPhoto(RAW,1,'Red',1)")
        for orientation,code in enumerate(['AB','BA','CD','DC','CB','BC','AD','DA'],1):
            self.lua.globals().CODE=code
            self.lua.execute("PHOTOS[1].getDevelopSettings=function() return {orientation=CODE,CropLeft=0.1,CropTop=0.2,CropRight=0.9,CropBottom=0.8,CropAngle=-2.5} end;require('develop').start(CATALOG,'test-id',MANIFEST,{{path=RAW,photo=PHOTOS[1]}})")
            fields=manifest.with_suffix('.exposure').read_text().splitlines()[0].split('\t')
            self.assertEqual(fields[1:],['-',str(orientation),'1','100000','200000','900000','800000','-2500000'])
        self.lua.execute("PHOTOS[1].getDevelopSettings=function() return {HasCrop=false,CropLeft=0.1,CropTop=0.2,CropRight=0.9,CropBottom=0.8} end;require('develop').start(CATALOG,'test-id',MANIFEST,{{path=RAW,photo=PHOTOS[1]}})")
        fields=manifest.with_suffix('.exposure').read_text().splitlines()[0].split('\t')
        self.assertEqual(fields[3:],['0','0','0','1000000','1000000','0'])
        self.lua.execute("PHOTOS[1].getDevelopSettings=function() error('SDK failed') end;require('develop').start(CATALOG,'test-id',MANIFEST,{{path=RAW,photo=PHOTOS[1]}})")
        self.assertEqual(manifest.with_suffix('.exposure').read_text().splitlines()[0].split('\t')[1:],['-'])
        self.assertFalse(raw.with_suffix('.xmp').exists())
        self.assertEqual(len(self.lua.globals().DEVELOP_WRITES),0)

    def test_corrupt_sidecar_does_not_acknowledge_or_modify_catalog(self):
        raw=Path(self.tmp.name)/'bad.ARW';raw.with_suffix('.xmp').write_text('<broken>',encoding='utf-8')
        self.manifest(raw);self.lua.execute("addPhoto(RAW,1,'Red',1)")
        self.script('SyncSelected.lua')
        self.assertEqual(len(self.lua.globals().WRITES),0)
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.assertIn('失败',self.lua.globals().MESSAGES[1])

    def test_session_cannot_sync_to_another_catalog_or_an_unlisted_photo(self):
        raw=Path(self.tmp.name)/'scope.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="4"/>',encoding='utf-8')
        self.manifest(raw);self.lua.execute("addPhoto(RAW,1,'Red',1); PREFS.sessions[1].catalog='another.lrcat'")
        self.script('SyncSelected.lua');self.assertEqual(len(self.lua.globals().WRITES),0)
        self.lua.execute("PREFS.sessions[1].catalog='catalog.lrcat';PREFS.sessions[1].paths={}")
        self.script('SyncSelected.lua');self.assertEqual(len(self.lua.globals().WRITES),0)
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)

    def test_uuid_preserves_original_virtual_copy_identity(self):
        raw=Path(self.tmp.name)/'virtual.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="4"/>',encoding='utf-8')
        self.manifest(raw)
        self.lua.execute("addPhoto(RAW,1,'Red',1,'original-uuid');ORIGINAL=PHOTOS[1];addPhoto(RAW,5,'Blue',0,'other-uuid');PREFS.sessions[1].photos={[RAW]={'original-uuid'}}")
        self.script('SyncSelected.lua')
        self.lua.execute('assert(WRITE_OWNERS[1]==ORIGINAL)')

    def test_field_write_failure_is_reported_and_keeps_session_pending(self):
        raw=Path(self.tmp.name)/'flag.ARW'
        raw.with_suffix('.xmp').write_text('<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmp:Rating="4" sr:Flag="pick"/>',encoding='utf-8')
        self.manifest(raw,marks=(1,0,1));self.lua.execute("addPhoto(RAW,1,'Red',1);local write=PHOTOS[1].setRawMetadata;PHOTOS[1].setRawMetadata=function(self,k,v) if k=='pickStatus' then error('unsupported flag') end;write(self,k,v) end")
        self.script('SyncSelected.lua')
        self.assertEqual(self.lua.globals().PREFS.sessions[1].synced,0)
        self.assertIn('失败',self.lua.globals().MESSAGES[1])

    def test_bridge_rejects_mismatched_identity_and_truncated_output(self):
        self.lua.execute("assert(not pcall(Common.parseBridge,'SESSION\\twrong\\tclosed\\t1\\nRESULT rows=0 error=0','expected'));assert(not pcall(Common.parseBridge,'SESSION\\ttest-id\\tclosed\\t1\\nROW\\t61\\t4\\t-\\t-\\nRESULT rows=0 error=0','test-id'))")

    def test_comments_and_wrong_namespace_do_not_supply_rating(self):
        raw=Path(self.tmp.name)/'namespace.ARW'
        raw.with_suffix('.xmp').write_text('''<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
            xmlns:xmp="urn:unrelated" xmlns:m="http://ns.adobe.com/xap/1.0/" xmp:Rating="5" m:Label="Blue">
            <!-- xmp:Rating="4" --></rdf:Description>''',encoding='utf-8')
        self.lua.globals().RAW=str(raw);self.lua.execute("addPhoto(RAW,1,'Red',1)")
        self.script('SyncSidecars.lua')
        self.assertEqual([tuple(v.values()) for _,v in self.lua.globals().WRITES.items()],[('colorNameForLabel','blue')])

    def test_session_path_uses_documented_sdk_and_accepts_existing_directory(self):
        self.lua.execute('os.getenv=nil')
        expected=Path(self.tmp.name)/'sdk/data/FastCull/sessions/session_test-id.txt'
        self.assertEqual(Path(self.lua.eval('Common.sessionPath')("test-id")),expected)
        self.assertTrue(expected.parent.is_dir())
        self.assertEqual(Path(self.lua.eval('Common.sessionPath')("test-id")),expected)

    def test_missing_app_data_falls_back_to_sdk_preferences_not_temp(self):
        self.lua.execute('SDK_PATHS.appData=nil')
        path=Path(self.lua.eval('Common.sessionPath')("test-id"))
        self.assertEqual(path,Path(self.tmp.name)/'sdk/prefs/FastCull/sessions/session_test-id.txt')
        self.assertTrue(path.parent.is_dir())

    def test_directory_returned_failure_uses_writable_fallback(self):
        self.lua.execute("local files=import 'LrFileUtils';local make=files.createAllDirectories;files.createAllDirectories=function(path) if path:find('/sdk/data/',1,true) then return false,'permission denied' end;return make(path) end")
        path=Path(self.lua.eval('Common.sessionPath')("test-id"))
        self.assertTrue(path.is_relative_to(Path(self.tmp.name)/'sdk/prefs'))
        self.assertTrue(path.parent.is_dir())

    def test_all_directory_failures_report_cause_without_launch_or_metadata_write(self):
        self.lua.globals().PROGRAM=str(PLUGIN/'fastcull.exe')
        self.lua.execute("PREFS.cullerPath=PROGRAM;local files=import 'LrFileUtils';files.createAllDirectories=function() return false,'permission denied' end;EXECUTED=0;Common.runCuller=function() EXECUTED=EXECUTED+1;return 'RESULT launched=1',nil end;addPhoto('test.ARW',4,'Purple',-1)")
        self.script('OpenCuller.lua')
        self.assertEqual(self.lua.globals().EXECUTED,0)
        self.assertEqual(len(self.lua.globals().WRITES),0)
        self.assertIn('permission denied',self.lua.globals().MESSAGES[1])

    def test_plugin_log_directory_is_created_using_supported_sdk(self):
        self.lua.eval('Common.log')('SDK path regression')
        files=list((Path(self.tmp.name)/'sdk/data/FastCull/logs').glob('fastcull_plugin_*.log'))
        self.assertEqual(len(files),1)
        self.assertIn('SDK path regression',files[0].read_text(encoding='utf-8'))

    @unittest.skipUnless(os.environ.get('FASTCULL_NATIVE_TEST')=='1', 'optional native GUI roundtrip')
    def test_sdk_open_to_native_gui_and_session_sync(self):
        jpeg=(ROOT/'tests/fixtures/tiny.jpg').read_bytes()
        raw=bytearray(64)
        raw[:8]=b'II\x2a\x00\x08\x00\x00\x00'
        struct.pack_into('<H',raw,8,2)
        for i,tag,value in [(0,0x0201,64),(1,0x0202,len(jpeg))]:
            struct.pack_into('<HHII',raw,10+i*12,tag,4,1,value)
        raw.extend(jpeg)
        for i in range(2):
            path=Path(self.tmp.name)/f'fixture{i}.ARW';path.write_bytes(raw)
            self.lua.globals().RAW=str(path)
            self.lua.execute(f"addPhoto(RAW,2,'Blue',1,'photo-{i}')")
        # Application-owned automation on private generated images; no real LR writes.
        self.backend_env={'FASTCULL_SMOKE_DIR':str(Path(self.tmp.name)/'gui'),'FASTCULL_STRESS':''}
        self.lua.execute("package.loaded.common=nil; Common=require 'common'")
        self.lua.globals().PROGRAM=str(Path(os.environ.get('CARGO_TARGET_DIR', ROOT/'target'))/'x86_64-pc-windows-msvc/debug/fastcull.exe')
        self.lua.globals().SDK_EXECUTE=self.sdk_execute
        self.lua.execute("PREFS.cullerPath=PROGRAM;local tasks=import 'LrTasks';tasks.execute=function(cmd) return SDK_EXECUTE(cmd) end;io.popen=function() error('native SDK path used popen') end")
        self.script('OpenCuller.lua')
        self.assertEqual(len(self.lua.globals().MESSAGES),0)
        self.assertEqual(len(self.lua.globals().PREFS.sessions),1)
        manifest=Path(self.lua.globals().PREFS.sessions[1].path)
        self.assertTrue(manifest.is_relative_to(Path(self.tmp.name)/'sdk/data'))
        deadline=time.monotonic()+15
        while time.monotonic()<deadline:
            if manifest.is_file() and '\tclosed\t' in manifest.read_text(encoding='utf-8').splitlines()[0]: break
            time.sleep(.05)
        self.assertIn('\tclosed\t',manifest.read_text(encoding='utf-8').splitlines()[0])
        result=(Path(self.tmp.name)/'gui/result.txt').read_text(encoding='utf-8')
        self.assertIn('first_draw=true',result);self.assertIn('error=None',result)
        self.lua.execute('PHOTOS={}')
        self.script('SyncSelected.lua')
        self.assertEqual([tuple(v.values()) for _,v in self.lua.globals().WRITES.items()],
                         [('rating',4),('colorNameForLabel','purple'),('pickStatus',-1)])
        self.assertEqual(len(self.lua.globals().PREFS.sessions),0)

    def test_unix_state_delimiters_fail_before_launch_or_metadata_writes(self):
        for path in ['/Photos/bad|name.ARW', '/Photos/bad\nname.ARW']:
            self.lua.globals().UNSAFE_PATH = path
            self.lua.execute('''
                PHOTOS={}; MESSAGES={}; WRITES={}; EXECUTED=0
                PREFS.cullerPath='/custom/fastcull'
                Common.fileExists=function() return true end
                Common.runCuller=function() EXECUTED=EXECUTED+1; return 'RESULT launched=1',nil end
                addPhoto(UNSAFE_PATH,4,'Red',1)
            ''')
            self.script('OpenCuller.lua')
            self.assertEqual(self.lua.globals().EXECUTED, 0)
            self.assertEqual(len(self.lua.globals().WRITES), 0)
            self.assertIn('重命名', self.lua.globals().MESSAGES[1])

if __name__ == "__main__":
    unittest.main()
