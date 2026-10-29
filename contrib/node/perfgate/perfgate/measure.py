"""Running the declared workloads of one revision (or of dashd) under the counters.

A bench workload runs as `taskset -c <cpu> setarch -R <bench exe> --bench` with the shim selecting one
benchmark and a fixed iteration count. Each repetition is two processes: one in counters mode, and (unless
the workload opts out) one under `strace -f` in syscalls mode, cut to the measured region by the shim's
marker calls. Two things make the counts repeat:
- ASLR is off, so address-dependent work (hashing pointers, allocator placement) repeats exactly;
- each process is pinned to one CPU (and RAYON_NUM_THREADS=1), so thread pools size themselves to one
  worker and idle workers do not spin. Spinning is scheduling, not work: with rayon's default pool on this
  box it moved filter_scan's instruction count by 5%, pinned it is 0.03%.

Every process sees the same environment and an executable path of the same length on both sides (a
read-only copy under $PERFGATE_ROOT/exe/): with ASLR off, the environment's size and argv[0] place the stack.

Every finished workload is cached under $PERFGATE_ROOT/cache/<sha>/, keyed by its declaration, iteration
count, repeat count and the fingerprint (toolchain, host, harness, counting path, the worktree path), so the
base side of a gate is measured once per integration head; `measure --label base` (and `warm`) seed that cache.
"""

import contextlib
import functools
import hashlib
import inspect
import json
import os
import platform
import queue
import re
import shutil
import statistics
import subprocess
import tempfile
import time
import types
from collections import Counter
from concurrent.futures import ThreadPoolExecutor

from . import build, node, paths, sandbox

# Syscalls whose counts follow thread scheduling and wall time rather than the work done: recorded as
# sched_syscalls (information only), excluded from the gated syscall count.
SCHED_SYSCALLS = frozenset({
    "futex", "futex_waitv", "sched_yield", "nanosleep", "clock_nanosleep", "epoll_wait", "epoll_pwait",
    "epoll_pwait2", "poll", "ppoll", "select", "pselect6", "restart_syscall", "membarrier",
})

# Filesystem syscalls: the storage benches' durable work (opens, metadata, directory creation, renames and
# syncs), gated as fs_syscalls where the store bench's total syscall count is not (its thread pool adds a
# varying number of other calls). Calls on /sys and /proc are runtime introspection, not filesystem work: glibc
# reads /sys/devices/system/cpu/online when the first thread creates a malloc arena, i.e. when the scheduler
# happens to start another pool thread.
FS_SYSCALLS = frozenset({
    "open", "openat", "openat2", "creat", "stat", "lstat", "fstat", "newfstatat", "statx", "statfs", "fstatfs",
    "access", "faccessat", "faccessat2", "mkdir", "mkdirat", "rmdir", "rename", "renameat", "renameat2", "link",
    "linkat", "symlink", "symlinkat", "unlink", "unlinkat", "truncate", "ftruncate", "fallocate", "getdents64",
    "fsync", "fdatasync", "sync_file_range", "syncfs", "sync", "msync",
})
INTROSPECTION = re.compile(r'"/(?:sys|proc)/')

# Environment variables that change what gets built; part of every cache key.
BUILD_ENV = frozenset({"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS", "CARGO_INCREMENTAL", "RUSTC",
                       "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS"})
BUILD_ENV_PREFIXES = ("CARGO_TARGET_", "CARGO_PROFILE_")

TARGET_INSTRUCTIONS = 200_000_000
MAX_ITERS = 10_000_000
RUN_TIMEOUT = 1800
# The counters around a bench process (run_bench); part of counter_hash.
PERF_COUNTER = ("perf", "stat", "-x,", "--no-big-num", "-e", "instructions:u")
STRACE_COUNTER = ("strace", "-f", "-qq")


def bench_argv(exe, cpu):
    """The bench process inside the sandbox: pinned, with ASLR off (part of counter_hash)."""
    return ["taskset", "-c", str(cpu), "setarch", platform.machine(), "-R", exe, "--bench"]

