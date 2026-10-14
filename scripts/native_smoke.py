"""Native-window QA on private fixture links, including the real cmd.exe pipe/detach chain.
The built-in smoke controller renders actual windows and captures GPU screenshots.
It exercises app actions; Lightroom itself is NOT simulated as a native app here.
"""
import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import subprocess
import shutil
import time

p = argparse.ArgumentParser()
p.add_argument("samples", type=Path)
p.add_argument("--exe", type=Path, default=Path("lrplugins/FastCull.lrplugin/fastcull.exe"))
p.add_argument("--scratch", type=Path, required=True)
p.add_argument("--count", type=int, default=4)
p.add_argument("--pipe", action="store_true")
p.add_argument("--session", action="store_true")
p.add_argument("--broken-sidecar", action="store_true")
p.add_argument("--features", action="store_true")
p.add_argument("--folders", action="store_true", help="Folder tree and non-destructive organization plan QA")
p.add_argument("--bridge", action="store_true", help="Bridge path-only launch through the native detached helper")
p.add_argument("--moves", action="store_true", help="Bridge continuous/batch real file move QA on private samples")
p.add_argument("--cross-target", type=Path, help="Private destination on another volume for --moves")
p.add_argument("--size", help="Private smoke viewport, e.g. 800x600")
args = p.parse_args()
assert not args.folders or (args.features and args.session and args.pipe), '--folders requires --features --session --pipe'
assert not args.bridge or (args.pipe and not args.session and not args.folders), '--bridge requires --pipe without --session/--folders'
assert not args.moves or (args.bridge and args.features and args.count >= 4), '--moves requires --bridge --features and at least four photos'
assert not args.cross_target or args.moves, '--cross-target requires --moves'
args.scratch.mkdir(parents=True, exist_ok=False)
photos = args.scratch / "photos"
photos.mkdir()
sources = sorted(args.samples.glob("*.ARW"))
assert sources and args.count > 0
seeds=args.scratch/'seeds';seeds.mkdir()
private=[]
for i,source in enumerate(sources):
    seed=seeds/f'{i}.ARW';shutil.copyfile(source,seed);private.append(seed)
for i in range(args.count):
    target=photos
    if args.folders or args.moves:
        target=photos/('上午' if i%2==0 else '下午');target.mkdir(exist_ok=True)
    os.link(private[i % len(private)], target / f"S{i:04}.ARW")
photo_paths=sorted(photos.rglob('*.ARW'))
if args.broken_sidecar:
    assert args.count > 1
    (photos/'S0000.xmp').write_text('<broken>',encoding='utf-8')
env = os.environ.copy()
if args.moves: env['FASTCULL_BRIDGE_MOVE_SMOKE']='1'
else: env.pop('FASTCULL_BRIDGE_MOVE_SMOKE',None)
if args.cross_target: env['FASTCULL_MOVE_SMOKE_CROSS']=str(args.cross_target.resolve())
else: env.pop('FASTCULL_MOVE_SMOKE_CROSS',None)
if args.folders: env['FASTCULL_FOLDER_SMOKE']='1'
else: env.pop('FASTCULL_FOLDER_SMOKE',None)
env["FASTCULL_LOG_DIR"] = str((args.scratch / "logs").resolve())
env["FASTCULL_SMOKE_DIR"] = str((args.scratch / "screenshots").resolve())
if args.features: env['FASTCULL_FEATURE_SMOKE']='1'
else: env.pop('FASTCULL_FEATURE_SMOKE',None)
if args.size: env['FASTCULL_SMOKE_SIZE']=args.size
else: env.pop('FASTCULL_SMOKE_SIZE',None)
env["TEMP"] = env["TMP"] = str(args.scratch.resolve())
if args.count > 50:
    env["FASTCULL_STRESS"] = "1"
else:
    env.pop("FASTCULL_STRESS", None)
