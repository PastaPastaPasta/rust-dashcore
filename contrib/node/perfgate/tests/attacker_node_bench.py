"""An attacker in node-bench's place (plan rev4.1 §11, D6): it runs one real measured spawn and many version spawns
through the stub, and also tries to reach the PR binary every other way. It prints one JSON line of findings.

    python3 -I attacker_node_bench.py CONFIG_JSON

CONFIG: {"port", "candidates" (host paths of the exe), "size", "sha256", "marker", "versions" (spawns to make)}.
Runs inside the node-bench view; the marker PR binary reports every start to the UDP listener here.
"""

import ctypes
import hashlib
import json
import os
import re
import signal
import socket
import stat
import subprocess
import sys
import threading
import time

STUB, SOCK = "/pf0/bin/node-launch", "/pf0/ctl/broker"
libc = ctypes.CDLL(None, use_errno=True)


def errname(e):
    import errno

    return errno.errorcode.get(e, str(e))


def attempt(fn):
    try:
        fn()
        return "ok"
    except OSError as e:
        return errname(e.errno)


def stub(cls, *argv, **kw):
    measure = "counter" if cls == "run" else "none"
    return subprocess.Popen([STUB, "--client", "--class", cls, "--measure", measure, "--", "x", *argv],
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kw)


def raw(payload, fds=None):
    """One request outside the stub; the broker's first reply (None when it closed)."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    s.settimeout(60)
    s.connect(SOCK)
    data = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
    with open(os.devnull, "rb") as r, open(os.devnull, "wb") as w:
        send = fds if fds is not None else [r.fileno(), w.fileno(), w.fileno()]
        try:
            socket.send_fds(s, [data], send)
            reply = s.recv(65536)
        except TimeoutError:
            return {"timeout": True}  # no answer is not a refusal
        except OSError as e:
            return {"oserror": errname(e.errno)}
        finally:
            s.close()
    return json.loads(reply) if reply else None


def req(cls, argv, **extra):
    return {"class": cls, "measure": "counter" if cls == "run" else "none", "cwd": "/pf0/run", "argv": argv, **extra}


def main():
    with open(sys.argv[1]) as f:
        cfg = json.load(f)
    out = {}
    grams = []
    lst = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    lst.bind(("127.0.0.1", cfg["port"]))
    lst.settimeout(0.2)
    done = threading.Event()

    def listen():
        while not done.is_set():
            try:
                grams.append(json.loads(lst.recv(4096)))
            except TimeoutError:
                continue

    t = threading.Thread(target=listen)
    t.start()

    # argv and env: the only paths are the stub's and the view's own
    strings = sys.argv + [f"{k}={v}" for k, v in os.environ.items()]
    out["paths"] = sorted({p for s in strings for p in re.findall(r"/[\w.+-]+(?:/[\w.+-]+)*", s)})

    # /proc of every visible pid
    links, mem = set(), {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        for name in ("exe", "root", "cwd"):
            try:
                links.add(os.readlink(f"/proc/{d}/{name}"))
            except OSError:
                pass
        for sub in ("fd", "map_files"):
            try:
                for e in os.listdir(f"/proc/{d}/{sub}"):
                    try:
                        links.add(os.readlink(f"/proc/{d}/{sub}/{e}"))
                    except OSError:
                        pass
            except OSError:
                pass
        mem[d] = attempt(lambda d=d: os.close(os.open(f"/proc/{d}/mem", os.O_RDONLY)))
    out["proc_links"] = sorted(links)
    out["visible_pids"] = len(mem)

    # the exe's host paths, and the jail's
    out["exec"] = {p: attempt(lambda p=p: subprocess.run([p, "-version"], capture_output=True, timeout=30))
                   for p in [*cfg["candidates"], "/jail/rdashd"]}

    # every visible filesystem: a file with the exe's size and sha256, or an ELF with the marker outside /usr
    found = []
    for root, dirs, files in os.walk("/", onerror=lambda e: None):
        if root == "/":
            dirs[:] = [x for x in dirs if x not in ("proc", "sys", "dev")]
        for name in files:
            p = os.path.join(root, name)
            try:
                st = os.lstat(p)
                if not stat.S_ISREG(st.st_mode) or p == sys.argv[1]:
                    continue
                if st.st_size == cfg["size"]:
                    with open(p, "rb") as f:
                        if hashlib.sha256(f.read()).hexdigest() == cfg["sha256"]:
                            found.append(p)
                elif not p.startswith("/usr/") and st.st_size < (64 << 20):
                    with open(p, "rb") as f:
                        data = f.read()
                    if data[:4] == b"\x7fELF" and cfg["marker"].encode() in data:
                        found.append(p)
            except OSError:
                continue
    out["found"] = found

    # capabilities
    CLONE_NEWUSER, PTRACE_ATTACH, PTRACE_DETACH, WALL = 0x10000000, 16, 17, 0x40000000

    def call(fn, *a):
        return "ok" if fn(*a) == 0 else errname(ctypes.get_errno())

    out["caps"] = {
        "unshare_user": call(libc.unshare, CLONE_NEWUSER),
        "mount": call(libc.mount, b"none", b"/pf0/run", b"tmpfs", 0, None),
        "umount": call(libc.umount2, b"/pf0/run", 0),
        "setns": call(libc.setns, os.open("/proc/1/ns/mnt", os.O_RDONLY), 0),
        # the view's own pid 1 (bwrap, dumpable, holding nothing protected: D27, D85); let it go again, or the
        # view would stay stopped until the invocation's bound
        "ptrace_init": call(libc.ptrace, PTRACE_ATTACH, 1, None, None),
    }
    if out["caps"]["ptrace_init"] == "ok":
        os.waitpid(1, WALL)
        libc.ptrace(PTRACE_DETACH, 1, None, None)
        os.kill(1, signal.SIGCONT)

    # one real measured spawn
    p = stub("run", "-datadir=/pf0/run/node")
    end = time.monotonic() + 120
    while not os.path.exists("/pf0/run/node/ready") and p.poll() is None and time.monotonic() < end:
        time.sleep(0.05)
    p.send_signal(signal.SIGTERM)
    out["run_exit"] = p.wait(120)

    # the broker: requests it must refuse
    os.symlink("/pf0/run/node", "/pf0/run/link")
    dirfd = os.open("/pf0/run", os.O_RDONLY | os.O_DIRECTORY)
    w = os.open(os.devnull, os.O_WRONLY)
    bad = {
        "outside RUN": req("start", ["-datadir=/tmp/x"]),
        "datadir not a class's": req("start", ["-datadir=/pf0/run/link"]),
        "unknown key": req("version", ["-datadir=/pf0/run/version", "-version"], extra=1),
        "exe field": req("version", ["-datadir=/pf0/run/version", "-version"], exe="/usr/bin/sh"),
        "bind field": req("version", ["-datadir=/pf0/run/version", "-version"], bind=["/", "/x"]),
        "bwrap field": req("version", ["-datadir=/pf0/run/version", "-version"], bwrap=["--bind", "/", "/"]),
        "second measured": req("run", ["-datadir=/pf0/run/node"]),
        "oversized": b"{" + b" " * (65 * 1024) + b"}",
    }
    out["refused"] = {k: raw(v) for k, v in bad.items()}
    out["refused"]["dir fd as stdin"] = raw(req("version", ["-datadir=/pf0/run/version", "-version"]),
                                            fds=[dirfd, w, w])

    # a symlink planted where a class's datadir goes: the broker replaces it, never follows it (D46)
    os.mkdir("/pf0/run/target")
    open("/pf0/run/target/canary", "w").close()
    if os.path.lexists("/pf0/run/version"):
        os.rename("/pf0/run/version", "/pf0/run/version-old")
    os.symlink("/pf0/run/target", "/pf0/run/version")

    # the rest of the spawn budget, then one more
    codes = [stub("version", "-datadir=/pf0/run/version", "-version").wait(120) for _ in range(cfg["versions"])]
    out["symlink"] = {"canary": os.path.exists("/pf0/run/target/canary"),
                      "left": os.path.islink("/pf0/run/version")}
    out["version_exits"] = sorted(set(codes))
    out["refused"]["over the cap"] = raw(req("version", ["-datadir=/pf0/run/version", "-version"]))

    time.sleep(1)
    done.set()
    t.join()
    out["grams"] = grams
    out["own_pidns"] = os.readlink("/proc/self/ns/pid")

    # the host disk: 2 GiB to its own stderr (a pipe outer_init caps, §6); last, so a traceback before it is kept
    chunk = b"z" * (1 << 20)
    for _ in range(2048):
        os.write(2, chunk)
    print(json.dumps(out), flush=True)


if __name__ == "__main__":
    main()