SYSCALL_LINE = re.compile(r"^(?:\d+\s+)?([a-z_][a-z0-9_]*)\(")
MARKER = re.compile(r'(?:access|faccessat2?)\(.*"/perfgate/(begin|end)"')


class RunError(RuntimeError):
    def __init__(self, msg, missing=False):
        super().__init__(msg)
        self.missing = missing

    def result(self):
        """The workload result this failure stands for."""
        return {"status": "missing" if self.missing else "error", "error": str(self)}


def split_sched(names):
    """(gated, scheduling) syscall totals of a {syscall: count} map."""
    sched = sum(n for k, n in names.items() if k in SCHED_SYSCALLS)
    return sum(names.values()) - sched, sched


def strace_region(lines):
    """Syscalls between the shim's begin/end markers: ({"syscalls", "sched_syscalls", "fs_syscalls"},
    {name: count}, spans)."""
    on, names, fs, spans = False, Counter(), 0, 0
    for line in lines:
        m = MARKER.search(line)
        if m:
            on = m.group(1) == "begin"
            spans += on
            continue
        if not on:
            continue
        s = SYSCALL_LINE.match(line)
        if s:
            names[s.group(1)] += 1
            fs += s.group(1) in FS_SYSCALLS and not INTROSPECTION.search(line)
    gated, sched = split_sched(names)
    return {"syscalls": gated, "sched_syscalls": sched, "fs_syscalls": fs}, dict(names), spans


def nice_iters(n):
    """Round down to 1, 2 or 5 times a power of ten, so calibrated counts read well and change rarely."""
    n = max(1, min(int(n), MAX_ITERS))
    mag = 10 ** (len(str(n)) - 1)
    return max(s * mag for s in (1, 2, 5) if s * mag <= n)


def cpu_slots(n):
    """CPUs for `n` concurrent runs: the highest allowed ones (node-bench pins its nodes to NUMA node 0)."""
    cpus = sorted(os.sched_getaffinity(0))
    return [cpus[-1 - i % len(cpus)] for i in range(n)]


COUNTER_KEYS = ("instructions", "instructions_total", "allocs", "alloc_bytes", "page_faults", "read_bytes",
                "write_bytes", "read_syscalls", "write_syscalls", "peak_rss_kib")


