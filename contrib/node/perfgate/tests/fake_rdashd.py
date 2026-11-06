#!/usr/bin/python3
"""A stand-in for rdashd in the broker tests: it prints what it sees of the jail as one JSON line, then acts on
its flags.

    -version      exit 0 at once
    -refuse       print rdashd's refusal line, exit 1
    -litter       leave files in the datadir, exit 0
    -spam-alloc   write 100 KiB of junk to the alloc fd first
    -spam-lines   write 64 Ki empty lines to the alloc fd first
    -orphan       leave a setsid grandchild behind, exit 0
    -hang         ignore SIGTERM after ready
    -reuse5       put a datadir file on fd 5 (the alloc fd's number) before running
    -fill         write the datadir's filesystem full (bytes, then inodes), exit 1 on ENOSPC
    -alloc-mib=N  touch N MiB of memory, then go on
    -forks=N      start N sleeping children, then go on
    -spew-mib=N   write N MiB to stdout, then go on
    otherwise     touch <datadir>/ready, exit 0 on SIGTERM (3 after 60 s)

Tests that need a node refusing real options write a copy with REFUSE set ({option: owner}): the first argv token
setting one of them gets rdashd's refusal line and exit 1. A copy with CHILD_MIB set runs a child that touches that
many MiB without the alloc preload (memory the preload does not see, plan §10's tree-memory) and reaps it first.
"""

import os

fds = sorted(int(x) for x in os.listdir("/proc/self/fd"))  # before ctypes: libffi keeps its own .so open

import ctypes  # noqa: E402
import errno  # noqa: E402
import json  # noqa: E402
import signal  # noqa: E402
import sys  # noqa: E402
import time  # noqa: E402

REFUSE = {}
CHILD_MIB = 0
args = sys.argv[1:]
for opt, owner in REFUSE.items():
    if any(a == opt or a.startswith(opt + "=") for a in args):
        print(f"Error: {opt} is not supported by rdashd yet ({owner}).", file=sys.stderr)
        sys.exit(1)
dd = [a.split("=", 1)[1] for a in args if a.startswith("-datadir=")][0]
try:
    pid = os.fork()
    if pid == 0:
        try:
            os.unshare(os.CLONE_NEWUSER)
            os._exit(0)
        except OSError as e:
            os._exit(e.errno)
    nested = os.waitpid(pid, 0)[1] >> 8
except OSError as e:
    nested = e.errno
libc = ctypes.CDLL(None, use_errno=True)


def errname(ok):
    return "ok" if ok else errno.errorcode.get(ctypes.get_errno(), "?")


mode = ctypes.c_int(-1)
mask = (ctypes.c_ulong * 16)()
get_mempolicy = {"x86_64": 239, "aarch64": 236}[os.uname().machine]
ok = libc.syscall(get_mempolicy, ctypes.byref(mode), mask, 1025, None, 0) == 0
mempolicy = [mode.value, mask[0]] if ok else errname(False)  # [MPOL_BIND = 2, node mask]
cgroup2 = errname(libc.mount(b"none", b"/tmp", b"cgroup2", 0, None) == 0)
try:
    with open("/proc/sys/kernel/hostname", "w") as f:
        f.write("x")
    proc_sys = "ok"
except OSError as e:
    proc_sys = errno.errorcode.get(e.errno, "?")


status = {line.split(":")[0]: line.split(":", 1)[1].strip() for line in open("/proc/self/status")}
print(json.dumps({
    "argv": args, "pid": os.getpid(), "cpus": sorted(os.sched_getaffinity(0)),
    "personality": open("/proc/self/personality").read().strip(),
    "fds": fds, "datadir": sorted(os.listdir(dd)),
    "cwd": os.getcwd(), "env": dict(os.environ), "nested_userns": errno.errorcode.get(nested, nested),
    "cgroup": open("/proc/self/cgroup").read().strip(), "sysfs_cgroup": os.path.exists("/sys/fs/cgroup"),
    "mempolicy": mempolicy, "mount_cgroup2": cgroup2, "proc_sys_write": proc_sys,
    "status": {k: status[k] for k in ("CapBnd", "CapEff", "NoNewPrivs", "Cpus_allowed_list")}}), flush=True)
if CHILD_MIB:
    import subprocess  # noqa: E402

    env = {k: v for k, v in os.environ.items() if k not in ("LD_PRELOAD", "PERFGATE_ALLOC_FD")}
    subprocess.run([sys.executable, "-c", f"b = bytearray({CHILD_MIB} << 20)\nfor o in range(0, len(b), 4096): b[o] = 1"],
                   env=env, check=True)
if "-reuse5" in args:
    fd = os.open(os.path.join(dd, "data"), os.O_WRONLY | os.O_CREAT, 0o600)
    os.dup2(fd, 5)
    os.close(fd)
if "-fill" in args:
    try:
        with open(os.path.join(dd, "fill"), "wb") as f:
            while True:
                f.write(b"x" * (1 << 20))
                f.flush()
    except OSError as e:
        if e.errno != errno.ENOSPC:
            raise
    try:
        for i in range(10 ** 7):
            open(os.path.join(dd, f"i{i}"), "w").close()
    except OSError as e:
        if e.errno != errno.ENOSPC:
            raise
    print("Error: No space left on device", file=sys.stderr)
    sys.exit(1)
for a in args:
    if a.startswith("-alloc-mib="):
        block = bytearray(int(a.split("=")[1]) << 20)
        for off in range(0, len(block), 4096):
            block[off] = 1
    if a.startswith("-spew-mib="):
        chunk = b"y" * (1 << 20)
        for _ in range(int(a.split("=")[1])):
            os.write(1, chunk)
    if a.startswith("-forks="):
        for _ in range(int(a.split("=")[1])):
            if os.fork() == 0:
                time.sleep(600)
                os._exit(0)
if "-spam-alloc" in args:
    os.write(5, b"x" * 100 * 1024)
if "-spam-lines" in args:
    os.write(5, b"\n" * 64 * 1024)
if "-orphan" in args:
    if os.fork() == 0:
        os.setsid()
        time.sleep(600)
        os._exit(0)
    sys.exit(0)
if "-version" in args:
    sys.exit(0)
if "-refuse" in args:
    print("Error: -refuse is not supported by rdashd yet (XX1).", file=sys.stderr)
    sys.exit(1)
if "-litter" in args:
    for name in ("blocks", "settings.json"):
        open(os.path.join(dd, name), "w").close()
    sys.exit(0)
stop = []
signal.signal(signal.SIGTERM, (lambda *_: None) if "-hang" in args else (lambda *_: stop.append(1)))
open(os.path.join(dd, "ready"), "w").close()
t = time.time()
while not stop and time.time() - t < 60:
    time.sleep(0.05)
sys.exit(0 if stop else 3)
