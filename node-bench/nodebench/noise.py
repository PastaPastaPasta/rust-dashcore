"""Host-noise accounting: what else was running, how busy the box was, quiet windows.

agentbox-style hosts are shared: reference nodes, cargo builds and other test
suites run next to the benchmark. We cannot remove that noise, so every result
carries a measurement of it and is classified.

"Own" processes are everything carrying this run's NODE_BENCH_MARK in its
environment: the harness, its nodes, seeds, load-generator workers and bench
binaries (they all inherit it). A paired comparison gives both sides the same
mark, so in concurrent mode neither side counts the other as foreign. Foreign
CPU is measured host-wide and on the node's own CPU slice:

    host foreign  = busy cores on the host  - CPU of own processes
    slice foreign = busy cores on the slice - CPU of own processes pinned to it

Every sampling interval (2 s) records host and slice foreign load and the
CPU/IO/memory pressure (PSI some-avg10). Classes use robust statistics over the
intervals of the measured work only (from the scenario's "start" mark, so
preparation does not dilute a busy measurement), never whole-run means:

    quiet     p90 of slice foreign <= 10% of the slice and no interval > 30%,
              p90 of host foreign <= 25%
              (+ quiet-window admission only: p90 PSI cpu < 5%, io < 10%, memory < 5%)
    heavy     median slice foreign > 30% or median host foreign > 50%
              (+ admission only: p90 IO PSI >= 30%)
    moderate  otherwise

PSI is host-wide: during a measurement it includes the node under test and our
own load generators (an IBD with 16 script threads, 64 RPC clients), so it only
decides admission (wait_for_quiet, before any of our work runs). During the
measurement it is recorded, not classified; foreign disk contention during the
run is therefore visible only in the stored intervals (and is what the
reserved window rules out).

A result is "official" only when it was taken with --quiet-window, in a
manager-reserved window (--reservation, DEC-23(b)), and its measured work was
quiet; everything else is "provisional" and can never be merge evidence.
"""

import os
import threading
import time

from .proc import CLK_TCK, cpu_set, slice_cpus
from .stats import percentile

MARK_VAR = "NODE_BENCH_MARK"

QUIET_SLICE, QUIET_HOST, QUIET_PSI = 0.10, 0.25, 5.0
QUIET_IO_PSI, QUIET_MEM_PSI = 10.0, 5.0
HEAVY_SLICE, HEAVY_HOST, HEAVY_IO_PSI = 0.30, 0.50, 30.0
MAX_INTERVALS = 1500  # stored per result (evenly thinned); statistics use all of them

CATEGORIES = {
    "dashd": ("dashd",),
    "rust_build": ("cargo", "rustc", "clippy-driver", "rustdoc", "sccache"),
    "c_build": ("cc1", "cc1plus", "ld", "ld.lld", "collect2", "ccache", "make"),
    "test_binaries": ("dashd_sync", "dash-spv-bench", "test_dash", "bench_dash"),
    "containers": ("containerd-shim", "containerd-shim-runc-v2"),
}


def ensure_mark(value=None):
    """Set (or inherit) this run's mark; children inherit it through the environment."""
    if value:
        os.environ[MARK_VAR] = value
    os.environ.setdefault(MARK_VAR, f"run-{os.getpid()}-{int(time.time())}")
    return os.environ[MARK_VAR]


def _read(path, mode="r"):
    try:
        with open(path, mode) as f:
            return f.read()
    except OSError:
        return b"" if "b" in mode else ""


def own_pids():
    """Pids whose environment carries our mark (readable for our own uid only, which is all we need)."""
    needle = f"{MARK_VAR}={os.environ.get(MARK_VAR, '')}".encode() + b"\0"
    # /proc/<pid>/environ is the environment at exec: a mark set later by this
    # process itself only shows in its children, so add ourselves explicitly.
    out = {os.getpid()}
    for pid in os.listdir("/proc"):
        if pid.isdigit() and needle in _read(f"/proc/{pid}/environ", "rb"):
            out.add(int(pid))
    return out


