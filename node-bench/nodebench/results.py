"""Result files: one JSON document per (scenario, impl) run plus a Markdown summary."""

import glob
import json
import math
import os
import time
from dataclasses import asdict, is_dataclass

from . import stats

SCHEMA = "node-bench/1"


class Result:
    def __init__(self, scenario, impl, env, params, suffix=""):
        self.started = time.time()
        self.out_dir = None
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime(self.started))
        self.doc = {
            "schema": SCHEMA,
            "run_id": f"{stamp}-{scenario}-{impl['impl']}{suffix}",
            "scenario": scenario,
            "impl": impl,
            "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(self.started)),
            "finished_at": None,
            "status": "running",
            "env": env,
            "params": {k: (asdict(v) if is_dataclass(v) else v) for k, v in params.items()},
            "metrics": {},
            "artifacts": {},
            "notes": [],
        }

    @property
    def run_id(self):
        return self.doc["run_id"]

    def metric(self, name, samples, unit, *, statistic="median", better="lower", definition="",
               gate=True, target_2x=True, extra=None, max_samples=None):
        """Record a metric.

        statistic: which summary of `samples` is the headline `value` compared
                   across implementations.
        gate:      participates in the "never slower" merge gate.
        target_2x: the 2x-dashd target applies (node's own work is the bottleneck).
        max_samples: keep at most this many raw samples (an even stride) in the
                   file; `value` and `stats` always use all of them.
        """
        samples = [s for s in samples if s is not None]
        bad = [s for s in samples if not isinstance(s, (int, float)) or not math.isfinite(s)]
        if bad:
            # NaN/inf must never reach a comparison: the metric is not recorded (a required one then blocks).
            raise ValueError(f"metric {name}: non-finite samples {bad[:3]}")
        value = stats.STATISTICS[statistic](samples) if samples else None
        described = stats.describe(samples)
        if max_samples and len(samples) > max_samples:
            step = len(samples) / max_samples
            samples = [samples[int(i * step)] for i in range(max_samples)]
        self.doc["metrics"][name] = {
            "value": value, "unit": unit, "statistic": statistic, "better": better,
            "gate": gate, "target_2x": target_2x, "definition": definition,
            "stats": described, "samples": samples, **(extra or {}),
        }
        return value

    def info(self, name, value, unit="", definition=""):
        """A recorded, non-gated observation (sizes, counts, hashes...), stored as given (non-finite -> None)."""
        if isinstance(value, float) and not math.isfinite(value):
            self.note(f"{name}: non-finite value {value} recorded as null")
            value = None
        self.doc["metrics"][name] = {"value": value, "unit": unit, "statistic": "value", "better": "none",
                                     "gate": False, "target_2x": False, "definition": definition,
                                     "stats": stats.describe([value] if isinstance(value, (int, float)) else []),
                                     "samples": [value] if isinstance(value, (int, float)) else []}

    def note(self, text):
        self.doc["notes"].append(text)

    def artifact(self, name, path):
        self.doc["artifacts"][name] = path

    def checkpoint(self):
        """Write what we have so far (status "partial"), so a late failure cannot lose a long run."""
        if self.out_dir:
            self._write(self.out_dir, "partial")

    def finish(self, out_dir, status="ok"):
        self.doc["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        return self._write(out_dir, status)

    def _write(self, out_dir, status):
        self.doc["duration_s"] = round(time.time() - self.started, 1)
        self.doc["status"] = status
        os.makedirs(out_dir, exist_ok=True)
        path = os.path.join(out_dir, f"{self.run_id}.json")
        write_json(path, self.doc)
        write_text(os.path.join(out_dir, f"{self.run_id}.md"), markdown(self.doc))
        return path


def _finite(obj):
    """Copy of obj with non-finite floats as None: result files are strict JSON (no NaN/Infinity)."""
    if isinstance(obj, float):
        return obj if math.isfinite(obj) else None
    if isinstance(obj, dict):
        return {k: _finite(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_finite(v) for v in obj]
    return obj


def write_text(path, text):
    """Atomic publish: readers see the old file or the new one, never a torn write."""
    tmp = f"{path}.tmp-{os.getpid()}"
    with open(tmp, "w") as f:
        f.write(text)
    os.replace(tmp, path)


def write_json(path, obj):
    write_text(path, json.dumps(_finite(obj), indent=1, allow_nan=False, default=str))


def iter_docs(path):
    """Result documents in a directory (or the single file `path`), skipping unreadable and partial ones."""
    files = [path] if os.path.isfile(path) else sorted(glob.glob(os.path.join(path, "*.json")))
    for p in files:
        try:
            doc = load(p)
        except (ValueError, OSError):
            continue
        if isinstance(doc, dict) and doc.get("status") != "partial":
            yield doc


def fmt(v, unit=""):
    if v is None:
        return "—"
    if isinstance(v, float):
        a = abs(v)
        s = f"{v:.0f}" if a >= 1000 else f"{v:.1f}" if a >= 100 else f"{v:.2f}" if a >= 1 else f"{v:.3g}"
    else:
        s = str(v)
    return f"{s} {unit}".strip()


def markdown(doc):
    env = doc["env"]
    lines = [
        f"# {doc['scenario']} — {doc['impl']['impl']} ({doc['impl'].get('version', '?')})",
        "",
        f"- run: `{doc['run_id']}` status **{doc['status']}**, {doc.get('duration_s', '?')} s",
        f"- host: {env.get('hostname')} · {env.get('cpu_model')} · slice `{env.get('cpu_slice')}` "
        f"({env.get('slice_cpus')} cpus) · kernel {env.get('kernel')}",
        f"- load at start: {' '.join(env.get('loadavg_at_start') or [])}",
        load_line(doc),
        "",
        "| metric | value | statistic | n | min | median | p95 | max | cv |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for name, m in doc["metrics"].items():
        s = m["stats"]
        cv = s.get("cv")
        lines.append(
            f"| {name} | {fmt(m['value'], m['unit'])} | {m['statistic']} | {s.get('n', 0)} | {fmt(s.get('min'))} "
            f"| {fmt(s.get('median'))} | {fmt(s.get('p95'))} | {fmt(s.get('max'))} "
            f"| {'—' if cv is None else f'{cv * 100:.1f}%'} |")
    if doc["notes"]:
        lines += ["", "Notes:", ""] + [f"- {n}" for n in doc["notes"]]
    return "\n".join(lines) + "\n"


def load_line(doc):
    """One-line noise verdict; runs without a load record predate noise accounting."""
    ld = doc.get("load")
    if not ld:
        return "- noise: **PROVISIONAL** (no load record: taken before noise accounting)"
    d = ld.get("during", {})
    tag = "OFFICIAL" if ld.get("official") else "PROVISIONAL"
    start = ld.get("start") or {}
    return (f"- noise: **{tag}**, host {ld.get('class')} — foreign load {d.get('foreign_cores_mean', 0):.1f} cores "
            f"mean / {d.get('foreign_cores_p90', 0):.1f} p90 / {d.get('foreign_cores_max', 0):.1f} max, load1 "
            f"{d.get('load1_mean', 0):.1f}, PSI p90 cpu {d.get('psi_cpu_p90')}% io {d.get('psi_io_p90')}%; "
            f"other dashd: {len(start.get('dashd_processes') or [])}, "
            f"rust builds: {(start.get('process_counts') or {}).get('rust_build')}")


def load(path):
    with open(path) as f:
        return json.load(f)
