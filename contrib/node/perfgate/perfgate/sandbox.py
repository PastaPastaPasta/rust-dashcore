"""Running the code under test where it cannot write the gate's state or reach anything else (PF0 r2 ruling 2).

Everything a PR controls runs inside a bubblewrap sandbox: the build (build scripts, proc macros, the
compiler on the PR's sources) and every measured process (bench binaries, the node).

- The filesystem is read-only. The home directory and /run are replaced by empty mounts, so credentials
  (~/.ssh, ~/.config/gh), the user's runtime sockets (systemd, D-Bus) and /run/docker.sock are not there; only
  the toolchain, the cargo home, the repository's git directory, the harness and the gate's root are bound
  back, read-only. /tmp and /dev/shm are private.
- The only writable paths are the ones the caller names: the side's target directory for a build, a fresh
  scratch directory for a run. They are resolved and must lie under the gate's root. (No node runs here: node
  rows run on the PF0 chain, nodegate.py. `external` has no caller since PF0-N and stays for counter_hash, D45.)
- Network, PID, IPC, UTS and cgroup namespaces are private: no TCP (localhost included, so not the sccache
  server), no abstract sockets, no signals to host processes. Builds are offline; deps are fetched first.
- The environment is cleared and rebuilt from an allowlist, and sccache is bypassed (its server would run the
  compiler, and so the PR's proc macros, outside the sandbox).

So the ledger, the anchor, the cache, the reports, the shim snapshot, the copied executables, the other side's
build and every source file are out of reach. Only this tool's own process, which validates what the
children report, writes state.

What the sandbox protects is the gate's inputs and state, not the counts (DEC-76: the gate catches accidental
regressions; deliberate forgery is out of scope where defending is not cheap). The authoritative gated count is
`instructions_process`, a `perf stat` from outside the sandbox: the measured code cannot disable it or hide a child
from it, nor reach its output. perf keeps its `-o` file open in the process it starts, and a descriptor that
reaches the sandbox can be reopened through /proc/self/fd although the file is not mounted there; so every bench
counter runs its command through `launch --exec` (launch.c), which leaves only an empty stdin, stdout and stderr
(F0; node runs are counted on the PF0 chain instead: broker.py, node_launch.c). Everything else is
self-reportable by gate-aware code. The in-region counters come from the measured process itself, and strace's log,
although written outside the sandbox, follows marker syscalls the process makes (an extra `/perfgate/end` hides the
rest of a span) and misses `CLONE_UNTRACED` children.
"""

import functools
import hashlib
import os
import shutil
import stat
import subprocess
import tempfile
import threading

from . import paths

CC_FLAGS = ("-O2", "-Wall")
LAUNCH_SRC = os.path.join(os.path.dirname(os.path.realpath(__file__)), "launch.c")

# Build environment passed through when set (the fingerprint covers the build-affecting ones).
BUILD_PASSTHROUGH = ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "CARGO_BUILD_JOBS", "RUSTFLAGS",
                     "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS", "CARGO_INCREMENTAL", "RUSTC", "CC", "CXX",
                     "CFLAGS", "CXXFLAGS", "LDFLAGS")
BUILD_PREFIXES = ("CARGO_PROFILE_", "CARGO_TARGET_")


class SandboxError(RuntimeError):
    pass


def available():
    return shutil.which("bwrap") is not None


def git_dir():
    """The repository's common git directory (worktrees' .git files point into it)."""
    p = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], cwd=paths.REPO,
                       capture_output=True, text=True)
    return p.stdout.strip() or None


def readonly_binds():
    """What the code under test may read inside the home directory: the toolchain, the cargo home (registry
    sources), the repository's git directory, the harness and the gate's root."""
    home = os.path.expanduser("~")
    out = [os.environ.get("RUSTUP_HOME") or os.path.join(home, ".rustup"),
           os.environ.get("CARGO_HOME") or os.path.join(home, ".cargo"), git_dir(), paths.REPO, paths.root()]
    out = [os.path.realpath(p) for p in out if p and os.path.exists(p)]
    real_home = os.path.realpath(home)
    for p in out:  # e.g. REPO's fallback outside a git checkout: binding it would put the whole home back
        if os.path.commonpath([p, real_home]) == p:
            raise SandboxError(f"refusing to bind {p} into the sandbox: it contains the home directory "
                               f"{real_home}")
    return out


def checked_writable(path):
    """`path` resolved, which must be a directory under the gate's root (a symlinked target or scratch
    directory could otherwise bind something else writable)."""
    real = os.path.realpath(path)
    root = os.path.realpath(paths.root())
    if os.path.islink(path) or not os.path.isdir(real) or not real.startswith(root + os.sep):
        raise SandboxError(f"refusing to make {path} writable in the sandbox: it must be a real directory under "
                           f"{root}")
    return real


