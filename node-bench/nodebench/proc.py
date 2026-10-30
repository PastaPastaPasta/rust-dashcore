"""Process control: CPU/NUMA pinning, /proc sampling and on-disk footprint."""

import csv
import os
import shutil
import signal
import subprocess
import threading
import time

CLK_TCK = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")


def numa_nodes():
    """{node_id: 'cpulist'} from sysfs; empty when the kernel exposes no NUMA info."""
    base = "/sys/devices/system/node"
    out = {}
    if os.path.isdir(base):
        for d in sorted(os.listdir(base)):
            if d.startswith("node") and d[4:].isdigit():
                with open(f"{base}/{d}/cpulist") as f:
                    out[int(d[4:])] = f.read().strip()
    return out


def slice_cpus(slice_spec):
    """The cpu list of a slice ("numa:1" -> "16-31", "0-13@0" -> "0-13"); None = all."""
    if not slice_spec:
        return None
    if slice_spec.startswith("numa:"):
        return numa_nodes()[int(slice_spec[5:])]
    return slice_spec.partition("@")[0]


def pin_prefix(slice_spec):
    """Command prefix that confines a process to a CPU slice.

    slice_spec: None/"" (no pinning); "numa:N" (cpus *and* memory of NUMA node N,
    the default for the node under test); "CPUS@N" (cpu list CPUS, memory of
    node N: equal-size disjoint slices for concurrent pairs); or a bare cpu list.
    """
    if not slice_spec:
        return []
    numactl = shutil.which("numactl")
    if slice_spec.startswith("numa:"):
        node = slice_spec[5:]
        if numactl:
            return ["numactl", "-a", f"--cpunodebind={node}", f"--membind={node}"]
        return ["taskset", "-c", numa_nodes()[int(node)]]
    cpus, _, mem = slice_spec.partition("@")
    if mem and numactl:
        return ["numactl", "-a", f"--physcpubind={cpus}", f"--membind={mem}"]
    return ["taskset", "-c", cpus]


def cpu_set(cpulist):
    out = set()
    for part in cpulist.split(","):
        a, _, b = part.partition("-")
        out.update(range(int(a), int(b or a) + 1))
    return out


def slice_set(slice_spec):
    """The set of CPUs of a slice (None = unpinned); refuses CPUs this host does not have."""
    cpus = slice_cpus(slice_spec)
    if cpus is None:
        return None
    out = cpu_set(cpus)
    if not out or max(out) >= os.cpu_count():
        raise SystemExit(f"slice {slice_spec!r}: CPUs {cpus} do not exist on this host ({os.cpu_count()} CPUs)")
    return out


def pin_method(slice_spec):
    """How pin_prefix confines a slice; memory binding is lost when numactl is missing."""
    if not slice_spec:
        return "unpinned"
    pre = pin_prefix(slice_spec)
    if pre[0] == "numactl":
        return "numactl: cpus + memory"
    return "taskset: cpus only" + (" (numactl missing: memory NOT bound)" if "numa:" in slice_spec or "@" in
                                   slice_spec else "")


def slice_cpu_count(slice_spec):
    cpus = slice_cpus(slice_spec)
    return len(cpu_set(cpus)) if cpus else os.cpu_count()


def read_proc(pid):
    """Point-in-time resource counters for one process (all threads)."""
    with open(f"/proc/{pid}/stat") as f:
        stat = f.read().rsplit(")", 1)[1].split()
    status = {}
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            k, _, v = line.partition(":")
            status[k] = v.strip()
    io = {}
    try:
        with open(f"/proc/{pid}/io") as f:
            for line in f:
                k, _, v = line.partition(":")
                io[k] = int(v)
    except PermissionError:
        pass
    kib = lambda k: int(status.get(k, "0 kB").split()[0])  # noqa: E731
    return {
        "cpu_user_s": int(stat[11]) / CLK_TCK,
        "cpu_sys_s": int(stat[12]) / CLK_TCK,
        "rss_mib": kib("VmRSS") / 1024,
        "hwm_mib": kib("VmHWM") / 1024,
        "threads": int(status.get("Threads", 0)),
        "read_bytes": io.get("read_bytes", 0),
        "write_bytes": io.get("write_bytes", 0),
    }


