"""Exercise the actual Mac RAW window with synthetic pixels and private metadata.

Runs only on a Mac desktop/CI runner. No Adobe software or user photos are used.
The app's existing opt-in smoke path edits, fits, loads full pixels and saves.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import xml.etree.ElementTree as ET

from macos_native_smoke import write_linear_dng


def main():
    if sys.platform != 'darwin':
        raise SystemExit('This test requires a macOS graphics session.')
    executable = Path(sys.argv[1]).resolve()
    directory = Path(sys.argv[2]).resolve()
    directory.mkdir(parents=True, exist_ok=False)
    source = directory / "合成 DNG ' $ sample.dng"
    write_linear_dng(source)
    original = source.read_bytes()
    request = directory / 'request.txt'
    receipt = directory / 'saved.txt'
    request.write_text('FASTCULL_RAW_WINDOW_V1\n' +
                       str(source).encode('utf-8').hex() + '\n' +
                       str(receipt).encode('utf-8').hex() + '\n-\n-\n', encoding='utf-8')
    env = os.environ.copy()
    for variable, leaf in [('FASTCULL_RAW_EDIT_SMOKE_DIR', 'screenshots'),
                           ('FASTCULL_RAW_CACHE_DIR', 'cache'),
                           ('FASTCULL_LOG_DIR', 'logs')]:
        path = directory / leaf
        path.mkdir()
        env[variable] = str(path)
    subprocess.run([str(executable), '--raw-editor', str(request)],
                   env=env, check=True, timeout=120)
    screenshots = directory / 'screenshots'
    error = screenshots / 'error.txt'
    if error.exists():
        raise AssertionError(error.read_text(encoding='utf-8'))
    result = (screenshots / 'result.txt').read_text(encoding='utf-8')
    assert 'saved=true' in result, result
    assert len(list(screenshots.glob('*.png'))) == 4
    attrs = {key: value for node in ET.fromstring(source.with_suffix('.xmp').read_text(encoding='utf-8')).iter()
             for key, value in node.attrib.items()}
    prefix = '{http://ns.adobe.com/camera-raw-settings/1.0/}'
    expected = dict(Exposure2012='0.500000', WhiteBalance='Custom', Temperature='4850', Tint='17',
                    Contrast2012='20', Highlights2012='-35', Shadows2012='25', Whites2012='15',
                    Blacks2012='-10', Vibrance='20', Saturation='10')
    for key, value in expected.items():
        assert attrs[prefix + key] == value, (key, attrs.get(prefix + key))
    assert source.read_bytes()[8:len(original)] == original[8:], 'DNG payload bytes changed'
    assert len(list(directory.glob('saved-*.txt'))) == 1, 'Typed save receipt missing'
    report = dict(passed=True, synthetic_dng=True, gpu_screenshots=4,
                  raw_payload_preserved=True, typed_receipt=True,
                  xmp_fields=len(expected), adobe_required=False)
    (directory / 'report.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print('macOS RAW GUI:', json.dumps(report))


if __name__ == '__main__':
    main()
