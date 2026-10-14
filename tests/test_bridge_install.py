"""Registration tests use private Adobe directories, never live preferences."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
PWSH = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "PowerShell/7/pwsh.exe"


@unittest.skipUnless(os.name == "nt" and PWSH.exists(), "Windows PowerShell 7")
class BridgeInstallTests(unittest.TestCase):
    def test_installs_and_upgrades_only_own_files_with_backups(self):
        with tempfile.TemporaryDirectory(prefix="bridge-install-") as directory:
            adobe = Path(directory) / "Adobe"
            startup = adobe / "Bridge 2026/Startup Scripts"
            startup.mkdir(parents=True)
            other = startup / "Other.jsx"
            other.write_text("other plugin", encoding="utf-8")
            command = [
                str(PWSH), "-NoProfile", "-File", str(ROOT / "scripts/install-bridge.ps1"),
                "-AdobeUserDirectory", str(adobe), "-SkipShellRegistration",
            ]
            subprocess.run(command, check=True, capture_output=True)
            config = (startup / "FastCull-path.txt").read_text(encoding="utf-8-sig").strip()
            self.assertEqual(Path(config), ROOT / "lrplugins/FastCull.lrplugin/fastcull.exe")
            self.assertEqual((startup / "FastCull.jsx").read_text(encoding="utf-8-sig"),
                             (ROOT / "bridge/FastCull.jsx").read_text(encoding="utf-8"))
            subprocess.run(command, check=True, capture_output=True)
            self.assertEqual(other.read_text(encoding="utf-8"), "other plugin")
            self.assertEqual(len(list(startup.glob("FastCull*.bak"))), 2)

    def test_rejects_directory_traversal_without_creating_scripts(self):
        with tempfile.TemporaryDirectory(prefix="bridge-install-invalid-") as directory:
            adobe = Path(directory) / "Adobe"
            adobe.mkdir()
            result = subprocess.run([
                str(PWSH), "-NoProfile", "-File", str(ROOT / "scripts/install-bridge.ps1"),
                "-AdobeUserDirectory", str(adobe), "-BridgeVersions", "../escape",
            ], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(adobe.iterdir()), [])
