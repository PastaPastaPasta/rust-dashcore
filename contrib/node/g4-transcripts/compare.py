#!/usr/bin/env python3
"""Two-sample empirical KS statistic for G4's equally quantized timing samples.

Usage: compare.py CORE_JSON RUST_JSON
The usual continuous two-sample 5% threshold is reported as a reference; both
samples have one-second ties, so this is a conservative distribution check.
"""
import bisect
import hashlib
import json
import math
import pathlib
import sys

core_path, rust_path = map(pathlib.Path, sys.argv[1:3])
core = sorted(json.loads(core_path.read_text())["intervals_s"])
rust = sorted(json.loads(rust_path.read_text()))
n, m = len(core), len(rust)
d = max(abs(bisect.bisect_right(core, x) / n - bisect.bisect_right(rust, x) / m)
        for x in set(core + rust))
threshold = 1.36 * math.sqrt((n + m) / (n * m))
print(json.dumps({"core_n": n, "rust_n": m, "core_mean_s": sum(core) / n,
                  "rust_mean_s": sum(rust) / m, "ks_D": d,
                  "ks_5pct_threshold": threshold, "below_threshold": d < threshold,
                  "resolution_s": 1,
                  "core_sha256": hashlib.sha256(core_path.read_bytes()).hexdigest(),
                  "rust_sha256": hashlib.sha256(rust_path.read_bytes()).hexdigest()}, indent=2))
