"""Read-only RAW benchmark runner; input photos and sidecars are never written.

Example: python scripts/evaluate_raw_pipeline.py --raw-bench PATH --preview-bench PATH
  --sample PHOTO --runs 15 --threads 4 --output .agent-temp/TASK/results.json
The first iteration of each configuration is discarded. OS cache is NOT flushed.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import statistics
import subprocess


def stats(values):
    values = sorted(values)
    return {"n": len(values), "median": statistics.median(values),
            "p95": values[math.ceil(.95 * len(values)) - 1], "min": values[0], "max": values[-1]}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--raw-bench", type=Path, required=True)
    p.add_argument("--preview-bench", type=Path)
    p.add_argument("--sample", type=Path, action="append", required=True)
    p.add_argument("--runs", type=int, default=15)
    p.add_argument("--threads", type=int, default=1)
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args()
    a.output.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, TEMP=str(a.output.parent.resolve()), TMP=str(a.output.parent.resolve()))
    report = {"method": "warm OS cache; first iteration excluded; sequential configurations; no Adobe edits", "samples": []}
    for path in a.sample:
        source_hash = hashlib.sha256(path.read_bytes()).hexdigest()
        sample = {"file": path.name, "size_bytes": path.stat().st_size, "sha256": source_hash, "raw": {}}
        if a.preview_bench:
            proc = subprocess.run([str(a.preview_bench.resolve()), str(path.resolve()), str(a.runs)],
                                  env=env, capture_output=True, text=True, check=True, timeout=300)
            sample["preview_stdout"] = proc.stdout
            groups = {}
            for level, run, value, w, h in re.findall(r"(Thumb|Fit\(\d+\)|Full) run=(\d+) ms=([\d.]+) size=(\d+)x(\d+)", proc.stdout):
                if int(run):
                    g = groups.setdefault(level, {"ms": [], "width": int(w), "height": int(h)})
                    g["ms"].append(float(value))
            sample["preview"] = {k: {**v, "stats_ms": stats(v["ms"])} for k, v in groups.items()}
        for mode in ["unpack", "half", "bilinear", "ahd"]:
            proc = subprocess.run([str(a.raw_bench.resolve()), str(path.resolve()), mode, str(a.runs), str(a.threads)],
                                  env=env, capture_output=True, text=True, check=True, timeout=600)
            result = json.loads(proc.stdout)
            measured = [v for v in result["runs"] if not v["warmup"]]
            result["stats_ms"] = {k: stats([v[k] for v in measured]) for k in ["open_ms", "unpack_ms", "process_ms", "output_ms", "total_ms"]}
            sample["raw"][mode] = result
            print(path.name, mode, result["stats_ms"]["total_ms"], flush=True)
            # Save progress after each completed mode, including original per-run timings.
            a.output.write_text(json.dumps({**report, "samples": report["samples"] + [sample]}, indent=2), encoding="utf-8")
        assert hashlib.sha256(path.read_bytes()).hexdigest() == source_hash, "source changed during benchmark"
        sample["source_unchanged"] = True
        report["samples"].append(sample)
        a.output.write_text(json.dumps(report, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
