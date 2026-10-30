#!/usr/bin/env python3
# EMBARGO-1
"""run.sh's CPU-time driver: one libFuzzer process (tree) that stops on its own CPU time.

  window_driver.py [--farm RUN_STOP] SEG WINDOW WALL_CAP TMP TARGET PIN RUN_ID CMD...

A certify window (the default; DEC-186 amendments 2 and 3) is one process that runs until it has
used WINDOW seconds of its own CPU time (utime and stime from /proc, not its reaped children's; Sol
r2 M1) after its INITED, checked about every 10 s. A farm segment (`--farm`, BL-228) is a budgeted
run's libFuzzer process with its -jobs workers: it runs until its whole process tree has used WINDOW
CPU seconds from its start (live members' utime, stime, and their reaped children's), checked about
every 2 s. Wall time never ends either: on a loaded box at the farm's priority a segment that
stopped on a wall clock ended after about one corpus reload, and the next one reloaded the corpus
again.

The driver ends the window by writing SEG/stop, which CMD must have been given as -stop_file:
libFuzzer reads it at the top of its fuzzing loop, between two MutateAndTestOne calls, so a
discovery in flight is printed, saved and counted before it stops; it then prints its DONE line, its
final stats, and exits 0. No signal interrupts it (Sol r3 B1). A farm segment also copies the run's
stop file (RUN_STOP, written by the farm, BL-173) to SEG/stop, so that one -stop_file serves both.
WALL_CAP seconds of CLOCK_MONOTONIC from the start stop it with SIGINT (`wall-cap`: libFuzzer's
interrupted exit is 72; a farm segment's cap is a backstop of a day, never the normal stop), and
SIGKILL follows any stop that the process outlives by GRACE seconds, before INITED or after it (Sol
r2 M4). SEG/window.json (a farm segment's: segment.json) records what stopped it, its CPU time at
the stop, and the time from the stop to the exit (CLOCK_MONOTONIC); an exit later than GRACE seconds
after the stop fails the certify run whatever its status, so no path that misses the kill certifies
(Sol r4 B1). SEG/cpu is `<utime> <stime>` of the process and the children it reaped, as
`/usr/bin/time -f '%U %S'` writes it. libFuzzer's own -max_total_time is only a backstop.
"""
import ctypes, json, os, re, signal, subprocess, sys, time
args = sys.argv[1:]
farm = None
if args[:1] == ["--farm"]:
    farm, args = args[1], args[2:]
seg, window, wall_cap, tmp, target, pin, run_id, *cmd = args
window, wall_cap = int(window), int(wall_cap)
GRACE, POLL = 1500, 10  # run.sh's $stop_grace, and the seconds between CPU checks
FARM_POLL = 2  # a segment's poll: its overshoot is up to this many CPU-s per worker
def shortened(name, limit, default):
    """`default`, or the selftest's shorter value from the environment: a finite number of seconds
    above 0 and at most `limit`, else no run (Sol r4 B1)."""
    try:
        value = float(os.environ.get(name, default))
    except ValueError:
        value = float("nan")
    if not 0 < value <= limit:  # NaN and infinity fail too
        sys.exit(f"{name} must be above 0 and at most {limit} seconds")
    return value
poll = shortened("NODE_FUZZ_CERTIFY_POLL", POLL, FARM_POLL if farm else POLL)
grace = shortened("NODE_FUZZ_CERTIFY_GRACE", GRACE, GRACE)
env = {k: os.environ[k] for k in ("PATH", "HOME", "DASH_EMBARGO_DIR")}
env.update(TMPDIR=tmp, NODE_FUZZ_PIN=pin, NODE_FUZZ_RUN=run_id, RUST_BACKTRACE="1")
log, stop_file = os.path.join(seg, "fuzz.out"), os.path.join(seg, "stop")
if farm:
    # A signal to the run's process group (BL-173's fallbacks) reaches libFuzzer and its workers
    # as well, and libFuzzer answers it with its own exit; the driver outlives it to record the
    # segment. (A handler, not SIG_IGN, which libFuzzer would inherit: an exec resets handlers.)
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: None)
with open(log, "wb") as out:
    # libFuzzer dies with this driver (PR_SET_PDEATHSIG), so it is never left fuzzing on its own.
    # A farm segment keeps run.sh's leg lock (fd 8) open in libFuzzer and its workers, as it did
    # under /usr/bin/time: the lock is held until the last worker is gone.
    proc = subprocess.Popen(cmd, cwd=seg, env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT,
                            close_fds=not farm, preexec_fn=lambda: ctypes.CDLL(None).prctl(1, signal.SIGKILL))
