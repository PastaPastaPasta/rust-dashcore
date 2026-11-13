#!/usr/bin/env python3
# EMBARGO-1
"""run.sh's CPU-time driver: one libFuzzer process (tree) that stops on its own CPU time.

  window_driver.py [--encoding ENCODING] [--farm RUN_STOP [--shards J]] SEG WINDOW WALL_CAP TMP TARGET PIN RUN_ID CMD...

A certify window (the default; DEC-186 amendments 2 and 3) is one process that runs until it has
used WINDOW seconds of its own CPU time (utime and stime from /proc, not its reaped children's; Sol
r2 M1) after its INITED, checked about every 10 s. A farm segment (`--farm`, BL-228) is a budgeted
run's libFuzzer process with its -jobs workers: it runs until its whole process tree has used WINDOW
CPU seconds from its start (its tracked processes' utime and stime and their reaped children's;
below), checked about every 2 s. Wall time never ends either: on a loaded box at the farm's priority a segment that
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
A chain target's ENCODING (O2b-4 §3a.3) goes to it as NODE_L1_CHAIN_ENCODING.

A farm segment's driver is the child subreaper of its processes (BL-820), or the segment does not
start: libFuzzer reaps no -jobs worker it leaves behind when it aborts or is killed, and such a
worker is reparented to the driver, not to init. At an unsignalled exit every process still running
gets the stop file and up to ORPHAN_WAIT seconds to stop by itself; at a signalled one it gets none.
Then it is killed (`orphans_killed`: it would
hold the leg lock and spend CPU that nobody counts), and every child of the driver is reaped.
libFuzzer's rusage and the adopted processes' are disjoint and hold each process of the segment
once. SEG/cpu is the larger of their sum and the tracked figure, which samples the driver's
descendants every poll keyed by pid and start time (a process that dies unreaped between two samples
loses at most one poll of its CPU there) and covers a process the bounded reap missed. A kill stops
the processes and samples them first. A segment's INITED fields are null; `process_inited` has each
fuzzing process's, from its own log (BL-826), and the count of inputs it loaded (`loaded`, libFuzzer's
`seed corpus: files:` line before its INITED; null without one, BL-841b).

A shard segment (`--shards J`, BL-841b) runs J libFuzzer processes instead of one -jobs parent: CMD
once per worker k, with every `{shard}` in it replaced by k, its stdout and stderr in SEG/fuzz-<k>.log
(the names -jobs uses). The driver starts them through a small parent of its own (this file with
`--shard-parent`), which plays the -jobs parent's part: it is the process the driver waits for and
signals, it passes a SIGINT or SIGTERM on to the workers, it copies each worker's log to its own
output (fuzz.out) when that worker exits, as libFuzzer's parent does, and a worker that exits non-zero
makes it write SEG/stop, so that the others stop at their loop boundary and the segment ends; its
exit status is the first non-zero one, else 0. Each worker dies with that parent (PR_SET_PDEATHSIG).
Everything else (the CPU of the whole tree, the stop file, the orphan kill, `process_inited`) is the
farm segment's as above.
"""
import ctypes, json, os, re, signal, subprocess, sys, time
def shard_parent(seg, jobs, cmd):
    """A shard segment's parent (above): starts CMD for each of `jobs` workers and waits for all."""
    workers = {}
    def deathsig():
        ctypes.CDLL(None).prctl(1, signal.SIGKILL)  # PR_SET_PDEATHSIG
    for k in range(jobs):
        with open(os.path.join(seg, f"fuzz-{k}.log"), "wb") as out:
            p = subprocess.Popen([a.replace("{shard}", str(k)) for a in cmd], cwd=seg, stdin=subprocess.DEVNULL,
                                 stdout=out, stderr=subprocess.STDOUT, close_fds=False, preexec_fn=deathsig)
        workers[p.pid] = k
    def forward(sig, _):
        for pid in workers:
            try:
                os.kill(pid, sig)
            except OSError:
                pass
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, forward)
    code, left = 0, dict(workers)
    while left:
        pid, wstatus = os.wait()
        if left.pop(pid, None) is None:
            continue
        status = os.waitstatus_to_exitcode(wstatus)
        with open(os.path.join(seg, f"fuzz-{workers[pid]}.log"), "rb") as log:
            sys.stdout.buffer.write(log.read())
        sys.stdout.buffer.flush()
        if status != 0 and code == 0:
            code = status if status > 0 else 128 - status
            with open(os.path.join(seg, "stop"), "wb") as f:
                f.write(b"stop\n")
    sys.exit(code)
