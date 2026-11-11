"""The declared workload set (workloads.toml): one line per workload.

    name = { bench = "<package>/<bench target>", id = "<criterion bench id>" [, iters = N] [, syscalls = false] }
    name = { node = "<node-bench workload>", bin = "<rust node binary>" [, args = [...]] }  (no strace pass, D39)

A `bench` workload is one criterion benchmark, run under the criterion shim for a fixed number of
iterations (calibrated on the base revision unless `iters` pins it). A `node` workload is one
`node-bench/run workload`, with the whole node process under the counter wrapper; it stays pending until
the named binary target exists in the tree.
"""

import fnmatch
import hashlib
import json
import os
import re
import tomllib
from dataclasses import asdict, dataclass

DEFAULT_FILE = os.path.join(os.path.dirname(os.path.dirname(os.path.realpath(__file__))), "workloads.toml")

NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*")
BENCH_KEYS = {"bench", "id", "iters", "syscalls", "ungated"}
NODE_KEYS = {"node", "bin", "args", "syscalls", "ungated"}


@dataclass(frozen=True)
class Workload:
    name: str
    kind: str  # "bench" | "node"
    package: str = ""
    target: str = ""
    id: str = ""
    iters: int | None = None
    syscalls: bool = True
    node: str = ""
    bin: str = ""
    args: tuple[str, ...] = ()
    # Counters reported but not gated for this workload; each needs a documented reason in workloads.toml.
    ungated: tuple[str, ...] = ()

    def describe(self):
        if self.kind == "bench":
            return f"{self.package}/{self.target}: {self.id}"
        return f"node-bench {self.node} {' '.join(self.args)} ({self.bin})".replace("  ", " ")

    def key(self):
        """Stable identity of what is measured, for measurement caches and anchors (`ungated` changes how a
        measurement is judged, not what it is)."""
        d = {k: v for k, v in asdict(self).items() if k != "ungated"}
        return hashlib.sha256(json.dumps(d, sort_keys=True).encode()).hexdigest()[:16]


class SpecError(ValueError):
    pass


def ungated(d, origin, name):
    from .gate import BY_KEY

    names = d.get("ungated", [])
    if not isinstance(names, list) or any(n not in BY_KEY for n in names):
        raise SpecError(f"{origin}: {name}: ungated must list counters among {sorted(BY_KEY)}")
    return tuple(names)


def parse(text, origin="workloads.toml"):
    data = tomllib.loads(text)
    unknown = set(data) - {"workload"}
    if unknown:
        raise SpecError(f"{origin}: unknown top-level keys {sorted(unknown)}; workloads go under [workload]")
    out = []
    for name, d in data.get("workload", {}).items():
        if not NAME.fullmatch(name):
            raise SpecError(f"{origin}: workload name {name!r}: letters, digits, '-' and '_' only")
        if not isinstance(d, dict):
            raise SpecError(f"{origin}: {name} must be an inline table")
        if "bench" in d:
            extra = set(d) - BENCH_KEYS
            if extra or "id" not in d:
                raise SpecError(f"{origin}: {name}: bench workloads take {sorted(BENCH_KEYS)} and need id"
                                + (f" (unknown: {sorted(extra)})" if extra else ""))
            package, sep, target = d["bench"].partition("/")
            if not sep or not package or not target:
                raise SpecError(f"{origin}: {name}: bench must be \"<package>/<bench target>\"")
            iters = d.get("iters")
            if iters is not None and (not isinstance(iters, int) or iters < 1):
                raise SpecError(f"{origin}: {name}: iters must be a positive integer")
            out.append(Workload(name, "bench", package=package, target=target, id=d["id"], iters=iters,
                                syscalls=bool(d.get("syscalls", True)), ungated=ungated(d, origin, name)))
        elif "node" in d:
            extra = set(d) - NODE_KEYS
            if extra or "bin" not in d:
                raise SpecError(f"{origin}: {name}: node workloads take {sorted(NODE_KEYS)} and need bin"
                                + (f" (unknown: {sorted(extra)})" if extra else ""))
            if d.get("syscalls", False):
                raise SpecError(f"{origin}: {name}: node rows have no strace pass (PF0-N D39)")
            out.append(Workload(name, "node", node=d["node"], bin=d["bin"], args=tuple(d.get("args", ())),
                                syscalls=False, ungated=ungated(d, origin, name)))
        else:
            raise SpecError(f"{origin}: {name}: needs either bench = ... or node = ...")
    return out


def load(path=None):
    path = path or DEFAULT_FILE
    with open(path) as f:
        return parse(f.read(), origin=path)


def select(workloads, only=None, kinds=None):
    """Workloads whose name matches any `only` glob (all if none), restricted to `kinds`."""
    return [w for w in workloads
            if (not only or any(fnmatch.fnmatchcase(w.name, g) for g in only)) and (not kinds or w.kind in kinds)]