def wrap(argv, writable=(), cwd=None, env=None, external=()):
    """`argv` under bwrap, as described above; only `writable` (directories under the root) and `external`
    (directories outside both the root and the home directory, e.g. node-bench's data root) are writable, and
    only `env` is in the environment."""
    if not available():
        raise SandboxError("bubblewrap (bwrap) is required to run code under test")
    root = paths.root()
    for private in ("/tmp", "/dev/shm", "/run"):  # the sandbox mounts fresh, empty ones there
        if root == private or root.startswith(private + "/"):
            raise SandboxError(f"PERFGATE_ROOT {root} is under {private}, which the sandbox replaces: put it on disk")
    home = os.path.realpath(os.path.expanduser("~"))
    cmd = ["bwrap", "--ro-bind", "/", "/", "--tmpfs", home, "--tmpfs", "/run", "--dev", "/dev", "--proc", "/proc",
           "--tmpfs", "/tmp", "--tmpfs", "/dev/shm", "--unshare-net", "--unshare-pid", "--unshare-ipc",
           "--unshare-uts", "--unshare-cgroup-try", "--die-with-parent", "--new-session", "--clearenv"]
    for r in readonly_binds():
        cmd += ["--ro-bind", r, r]
    for w in writable:
        w = checked_writable(w)
        cmd += ["--bind", w, w]
    for w in external:
        real = os.path.realpath(w)
        for other in (os.path.realpath(root), home):
            if real == other or real.startswith(other + os.sep) or other.startswith(real + os.sep):
                raise SandboxError(f"refusing to make {w} writable in the sandbox: it overlaps {other}")
        cmd += ["--bind", real, real]
    for k, v in sorted((env or {}).items()):
        cmd += ["--setenv", k, v]
    if cwd:
        cmd += ["--chdir", cwd]
    return cmd + ["--", *argv]


@functools.cache
def label():
    """The security label the sandbox's processes run under (AppArmor's, from inside a bwrap like wrap()'s), or None.

    The exec of bwrap changes the process's AppArmor profile (`bwrap//&unpriv_bwrap` here), and with it the
    personality and PDEATHSIG; a policy change there moves what perf counts of bwrap without touching any
    source, so the fingerprint records it (PF0-N D68)."""
    if not available():
        return None
    for attr in ("/proc/self/attr/apparmor/current", "/proc/self/attr/current"):
        try:
            p = subprocess.run(wrap(["cat", attr]), capture_output=True, text=True, timeout=60)
        except (OSError, subprocess.TimeoutExpired, SandboxError):
            return None
        if p.returncode == 0 and p.stdout.strip():
            return p.stdout.strip()
    return None


_COMPILED = {}
_COMPILE_LOCK = threading.Lock()  # bench runs start from a thread pool, and the first ones would race to compile


def _compiled(src, stem, flags, libs, root):
    with _COMPILE_LOCK:
        key = (src, stem, flags, libs, root)
        if key not in _COMPILED:
            # The name covers everything the binary depends on, so a flag or compiler change builds a new one.
            cc = subprocess.run(["cc", "--version"], capture_output=True, text=True).stdout
            with open(src, "rb") as f:
                h = hashlib.sha256(f.read() + repr((CC_FLAGS, flags, libs, cc)).encode()).hexdigest()[:12]
            out = os.path.join(root, "lib", f"{stem}-{h}{'.so' if '-shared' in flags else ''}")
            if not os.path.exists(out):
                os.makedirs(os.path.dirname(out), exist_ok=True)
                fd, tmp = tempfile.mkstemp(prefix=os.path.basename(out) + ".", dir=os.path.dirname(out))
                os.close(fd)
                try:
                    subprocess.run(["cc", *CC_FLAGS, *flags, "-o", tmp, src, *libs], check=True)
                    os.replace(tmp, out)
                finally:
                    if os.path.exists(tmp):
                        os.remove(tmp)
            _COMPILED[key] = out
        return _COMPILED[key]


def compiled(src, stem, flags=(), libs=()):
    """`src` compiled once per source version under the root (by this tool, from its own sources: no PR code)."""
    return _compiled(src, stem, tuple(flags), tuple(libs), paths.root())


def launcher():
    return compiled(LAUNCH_SRC, "perfgate_launch")


def counted(counter, argv):
    """`argv` (a sandbox command line) under `counter` (perf or strace writing its output file), with stdin made
    /dev/null and every other descriptor but stdout and stderr closed in between: nothing of the counter's output
    reaches the code under test (F0). The caller's stdout and stderr must be pipes or files the code may write."""
    return [*counter, "--", launcher(), "--exec", "--", *argv]


def build_env(tmpdir):
    """The environment for a sandboxed cargo build: an allowlist, with sccache bypassed."""
    env = {k: v for k, v in os.environ.items() if k in BUILD_PASSTHROUGH or k.startswith(BUILD_PREFIXES)}
    env.pop("CARGO_TARGET_DIR", None)
    env.update(LANG="C", TMPDIR=tmpdir, RUSTC_WRAPPER="")
    return env


def scratch(*parts):
    """A fresh, empty directory under the root for a sandboxed process to write (nothing trusted lives there)."""
    d = os.path.join(paths.sub("sandbox"), *parts)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    return d


def read_regular(path):
    """The contents of a file a sandboxed process wrote, refusing anything but a regular file (a symlink there
    would make the gate read a file of the PR's choosing)."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise SandboxError(f"{path} is not a regular file")
        with os.fdopen(fd, "r", errors="replace", closefd=False) as f:
            return f.read()
    finally:
        os.close(fd)