if sys.argv[1:2] == ["--shard-parent"]:
    shard_parent(sys.argv[2], int(sys.argv[3]), sys.argv[4:])
args = sys.argv[1:]
encoding = None
if args[:1] == ["--encoding"]:
    encoding, args = args[1], args[2:]
farm = None
if args[:1] == ["--farm"]:
    farm, args = args[1], args[2:]
shards = 0
if farm and args[:1] == ["--shards"]:
    if not re.fullmatch(r"[1-9][0-9]{0,2}", args[1]):
        sys.exit("window_driver: --shards takes a number of workers from 1 to 999")
    shards, args = int(args[1]), args[2:]
seg, window, wall_cap, tmp, target, pin, run_id, *cmd = args
window, wall_cap = int(window), int(wall_cap)
GRACE, POLL = 1500, 10  # run.sh's $stop_grace, and the seconds between CPU checks
# The longest a farm segment's processes left running at its unsignalled exit get to stop, and the
# exit reap's bound: with a loop's latency they stay under the farm's SIGINT-to-SIGKILL step
# (Farm.int_wait), so a slice the farm stops always records its segment (fuzzctl_test pins this).
ORPHAN_WAIT, REAP_WAIT = 15, 10
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
orphan_wait = shortened("NODE_FUZZ_ORPHAN_WAIT", ORPHAN_WAIT, ORPHAN_WAIT) if farm else None
env = {k: os.environ[k] for k in ("PATH", "HOME", "DASH_EMBARGO_DIR")}
env.update(TMPDIR=tmp, NODE_FUZZ_PIN=pin, NODE_FUZZ_RUN=run_id, RUST_BACKTRACE="1")
if encoding:
    env["NODE_L1_CHAIN_ENCODING"] = encoding
log, stop_file = os.path.join(seg, "fuzz.out"), os.path.join(seg, "stop")
if farm:
    # A signal to the run's process group (BL-173's fallbacks) reaches libFuzzer and its workers
    # as well, and libFuzzer answers it with its own exit; the driver outlives it to record the
    # segment. (A handler, not SIG_IGN, which libFuzzer would inherit: an exec resets handlers.)
    # A signalled segment skips the exit's stop wait: its workers got the same signal, libFuzzer's
    # own interrupt, and the farm's SIGKILL follows within Farm.int_wait.
    signalled = False
    def on_signal(*_):
        global signalled
        signalled = True
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, on_signal)
    # Every process of the segment whose parent dies is reparented to this driver (BL-820), which
    # kills, reaps and counts it at the exit, whenever it started. Without that the segment would
    # leave workers fuzzing uncounted, so it does not start.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        sys.exit(f"window_driver: the segment cannot adopt its orphans (PR_SET_CHILD_SUBREAPER: "
                 f"{os.strerror(ctypes.get_errno())}); not started")
with open(log, "wb") as out:
    # libFuzzer dies with this driver (PR_SET_PDEATHSIG), so it is never left fuzzing on its own.
    # A farm segment keeps run.sh's leg lock (fd 8) open in libFuzzer and its workers, as it did
    # under /usr/bin/time: the lock is held until the last worker is gone.
    # A shard segment's process is its parent (`shard_parent`), which starts the workers.
    argv = [sys.executable, "-I", os.path.abspath(__file__), "--shard-parent", seg, str(shards), *cmd] if shards else cmd
    proc = subprocess.Popen(argv, cwd=seg, env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT,
                            close_fds=not farm, preexec_fn=lambda: ctypes.CDLL(None).prctl(1, signal.SIGKILL))