exe = args.exe.resolve()
started = time.perf_counter()
peak = 0
if args.pipe:
    state = args.scratch / "中文 状态.txt"
    state.write_text(("FASTCULL_BRIDGE_V1\n" + "\n".join(str(v.resolve()) for v in photo_paths) + "\n")
                     if args.bridge else "\n".join(f"{v.resolve()}|0||0" for v in photo_paths) + "\n", encoding="utf-8")
    # Same nested quoting as common.lua's io.popen on Windows.
    command = f'""{exe}" {"--bridge-open" if args.bridge else "--open"} "{state.resolve()}" 2>&1"'
    if args.session:
        if args.features:
            (args.scratch/'session.exposure').write_text(''.join(str(v.resolve()).encode().hex()+'\t100000\n' for v in photo_paths)+'END\n',encoding='utf-8')
        command = f'""{exe}" --open "{state.resolve()}" --session "{(args.scratch / "session.txt").resolve()}" --session-id native-test-session 2>&1"'
    # _wpopen is the Windows C-runtime path used by Lua io.popen. Passing cmd's
    # command via Python argv quoting would add another, incompatible quote layer.
    os.environ.update(env)
    crt = ctypes.CDLL("ucrtbase")
    crt._wpopen.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p]
    crt._wpopen.restype = ctypes.c_void_p
    crt.fread.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_size_t, ctypes.c_void_p]
    crt.fread.restype = ctypes.c_size_t
    crt._pclose.argtypes = [ctypes.c_void_p]
    pipe = crt._wpopen(command, "rb")
    assert pipe, "C runtime popen failed"
    chunks = []
    while True:
        buf = ctypes.create_string_buffer(4096)
        count = crt.fread(buf, 1, 4096, pipe)
        if not count:
            break
        chunks.append(buf.raw[:count])
    returncode = crt._pclose(pipe)
    stdout = b"".join(chunks).decode("utf-8", errors="replace")
    assert returncode == 0, stdout
    assert "RESULT launched=1" in stdout, stdout
    helper_seconds = time.perf_counter() - started
    if args.bridge:
        assert not state.exists(), 'successful Bridge helper must consume its original request'
    print(stdout.strip())
    # Deliberately replace parent input immediately. The child must use its OWN snapshot.
    state.write_text("invalid-parent-state-now", encoding="utf-8")
    proc = None
else:
    proc = subprocess.Popen([str(exe), str(photos.resolve())], env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)

class Memory(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
        (k, ctypes.c_size_t) for k in ["PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
        "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage"]]
handle = None
if proc:
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Memory), wintypes.DWORD]
    handle = kernel.OpenProcess(0x0410, False, proc.pid)
while time.perf_counter() - started < 65:
    if handle:
        mem = Memory()
        mem.cb = ctypes.sizeof(mem)
        if psapi.GetProcessMemoryInfo(handle, ctypes.byref(mem), mem.cb):
            peak = max(peak, mem.PeakWorkingSetSize)
    if (args.scratch / "screenshots/result.txt").exists():
        break
    if proc and proc.poll() is not None:
        break
    time.sleep(0.1)
if handle:
    kernel.CloseHandle(handle)
if proc:
    proc.wait(timeout=10)
    assert proc.returncode == 0, proc.returncode
result = (args.scratch / "screenshots/result.txt").read_text(encoding="utf-8")
assert "first_draw=true" in result and "error=None" in result and "stress_remaining=0" in result, result
assert len(list((args.scratch / "screenshots").glob("*.png"))) == (21 if args.moves else (19 if args.folders else (13 if args.features else 5)))
if args.moves:
    from hashlib import sha256
    old_paths=photo_paths
    photo_paths=[Path(bytes.fromhex(line.split('\t')[1]).decode()) for line in (args.scratch/'screenshots/moved-paths.txt').read_text(encoding='utf-8').splitlines()]
    assert all(not p.exists() and not p.with_suffix('.xmp').exists() for p in old_paths)
    assert all(p.exists() and sha256(p.read_bytes()).digest()==sha256(private[int(p.stem[1:])%len(private)].read_bytes()).digest() for p in photo_paths)
    assert all('END\n' in p.read_text(encoding='utf-8') for p in (args.scratch/'logs/moves').glob('*.txt'))
if args.features:
    from verify_feature_images import verify
    visual_report=verify(args.scratch/'screenshots')