def run_bench(exe, cwd, wl, iters, mode, cpu):
    """One process of one bench workload; returns the shim's counters (counters) or strace's (syscalls).

    The bench (the PR's code) runs in the sandbox and can write only `tmp`. What it reports itself (the
    region counters in result.json) is read without following links and validated. strace (syscalls mode) and
    the whole-process instruction count (`instructions_process`, counters mode) run outside the sandbox and
    write where the bench cannot, not even through an inherited descriptor (sandbox.counted). Only
    `instructions_process` is beyond the process's reach: strace's region follows the marker calls the process
    makes (sandbox.py, DEC-76).
    """
    tmp = tempfile.mkdtemp(prefix=f"{wl.name}-", dir=paths.sub("tmp"))
    outside = tempfile.mkdtemp(prefix=f"{wl.name}-", dir=paths.sub("runlogs"))
    try:
        out, scratch = os.path.join(tmp, "result.json"), os.path.join(tmp, "tmp")
        log, outer = os.path.join(outside, "strace.log"), os.path.join(outside, "perf.csv")
        os.mkdir(scratch)
        # A fixed environment: inherited variables (MALLOC_*, GLIBC_TUNABLES, RUST_LOG, even the
        # environment's size, which moves the stack with ASLR off) would differ between a cached base and
        # a fresh head.
        env = {"PATH": "/usr/bin:/bin", "HOME": paths.sub("home"), "LANG": "C",
               "PERFGATE_MODE": mode, "PERFGATE_BENCH": wl.id, "PERFGATE_ITERS": str(iters), "PERFGATE_OUT": out,
               "TMPDIR": scratch, "RAYON_NUM_THREADS": "1"}
        argv = sandbox.wrap(bench_argv(exe, cpu),
                            writable=[tmp], cwd=cwd, env=env)
        if mode == "syscalls":
            argv = sandbox.counted([*STRACE_COUNTER, "-o", log], argv)
        else:
            argv = sandbox.counted([*PERF_COUNTER, "-o", outer], argv)
        try:
            p = subprocess.run(argv, cwd=cwd, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                               timeout=RUN_TIMEOUT)
        except subprocess.TimeoutExpired:
            raise RunError(f"{wl.name}: timed out after {RUN_TIMEOUT} s ({mode}, {iters} iters)") from None
        if p.returncode or not os.path.lexists(out):
            err = (p.stderr or p.stdout).strip()[-1500:]
            if "no benchmark with id" in err:
                raise RunError(f"{wl.name}: no benchmark {wl.id!r} in {os.path.basename(exe)}", missing=True)
            if not err and p.returncode == 0:
                err = "the bench wrote no result: is it a criterion version the shim covers (0.5, 0.8)?"
            raise RunError(f"{wl.name}: {mode} run failed (exit {p.returncode}): {err}")
        try:
            res = json.loads(sandbox.read_regular(out))
            if mode == "counters":
                if res["time_running"] < res["time_enabled"]:
                    raise RunError(f"{wl.name}: the instruction counter was multiplexed ({res['time_running']}/"
                                   f"{res['time_enabled']} ns); another perf user holds the PMU")
                counters = {k: int(res[k]) for k in COUNTER_KEYS}
                with open(outer) as f:
                    counters["instructions_process"] = node.parse_perf_csv(f.read())["instructions"]
                return counters
            spans = int(res["spans"])
        except (ValueError, KeyError, TypeError, OSError, sandbox.SandboxError) as e:
            raise RunError(f"{wl.name}: unusable {mode} result: {e!r}") from None
        with open(log, errors="replace") as f:
            counts, names, seen = strace_region(f)
        if seen != spans:
            raise RunError(f"{wl.name}: strace saw {seen} measured spans, the bench ran {spans}")
        return {**counts, "_by_name": names}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
        shutil.rmtree(outside, ignore_errors=True)


def with_retry(fn, *a):
    """Retry once: a multiplexed counter or a transient failure should not fail the gate."""
    try:
        return fn(*a)
    except RunError as e:
        if e.missing:
            raise
        return fn(*a)


def calibrate(exe, cwd, wl, cpu, log):
    """Iterations that put the measured region near TARGET_INSTRUCTIONS."""
    iters = 1
    per = with_retry(run_bench, exe, cwd, wl, iters, "counters", cpu)["instructions"] / iters
    est = TARGET_INSTRUCTIONS / max(per, 1)
    if est > 50:  # refine: one iteration over-weights the snapshot overhead and cold caches
        iters = int(min(est / 4, 10_000))
        per = with_retry(run_bench, exe, cwd, wl, iters, "counters", cpu)["instructions"] / iters
        est = TARGET_INSTRUCTIONS / max(per, 1)
    n = nice_iters(est)
    log(f"  calibrated {wl.name}: {per:,.0f} instructions/iter -> {n} iters")
    return n


def _counting_sources():
    """The sources of the counting path, read once at import (the checkout running the tool may change during
    a gate; see build.shim_snapshot)."""
    with open(sandbox.LAUNCH_SRC, "rb") as f:
        launch = f.read().decode()
    funcs = (bench_argv, node.parse_perf_csv, strace_region, split_sched, sandbox.wrap, sandbox.counted,
             sandbox.readonly_binds)
    return [launch, *(inspect.getsource(f) for f in funcs)]


_COUNTING_SOURCES = _counting_sources()


def _version(*cmd):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True)
    except OSError:
        return None
    lines = (p.stdout or p.stderr).strip().splitlines()
    return lines[0] if p.returncode == 0 and lines else None


