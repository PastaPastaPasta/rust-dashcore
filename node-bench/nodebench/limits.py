"""Identical resource caps for paired runs, via cgroup v2 (needs passwordless sudo).

    --limits cpu=12,mem=24G,io=100

cpu: CPU quota in cores (cpu.max), mem: memory.max (swap disabled for the
group), io: io.weight (1-10000; only effective with the BFQ scheduler or io.cost, which
`io_weight_effective()` reports). A cgroup is created per node process under
/sys/fs/cgroup/nodebench.slice/ *before* the process starts; the child moves
itself in (join_prefix) before it execs the wrapper or the node, so everything
they fork is born inside the caps. Caps apply on top of the CPU slice pinning
(numactl/taskset), so a pair can get equal cpusets *and* equal quotas.
Removal kills anything still inside (cgroup.kill) and failures are kept in
LEFTOVERS, which `run` reports in the result.
"""

import itertools
import os
import subprocess
import time

ROOT = "/sys/fs/cgroup/nodebench.slice"
CONTROLLERS = "+cpu +memory +io"

SLICE_LIMITS = {}  # cpu slice spec -> parsed limits, set by the CLI for the node slice
LEFTOVERS = []  # cgroups that could not be removed (reported by `run`)
_SEQ = itertools.count()


def parse(spec):
    if not spec:
        return None
    out = {}
    for part in spec.split(","):
        k, _, v = part.partition("=")
        k = k.strip()
        if k not in ("cpu", "mem", "io"):
            raise SystemExit(f"--limits: unknown key {k!r} (cpu, mem, io)")
        out[k] = v.strip()
    return out


def _sudo_write(path, value):
    subprocess.run(["sudo", "-n", "tee", path], input=f"{value}\n", text=True, check=True,
                   stdout=subprocess.DEVNULL)


def _ensure_root():
    if not os.path.isdir(ROOT):
        subprocess.run(["sudo", "-n", "mkdir", "-p", ROOT], check=True)
    enabled = set(open(f"{ROOT}/cgroup.subtree_control").read().split())
    if not {"cpu", "memory", "io"} <= enabled:
        _sudo_write(f"{ROOT}/cgroup.subtree_control", CONTROLLERS)


class Cgroup:
    def __init__(self, name, limits):
        self.path = f"{ROOT}/{name}"
        self.limits = limits

    def create(self):
        try:
            _ensure_root()
            subprocess.run(["sudo", "-n", "mkdir", self.path], check=True)
            lim = self.limits
            if "cpu" in lim:
                _sudo_write(f"{self.path}/cpu.max", f"{int(float(lim['cpu']) * 100000)} 100000")
            if "mem" in lim:
                _sudo_write(f"{self.path}/memory.max", lim["mem"])
                _sudo_write(f"{self.path}/memory.swap.max", "0")
            if "io" in lim:
                _sudo_write(f"{self.path}/io.weight", f"default {lim['io']}")
        except BaseException:
            self.remove()
            raise
        return self

    def join_prefix(self):
        """argv prefix: the child writes its own pid into cgroup.procs, then execs the rest of the command."""
        return ["sh", "-c", f'echo $$ | sudo -n tee {self.path}/cgroup.procs >/dev/null && exec "$@"', "sh"]

    def procs(self):
        try:
            with open(f"{self.path}/cgroup.procs") as f:
                return [int(x) for x in f.read().split()]
        except OSError:
            return []

    def stats(self):
        out = {}
        for f in ("memory.peak", "cpu.stat"):
            try:
                with open(f"{self.path}/{f}") as fh:
                    out[f] = fh.read().strip()
            except OSError:
                pass
        return out

    def remove(self):
        """Kill anything still inside, wait until empty, rmdir. Returns True on success (else in LEFTOVERS)."""
        if not os.path.isdir(self.path):
            return True
        if self.procs():
            subprocess.run(["sudo", "-n", "tee", f"{self.path}/cgroup.kill"], input="1\n", text=True, check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 30
            while self.procs() and time.monotonic() < deadline:
                time.sleep(0.05)
        r = subprocess.run(["sudo", "-n", "rmdir", self.path], check=False, capture_output=True, text=True)
        if r.returncode and os.path.isdir(self.path):
            LEFTOVERS.append(f"{self.path}: {r.stderr.strip() or 'rmdir failed'} (procs {self.procs()})")
            return False
        return True


def create_for(cpu_slice):
    """A fresh capped cgroup for one process started on `cpu_slice`, or None when the slice has no caps."""
    lim = SLICE_LIMITS.get(cpu_slice)
    if not lim:
        return None
    return Cgroup(f"{os.getpid()}-{next(_SEQ)}", lim).create()


def io_weight_effective(device_glob="/sys/block/*/queue/scheduler"):
    """True when some block device uses BFQ (io.weight is ignored by none/mq-deadline)."""
    import glob

    return any("[bfq]" in open(p).read() for p in glob.glob(device_glob))
