"""Optional real-ARW golden comparison. Works only on COPIES in a fresh scratch folder."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument("samples", type=Path)
parser.add_argument("--exe", type=Path, default=Path("lrplugins/FastCull.lrplugin/fastcull.exe"))
parser.add_argument("--scratch", type=Path, required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
args.scratch.mkdir(parents=True, exist_ok=False)
os.environ["FASTCULL_LOG_DIR"] = str(args.scratch / "logs")
spec = importlib.util.spec_from_file_location("prototype", root / "arw_culler.py")
prototype = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prototype)
exe = str(args.exe.resolve())
report = []
for source in sorted(args.samples.glob("*.ARW")):
    raw = args.scratch / source.name
    shutil.copyfile(source, raw)
    info = prototype.ArwInfo(raw)
    out = subprocess.check_output([exe, "--benchmark", str(raw), "7"], encoding="utf-8")
    assert f"offset={info.jpeg_off} len={info.jpeg_len} orientation={info.orientation}" in out
    expected = prototype.decode_arw(info).size
    assert f"{expected[0]}x{expected[1]}" in out
    py_times = []
    for _ in range(7):
        start = time.perf_counter()
        image = prototype.decode_arw(prototype.ArwInfo(raw))
        py_times.append((time.perf_counter() - start) * 1000)
        image.close()
    rust_ms = float(re.search(r"median_ms=([\d.]+)", out).group(1))
    sensor = subprocess.check_output([exe, "--benchmark-sensor", str(raw), "7"], encoding="utf-8")
    sensor_ms = float(re.search(r"median_ms=([\d.]+)", sensor).group(1))
    report.append(dict(file=source.name, dimensions=expected, orientation=info.orientation,
                       python_median_ms=statistics.median(py_times), rust_median_ms=rust_ms,
                       rust_gui_decode_median_ms=sensor_ms))
    statefile = args.scratch / "state.txt"
    for rating, label, flag in [(5, "Purple", "1"), (0, "", "0"), (3, "Blue", "-1")]:
        statefile.write_text(f"{raw}|{rating}|{label}|{flag}\n", encoding="utf-8")
        result = subprocess.run([exe, "--apply-state", str(statefile)], capture_output=True, text=True, encoding="utf-8", timeout=10)
        assert result.returncode == 0, result.stdout
        values = prototype.XmpSidecar(raw).read()
        assert values == (rating, label or None, {"1": "pick", "-1": "reject", "0": None}[flag]), values
print(json.dumps(report, indent=2, ensure_ascii=False))
(args.scratch / "benchmark.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