@functools.cache
def counter_hash():
    """Hash of what turns a bench run into the counts taken outside it (`instructions_process`, the strace pass),
    other than the bench and the toolchain: everything perf counts besides the bench (the launcher, bwrap,
    taskset and setarch; bwrap's work grows with its binds) and how the counts are read (the command
    lines, the parsers and their syscall classes, the tools' versions). The shim hash does not cover these, and
    they move the counts (PF0-N), so an anchor must not outlive a change to them."""
    parts = [_COUNTING_SOURCES, sandbox.CC_FLAGS, PERF_COUNTER, STRACE_COUNTER, sorted(SCHED_SYSCALLS),
             sorted(FS_SYSCALLS), INTROSPECTION.pattern, SYSCALL_LINE.pattern, MARKER.pattern,
             len(sandbox.readonly_binds()),
             *(_version(*c) for c in (("perf", "--version"), ("strace", "-V"), ("bwrap", "--version"),
                                      ("taskset", "--version")))]
    return hashlib.sha256(json.dumps(parts).encode()).hexdigest()[:16]


def fingerprint(wt):
    """Everything besides the commit that moves the counts: a cached result is only reused when it matches.

    `lock` may differ between base and head (a PR may change dependencies); the rest must not.
    """
    lock = os.path.join(wt, "Cargo.lock")
    with open(lock, "rb") as f:
        lock_hash = hashlib.sha256(f.read()).hexdigest()[:16]
    return {"harness": build.harness_hash(), "shim": build.shim_hash(), "counter": counter_hash(), "lock": lock_hash,
            "worktree": wt,
            **build.toolchain(wt),
            "libc": " ".join(platform.libc_ver()), "kernel": platform.release(), "cpu": host_meta()["cpu"],
            "ncpu": online_cpus(), "sandbox_label": sandbox.label(),
            "env": {k: v for k, v in sorted(os.environ.items()) if k in BUILD_ENV or k.startswith(BUILD_ENV_PREFIXES)}}


class Side:
    """One revision under measurement: its worktree, fingerprint, run plan and results."""

    def __init__(self, label, rev, lock_from=None, log=print):
        self.label, self.ref = label, rev
        self.sha = build.resolve(rev)
        self.wt = build.prepare_worktree(label, self.sha, lock_from)
        # Every slot but the base starts from a fresh copy of the base's target: a head's build scripts can
        # write their target directory, which the next gate's head must not inherit.
        build.target_dir(self.wt, fresh_from=None if label == "base" else "base")
        build.lock_dependencies(self.wt, log)
        self.fp = fingerprint(self.wt)
        self.ready = {}  # name -> (workload, exe, cwd, iters): built and calibrated
        self.plan = {}  # the subset of `ready` the next run_plans runs
        self.bins = {}  # node binary name -> path
        self.results = {}
        self.cache_results = True

    def key(self, *parts):
        return hashlib.sha256(json.dumps([*parts, self.fp], sort_keys=True).encode()).hexdigest()[:16]

    def cache_path(self, wl, iters, repeat):
        return os.path.join(paths.root(), "cache", self.sha, f"{wl.name}-{self.key(wl.key(), iters, repeat)}.json")

    def calibration_path(self, wl):
        return os.path.join(paths.root(), "cache", self.sha, f"{wl.name}-{self.key(wl.key())}.iters")

    def build_log(self):
        return os.path.join(paths.sub("logs"), f"build-{self.label}-{self.sha[:12]}.log")

    def cached_iters(self, wl):
        try:
            with open(self.calibration_path(wl)) as f:
                return int(f.read())
        except (OSError, ValueError):
            return None

    def iters(self):
        """{workload: iterations} of everything ready or finished on this side."""
        out = {n: r["iters"] for n, r in self.results.items() if r.get("status") == "ok"}
        out.update({n: p[3] for n, p in self.ready.items()})
        return out

    def measurement(self, wls, repeat):
        meta = {"label": self.label, "rev": self.sha, "ref": self.ref, "impl": "rust", "repeat": repeat,
                "fingerprint": self.fp, "worktree": self.wt, **host_meta()}
        workloads = {}
        for w in wls:
            r = dict(self.results.get(w.name) or {"status": "error", "error": "not measured"})
            r["kind"], r["describe"], r["ungated"] = w.kind, w.describe(), list(w.ungated)
            workloads[w.name] = r
        return {"meta": meta, "workloads": workloads}


