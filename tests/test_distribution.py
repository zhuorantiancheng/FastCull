"""Exercise Windows 5.1 registration in isolated profiles; cleanup is previewed.

No live Adobe profile or registry is used. Fixtures remain in project scratch.
"""
from datetime import datetime
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
POWERSHELL = Path(os.environ.get('SystemRoot', 'C:/Windows'))/'System32/WindowsPowerShell/v1.0/powershell.exe'
spec = importlib.util.spec_from_file_location('release', ROOT/'scripts/release.py')
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


@unittest.skipUnless(os.name == 'nt' and POWERSHELL.is_file(), 'Windows PowerShell 5.1 required')
class DistributionTests(unittest.TestCase):
    def setUp(self):
        self.root = ROOT/'.agent-temp'/f'{datetime.now():%Y-%m-%d}-distribution-tests'/uuid.uuid4().hex
        self.payload = self.root/'软件 空格 & test'
        self.adobe = self.root/'Adobe'
        self.state = self.root/'state'
        (self.payload/'scripts').mkdir(parents=True)
        (self.payload/'bridge').mkdir()
        (self.payload/'FastCull.lrplugin').mkdir()
        self.script = self.payload/'scripts/register-integrations.ps1'
        release.copy(ROOT/'scripts/register-integrations.ps1', self.script)
        (self.payload/'FastCull.exe').write_bytes(b'MZ fixture')
        (self.payload/'bridge/FastCull.jsx').write_text('// new script', encoding='utf-8')
        for name in release.LUA + release.BINARIES + release.PLUGIN_SUPPORT:
            (self.payload/'FastCull.lrplugin'/name).write_text('fixture '+name)
        for version in ['2025','2026']:
            (self.adobe/f'Bridge {version}/Startup Scripts').mkdir(parents=True)

    def invoke(self, *args, success=True):
        result = subprocess.run([str(POWERSHELL), '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
                                 '-File', str(self.script), '-AdobeDirectory', str(self.adobe),
                                 '-StateDirectory', str(self.state), '-SkipRegistry', *args], capture_output=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def receipt(self):
        return json.loads((self.state/'integrations.json').read_text(encoding='utf-8'))

    def test_install_upgrade_backups_and_uninstall_preview(self):
        startup = self.adobe/'Bridge 2026/Startup Scripts'
        (startup/'FastCull.jsx').write_text('// old script')
        (startup/'Unrelated.jsx').write_text('// keep')
        self.invoke('-Bridge', '-Lightroom')
        receipt = self.receipt()
        self.assertEqual(len(receipt['files']), len(release.LUA)+len(release.BINARIES)+len(release.PLUGIN_SUPPORT)+4)
        self.assertEqual((startup/'FastCull-path.txt').read_text(encoding='utf-8-sig').strip(), str(self.payload/'FastCull.exe'))
        entries = [e for e in receipt['files'] if e['backup']]
        self.assertEqual(len(entries), 1)
        self.assertEqual(Path(entries[0]['backup']).read_text(), '// old script')
        self.invoke('-Bridge', '-Lightroom')
        self.assertEqual(self.receipt(), receipt)
        self.assertEqual(len(list((self.state/'backups').iterdir())), 1)
        self.assertEqual((startup/'Unrelated.jsx').read_text(), '// keep')
        before = {e['path']: release.sha(Path(e['path'])) for e in receipt['files']}
        self.invoke('-Mode', 'Uninstall', '-Preview')
        self.assertEqual(before, {p: release.sha(Path(p)) for p in before})
        self.assertEqual(self.receipt(), receipt)

    def test_preview_has_no_writes(self):
        self.invoke('-Bridge', '-Lightroom', '-Preview')
        self.assertFalse(self.state.exists())
        self.assertFalse((self.adobe/'Lightroom').exists())

    def test_modified_script_preserved_by_cleanup_preview(self):
        self.invoke('-Bridge')
        path = self.adobe/'Bridge 2026/Startup Scripts/FastCull.jsx'
        path.write_text('// edited by user')
        self.invoke('-Mode', 'Uninstall', '-Preview')
        self.assertEqual(path.read_text(), '// edited by user')

    def test_tampered_receipt_rejected(self):
        self.invoke('-Bridge')
        receipt = self.receipt()
        receipt['files'][0]['path'] = str(self.root/'unrelated.txt')
        (self.state/'integrations.json').write_text(json.dumps(receipt), encoding='utf-8')
        self.invoke('-Mode', 'Uninstall', '-Preview', success=False)

    def test_missing_payload_fails_before_registration(self):
        source = self.payload/'FastCull.lrplugin/Info.lua'
        source.rename(source.with_suffix('.held'))
        self.invoke('-Bridge', '-Lightroom', success=False)
        self.assertFalse(self.state.exists())
        self.assertFalse((self.adobe/'Lightroom').exists())

    def test_changed_install_root_rejected_while_registered(self):
        self.invoke('-Bridge')
        self.invoke('-InstallRoot', str(self.root/'another'), '-Bridge', success=False)

    def test_standalone_uninstall_does_not_claim_another_installation(self):
        self.invoke('-Bridge')
        receipt = self.receipt()
        self.invoke('-InstallRoot', str(self.root/'another'), '-Mode', 'Uninstall', '-Preview')
        self.assertEqual(self.receipt(), receipt)

    def test_empty_uninstall_receipt_allows_reregister_after_move(self):
        self.state.mkdir()
        (self.state/'integrations.json').write_text(json.dumps({'schema':1,'root':'D:/old','adobe':str(self.adobe),'files':[],'registry':[]}), encoding='utf-8')
        self.invoke('-Bridge')
        self.assertEqual(self.receipt()['root'], str(self.payload))


class SourceManifestTests(unittest.TestCase):
    def test_source_manifest_is_complete_and_excludes_private_outputs(self):
        paths = release.source_paths()
        self.assertGreater(len(paths), 60)
        for path in paths:
            self.assertTrue(path.is_file(), str(path))
            relative = path.relative_to(ROOT)
            self.assertFalse(any(p.startswith('.agent') or p in {'.Codex','dist','__pycache__'} for p in relative.parts))
            self.assertNotIn(path.suffix.lower(), {'.exe','.dll','.arw','.nef','.cr2','.log'})


if __name__ == '__main__':
    unittest.main()
