"""Where large state lives (never inside the repo), the pinned dashd, and the write boundary.

node-bench only writes, moves and deletes under NODE_BENCH_ROOT (default /work/bench), and never reads from or
writes to a protected tree (default /work/chains: the reference oracles, owned by another task;
NODE_BENCH_PROTECTED adds more, colon-separated). Paths are compared after resolving symlinks, so a link cannot
smuggle a write out of the boundary.
"""

import os
import platform
import shutil
import subprocess
import tempfile

ROOT = os.environ.get("NODE_BENCH_ROOT", "/work/bench")
HARNESS_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # node-bench/
REPO_ROOT = os.path.dirname(HARNESS_DIR)
PROTECTED = tuple(p for p in os.environ.get("NODE_BENCH_PROTECTED", "/work/chains").split(":") if p)


class UnsafePath(RuntimeError):
    """A path outside the bench boundary, or inside a protected tree."""


def canon(path):
    return os.path.realpath(os.path.abspath(os.path.expanduser(path)))


def under(path, root):
    """True if canonical `path` is `root` or inside it."""
    return path == root or path.startswith(root.rstrip("/") + "/")


_REFLINK = {}


def reflink_supported(directory):
    """True if `directory`'s filesystem can clone files (cp --reflink=always), cached per device."""
    dev = os.stat(directory).st_dev
    if dev not in _REFLINK:
        with tempfile.TemporaryDirectory(dir=directory) as t:
            src = os.path.join(t, "a")
            with open(src, "w") as f:
                f.write("x")
            _REFLINK[dev] = subprocess.run(["cp", "--reflink=always", src, os.path.join(t, "b")],
                                           capture_output=True).returncode == 0
    return _REFLINK[dev]


def free_gib(path):
    return shutil.disk_usage(path).free / 2**30


def check_free(path, need_gib, what):
    """Refuse to start something that would fill the shared filesystem."""
    free = free_gib(path)
    if free < need_gib:
        raise RuntimeError(f"{what}: {free:.0f} GiB free under {path}, need {need_gib:.0f} GiB")


def check_root():
    """The bench root must neither be inside a protected tree nor contain one."""
    root = canon(ROOT)
    for prot in PROTECTED:
        q = canon(prot)
        if under(root, q) or under(q, root):
            raise UnsafePath(f"NODE_BENCH_ROOT {root} overlaps the protected tree {prot}")


def not_protected(path, what="path"):
    """Canonical `path`; refuses anything inside a protected tree (reading included: a copy of a live oracle
    datadir is no snapshot, and the rule is not to touch them)."""
    p = canon(path)
    for prot in PROTECTED:
        if under(p, canon(prot)):
            raise UnsafePath(f"refusing {what} {path} ({p}): {prot} is protected (reference oracles)")
    return p


def writable(path, what="path"):
    """Canonical `path` if node-bench may create, write or move it: under NODE_BENCH_ROOT, not protected."""
    p = not_protected(path, what)
    root = canon(ROOT)
    if not under(p, root):
        raise UnsafePath(f"refusing to write {what} {path} ({p}): outside NODE_BENCH_ROOT {root}")
    return p


def safe_rmtree(path, what="directory"):
    """Delete a tree node-bench owns. Refuses the root and its first two levels (bin/, data/seed-*, results/...),
    anything outside the root and anything protected; a symlink is unlinked, never followed."""
    if not os.path.lexists(path):
        return
    if os.path.islink(path):
        writable(os.path.dirname(os.path.abspath(path)), what)
        os.unlink(path)
        return
    p = writable(path, what)
    rel = os.path.relpath(p, canon(ROOT))
    if rel == "." or len(rel.split(os.sep)) <= 2:
        raise UnsafePath(f"refusing to delete {what} {p}: a top-level bench directory")
    shutil.rmtree(p, ignore_errors=True)
    if os.path.lexists(p):
        raise RuntimeError(f"could not delete {what} {p}")


def harness_commit():
    return subprocess.run(["git", "-C", HARNESS_DIR, "rev-parse", "HEAD"], capture_output=True,
                          text=True).stdout.strip()


# Pinned baseline. Bump deliberately: a new pin needs a new baseline run.
DASHD_VERSION = os.environ.get("NODE_BENCH_DASHD_VERSION", "23.1.8")
# sha256 of the release tarball, from the signed SHA256SUMS.asc of the release.
DASHD_SHA256 = {
    ("23.1.8", "x86_64"): "e664cad0fe860afa3d005be0db1eb06da598f2f83421083afae77191e3115825",
    ("23.1.8", "aarch64"): "b96fef477ea01f5c47cc3d3f7de3fdd0bc8cc86f64660f720e8206d889604523",
    # Second baseline (A1 rev. PLAN §8.1): testnet runs v24; replace with 24.0.0 final when it ships.
    ("24.0.0-rc.3", "x86_64"): "9327f00018afd12f11676ac66e688c76b253e4783bf0d595f08476de4269ff52",
}


def path(*parts):
    return os.path.join(ROOT, *parts)


def arch():
    m = platform.machine()
    return {"amd64": "x86_64", "arm64": "aarch64"}.get(m, m)


def pinned_dashd():
    return path("bin", f"dashcore-{DASHD_VERSION}", "bin", "dashd")


def results_dir():
    return path("results")


def data_dir(*parts):
    return path("data", *parts)