def write_atomic(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = f"{path}.{os.getpid()}"
    with open(tmp, "w") as f:
        f.write(text)
    os.replace(tmp, path)


def prepare_benches(side, wls, repeat, iters_from, reuse, log):
    """Load reusable cached results, build the rest at this revision, and plan (calibrating) their runs.

    `reuse(result)` decides whether a cached result may stand in for fresh runs.
    """
    todo = []
    for w in wls:
        iters = w.iters or iters_from.get(w.name) or side.cached_iters(w)
        cp = side.cache_path(w, iters, repeat) if iters else None
        if cp and os.path.exists(cp):
            with open(cp) as f:
                cached = json.load(f)
            if reuse(cached):
                side.results[w.name] = cached
                continue
        todo.append((w, iters))
    log(f"{side.label}: {len(wls) - len(todo)} of {len(wls)} bench workloads reused from the cache")
    if not todo:
        return
    wanted = {(w.package, w.target) for w, _ in todo}
    blog = side.build_log()
    log(f"{side.label}: building {len(wanted)} bench targets at {side.sha[:12]} (log {blog})")
    t0 = time.monotonic()
    exes, missing = build.build_benches(side.wt, wanted, blog)
    log(f"{side.label}: built in {time.monotonic() - t0:.0f} s")
    cpu = cpu_slots(1)[0]
    for w, iters in todo:
        pt = (w.package, w.target)
        if pt in missing:
            side.results[w.name] = {"status": "missing", "error": missing[pt]}
            continue
        exe, cwd = exes[pt]
        if not build.uses_shim(exe):
            side.results[w.name] = {"status": "error", "error": f"{os.path.basename(exe)} is linked against "
                                    "crates.io criterion, not the shim (see the Cargo.lock warning)"}
            continue
        exe = fixed_path(exe)
        if not iters:
            try:
                iters = calibrate(exe, cwd, w, cpu, log)
            except RunError as e:
                side.results[w.name] = e.result()
                continue
            write_atomic(side.calibration_path(w), str(iters))
        side.ready[w.name] = side.plan[w.name] = (w, exe, cwd, iters)


def fixed_path(exe):
    """A read-only copy of `exe` at $PERFGATE_ROOT/exe/<content hash>: the same path length for every side
    and label, so argv[0] and AT_EXECFN place the stack identically.

    A copy, never a hardlink: the linker rewrites cargo's output files in place, so a link would silently
    take on whatever a later build of that worktree writes (a gate once ran the head's code on both sides
    that way)."""
    h = hashlib.sha256()
    with open(exe, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    dest = os.path.join(paths.sub("exe"), h.hexdigest()[:16])
    if not os.path.exists(dest):
        tmp = f"{dest}.{os.getpid()}"
        shutil.copyfile(exe, tmp)
        os.chmod(tmp, 0o555)
        os.replace(tmp, dest)
    return dest


def run_plans(sides, repeat, jobs, log, append=False):
    """Run every planned workload of every side `repeat` times, interleaving the sides within each round
    so that base and head runs see the same conditions. Results land in each side's `results`; with
    `append` (an escalation) the runs extend the existing samples and nothing is cached."""
    tasks = [(side, name, mode) for _ in range(repeat) for side in sides for name, (w, *_r) in side.plan.items()
             for mode in (("counters", "syscalls") if w.syscalls else ("counters",))]
    if not tasks:
        return
    log(f"{'+'.join(s.label for s in sides)}: {len(tasks)} runs, {jobs} at a time")
    t0 = time.monotonic()
    free = queue.Queue()
    for c in cpu_slots(jobs):
        free.put(c)

    def one(task):
        side, name, mode = task
        w, exe, cwd, iters = side.plan[name]
        cpu = free.get()
        try:
            return task, with_retry(run_bench, exe, cwd, w, iters, mode, cpu)
        except RunError as e:
            return task, e
        finally:
            free.put(cpu)

    runs, errors = {}, {}
    with ThreadPoolExecutor(max_workers=jobs) as ex:
        for (side, name, _mode), r in ex.map(one, tasks):
            key = (side.label, name)
            if isinstance(r, RunError):
                errors.setdefault(key, r)
            else:
                runs.setdefault(key, []).append(r)
    log(f"ran in {time.monotonic() - t0:.0f} s")

    for side in sides:
        for name, (w, _exe, _cwd, iters) in side.plan.items():
            e = errors.get((side.label, name))
            if e:
                if append:  # the workload cannot be decided: compare() makes it an ERROR
                    log(f"  {side.label} {name}: escalation runs failed ({e})")
                    side.results[name]["escalation_error"] = str(e)
                else:
                    side.results[name] = e.result()
                continue
            samples, breakdown = collect(runs[(side.label, name)])
            if append:
                extend(side.results[name], samples, repeat)
                continue
            side.results[name] = {"status": "ok", "iters": iters, "samples": samples, "kind": w.kind,
                                  "syscall_breakdown": breakdown}
            if side.cache_results:
                write_atomic(side.cache_path(w, iters, repeat), json.dumps(side.results[name]))
        side.plan = {}


def extend(result, samples, extra):
    """Append an escalation's runs to a round-1 result (whose run count becomes `first_round`)."""
    if not result.get("escalated"):
        result["escalated"] = True
        result["first_round"] = len(next(iter(result["samples"].values())))
    for k, v in samples.items():
        result["samples"].setdefault(k, []).extend(v)
    result["escalation_runs"] = result.get("escalation_runs", 0) + extra


def node_session(sides, wls, runs, log):
    """Build each side's node binaries (in the build sandbox, like the benches) and start the gate's node path
    (nodegate.Session, PF0-N): None when no side has a binary of a declared node row. `runs` bounds each row's
    measured runs per side, escalation included."""
    from . import nodegate

    if not wls:
        return None
    exes = {}
    for side in sides:
        present = {t for (_p, kind, t) in build.targets(build.metadata(side.wt)) if kind == "bin"}
        side.bins = build.build_bins(side.wt, {w.bin for w in wls} & present, side.build_log())
        exes.update({nodegate.exe_key(side.label, b): p for b, p in side.bins.items()})
    live = [w for w in wls if any(w.bin in side.bins for side in sides)]
    return nodegate.Session(exes, live, runs, log) if live else None


def measure_nodes(session, sides, wls, repeat, log):
    """Node workloads on every side through the PF0 chain, on any revision (PF0-N lifted DEC-76 b): pending until
    the named binary target exists, else the preflight's ok, refused or error, and an ok row's interleaved runs."""
    from . import nodegate

    for w in wls:
        keys = [nodegate.exe_key(side.label, w.bin) for side in sides if w.bin in side.bins]
        if keys:
            log(f"  {w.name}: preflight, then {repeat} x node-bench {w.node} per side on the PF0 chain")
        res = session.measure(keys, w, repeat) if keys else {}
        for side in sides:
            r = res.get(nodegate.exe_key(side.label, w.bin)) or {
                "status": "pending", "error": f"no `{w.bin}` binary target in this revision"}
            side.results[w.name] = dict(r, kind="node")


def escalate(sides, wls, extra, jobs, log, session=None):
    """Run `extra` more interleaved repetitions of the given workloads on every side (M1); node rows on the gate's
    node `session`."""
    benches = [w for w in wls if w.kind == "bench"]
    for side in sides:
        side.plan = {w.name: side.ready[w.name] for w in benches if w.name in side.ready}
        cold = [w for w in benches if w.name not in side.ready and side.results.get(w.name, {}).get("status") == "ok"]
        if cold:  # a base result reused from the cache: build it now
            prepare_benches(side, cold, extra, {w.name: side.results[w.name]["iters"] for w in cold},
                            lambda _r: False, log)
    run_plans(sides, extra, jobs, log, append=True)
    from . import nodegate

    for w in wls:
        if w.kind != "node":
            continue
        live = [side for side in sides if side.results.get(w.name, {}).get("status") == "ok"]
        # the sides' results carry their preflight record, whose argv every run is checked against (§7)
        out = {nodegate.exe_key(side.label, w.bin): side.results[w.name] for side in live}
        samples = session.repeat(list(out), w, extra, out)
        for side in live:
            k = nodegate.exe_key(side.label, w.bin)
            if out[k]["status"] == "ok":
                extend(side.results[w.name], samples[k], extra)
            else:
                side.results[w.name]["escalation_error"] = out[k].get("error", "failed")


def collect(runs):
    """Per-counter sample lists from a list of per-process results, plus the last syscall breakdown."""
    samples, breakdown = {}, None
    for r in runs:
        for k, v in r.items():
            if k == "_by_name":
                breakdown = v
            else:
                samples.setdefault(k, []).append(v)
    return samples, breakdown


def online_cpus():
    """The online CPU count: thread pools and RocksDB size themselves from it, and a vCPU resize must not be
    compared with an anchor taken before it (PF0-N D38)."""
    return os.sysconf("SC_NPROCESSORS_ONLN")


def host_meta():
    def read(p):
        try:
            with open(p) as f:
                return f.read().strip()
        except OSError:
            return None

    cpu = next((line.split(":", 1)[1].strip() for line in (read("/proc/cpuinfo") or "").splitlines()
                if line.startswith("model name")), None)
    return {"host": platform.node(), "kernel": platform.release(), "cpu": cpu, "ncpu": online_cpus(),
            "sandbox_label": sandbox.label(),
            "perf_event_paranoid": read("/proc/sys/kernel/perf_event_paranoid"),
            "loadavg": read("/proc/loadavg"), "time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}


def measure_rev(label, rev, wls, repeat=5, jobs=4, use_cache=True, iters_from=None, log=print):
    """Measure the declared workloads at `rev` alone (built in the `label` worktree)."""
    side = Side(label, rev, log=log)
    benches = [w for w in wls if w.kind == "bench"]
    prepare_benches(side, benches, repeat, iters_from or {}, lambda _r: use_cache, log)
    run_plans([side], repeat, jobs, log)
    nodes = [w for w in wls if w.kind == "node"]
    with node_session([side], nodes, repeat, log) or contextlib.nullcontext() as session:
        measure_nodes(session, [side], nodes, repeat, log)
    return side.measurement(wls, repeat)


def measure_dashd(dashd_bin, wls, repeat=5, log=print):
    """The node workloads with dashd as the node under test (A1 §3.11 'against dashd'), on the same PF0 chain
    and argv as rdashd: node-bench's PF0 mode builds the rust argv, which dashd accepts (D91)."""
    from . import nodegate

    meta = {"label": "dashd", "rev": os.path.realpath(dashd_bin), "impl": "dashd", "repeat": repeat,
            "harness": build.harness_hash(), **host_meta()}
    nodes = [w for w in wls if w.kind == "node"]
    side = types.SimpleNamespace(label="dashd", bins={w.bin: dashd_bin for w in nodes}, results={})
    exes = {nodegate.exe_key("dashd", b): p for b, p in side.bins.items()}
    if nodes:
        with nodegate.Session(exes, nodes, repeat, log) as session:
            measure_nodes(session, [side], nodes, repeat, log)
    for w in nodes:
        side.results[w.name]["describe"] = w.describe()
    return {"meta": meta, "workloads": side.results}


def summary(m):
    """One line per workload: per-iteration medians of the main counters."""
    out = []
    for name, w in m["workloads"].items():
        if w["status"] != "ok":
            why = w.get("error") or ", ".join(x["option"] for x in (w.get("preflight") or {}).get("refused_on") or ())
            out.append(f"  {name:32} {w['status']}: {why}")
            continue
        s, it = w["samples"], w["iters"]
        med = {k: statistics.median(v) / it for k, v in s.items() if v}
        out.append(f"  {name:32} iters={it:<8} instr/iter={med.get('instructions', 0):>14,.0f}  "
                   f"allocs/iter={med.get('allocs', 0):>10,.2f}  syscalls/iter={med.get('syscalls', 0):>8,.2f}")
    return "\n".join(out)
