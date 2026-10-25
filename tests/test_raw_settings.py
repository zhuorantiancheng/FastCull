"""Strict Lightroom SDK-shaped contract tests; these do not launch Lightroom."""
import shutil
import tempfile
import unittest
from pathlib import Path

from lupa import LuaRuntime

ROOT = Path(__file__).resolve().parents[1]
PLUGIN = ROOT / 'lrplugins/FastCull.lrplugin'
SCRATCH = ROOT / '.agent-temp/2026-10-23-raw-edit/lr-settings'


class RawSettingsTests(unittest.TestCase):
    def setUp(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.tmp = tempfile.TemporaryDirectory(prefix='sdk-', dir=SCRATCH)
        self.addCleanup(self.tmp.cleanup)
        self.scratch = Path(self.tmp.name)
        self.manifest = self.scratch / 'appdata/FastCull/sessions/session_session-1234.txt'
        self.manifest.parent.mkdir(parents=True)
        self.manifest.write_text('FASTCULL_SESSION_V4\tsession-1234\tactive\t0\n', encoding='utf-8')
        self.source = self.scratch / 'originals/照片 one.ARW'
        self.source.parent.mkdir()
        self.source.write_bytes(b'original camera raw bytes')
        self.source.with_suffix('.xmp').write_text('old manually saved XMP', encoding='utf-8')
        self.cache = self.manifest.parent.parent / 'raw-cache'
        self.exports = []
        self.created = []
        self.export_mode = 'ok'
        self.lua = LuaRuntime(unpack_returned_tuples=True)
        values = self.lua.globals()
        values.PLUGIN = str(PLUGIN)
        values.MANIFEST = str(self.manifest)
        values.SOURCE = str(self.source)
        values.PY_EXISTS = self.exists
        values.PY_CREATE = self.create
        values.PY_EXPORT = self.export
        self.lua.execute(r'''
            TASKS={}; IMPORTS={}; LOGS={}; UUIDS={}; METADATA_READS=0
            local function key(p) return p:gsub('\\','/'):lower() end
            local function unhex(token)
                if token=='~' then return '' end
                if #token==0 or #token%2~=0 or token:find('[^%x]') then error('bad hex') end
                return (token:gsub('%x%x',function(v) return string.char(tonumber(v,16)) end))
            end
            package.path=PLUGIN..'/?.lua;'..package.path
            package.loaded.common={
                isMac=false,pathKey=key,hexDecode=unhex,
                catalogKey=function(c) return key(c:getPath()) end,
                sessionPath=function(id) assert(id=='session-1234');return MANIFEST end,
                log=function(message) LOGS[#LOGS+1]=message end,
            }
            CATALOG={
                getPath=function() return 'catalog-current.lrcat' end,
                findPhotoByUuid=function(self,uuid) return UUIDS[uuid] end,
            }
            CURRENT_CATALOG=CATALOG
            function addPhoto(path,uuid)
                local p={path=path,uuid=uuid}
                function p:getRawMetadata(name)
                    METADATA_READS=METADATA_READS+1
                    assert(name=='path' or name=='uuid','unexpected metadata field '..name)
                    return self[name]
                end
                UUIDS[uuid]=p
                return p
            end
            PHOTO=addPhoto(SOURCE,'photo-uuid')
            PICKED={{path=SOURCE,photo=PHOTO}}
            local supported={
                LR_format=true,LR_export_destinationType=true,LR_export_destinationPathPrefix=true,
                LR_export_useSubfolder=true,LR_collisionHandling=true,LR_renamingTokensOn=true,
                LR_extensionCase=true,LR_reimportExportedPhoto=true,LR_minimizeEmbeddedMetadata=true,
                LR_embeddedMetadataOption=true,LR_metadata_keywordOptions=true,LR_removeLocationMetadata=true,
                LR_removeFaceMetadata=true,LR_size_doConstrain=true,LR_outputSharpeningOn=true,
                LR_useWatermark=true,LR_export_postProcessing=true,
            }
            local modules={
                LrTasks={
                    pcall=pcall,
                    startAsyncTask=function(fn) TASKS[#TASKS+1]=coroutine.create(fn) end,
                    sleep=function(seconds) assert(seconds==0.25);coroutine.yield('sleep') end,
                },
                LrApplication={activeCatalog=function() return CURRENT_CATALOG end},
                LrPathUtils={
                    child=function(parent,name) assert(parent and name);return parent..'/'..name end,
                    parent=function(path) return path:gsub('\\','/'):match('^(.*)/[^/]+$') end,
                },
                LrFileUtils={exists=PY_EXISTS,createAllDirectories=PY_CREATE},
                LrExportSession=function(parameters)
                    assert(type(parameters)=='table' and parameters.photosToExport and parameters.exportSettings)
                    assert(#parameters.photosToExport==1,'only the requested photo may be exported')
                    for k in pairs(parameters) do assert(k=='photosToExport' or k=='exportSettings',k) end
                    for k in pairs(parameters.exportSettings) do assert(supported[k],k) end
                    local s=parameters.exportSettings
                    assert(s.LR_format=='ORIGINAL' and s.LR_export_destinationType=='specificFolder')
                    assert(s.LR_export_useSubfolder==false and s.LR_collisionHandling=='skip')
                    assert(s.LR_embeddedMetadataOption=='all' and s.LR_minimizeEmbeddedMetadata==false)
                    assert(s.LR_removeLocationMetadata==false and s.LR_removeFaceMetadata==false)
                    assert(s.LR_reimportExportedPhoto==false and s.LR_renamingTokensOn==false)
                    return {renditions=function(self)
                        local index=0
                        return function()
                            index=index+1
                            if EXPORT_MODE=='empty' or index>(EXPORT_MODE=='multiple' and 2 or 1) then return end
                            local rendition={photo=EXPORT_MODE=='wrong-photo' and addPhoto('other.NEF','other-uuid') or parameters.photosToExport[1]}
                            function rendition:waitForRender() return PY_EXPORT(parameters.photosToExport[1],s) end
                            return index,rendition
                        end
                    end}
                end,
            }
            import=function(name)
                assert(modules[name],'unmocked SDK module '..name)
                IMPORTS[#IMPORTS+1]=name
                return modules[name]
            end
            RawSettings=require 'raw_settings'
            function tick()
                for _,thread in ipairs(TASKS) do
                    if coroutine.status(thread)~='dead' then
                        local ok,message=coroutine.resume(thread)
                        assert(ok,message)
                    end
                end
            end
        ''')
        self.lua.globals().EXPORT_MODE = self.export_mode

    def exists(self, path):
        target = Path(path)
        return 'file' if target.is_file() else ('directory' if target.is_dir() else False)

    def create(self, path):
        target = Path(path)
        self.assertTrue(target.resolve().is_relative_to(self.scratch.resolve()))
        self.created.append(target)
        target.mkdir(parents=True, exist_ok=True)
        return True

    def export(self, photo, settings):
        source = Path(photo.getRawMetadata(photo, 'path'))
        target = Path(settings.LR_export_destinationPathPrefix)
        self.exports.append((source, dict(settings.items())))
        if self.export_mode == 'failure':
            return False, 'SDK export failed'
        if self.export_mode == 'wrong-path':
            return True, str(source)
        output = target / (source.stem + source.suffix.lower())
        shutil.copyfile(source, output)
        if source.suffix.lower() != '.dng' and self.export_mode != 'missing-sidecar':
            output.with_suffix('.xmp').write_text('UNSAVED catalog develop XMP', encoding='utf-8')
        # The module must retain exported ACR data and arbitrary relative resources.
        output.with_suffix('.acr').write_bytes(b'fresh catalog ACR resource')
        (target / 'resources').mkdir(exist_ok=True)
        (target / 'resources/profile.bin').write_bytes(b'current profile data')
        if self.export_mode == 'switch-catalog':
            self.lua.execute("CURRENT_CATALOG={getPath=function() return 'other.lrcat' end}")
        if self.export_mode == 'change-uuid':
            photo.uuid = 'new-uuid'
        if self.export_mode == 'supersede':
            newer = self.target('request-0002')
            self.manifest.with_suffix('.raw-request').write_text(
                'session-1234\n' + str(source).encode().hex() + '\n' + str(newer).encode().hex() + '\n')
            self.export_mode = 'ok'
            self.lua.globals().EXPORT_MODE = 'ok'
        return True, str(output)

    def start(self):
        self.lua.execute("RawSettings.start(CATALOG,'session-1234',MANIFEST,PICKED);tick()")

    def target(self, nonce='request-0001'):
        return self.cache / ('lr-session-1234-' + nonce)

    def request(self, source=None, target=None, session='session-1234'):
        source = str(source or self.source)
        target = str(target or self.target())
        self.manifest.with_suffix('.raw-request').write_text(
            session + '\n' + source.encode('utf-8').hex() + '\n' + target.encode('utf-8').hex() + '\n', encoding='ascii')
        self.lua.globals().EXPORT_MODE = self.export_mode
        self.lua.execute('tick()')

    def result(self):
        lines = self.manifest.with_suffix('.raw-result').read_text(encoding='ascii').splitlines()
        self.assertEqual(lines[0], 'session-1234')
        source = bytes.fromhex(lines[1]).decode('utf-8')
        status, value = lines[2].split('\t')
        return source, status, bytes.fromhex(value).decode('utf-8')

    def test_start_is_lazy_deduplicated_and_closed_session_stops(self):
        self.start()
        self.lua.execute("RawSettings.start(CATALOG,'session-1234',MANIFEST,PICKED);tick();tick()")
        self.assertEqual(len(self.lua.globals().TASKS), 1)
        self.assertEqual(self.lua.globals().METADATA_READS, 0)
        self.assertNotIn('LrExportSession', list(self.lua.globals().IMPORTS.values()))
        self.assertFalse(self.cache.exists())
        self.manifest.write_text('FASTCULL_SESSION_V4\tsession-1234\tclosed\t0\n', encoding='utf-8')
        self.lua.execute('tick()')
        self.assertEqual(self.lua.eval('coroutine.status(TASKS[1])'), 'dead')

    def test_temporary_manifest_unavailability_does_not_close_the_watcher(self):
        saved = self.manifest.with_suffix('.snapshot')
        self.manifest.rename(saved)
        self.start()
        self.lua.execute('tick()')
        self.assertEqual(self.lua.eval('coroutine.status(TASKS[1])'), 'suspended')
        self.assertFalse(self.created)
        saved.rename(self.manifest)
        self.request()
        self.assertEqual(self.result()[1], 'OK')

    def test_request_exports_only_one_current_photo_and_retains_all_resources(self):
        before = self.source.read_bytes(), self.source.with_suffix('.xmp').read_bytes()
        self.lua.execute("PICKED[2]={path='another.NEF',photo=addPhoto('another.NEF','another-uuid')}")
        self.start()
        self.request()
        source, status, value = self.result()
        self.assertEqual((source, status), (str(self.source), 'OK'))
        output = Path(value)
        self.assertEqual(output, self.target() / '照片 one.arw')
        self.assertEqual(output.with_suffix('.xmp').read_text(), 'UNSAVED catalog develop XMP')
        self.assertEqual(output.with_suffix('.acr').read_bytes(), b'fresh catalog ACR resource')
        self.assertEqual((self.target() / 'resources/profile.bin').read_bytes(), b'current profile data')
        self.assertTrue((self.target() / '.fastcull-adobe-export').is_file())
        self.assertEqual((self.source.read_bytes(), self.source.with_suffix('.xmp').read_bytes()), before)
        self.assertEqual([item[0] for item in self.exports], [self.source])
        self.lua.execute('tick();tick()')
        self.assertEqual(len(self.exports), 1, 'unchanged requests must not export repeatedly')
        self.request(target=self.target('request-0002'))
        self.assertEqual(len(self.exports), 2)

    def test_nef_and_dng_original_exports_keep_their_format(self):
        paths = {}
        for extension in ['NEF', 'DNG']:
            path = self.source.with_suffix('.' + extension)
            paths[extension] = path
            path.write_bytes(b'fixture original ' + extension.encode())
            self.lua.globals().SOURCE = str(path)
            self.lua.globals().UUID = 'photo-' + extension
            self.lua.execute("PICKED[#PICKED+1]={path=SOURCE,photo=addPhoto(SOURCE,UUID)}")
        self.start()
        for extension, path in paths.items():
            with self.subTest(extension=extension):
                target = self.target('request-' + extension)
                self.request(source=path, target=target)
                _, status, output = self.result()
                self.assertEqual(status, 'OK')
                self.assertEqual(Path(output).suffix, '.' + extension.lower())
                self.assertEqual(path.read_bytes(), b'fixture original ' + extension.encode())

    def test_unselected_wrong_session_and_unsupported_sources_do_not_create_directories(self):
        self.start()
        for source, session in [(self.source.with_name('unselected.ARW'), 'session-1234'),
                                (self.source, 'other-session'),
                                (self.source.with_suffix('.JPG'), 'session-1234')]:
            self.request(source=source, session=session)
            self.assertEqual(self.result()[1], 'ERROR')
        self.assertFalse(self.created)
        self.assertFalse(self.exports)
        self.assertNotIn('LrExportSession', list(self.lua.globals().IMPORTS.values()))

    def test_target_must_be_a_new_direct_session_cache_child(self):
        self.start()
        existing = self.target('already-exists')
        existing.mkdir(parents=True)
        forbidden = [self.source.parent, existing, self.cache.parent / 'raw-cache-other/lr-session-1234-request-0001',
                     self.cache / '../outside/lr-session-1234-request-0001', self.cache / 'lr-other-session-request-0001',
                     self.cache / 'lr-session-1234-short', self.cache / 'lr-session-1234-request-0001/nested']
        for target in forbidden:
            with self.subTest(target=target):
                self.request(target=target)
                self.assertEqual(self.result()[1], 'ERROR')
        self.assertFalse(self.created)
        self.assertFalse(self.exports)

    def test_ambiguous_virtual_copies_and_changed_uuid_are_rejected(self):
        self.lua.execute("PICKED[2]={path=SOURCE,photo=addPhoto(SOURCE,'virtual-uuid')}")
        self.start()
        self.request()
        self.assertIn('虚拟副本', self.result()[2])
        self.assertFalse(self.exports)
        self.lua.execute("PICKED[2]=nil;UUIDS['photo-uuid']=nil")
        self.request(target=self.target('request-0002'))
        self.assertIn('身份', self.result()[2])
        self.assertFalse(self.exports)

    def test_sdk_may_return_equivalent_photo_wrappers_for_the_same_uuid(self):
        self.lua.execute('''
            function wrapper(photo)
                return {getRawMetadata=function(self,name) return photo:getRawMetadata(name) end}
            end
            PICKED[2]={path=SOURCE,photo=wrapper(PHOTO)}
            CATALOG.findPhotoByUuid=function(self,uuid)
                local found=UUIDS[uuid]
                return found and wrapper(found) or nil
            end
        ''')
        self.start()
        self.request()
        self.assertEqual(self.result()[1], 'OK')

    def test_newer_request_suppresses_a_stale_response_during_export(self):
        self.start()
        self.export_mode = 'supersede'
        self.request()
        self.assertFalse(self.manifest.with_suffix('.raw-result').exists())
        self.lua.execute('tick()')
        source, status, value = self.result()
        self.assertEqual((source, status), (str(self.source), 'OK'))
        self.assertEqual(Path(value).parent, self.target('request-0002'))

    def test_catalog_switch_before_or_during_export_never_returns_ok(self):
        self.start()
        self.lua.execute("CURRENT_CATALOG={getPath=function() return 'other.lrcat' end}")
        self.request()
        self.assertIn('目录已切换', self.result()[2])
        self.assertFalse(self.exports)
        self.lua.execute('CURRENT_CATALOG=CATALOG')
        self.export_mode = 'switch-catalog'
        self.request(target=self.target('request-0002'))
        self.assertEqual(self.result()[1], 'ERROR')
        self.assertIn('目录已切换', self.result()[2])

    def test_failed_unexpected_or_incomplete_export_is_not_accepted(self):
        self.start()
        for mode in ['failure', 'wrong-path', 'empty', 'multiple', 'wrong-photo', 'missing-sidecar', 'change-uuid']:
            with self.subTest(mode=mode):
                self.export_mode = mode
                self.request(target=self.target('request-' + mode))
                self.assertEqual(self.result()[1], 'ERROR')
                self.assertFalse((self.target('request-' + mode) / '.fastcull-adobe-export').exists())

    def test_mismatched_manifest_identity_cannot_use_the_cache(self):
        fake = self.manifest.with_name('forged.txt')
        fake.write_text(self.manifest.read_text(), encoding='utf-8')
        self.lua.globals().FAKE = str(fake)
        self.lua.execute("RawSettings.start(CATALOG,'session-1234',FAKE,PICKED);tick()")
        fake.with_suffix('.raw-request').write_text(
            'session-1234\n' + str(self.source).encode().hex() + '\n' + str(self.target()).encode().hex() + '\n')
        self.lua.execute('tick()')
        result = fake.with_suffix('.raw-result').read_text().splitlines()[2]
        self.assertTrue(result.startswith('ERROR\t'))
        self.assertFalse(self.exports)
        self.assertFalse(self.created)


if __name__ == '__main__':
    unittest.main()