def psi():
    """CPU/IO/memory pressure ("some" avg10/avg60, %) from /proc/pressure."""
    out = {}
    for res in ("cpu", "io", "memory"):
        for line in _read(f"/proc/pressure/{res}").splitlines():
            if line.startswith("some"):
                kv = dict(x.split("=") for x in line.split()[1:])
                out[res] = {"avg10": float(kv["avg10"]), "avg60": float(kv["avg60"])}
    return out


def cpu_ticks(cpus=None):
    """(total, idle) ticks over all CPUs or the given set. Guest time is already inside user/nice."""
    tot = idle = 0
    for line in _read("/proc/stat").splitlines():
        if not line.startswith("cpu") or line.startswith("cpu "):
            continue
        n = int(line[3:].split()[0])
        if cpus is not None and n not in cpus:
            continue
        f = [int(x) for x in line.split()[1:9]]  # user..steal; guest/guest_nice excluded (double count)
        tot += sum(f)
        idle += f[3] + f[4]
    return tot, idle


def proc_ticks(pid):
    try:
        stat = _read(f"/proc/{pid}/stat").rsplit(")", 1)[1].split()
        return int(stat[11]) + int(stat[12])
    except (IndexError, ValueError):
        return None


def pinned_within(pid, cpus):
    for line in _read(f"/proc/{pid}/status").splitlines():
        if line.startswith("Cpus_allowed_list:"):
            return cpu_set(line.split(":", 1)[1].strip()) <= cpus
    return False


def snapshot():
    """Who else is on the box right now (by category), load average and PSI."""
    own = own_pids()
    counts = {k: 0 for k in CATEGORIES}
    foreign_dashd, top = [], []
    for pid in os.listdir("/proc"):
        if not pid.isdigit() or int(pid) in own:
            continue
        comm = _read(f"/proc/{pid}/comm").strip()
        for cat, names in CATEGORIES.items():
            if comm in names or (cat == "test_binaries" and comm.split("-")[0] in names):
                counts[cat] += 1
        if comm == "dashd":
            args = _read(f"/proc/{pid}/cmdline").split("\0")
            dd = next((a.split("=", 1)[1] for a in args if a.startswith("-datadir=")), "?")
            foreign_dashd.append(f"{pid}:{dd}")
    for line in os.popen("ps -eo pid=,pcpu=,comm= --sort=-pcpu | head -15").read().splitlines():
        pid, pcpu, comm = (line.split(None, 2) + ["", ""])[:3]
        if pid.isdigit() and int(pid) not in own and float(pcpu or 0) >= 20:
            top.append(f"{comm}:{pcpu}%")
    load = _read("/proc/loadavg").split()
    return {"time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "loadavg": [float(x) for x in load[:3]],
            "ncpu": os.cpu_count(), "psi": psi(), "process_counts": counts, "dashd_processes": foreign_dashd,
            "top_cpu": top}