def system_snapshot():
    with open("/proc/loadavg") as f:
        load1 = float(f.read().split()[0])
    with open("/proc/stat") as f:
        cpu = [int(x) for x in f.readline().split()[1:]]
    idle = cpu[3] + cpu[4]
    return {"load1": load1, "cpu_total_ticks": sum(cpu), "cpu_idle_ticks": idle}


def dir_size_bytes(path):
    """Allocated bytes under path (like `du -sb` but counting blocks, not apparent size)."""
    total = 0
    for root, _, files in os.walk(path):
        for name in files:
            try:
                total += os.lstat(os.path.join(root, name)).st_blocks * 512
            except FileNotFoundError:
                pass
    return total


class CsvSink:
    """Append sampler rows to a CSV as they arrive (progress is visible while a long run is going)."""

    def __init__(self, path):
        self.fh = open(path, "w", newline="")
        self.writer = None

    def write(self, row):
        if self.writer is None:
            self.writer = csv.DictWriter(self.fh, fieldnames=list(row.keys()), extrasaction="ignore")
            self.writer.writeheader()
        self.writer.writerow(row)
        self.fh.flush()

    def close(self):
        self.fh.close()


class Sampler(threading.Thread):
    """Samples a process every `interval` seconds into a list of dicts.

    `probe` is an optional callable returning extra columns (e.g. chain height
    over RPC); it runs on this thread, so give it its own RPC connection.
    """

    def __init__(self, pid, interval=5.0, probe=None, sink=None, t0=None):
        super().__init__(daemon=True)
        self.pid, self.interval, self.probe, self.sink = pid, interval, probe, sink
        self.samples = []
        self.t0 = t0 if t0 is not None else time.monotonic()
        self._stop_evt = threading.Event()
        self.last = None

    def sample(self):
        row = {"t": round(time.monotonic() - self.t0, 3), "wall": time.time()}
        try:
            row.update(read_proc(self.pid))
        except (FileNotFoundError, ProcessLookupError):
            return None
        row.update(system_snapshot())
        if self.probe:
            try:
                row.update(self.probe() or {})
            except Exception as e:  # probe failures must never kill sampling
                row["probe_error"] = str(e)[:200]
        self.samples.append(row)
        self.last = row
        if self.sink:
            self.sink(row)
        return row

    def run(self):
        while not self._stop_evt.is_set():
            if self.sample() is None:
                break
            self._stop_evt.wait(self.interval)

    def stop(self):
        self._stop_evt.set()
        self.join(timeout=self.interval + 30)
        return self.samples


def summarize_samples(samples, settle_frac=0.5):
    """Peak / steady-state RSS, CPU seconds and IO totals from Sampler output.

    Steady-state RSS is the median RSS over the last `settle_frac` of samples.
    Host CPU busy % is the fraction of non-idle host ticks during the window: it
    is recorded so noisy (shared) runs can be identified and discarded.
    """
    if not samples:
        return {}
    first, last = samples[0], samples[-1]
    tail = sorted(s["rss_mib"] for s in samples[int(len(samples) * (1 - settle_frac)):])
    dt_ticks = last["cpu_total_ticks"] - first["cpu_total_ticks"]
    busy = 1 - (last["cpu_idle_ticks"] - first["cpu_idle_ticks"]) / dt_ticks if dt_ticks else None
    return {
        "peak_rss_mib": max(s["hwm_mib"] for s in samples),
        "steady_rss_mib": tail[len(tail) // 2],
        "cpu_user_s": last["cpu_user_s"],
        "cpu_sys_s": last["cpu_sys_s"],
        "read_bytes": last["read_bytes"],
        "write_bytes": last["write_bytes"],
        "host_load1_mean": sum(s["load1"] for s in samples) / len(samples),
        "host_cpu_busy_frac": busy,
    }


def terminate(proc, timeout=600):
    """SIGTERM, then SIGKILL after `timeout`. Returns the exit code."""
    if proc.poll() is not None:
        return proc.returncode
    proc.send_signal(signal.SIGTERM)
    try:
        return proc.wait(timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        return proc.wait()
