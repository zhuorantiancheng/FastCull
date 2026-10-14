"""Exercise Windows LR-style launch with unusual stdio and extra pipe handles.

Each run owns a new scratch directory and synthetic ARWs; no catalog is touched.
The invalid-stdin case supplies a pseudo handle that cannot have inheritance flags
changed. On the tested Windows host this reproduces LR's Windows error 5.
"""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import re
import struct
import time


class Security(c.Structure):
    _fields_ = [('length', w.DWORD), ('descriptor', c.c_void_p), ('inherit', w.BOOL)]


class Startup(c.Structure):
    _fields_ = [
        ('cb', w.DWORD), ('reserved', w.LPWSTR), ('desktop', w.LPWSTR), ('title', w.LPWSTR),
        ('x', w.DWORD), ('y', w.DWORD), ('cx', w.DWORD), ('cy', w.DWORD),
        ('chars_x', w.DWORD), ('chars_y', w.DWORD), ('fill', w.DWORD), ('flags', w.DWORD),
        ('show', w.WORD), ('reserved_size', w.WORD), ('reserved_bytes', c.c_void_p),
        ('stdin', w.HANDLE), ('stdout', w.HANDLE), ('stderr', w.HANDLE),
    ]


class Process(c.Structure):
    _fields_ = [('process', w.HANDLE), ('thread', w.HANDLE), ('pid', w.DWORD), ('tid', w.DWORD)]


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', type=Path, required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--mode', choices=['normal', 'invalid-stdin', 'extra-pipe'], required=True)
    parser.add_argument('--max-handoff-ms', type=float, default=1000)
    args = parser.parse_args()
    scratch = args.scratch.resolve()
    scratch.mkdir(parents=True, exist_ok=False)
    # Chinese/spaces test the CreateProcess command line and environment together.
    photos = scratch / '测试 图片'
    photos.mkdir()
    jpeg = Path('tests/fixtures/tiny.jpg').read_bytes()
    header = b'II' + struct.pack('<HIH', 42, 8, 2)
    header += struct.pack('<HHII', 0x201, 4, 1, 64)
    header += struct.pack('<HHII', 0x202, 4, 1, len(jpeg)) + bytes(4)
    paths = []
    for i in range(2):
        path = photos / f'{i}.ARW'
        path.write_bytes(header.ljust(64, b'\0') + jpeg)
        paths.append(path)
    state = scratch / '输入 文件.txt'
    state.write_text(''.join(f'{path}|0||0\n' for path in paths), encoding='utf-8')
    manifest = scratch / '选片 会话.txt'
    os.environ['FASTCULL_SMOKE_DIR'] = str(scratch / 'gui')
    os.environ['FASTCULL_LOG_DIR'] = str(scratch / 'logs')
    os.environ['TEMP'] = os.environ['TMP'] = str(scratch)

    kernel = c.WinDLL('kernel32', use_last_error=True)
    kernel.CreatePipe.argtypes = [c.POINTER(w.HANDLE), c.POINTER(w.HANDLE), c.POINTER(Security), w.DWORD]
    kernel.SetHandleInformation.argtypes = [w.HANDLE, w.DWORD, w.DWORD]
    kernel.GetCurrentProcess.restype = w.HANDLE
    kernel.DuplicateHandle.argtypes = [w.HANDLE, w.HANDLE, w.HANDLE, c.POINTER(w.HANDLE), w.DWORD, w.BOOL, w.DWORD]
    kernel.CreateProcessW.argtypes = [w.LPCWSTR, w.LPWSTR, c.c_void_p, c.c_void_p, w.BOOL, w.DWORD, c.c_void_p, w.LPCWSTR, c.POINTER(Startup), c.POINTER(Process)]
    kernel.ReadFile.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]
    kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
    kernel.GetExitCodeProcess.argtypes = [w.HANDLE, c.POINTER(w.DWORD)]
    kernel.CloseHandle.argtypes = [w.HANDLE]
    security = Security(c.sizeof(Security), None, True)
    reader, writer, extra = w.HANDLE(), w.HANDLE(), w.HANDLE()
    assert kernel.CreatePipe(c.byref(reader), c.byref(writer), c.byref(security), 0), c.WinError(c.get_last_error())
    assert kernel.SetHandleInformation(reader, 1, 0)
    if args.mode == 'extra-pipe':
        current = kernel.GetCurrentProcess()
        assert kernel.DuplicateHandle(current, writer, current, c.byref(extra), 0, True, 2)
    startup = Startup()
    startup.cb = c.sizeof(startup)
    startup.flags = 0x100  # STARTF_USESTDHANDLES
    startup.stdin = c.c_void_p(-2).value if args.mode == 'invalid-stdin' else None
    startup.stdout = startup.stderr = writer.value
    process = Process()
    command = c.create_unicode_buffer(
        f'"{args.exe.resolve()}" --open "{state}" --session "{manifest}" --session-id launch-probe'
    )
    start = time.perf_counter()
    ok = kernel.CreateProcessW(str(args.exe.resolve()), command, None, None, True, 0x08000000, None, None, c.byref(startup), c.byref(process))
    error = c.get_last_error()
    kernel.CloseHandle(writer)
    if extra.value:
        kernel.CloseHandle(extra)
    assert ok, c.WinError(error)
    kernel.CloseHandle(process.thread)
    chunks = []
    try:
        while True:
            buffer = c.create_string_buffer(4096)
            count = w.DWORD()
            if not kernel.ReadFile(reader, buffer, len(buffer), c.byref(count), None):
                assert c.get_last_error() == 109, c.WinError(c.get_last_error())  # EOF
                break
            chunks.append(buffer.raw[:count.value])
        handoff_ms = (time.perf_counter() - start) * 1000
        assert kernel.WaitForSingleObject(process.process, 2000) == 0
        code = w.DWORD()
        assert kernel.GetExitCodeProcess(process.process, c.byref(code))
    finally:
        kernel.CloseHandle(reader)
        kernel.CloseHandle(process.process)
    output = b''.join(chunks).decode('utf-8')
    report = {'mode': args.mode, 'exit_code': code.value, 'handoff_ms': round(handoff_ms, 1), 'stdout': output}
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
    (scratch / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    assert code.value == 0 and 'RESULT launched=1' in output, 'launcher rejected inherited stdio'
    # EOF must arrive while the GUI is still running, including extra hidden pipe writers.
    assert handoff_ms < args.max_handoff_ms, f'pipe held for {handoff_ms:.1f}ms'
    deadline = time.monotonic() + 20
    result = scratch / 'gui/result.txt'
    gui = ''
    while time.monotonic() < deadline:
        gui = result.read_text(encoding='utf-8') if result.exists() else ''
        if re.search(r'elapsed_ms=\d+$', gui.strip()):
            break
        time.sleep(.05)
    assert 'first_draw=true' in gui and 'error=None' in gui, gui
    # The screenshot report is saved before eframe closes and Drop marks the
    # manifest closed. Wait for that lifecycle boundary instead of racing it.
    while True:
        header = manifest.read_text(encoding='utf-8').splitlines()[0].split('\t')
        if header[2] == 'closed' or time.monotonic() >= deadline:
            break
        time.sleep(.05)
    assert header[:3] == ['FASTCULL_SESSION_V1', 'launch-probe', 'closed'] and int(header[3]) > 0, header
    report['gui'] = gui
    report['session_closed'] = True
    (scratch / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    print('PASS: pipe returned promptly; detached GUI and session completed')


if __name__ == '__main__':
    run()
