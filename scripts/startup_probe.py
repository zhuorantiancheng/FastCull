"""Compare LR --open handoff / first-image latency on private synthetic catalogs.
Only the cmd/exe side is measured: Lightroom SDK export time is excluded.
"""
import argparse,ctypes,json,os,re,subprocess,time,shutil
from pathlib import Path
p=argparse.ArgumentParser()
p.add_argument('--exe',type=Path,default=Path('lrplugins/FastCull.lrplugin/fastcull.exe'))
p.add_argument('--scratch',type=Path,required=True)
p.add_argument('--count',type=int,default=340)
p.add_argument('--sample',type=Path,required=True)
p.add_argument('--max-handoff-ms',type=float)
a=p.parse_args();a.scratch.mkdir(parents=True,exist_ok=False)
photos=a.scratch/'photos';photos.mkdir();logs=a.scratch/'logs';logs.mkdir()
source=a.sample.with_suffix('.xmp')
text=source.read_bytes() if source.exists() else b'<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="1"/>'
paths=[]
seed=a.scratch/'source.ARW';shutil.copyfile(a.sample,seed)
for i in range(a.count):
    raw=photos/f'S{i:04}.ARW';os.link(seed,raw);raw.with_suffix('.xmp').write_bytes(text);paths.append(raw.resolve())
state=a.scratch/'state.txt';state.write_text('\n'.join(f'{v}|2|Green|1' for v in paths)+'\n',encoding='utf-8')
env=os.environ.copy();env['FASTCULL_LOG_DIR']=str(logs.resolve());env['FASTCULL_SMOKE_DIR']=str((a.scratch/'screenshots').resolve());env['TEMP']=env['TMP']=str(a.scratch.resolve())
os.environ.update(env)
crt=ctypes.CDLL('ucrtbase');crt._wpopen.argtypes=[ctypes.c_wchar_p,ctypes.c_wchar_p];crt._wpopen.restype=ctypes.c_void_p
crt.fread.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_size_t,ctypes.c_void_p];crt.fread.restype=ctypes.c_size_t
crt._pclose.argtypes=[ctypes.c_void_p]
start=time.perf_counter();epoch=time.time()*1000
pipe=crt._wpopen(f'""{a.exe.resolve()}" --open "{state.resolve()}" 2>&1"','rb');assert pipe
chunks=[]
while True:
    buf=ctypes.create_string_buffer(4096);n=crt.fread(buf,1,4096,pipe)
    if not n:break
    chunks.append(buf.raw[:n])
code=crt._pclose(pipe);stdout=b''.join(chunks).decode('utf-8');assert code==0,stdout
handoff=(time.perf_counter()-start)*1000;print(stdout.strip())
deadline=time.monotonic()+65
result_path=a.scratch/'screenshots/result.txt'
gui=''
# Existence alone races File::create followed by write: a newly created report
# may still be empty. Wait for the complete final field before reading it.
while time.monotonic()<deadline:
    gui=result_path.read_text(encoding='utf-8') if result_path.exists() else ''
    if re.search(r'elapsed_ms=\d+$',gui.strip()):break
    time.sleep(.05)
report={'count':a.count,'handoff_ms':round(handoff,1)}
for f in logs.glob('*.log'):
    for line in f.read_text(encoding='utf-8').splitlines():
        if 'first loupe ' in line: report['first_image_ms']=round(int(line.split()[0])-epoch,1)
        if 'first UI frame ' in line: report['first_ui_frame_ms']=round(int(line.split()[0])-epoch,1)
report['gui']=gui
print(json.dumps(report,indent=2))
(a.scratch/'report.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
assert 'first_image_ms' in report
assert 'first_draw=true' in gui and 'error=None' in gui,gui
if a.max_handoff_ms is not None:assert handoff<a.max_handoff_ms, f'LR pipe held open {handoff:.1f}ms > {a.max_handoff_ms}ms'
# Startup must complete for ALL rows, while the final user edit survives.
for path in paths[:-1]:
    t=path.with_suffix('.xmp').read_text(encoding='utf-8')
    assert 'xmp:Rating="2"' in t and 'xmp:Label="Green"' in t and 'sr:Flag="pick"' in t
last=paths[-1].with_suffix('.xmp').read_text(encoding='utf-8')
assert 'xmp:Rating="4"' in last and 'xmp:Label="Purple"' in last and 'sr:Flag="reject"' in last
