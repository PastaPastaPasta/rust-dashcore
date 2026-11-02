"""Where the tool, the repository and the gate's state live.

All state (worktrees, builds, measurements, reports, the ledger) is under PERFGATE_ROOT (default
/work/perfgate). Benchmark processes get a private TMPDIR there, removed after each run.
"""

import fcntl
import hashlib
import json
import os
import subprocess
import sys
import time
from contextlib import contextmanager

TOOL_DIR = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
REPO = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=TOOL_DIR, capture_output=True,
                      text=True).stdout.strip() or os.path.dirname(os.path.dirname(os.path.dirname(TOOL_DIR)))


def root():
    r = os.path.realpath(os.environ.get("PERFGATE_ROOT") or "/work/perfgate")
    os.makedirs(r, exist_ok=True)
    return r


def sub(*parts):
    d = os.path.join(root(), *parts)
    os.makedirs(d, exist_ok=True)
    return d


@contextmanager
def locked():
    """One gate at a time per root: the worktrees and the shared target/ dirs are not concurrency-safe.

    The holder records itself in .lock.holder while it has the lock (DEC-707 c), so a waiting queue can show who
    holds the lane. The record is advisory only: exclusion is the flock, which the kernel drops with its holder.
    """
    with open(os.path.join(root(), ".lock"), "w") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        try:
            _write_holder()
            yield
        finally:
            _clear_holder()
            fcntl.flock(f, fcntl.LOCK_UN)


def _holder_path():
    return os.path.join(root(), ".lock.holder")


def _starttime(pid):
    """The process's start time in clock ticks since boot (/proc/PID/stat field 22): with the pid, it names one
    process, so a reused pid doesn't read as the holder."""
    with open(f"/proc/{pid}/stat") as f:
        return int(f.read().rsplit(")", 1)[1].split()[19])


def _read_holder():
    try:
        with open(_holder_path()) as f:
            rec = json.load(f)
    except (OSError, ValueError):
        return None
    return rec if isinstance(rec, dict) else None


def _write_holder():
    """Best effort: a record that can't be written (a full disk, say) costs the queue its view, not the run."""
    path = _holder_path()
    try:
        rec = {"pid": os.getpid(), "starttime": _starttime(os.getpid()), "argv": sys.argv,
               "start": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
        with open(path + ".tmp", "w") as f:
            json.dump(rec, f)
        os.replace(path + ".tmp", path)
    except (OSError, ValueError, IndexError) as e:
        print(f"perfgate: lane holder record not written: {e}", file=sys.stderr)
        try:
            os.remove(path + ".tmp")
        except OSError:
            pass


def _clear_holder():
    """Removes the record only while it is still this process's, so a later holder's record survives."""
    rec = _read_holder()
    if rec is not None and rec.get("pid") == os.getpid():
        try:
            os.remove(_holder_path())
        except OSError:
            pass


def lane_holder():
    """The last lock holder's record, with `alive` (that process still runs), or None when there is no record.

    Advisory: `alive` is false for a holder killed before its cleanup ran, and for a moment after a new holder takes
    the lock but before it writes its record. Whether the lock is free is the flock's answer, never this one.
    """
    rec = _read_holder()
    if rec is None:
        return None
    pid = rec.get("pid")
    try:  # an int pid only: "self" would read the reader's own /proc entry
        rec["alive"] = type(pid) is int and pid > 0 and _starttime(pid) == rec.get("starttime")
    except (OSError, ValueError, IndexError):
        rec["alive"] = False
    return rec


def tree_hash(*dirs, exts=(".rs", ".py", ".c", ".toml")):
    """Content hash of the tool's own sources: measurement caches are invalid when the harness changes."""
    h = hashlib.sha256()
    for d in dirs:
        for base, subdirs, files in sorted(os.walk(d)):
            subdirs[:] = sorted(s for s in subdirs if s not in ("target", "__pycache__", "tests"))
            for f in sorted(files):
                if f.endswith(exts) and f != "Cargo.lock":
                    p = os.path.join(base, f)
                    h.update(os.path.relpath(p, d).encode())
                    with open(p, "rb") as fh:
                        h.update(fh.read())
    return h.hexdigest()[:16]