def stat_fields(pid):
    """/proc/<pid>/stat after the command: state, ppid, ..., utime (11), stime, cutime, cstime (14)."""
    try:
        return open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    except (OSError, IndexError):
        return None
TICK = os.sysconf("SC_CLK_TCK")
def processes():
    """{pid: stat fields} of every process."""
    return {int(e): f for e in os.listdir("/proc") if e.isdigit() and (f := stat_fields(e))}
def tree(table, root):
    """The pids of `root` and its descendants in `table`."""
    children = {}
    for pid, fields in table.items():
        children.setdefault(int(fields[1]), []).append(pid)
    members, queue = [], [root]
    while queue:
        pid = queue.pop()
        queue.extend(children.get(pid, []))
        if pid in table:
            members.append(pid)
    return members
# A farm segment's tracked processes (BL-820): (pid, start time) -> [utime and cutime, stime and
# cstime (ticks), the parent's key, the last sample that read it, the last that read it running (0
# if none: `tracked_cpu` never takes a zombie for a child's reaper)].
tracked, samples, fuzzer_exe, fuzzer_reaped = {}, 0, None, False
def own_cpu(fields):
    return sum(int(x) for x in fields[11:13]) / TICK
def sample():
    """Reads the tree, the driver's descendants (libFuzzer's and the processes the driver adopted),
    and every tracked process that is still the same one into `tracked`; {pid: key} of those found.
    Notes libFuzzer's binary while it runs (`owner`); once wait4 has reaped it, its pid may name
    another process."""
    global samples, fuzzer_exe
    samples += 1
    table = processes()
    me = os.getpid()
    found = {pid: (pid, table[pid][19]) for pid in tree(table, me) if pid != me}
    if proc.pid in found and not fuzzer_reaped:
        try:
            fuzzer_exe = os.readlink(f"/proc/{proc.pid}/exe")
        except OSError:
            pass
    found.update({key[0]: key for key in tracked if key[0] in table and table[key[0]][19] == key[1]})
    for pid, key in found.items():
        f = table[pid]
        if f[0] == "Z" and key in tracked:
            # Its figure now holds the children it reaped since it was last read running, which still
            # count by their own last samples: it keeps that figure, and its parent has not reaped it.
            tracked[key][3] = samples
            continue
        # A process whose parent is not found was reparented out of the tree: nothing tracked reaps it.
        tracked[key] = [int(f[11]) + int(f[13]), int(f[12]) + int(f[14]), found.get(int(f[1])), samples,
                        0 if f[0] == "Z" else samples]
    return found
def tracked_cpu():
    """(user, system) seconds of the tracked processes: each one's last sample, unless an ancestor
    was seen running after it, whose reaped children's times then hold it (a process leaves /proc
    while an ancestor runs only by being reaped up the chain)."""
    def reaped(key):
        last, parent, seen = tracked[key][3], tracked[key][2], set()
        while parent is not None and parent not in seen:
            if tracked[parent][4] > last:
                return True
            last = max(last, tracked[parent][3])  # an unreaped zombie still holds what it reaped
            seen.add(parent)
            parent = tracked[parent][2]
        return False
    kept = [v for k, v in tracked.items() if not reaped(k)]
    return sum(v[0] for v in kept) / TICK, sum(v[1] for v in kept) / TICK
def signal_all(found, sig):
    """`sig` to each of `found` ({pid: key}) that is still the process its key names and has not
    exited; how many were signalled."""
    count = 0
    for pid, key in found.items():
        try:
            fd = os.pidfd_open(pid)
        except OSError:
            continue
        try:
            fields = stat_fields(pid)
            if fields and fields[19] == key[1] and fields[0] != "Z":
                signal.pidfd_send_signal(fd, sig)
                count += 1
        except OSError:
            pass
        finally:
            os.close(fd)
    return count
def running(found):
    """Those of `found` ({pid: key}) still running as the process their key names."""
    return {pid: key for pid, key in found.items()
            if (f := stat_fields(pid)) and f[19] == key[1] and f[0] != "Z"}
