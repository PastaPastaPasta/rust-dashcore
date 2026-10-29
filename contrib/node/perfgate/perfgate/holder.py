"""The gate holder (PF0-N plan rev4.1 §2, §3, D13, D56, D57, D67).

    unshare --user --map-root-user --mount --fork --kill-child -- python3 -I -B holder.py --config CONFIG
    nsenter <the holder's user and mount ns> -- python3 -I -B holder.py --prep-mount DIR SIZE INODES | --prep-ro DIR

One per gate. In its own user and mount namespace it mounts the `GATE` tmpfs over an empty host directory,
copies each side's PR binary there (0555, sha256 checked against the build output), writes one byte on the ready
fd, and then waits until it is stopped. Invocations join its namespaces by the fds the harness opened after that
byte (D56), so they see `GATE`; the host never does, and the shared /tmp quota never counts it (D14). The harness
adds a row's `PREP` tmpfs the same way (`--prep-mount`) and makes its superblock read-only after the prep
invocation (`--prep-ro`), for the rest of the gate (§2).
Self-contained: it runs under `python3 -I`.
"""

import ctypes
import hashlib
import json
import os
import signal
import sys

_libc = ctypes.CDLL(None, use_errno=True)


MS_RDONLY, MS_REMOUNT = 0x1, 0x20


def mount_tmpfs(target, size, inodes):
    opts = f"size={size},nr_inodes={inodes},mode=0700".encode()
    if _libc.mount(b"tmpfs", target.encode(), b"tmpfs", 0, opts) != 0:
        raise OSError(ctypes.get_errno(), f"mount tmpfs on {target}")


def prep(argv):
    if argv[1] == "--prep-mount" and len(argv) == 5:
        os.makedirs(argv[2], 0o700)
        mount_tmpfs(argv[2], int(argv[3]), int(argv[4]))
    elif argv[1] == "--prep-ro" and len(argv) == 3:
        if _libc.mount(None, argv[2].encode(), None, MS_REMOUNT | MS_RDONLY, None) != 0:
            raise OSError(ctypes.get_errno(), f"remount {argv[2]} read-only")
    else:
        sys.exit("usage: holder.py --prep-mount DIR SIZE INODES | --prep-ro DIR")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main(argv):
    if len(argv) > 1 and argv[1].startswith("--prep-"):
        return prep(argv)
    if len(argv) != 3 or argv[1] != "--config":
        sys.exit("usage: holder.py --config CONFIG")
    with open(argv[2]) as f:
        cfg = json.load(f)
    gate = cfg["gate"]
    mount_tmpfs(gate, cfg["size"], cfg["inodes"])
    os.mkdir(os.path.join(gate, "exe"), 0o700)
    os.mkdir(os.path.join(gate, "prep"), 0o700)
    for side, exe in sorted(cfg["exes"].items()):
        dst = os.path.join(gate, "exe", side)
        try:
            with open(exe["path"], "rb") as fi, open(dst, "xb") as fo:
                while chunk := fi.read(1 << 20):
                    fo.write(chunk)
        except OSError as e:
            if e.errno == 28:  # ENOSPC
                sys.exit(f"holder: GATE tmpfs full ({cfg['size']})")
            raise
        os.chmod(dst, 0o555)
        if sha256(dst) != exe["sha256"]:
            sys.exit(f"holder: the {side} exe copy does not match the build output")
    os.write(cfg["ready_fd"], b"R")
    os.close(cfg["ready_fd"])
    while True:  # until the harness stops the unit, or the --kill-child chain ends it
        signal.pause()


if __name__ == "__main__":
    main(sys.argv)