sidecar = photo_paths[-1].with_suffix('.xmp').read_text(encoding="utf-8")
assert 'xmp:Rating="4"' in sidecar and 'xmp:Label="Purple"' in sidecar and 'sr:Flag="reject"' in sidecar, sidecar
if args.broken_sidecar:
    assert (photos/'S0000.xmp').read_text(encoding='utf-8')=='<broken>'
    assert any('metadata ' in p.read_text(encoding='utf-8') for p in (args.scratch/'logs').glob('*.log'))
report = dict(count=args.count, pipe=args.pipe, bridge=args.bridge, elapsed_seconds=round(time.perf_counter()-started, 3),
              peak_working_set_mb=round(peak/1024**2, 1), app=result)
if args.pipe: report['helper_return_seconds'] = round(helper_seconds, 3)
if args.features: report['visual']=visual_report
if args.features:
    relative=json.loads((args.scratch/'screenshots/relative-link.json').read_text(encoding='utf-8'))
    assert relative['verified'] and relative['a']['pan'] != relative['b']['pan']
    for axis in range(2):
        assert abs(relative['a']['pan'][axis]-relative['b']['pan'][axis]-relative['offset'][axis]) < 1e-5
    report['relative_link']=relative
if args.session:
    assert args.pipe, '--session requires --pipe'
    manifest=args.scratch/'session.txt'
    fields=manifest.read_text(encoding='utf-8').splitlines()[0].split('\t')
    deadline=time.monotonic()+10
    while fields[2]!='closed' and time.monotonic()<deadline:
        time.sleep(.05);fields=manifest.read_text(encoding='utf-8').splitlines()[0].split('\t')
    assert fields[1]=='native-test-session' and fields[2]=='closed'
    output=subprocess.check_output([str(exe),'--sync-session',str(manifest.resolve())],env=env).decode('utf-8')
    rows=[line.split('\t') for line in output.splitlines() if line.startswith(('ROW\t','ROW_V3\t'))]
    assert len(rows)==1,output
    row=rows[0]
    assert bytes.fromhex(row[1]).decode('utf-8')==str(photo_paths[-1].resolve())
    assert row[2]=='4' and bytes.fromhex(row[3]).decode()=='Purple' and bytes.fromhex(row[4]).decode()=='reject'
    assert row[0]=='ROW_V3' and len(row)==13 and row[6] in {'1','2','3','4','5','6','7','8'},row
    report['session_revision']=int(fields[3]);report['session_export_rows']=len(rows)
    assert 'RESULT rows=0 error=0' in subprocess.check_output([str(exe),'--sync-session',str(manifest.resolve()),fields[3]],env=env).decode('utf-8')
    if args.features:
        assert abs(float(row[5])-1.1)<1e-6,row
        deadline=time.monotonic()+5
        while not manifest.with_suffix('.stars-request').exists() and time.monotonic()<deadline: time.sleep(.05)
        assert manifest.with_suffix('.stars-request').read_text().splitlines()==['native-test-session','all']
        stars=subprocess.check_output([str(exe),'--sync-stars',str(manifest.resolve())],env=env).decode('utf-8')
        assert 'ROW\t' in stars and '\t4\t-\t-' in stars,stars
        if args.folders:
            from hashlib import sha256
            output=subprocess.check_output([str(exe),'--read-folders',str(manifest.resolve())],env=env).decode('utf-8')
            moves=[line.split('\t') for line in output.splitlines() if line.startswith('MOVE\t')]
            assert len(moves)==2,output
            for _,source,target in moves:
                source=Path(bytes.fromhex(source).decode());target=Path(bytes.fromhex(target).decode())
                assert source.exists() and target.parent.is_dir() and not target.exists()
            assert all(sha256(p.read_bytes()).digest()==sha256(private[int(p.stem[1:])%len(private)].read_bytes()).digest() for p in photo_paths)
            report['folders']={'planned_photos':len(moves),'original_raw_files_unchanged':True,
                               'files_actually_moved':False,'new_directory_created':True,'exit_receipt_mode':'all'}
print(json.dumps(report, ensure_ascii=False, indent=2))
(args.scratch / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