def kill():
    """SIGKILL for the tree and every tracked process still running, after a SIGSTOP and a sample,
    so that their CPU up to the kill is counted. How many were killed. (The exit's `reap` waits for
    them, and so for the leg lock.)"""
    signal_all(sample(), signal.SIGSTOP)
    return signal_all(sample(), signal.SIGKILL)
def reap(wait=REAP_WAIT):
    """wait4 for every child of the driver, libFuzzer reaped, for up to `wait` seconds: the summed
    (user, system) seconds of their rusage, each holding the descendants that child reaped."""
    deadline, u, s = time.monotonic() + wait, 0.0, 0.0
    while True:
        try:
            pid, _, usage = os.wait4(-1, os.WNOHANG)
        except ChildProcessError:
            return u, s
        if pid:
            u, s = u + usage.ru_utime, s + usage.ru_stime
        elif time.monotonic() >= deadline:
            return u, s
        else:
            time.sleep(0.02)
def kill_all():
    """SIGKILL for the process, and for a farm segment's workers: they have no parent-death signal
    (libFuzzer starts them with system()), and would fuzz on with nobody to stop them."""
    if farm:
        kill()
        return
    try:
        os.kill(proc.pid, signal.SIGKILL)
    except OSError:
        pass
def cpu_now():
    """The process's own CPU seconds (utime and stime, all its threads; not its reaped children's),
    or None once it is gone. A farm segment's is the tree's tracked figure."""
    if farm:
        return sum(tracked_cpu()) if proc.pid in sample() and not fuzzer_reaped else None
    fields = stat_fields(proc.pid)
    return None if fields is None else own_cpu(fields)
# A farm segment's fuzzing processes (BL-826): -jobs=N's workers or a shard segment's, each with its
# own log, else libFuzzer itself.
jobs = shards or next((int(a[6:]) for a in reversed(cmd) if re.fullmatch(r"-jobs=[1-9][0-9]*", a)), 0)
names = [f"fuzz-{c}.log" for c in range(jobs)] or ["fuzz.out"]
process_inited = [{"log": name, "pid": None, "seconds_to_inited": None, "cpu_at_inited": None, "loaded": None}
                  for name in names] if farm else []
offsets = [0] * len(process_inited)
def owner(name, found):
    """The pid of the fuzzing process that writes SEG/`name`, or None: libFuzzer itself for
    fuzz.out; for fuzz-C.log the tracked process running libFuzzer's binary (not the sh that
    system() starts it under, which has the log as its output as well) whose output is that log;
    in a shard segment, the child of its parent (`shard_parent` starts each worker itself) whose
    output is that log."""
    if name == "fuzz.out":
        return proc.pid if proc.pid in found and not fuzzer_reaped else None
    path = os.path.realpath(os.path.join(seg, name))
    for pid in found:
        try:
            ours = ((stat_fields(pid) or [None, None])[1] == str(proc.pid) if shards
                    else os.readlink(f"/proc/{pid}/exe") == fuzzer_exe)
            if ours and os.readlink(f"/proc/{pid}/fd/1") == path:
                return pid
        except OSError:
            pass
    return None
def scan(now):
    """Reads each fuzzing process's new whole lines for its loaded count and its first INITED."""
    found = None
    for i, p in enumerate(process_inited):
        if p["seconds_to_inited"] is not None:
            continue
        try:
            with open(os.path.join(seg, p["log"]), "rb") as f:
                f.seek(offsets[i])
                data = f.read()
        except OSError:
            continue
        end = data.rfind(b"\n") + 1
        offsets[i] += end
        at = INITED.search(data[:end])
        seed = p["loaded"] is None and SEED.search(data, 0, at.start() if at else end)
        if seed:
            p["loaded"] = int(seed.group(1))
        if at:
            found = sample() if found is None else found
            pid = owner(p["log"], found)
            fields = stat_fields(pid) if pid else None
            p["seconds_to_inited"] = round(now - start, 3)
            if fields and fields[19] == found[pid][1]:
                p["pid"], p["cpu_at_inited"] = pid, own_cpu(fields)
