"""Where the tool, the repository and the gate's state live.

All state (worktrees, builds, measurements, reports, the ledger) is under PERFGATE_ROOT (default
/work/perfgate). Benchmark processes get a private TMPDIR there, removed after each run.
"""

import fcntl
import hashlib
import os
import subprocess
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
    """One gate at a time per root: the worktrees and the shared target/ dirs are not concurrency-safe."""
    with open(os.path.join(root(), ".lock"), "w") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(f, fcntl.LOCK_UN)


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