class LoadMonitor(threading.Thread):
    """Foreign CPU (cores), host-wide and on `node_slice`, every `interval` s for a run's duration."""

    def __init__(self, node_slice=None, interval=2.0):
        super().__init__(daemon=True)
        self.interval = interval
        cpus = slice_cpus(node_slice)
        self.slice = cpu_set(cpus) if cpus else None
        self.samples = []
        self.marks = {}
        self.t0 = time.monotonic()
        self._pinned = {}
        self._stop_evt = threading.Event()

    def mark(self, name):
        """A measurement boundary ("start": the scenario's measured work begins; see common.barrier)."""
        self.marks.setdefault(name, time.monotonic())

    def _own(self):
        """{pid: (ticks, pinned to the node slice, is our direct child)} for our marked processes.

        Direct children seen before stay in the set until they are reaped: a zombie's environ is
        empty, so the mark no longer finds it, but its CPU is not in os.times() yet either.
        """
        out, me = {}, os.getpid()
        known = getattr(self, "_last_own", {})
        for pid in own_pids() | {p for p, v in known.items() if v[2]}:
            stat = _read(f"/proc/{pid}/stat").rsplit(")", 1)
            try:
                f = stat[1].split()
                ticks, ppid = int(f[11]) + int(f[12]), int(f[1])
            except (IndexError, ValueError):
                continue
            pinned = self._pinned.get(pid)
            if pinned is None and self.slice is not None:
                pinned = pinned_within(pid, self.slice)
                if pinned:  # only cache positives: setpriv/numactl may not have pinned it yet
                    self._pinned[pid] = True
            out[pid] = (ticks, bool(pinned), ppid == me)
        self._last_own = out
        return out

    def _now(self):
        t = os.times()
        children = int((t.children_user + t.children_system) * CLK_TCK)
        return cpu_ticks(), cpu_ticks(self.slice) if self.slice else None, self._own(), children, time.monotonic()

    def run(self):
        prev = self._now()
        while True:
            stopping = self._stop_evt.wait(self.interval)
            now = self._now()
            # The final, partial interval counts too, so short runs get a sample.
            if now[4] - prev[4] >= 0.2:
                self._record(prev, now)
                prev = now
            if stopping:
                return

    def _record(self, prev, now):
        (h0, s0, own0, ch0, t0), (h1, s1, own1, ch1, t1) = prev, now
        dt = t1 - t0

        def busy(a, b, ncpu):
            d = b[0] - a[0]
            return (d - (b[1] - a[1])) / d * ncpu if d > 0 else 0.0

        # Per-pid deltas: processes that appear count from 0, so nothing is double counted.
        mine = sum(t - own0.get(pid, (0,))[0] for pid, (t, _, _) in own1.items())
        mine_slice = sum(t - own0.get(pid, (0,))[0] for pid, (t, p, _) in own1.items() if p)
        # Direct children that exited: their CPU since our last look arrives in os.times() children.
        gone = [(pid, v) for pid, v in own0.items() if pid not in own1 and v[2]]
        lost = max(0, (ch1 - ch0) - sum(v[0] for _, v in gone))
        mine += lost
        if any(v[1] for _, v in gone):
            mine_slice += lost
        mine, mine_slice = mine / CLK_TCK / dt, mine_slice / CLK_TCK / dt
        host = max(0.0, busy(h0, h1, os.cpu_count()) - mine)
        sl = max(0.0, busy(s0, s1, len(self.slice)) - mine_slice) if self.slice else None
        load1 = float(_read("/proc/loadavg").split()[0])
        p = psi()
        self.samples.append({"t0": t0, "t1": t1, "host": host, "slice": sl, "load1": load1,
                             "psi_cpu": p.get("cpu", {}).get("avg10"), "psi_io": p.get("io", {}).get("avg10"),
                             "psi_mem": p.get("memory", {}).get("avg10")})

    def stop(self):
        self._stop_evt.set()
        if self.ident is not None:
            self.join(self.interval + 5)
        start = self.marks.get("start")
        window = [s for s in self.samples if start is None or s["t1"] > start]
        out = summarize(window, len(self.slice) if self.slice else None)
        out["window"] = "measurement (from the start mark)" if start is not None else "whole run (no start mark)"
        out["marks_s"] = {k: round(v - self.t0, 3) for k, v in self.marks.items()}
        step = max(1, -(-len(self.samples) // MAX_INTERVALS))
        out["intervals"] = [[round(s["t1"] - self.t0, 1), round(s["host"], 2),
                             None if s["slice"] is None else round(s["slice"], 2),
                             s["psi_cpu"], s["psi_io"], s["psi_mem"]] for s in self.samples[::step]]
        out["intervals_columns"] = ["t_s", "host_foreign_cores", "slice_foreign_cores", "psi_cpu", "psi_io", "psi_mem"]
        return out


def _quantiles(xs):
    xs = sorted(xs)
    return {"mean": sum(xs) / len(xs), "p50": percentile(xs, 50), "p90": percentile(xs, 90), "max": xs[-1]}


def summarize(samples, slice_ncpu=None):
    if not samples:
        return {"n": 0}
    ncpu = os.cpu_count()
    h = _quantiles([s["host"] for s in samples])
    hf = _quantiles([s["host"] / ncpu for s in samples])
    out = {"n": len(samples), "foreign_cores_mean": h["mean"], "foreign_cores_p90": h["p90"],
           "foreign_cores_max": h["max"], **{f"foreign_frac_{k}": v for k, v in hf.items()},
           "load1_mean": sum(s["load1"] for s in samples) / len(samples)}
    for res in ("cpu", "io", "mem"):
        xs = [s[f"psi_{res}"] for s in samples if s.get(f"psi_{res}") is not None]
        if xs:
            q = _quantiles(xs)
            out.update({f"psi_{res}_p90": q["p90"], f"psi_{res}_max": q["max"]})
            if res == "cpu":
                out["psi_cpu_avg10_mean"] = q["mean"]
        elif res == "cpu":
            out["psi_cpu_avg10_mean"] = None
    sl = [s["slice"] for s in samples if s["slice"] is not None]
    if sl and slice_ncpu:
        out.update(slice_cpus=slice_ncpu, slice_foreign_cores_mean=sum(sl) / len(sl),
                   **{f"slice_foreign_frac_{k}": v for k, v in _quantiles([x / slice_ncpu for x in sl]).items()})
    return out


def classify(summary, max_slice=QUIET_SLICE, use_psi=True):
    """quiet / moderate / heavy from robust interval statistics (older records with means only still classify).

    use_psi=False for a measurement window: host-wide pressure then includes our own work (see module doc)."""
    if not summary.get("n"):
        return "unknown"
    s = summary
    host_p90 = s.get("foreign_frac_p90", s["foreign_frac_mean"])
    host_p50 = s.get("foreign_frac_p50", s["foreign_frac_mean"])
    sl_p90 = s.get("slice_foreign_frac_p90", s.get("slice_foreign_frac_mean", host_p90))
    sl_p50 = s.get("slice_foreign_frac_p50", s.get("slice_foreign_frac_mean", host_p50))
    sl_max = s.get("slice_foreign_frac_max", sl_p90)
    cpu = (s.get("psi_cpu_p90", s.get("psi_cpu_avg10_mean")) or 0) if use_psi else 0
    io, mem = ((s.get("psi_io_p90") or 0), (s.get("psi_mem_p90") or 0)) if use_psi else (0, 0)
    if (sl_p90 <= max_slice and sl_max <= HEAVY_SLICE and host_p90 <= QUIET_HOST and cpu < QUIET_PSI
            and io < QUIET_IO_PSI and mem < QUIET_MEM_PSI):
        return "quiet"
    return "heavy" if sl_p50 > HEAVY_SLICE or host_p50 > HEAVY_HOST or io >= HEAVY_IO_PSI else "moderate"


def measure(seconds, node_slice=None):
    m = LoadMonitor(node_slice, interval=min(2.0, seconds / 3))
    m.start()
    time.sleep(seconds)
    return m.stop()


def wait_for_quiet(log, max_frac=QUIET_SLICE, node_slice=None, window=30, timeout=6 * 3600):
    """Block until a `window`-second measurement is quiet; False after `timeout`.

    max_frac is the allowed foreign share of the node slice (host-wide stays <= QUIET_HOST).
    """
    deadline = time.monotonic() + timeout
    while True:
        s = measure(window, node_slice)
        sl = s.get("slice_foreign_frac_p90", s.get("foreign_frac_p90", 1))
        ok = classify(s, max_slice=max_frac) == "quiet"
        log(f"quiet-window check: foreign p90 {s.get('foreign_cores_p90', 0):.1f} cores on host "
            f"({s.get('foreign_frac_p90', 0):.0%}), {sl:.0%} of the node slice, load1 "
            f"{s.get('load1_mean', 0):.1f}, PSI p90 cpu {s.get('psi_cpu_p90')}% io {s.get('psi_io_p90')}% "
            f"mem {s.get('psi_mem_p90')}% -> {'quiet' if ok else 'busy'}")
        if ok:
            return True
        if time.monotonic() > deadline:
            return False
        time.sleep(max(0, min(300, deadline - time.monotonic())))