INITED = re.compile(rb"^#\d+\s+INITED\b", re.M)  # the status line, as fuzzctl reads it
SEED = re.compile(rb"^INFO: seed corpus: files: (\d+) ", re.M)  # printed once, before the corpus loads
start = checked = time.monotonic()
inited = stopped = stopped_by = cpu_at_inited = cpu_at_stop = None
# The CPU the window counts from: a certify window's at its INITED; a segment's allotment counts
# from its start, load included (BL-228).
base = 0.0 if farm else None
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
    if farm:
        scan(now)
    if stopped is not None:
        if not killed and now - stopped > grace:
            kill_all()
            killed = True
        elif farm and now - checked >= poll:
            checked = now
            sample()
    elif now - start >= wall_cap:
        stop("wall-cap", now, cpu_now())
    elif farm and run_stopped():
        stop("run-stop", now, cpu_now())
    elif not farm and inited is None:
        with open(log, "rb") as f:
            f.seek(read)
            data = f.read()
        end = data.rfind(b"\n") + 1
        if INITED.search(data[:end]):
            inited, cpu_at_inited = now, cpu_now()
            base = cpu_at_inited
        read += end
    elif now - checked >= poll:
        checked, cpu = now, cpu_now()
        if cpu is not None and base is not None and cpu - base >= window:
            stop("cpu-window", now, cpu)
    time.sleep(min(1.0, poll))
ended = time.monotonic()
code = os.waitstatus_to_exitcode(wstatus)
cpu = (usage.ru_utime, usage.ru_stime)
fuzzer_reaped = True
record = {}
if farm:
    # The processes still running have outlived libFuzzer (BL-820); the driver adopted those whose
    # parent died. Once killed, every child of the driver is reaped: libFuzzer (above) and they
    # reaped every process of the segment once between them. A zombie's sample is final, and a
    # zombie is never taken for the parent that reaped a child.
    # Those still running first get the stop file, as libFuzzer's own stop: a discovery in flight
    # is printed and saved, and their executions are counted. They are sampled every poll for up to
    # `orphan_wait` seconds, then killed. A signalled segment (above) is killed at once.
    live = running(sample())
    if live and not signalled:
        with open(stop_file, "wb") as f:
            f.write(b"stop\n")
        checked = time.monotonic()
        deadline = checked + orphan_wait
        while live and not signalled and time.monotonic() < deadline:
            time.sleep(0.1)
            if time.monotonic() - checked >= poll:
                checked, live = time.monotonic(), running(sample())
            else:
                live = running(live)
    orphans = kill()
    adopted = reap()
    sample()
    scan(time.monotonic())
    tree_figure = tracked_cpu()
    reaped = (cpu[0] + adopted[0], cpu[1] + adopted[1])
    reaped_cpu, tree_cpu = sum(reaped), sum(tree_figure)
    source = "tree" if tree_cpu > reaped_cpu else "wait4"
    record = {"cpu_source": source, "cpu_wait4": round(sum(cpu), 3), "cpu_adopted": round(sum(adopted), 3),
              "cpu_tree": round(tree_cpu, 3), "orphans_killed": orphans, "process_inited": process_inited}
    cpu = tree_figure if source == "tree" else reaped
with open(os.path.join(seg, "cpu"), "w") as f:
    f.write(f"{cpu[0]} {cpu[1]}\n")
with open(os.path.join(seg, "segment.json" if farm else "window.json"), "w") as f:
    json.dump({"target": target, "pin": pin, "run": run_id, "cpu_window": window, "wall_cap": wall_cap,
               "elapsed": round((stopped or ended) - start, 3),
               "load_seconds": None if inited is None else round(inited - start, 3),
               "wall_seconds": None if inited is None else round((stopped or ended) - inited, 3),
               "cpu_at_inited": cpu_at_inited, "cpu_at_stop": cpu_at_stop,
               "stopped_by": stopped_by, "killed": killed, "status": code,
               # Unrounded: rounding 1500.0004 s down to 1500.0 would let it pass (Sol r5 B1).
               "stop_to_exit": None if stopped is None else ended - stopped, **record}, f)
sys.exit(code if code >= 0 else 128 - code)