def stat_fields(pid):
    """/proc/<pid>/stat after the command: state, ppid, ..., utime (11), stime, cutime, cstime (14)."""
    try:
        return open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    except (OSError, IndexError):
        return None
def tree():
    """{pid: stat fields} of the process and its live descendants."""
    live, children = {}, {}
    for entry in os.listdir("/proc"):
        if entry.isdigit() and (fields := stat_fields(entry)):
            live[int(entry)] = fields
            children.setdefault(int(fields[1]), []).append(int(entry))
    members, queue = {}, [proc.pid]
    while queue:
        pid = queue.pop()
        queue.extend(children.get(pid, []))
        if pid in live:
            members[pid] = live[pid]
    return members
def tree_cpu():
    """The process tree's CPU seconds: each live member's utime and stime, and its reaped children's
    (cutime and cstime; a dead child counts once, in the parent that reaped it)."""
    return sum(int(x) for fields in tree().values() for x in fields[11:15]) / os.sysconf("SC_CLK_TCK")
def kill_all():
    """SIGKILL for the process, and for a farm segment's workers: they have no parent-death signal
    (libFuzzer starts them with system()), and would fuzz on with nobody to stop them."""
    for pid in (tree() if farm else [proc.pid]):
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
def cpu_now():
    """The process's own CPU seconds (utime and stime, all its threads; not its reaped children's),
    or None once it is gone. A farm segment's is the tree's."""
    if farm:
        return tree_cpu() if stat_fields(proc.pid) else None
    fields = stat_fields(proc.pid)
    return None if fields is None else sum(int(x) for x in fields[11:13]) / os.sysconf("SC_CLK_TCK")
INITED = re.compile(rb"^#\d+\s+INITED\b", re.M)  # the status line, as fuzzctl reads it
start = checked = time.monotonic()
inited = stopped = stopped_by = cpu_at_inited = cpu_at_stop = None
if farm:
    inited, cpu_at_inited = start, 0.0  # a segment's allotment counts from its start, load included
killed = False
read = 0
def stop(why, now, cpu):
    """The window ends at libFuzzer's loop boundary (its stop file, which it reads only once
    fuzzing, so only after INITED); the wall cap interrupts it wherever it is."""
    global stopped, stopped_by, cpu_at_stop
    stopped, stopped_by, cpu_at_stop = now, why, cpu
    if why != "wall-cap":
        with open(stop_file, "wb") as f:
            f.write(b"stop\n")  # libFuzzer stops on a stop file with at least one byte
    else:
        os.kill(proc.pid, signal.SIGINT)
def run_stopped():
    """The farm asked the run to stop (a stop file of at least one byte, as libFuzzer reads it)."""
    try:
        return os.path.getsize(farm) > 0
    except OSError:
        return False
while True:
    pid, wstatus, usage = os.wait4(proc.pid, os.WNOHANG)
    if pid:
        break
    now = time.monotonic()
    if stopped is not None:
        if not killed and now - stopped > grace:
            kill_all()
            killed = True
    elif now - start >= wall_cap:
        stop("wall-cap", now, cpu_now())
    elif farm and run_stopped():
        stop("run-stop", now, cpu_now())
    elif inited is None:
        with open(log, "rb") as f:
            f.seek(read)
            data = f.read()
        end = data.rfind(b"\n") + 1
        if INITED.search(data[:end]):
            inited, cpu_at_inited = now, cpu_now()
        read += end
    elif now - checked >= poll:
        checked, cpu = now, cpu_now()
        if cpu is not None and cpu_at_inited is not None and cpu - cpu_at_inited >= window:
            stop("cpu-window", now, cpu)
    time.sleep(min(1.0, poll))
ended = time.monotonic()
code = os.waitstatus_to_exitcode(wstatus)
with open(os.path.join(seg, "cpu"), "w") as f:
    f.write(f"{usage.ru_utime} {usage.ru_stime}\n")
with open(os.path.join(seg, "segment.json" if farm else "window.json"), "w") as f:
    json.dump({"target": target, "pin": pin, "run": run_id, "cpu_window": window, "wall_cap": wall_cap,
               "elapsed": round((stopped or ended) - start, 3),
               "load_seconds": None if inited is None else round(inited - start, 3),
               "wall_seconds": None if inited is None else round((stopped or ended) - inited, 3),
               "cpu_at_inited": cpu_at_inited, "cpu_at_stop": cpu_at_stop,
               "stopped_by": stopped_by, "killed": killed, "status": code,
               # Unrounded: rounding 1500.0004 s down to 1500.0 would let it pass (Sol r5 B1).
               "stop_to_exit": None if stopped is None else ended - stopped}, f)
sys.exit(code if code >= 0 else 128 - code)
