"""The broker's test hooks (D9), loaded only by tests/broker_hooked.py; production never has them.

Settings (the test JSON):

    node_launch_hooks  {"<k>": {"into": LIST, "jail": LIST}}: `--test-hooks LIST` for spawn k's node-launch steps
                       (the config's node_launch must then be the -DPF0_TEST_HOOKS build)
    jail_hello_only    ["<k>", ...]: spawn k's jail step says its hello, takes the spec and exits 0 without running
                       bwrap (the chain ends before the jail ran, D81)
    hold_cleanup_s     keep the cgroup-empty wait open this long after the kill (kill-matrix point 6)
    rmdir_ebusy        report EBUSY from this many rmdir attempts first (point 7)
    comm               the broker's comm, for the view-side identity check (D50, D72)
    bait               start a process the census must reject (D40, D64): "sleep" or "python3" reparented to
                       outer_init, or "child": a python3 child of the broker that is never registered
"""

import ctypes
import errno
import os
import sys
import time

# The jail step of jail_hello_only: `--jail SOCK ...` as node_launch takes it; the hello, the spec, no bwrap.
JAIL_HELLO_ONLY = ("import socket, sys; s = socket.socket(fileno=int(sys.argv[2])); "
                   "s.send(b'{\"hello\": \"jail\", \"dumpable\": 0}'); socket.recv_fds(s, 64, 4)")


class Hooks:
    def __init__(self, test):
        self.test = test
        self.rmdirs = 0

    def chain_argv(self, k, argv):
        if str(k) in self.test.get("jail_hello_only", ()):
            i = next(i for i, a in enumerate(argv) if os.path.basename(a).startswith("perfgate_node_launch")
                     and argv[i + 1:i + 2] == ["--jail"])
            argv = argv[:i] + [sys.executable, "-c", JAIL_HELLO_ONLY] + argv[i + 1:]
        spec = self.test.get("node_launch_hooks", {}).get(str(k))
        if not spec:
            return argv
        out = []
        for i, a in enumerate(argv):
            out.append(a)
            if os.path.basename(a).startswith("perfgate_node_launch") and i + 1 < len(argv):
                hooks = spec.get(argv[i + 1].lstrip("-"))
                if hooks:
                    out += ["--test-hooks", hooks]
        return out

    def hold_cleanup(self, sp):
        return time.monotonic() - sp.killed_at < self.test.get("hold_cleanup_s", 0)

    def rmdir(self, path):
        self.rmdirs += 1
        if self.rmdirs <= self.test.get("rmdir_ebusy", 0):
            raise OSError(errno.EBUSY, os.strerror(errno.EBUSY), path)
        os.rmdir(path)

    def started(self):
        comm = self.test.get("comm")
        if comm:
            ctypes.CDLL(None).prctl(15, comm.encode(), 0, 0, 0)  # PR_SET_NAME
        bait = self.test.get("bait")
        if bait:
            self._bait(bait)

    @staticmethod
    def _bait(kind):
        argv = ["/usr/bin/sleep", "30"] if kind == "sleep" else ["/usr/bin/python3", "-c",
                                                                  "import time; time.sleep(30)"]
        pid = os.fork()
        if pid:
            if kind != "child":
                os.waitpid(pid, 0)
            return
        try:
            if kind != "child" and os.fork():  # the grandchild is reparented to outer_init
                os._exit(0)
            os.execv(argv[0], argv)
        finally:
            os._exit(127)
