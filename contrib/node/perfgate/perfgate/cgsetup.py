"""First step inside an invocation's delegated scope (PF0-N plan rev4.1 §3).

    python3 -I -B cgsetup.py -- CMD...

systemd-run starts this as the scope's only process. It moves itself into `<scope>/harness` (a cgroup with
processes cannot delegate controllers to children), enables memory and pids for the scope's children, so the
broker can make `spawn-<k>` siblings with their own limits, and execs CMD. Self-contained: it runs under
`python3 -I`.
"""

import os
import sys

CG = "/sys/fs/cgroup"


def main(argv):
    if len(argv) < 3 or argv[1] != "--":
        sys.exit("usage: cgsetup.py -- CMD...")
    with open("/proc/self/cgroup") as f:
        line = f.read().strip()
    if not line.startswith("0::/") or "\n" in line:
        sys.exit(f"cgsetup: not on a unified cgroup hierarchy: {line!r}")
    scope = os.path.join(CG, line[4:])
    if not scope.endswith(".scope"):
        sys.exit(f"cgsetup: not started in a systemd scope: {scope}")
    harness = os.path.join(scope, "harness")
    os.mkdir(harness)
    with open(os.path.join(harness, "cgroup.procs"), "w") as f:
        f.write(str(os.getpid()))
    with open(os.path.join(scope, "cgroup.subtree_control"), "w") as f:
        f.write("+memory +pids")
    os.execvp(argv[2], argv[2:])


if __name__ == "__main__":
    main(sys.argv)
