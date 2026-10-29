#!/usr/bin/env python3
"""Records Core v24's cache sizes and its "Cache configuration" log lines for dashd-chainstate's
`CacheSizes` tests (CH2 §2.5, §7.1 a3).

- `cache_sizes_v24.txt`: Core's `CalculateCacheSizes` (node/caches.cpp) for a grid of `-dbcache`,
  `-txindex`, `-addressindex`, `-timestampindex`, `-spentindex` and the number of block filter
  indexes, from `probe.cpp` linked against a Core build tree whose sources match the pin.
- `cache_sizes_v24_32bit.txt`: the same grid on a 32-bit build, where `-dbcache` is capped at
  1,024 MiB (txdb.h). `probe.cpp` and the tree's node/caches.cpp are built for i386 in a Docker
  container, against node/caches.h, the constants of txdb.h and the index headers, and the stub
  ArgsManager in `stub/`. The same stub build for the host must reproduce `cache_sizes_v24.txt`.
- `cache_log_v24.txt`: the lines a real dashd v24 logs (`init.cpp:1986-2023`) for a few of those
  configurations and `-maxmempool` values, timestamps stripped.

Usage:
  gen.py --core <Core build tree at the v24 pin> --dashd <dashd v24 binary> --out <dir>
  gen.py ... --check   # regenerate into a scratch dir and compare with --out

The build tree must have built `libbitcoin_node.a`, `libbitcoin_util.a`, univalue and the crypto
libraries, and its sources below must equal the pin's (checked with git). The 32-bit build needs
Docker and the network (it installs g++ in the container).
"""

import argparse
import hashlib
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PIN = "v24.0.0-rc.3"
# The sources the sizes and the log lines come from.
SOURCES = [
    "src/node/caches.cpp", "src/node/caches.h", "src/txdb.h", "src/util/system.cpp",
    "src/util/system.h", "src/index/txindex.h", "src/index/addressindex.h",
    "src/index/spentindex.h", "src/index/timestampindex.h", "src/init.cpp",
]
HERE = Path(__file__).resolve().parent
# The constants node/caches.cpp reads from each header, which the stub build copies from the tree:
# the rest of those headers needs a Core build tree.
CONSTANTS = {
    "txdb.h": r"static const int64_t \w+ = [^;]*;",
    "index/txindex.h": r"static constexpr bool DEFAULT_TXINDEX\{\w+\};",
    "index/addressindex.h": r"static constexpr bool DEFAULT_ADDRESSINDEX\{\w+\};",
    "index/spentindex.h": r"static constexpr bool DEFAULT_SPENTINDEX\{\w+\};",
    "index/timestampindex.h": r"static constexpr bool DEFAULT_TIMESTAMPINDEX\{\w+\};",
}
# The 32-bit build's container.
IMAGE_386 = "debian:bookworm"

DBCACHE = [
    -(1 << 63), -5, 0, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 64, 100, 128, 150, 299, 300,
    301, 450, 512, 1000, 1024, 2048, 2100, 4096, 8191, 8192, 16383, 16384, 16385, 100000,
    (1 << 43) - 1, 1 << 43, 1 << 44, (1 << 63) - 1,
]
# Configurations dashd itself logs: (options, filter types).
LOGGED = [
    [],
    ["-dbcache=4"],
    ["-dbcache=7", "-addressindex=1", "-timestampindex=1", "-spentindex=1", "-blockfilterindex=1",
     "-maxmempool=50"],
    ["-dbcache=300", "-txindex=0"],
    ["-dbcache=300", "-blockfilterindex=basic", "-maxmempool=5"],
    ["-dbcache=100", "-addressindex=1", "-maxmempool=1000"],
    ["-dbcache=450", "-spentindex=1", "-timestampindex=1"],
    ["-dbcache=1024", "-txindex=0", "-addressindex=1", "-maxmempool=12"],
    ["-dbcache=16384", "-maxmempool=300"],
    ["-dbcache=100000", "-txindex=0", "-blockfilterindex=1"],
    ["-dbcache=-5"],
    ["-dbcache=33", "-timestampindex=1", "-maxmempool=7"],
]


def sh(*args, **kw):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kw).stdout


def check_sources(core: Path):
    root = Path(sh("git", "-C", core, "rev-parse", "--show-toplevel").strip())
    rel = core.resolve().relative_to(root)
    paths = [str(rel / p) for p in SOURCES]
    diff = subprocess.run(["git", "-C", root, "diff", "--quiet", PIN, "--", *paths])
    if diff.returncode != 0:
        sys.exit(f"{core}: sources differ from {PIN}: {' '.join(SOURCES)}")
    return sh("git", "-C", core, "describe", "--tags", "--always").strip()


def build_probe(core: Path, work: Path) -> Path:
    src = core / "src"
    dep = next((core / "depends").glob("*-linux-gnu"))
    probe = work / "probe"
    # The base library first: it calls the SSE4.1, AVX2 and SHA-NI ones.
    libs = src / "crypto" / ".libs"
    base = libs / "libbitcoin_crypto_base.a"
    crypto = [base, *sorted(set(libs.glob("libbitcoin_crypto_*.a")) - {base})]
    sh("g++", "-std=c++20", "-O1", "-DHAVE_CONFIG_H", f"-I{src}", f"-I{src}/config",
       f"-I{src}/univalue/include", "-isystem", dep / "include", HERE / "probe.cpp", "-o", probe,
       src / "libbitcoin_node.a", src / "libbitcoin_util.a", src / ".libs" / "libunivalue.a",
       *crypto, f"-L{dep}/lib", "-lbacktrace", "-pthread")
    return probe


def grid():
    for dbcache in DBCACHE:
        for flags in range(16):
            for n in range(3):
                yield dbcache, [(flags >> i) & 1 for i in range(4)], n


def stub_tree(core: Path, work: Path) -> Path:
    """A source tree for the stub build: probe.cpp, the tree's node/caches.{cpp,h}, `CONSTANTS`
    and `stub/`."""
    tree = work / "stub"
    shutil.copytree(HERE / "stub", tree)
    shutil.copy(HERE / "probe.cpp", tree)
    (tree / "node").mkdir()
    for name in ["caches.cpp", "caches.h"]:
        shutil.copy(core / "src" / "node" / name, tree / "node")
    for header, pattern in CONSTANTS.items():
        found = re.findall(pattern, (core / "src" / header).read_text())
        assert found, f"{header}: no {pattern}"
        (tree / header).parent.mkdir(exist_ok=True)
        (tree / header).write_text("#pragma once\n#include <cstdint>\n" + "\n".join(found) + "\n")
    return tree


def stub_build(tree, out) -> list[str]:
    """The command that builds the stub probe from `tree` into `out`."""
    return ["g++", "-std=c++20", "-O1", f"-I{tree}", f"{tree}/probe.cpp",
            f"{tree}/node/caches.cpp", "-o", str(out)]


def stub_sizes_32bit(tree: Path) -> str:
    """The stub probe's grid, built and run on i386 in `IMAGE_386`. The grid is the probe's
    stdin, which apt must not read."""
    script = (
        'test "$(getconf LONG_BIT)" = 32 && export DEBIAN_FRONTEND=noninteractive'
        " && apt-get -qq update </dev/null >/dev/null"
        " && apt-get -qq install -y g++ </dev/null >/dev/null 2>&1"
        f" && {shlex.join(stub_build('/src', '/tmp/probe'))} && /tmp/probe"
    )
    return sizes("docker", "run", "--rm", "-i", "--platform", "linux/386", "-v", f"{tree}:/src:ro",
                 IMAGE_386, "sh", "-c", script)


def sizes(*probe) -> str:
    """The grid's rows, with the sizes the command `probe` gives for each, from its stdin."""
    rows = list(grid())
    names = ["-txindex", "-addressindex", "-timestampindex", "-spentindex"]
    lines = [
        " ".join([f"-dbcache={d}"] + [f"{k}={v}" for k, v in zip(names, f)] + [f"n_indexes={n}"])
        for d, f, n in rows
    ]
    out = sh(*probe, input="\n".join(lines) + "\n").splitlines()
    assert len(out) == len(rows)
    body = [
        f"{d} {' '.join(map(str, f))} {n} {o}" for (d, f, n), o in zip(rows, out)
    ]
    return "\n".join(body) + "\n"


def logged(dashd: Path) -> str:
    blocks = []
    for opts in LOGGED:
        with tempfile.TemporaryDirectory() as d:
            proc = subprocess.Popen(
                [dashd, "-regtest", f"-datadir={d}", "-listen=0", "-server=0", "-dnsseed=0",
                 "-printtoconsole=0", *opts],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            log = Path(d) / "regtest" / "debug.log"
            deadline = time.monotonic() + 120
            try:
                while True:
                    text = log.read_text() if log.exists() else ""
                    # The whole line, not one being written.
                    if "of unused mempool space)\n" in text:
                        break
                    if proc.poll() is not None or time.monotonic() > deadline:
                        sys.exit(f"dashd {' '.join(opts)} exited or stalled before its cache lines")
                    time.sleep(0.2)
            finally:
                proc.terminate()
                proc.wait()
            lines = [ln.split(" ", 1)[-1] for ln in text.splitlines()]
            start = lines.index("Cache configuration:")
            end = next(i for i in range(start, len(lines)) if "in-memory UTXO set" in lines[i])
            cache = [ln for ln in lines[start:end + 1]
                     if ln == "Cache configuration:" or ln.startswith("* Using ")]
            blocks.append("## " + " ".join(opts) + "\n" + "\n".join(cache) + "\n")
    return "\n".join(blocks)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--core", type=Path, required=True)
    ap.add_argument("--dashd", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    describe = check_sources(a.core)
    version = sh(a.dashd, "--version").splitlines()[0]
    digest = hashlib.sha256(a.dashd.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory() as w:
        work = Path(w)
        probe = build_probe(a.core, work)
        columns = (
            "# dbcache_mib txindex addressindex timestampindex spentindex n_filter -> "
            "block_tree_db coins_db coins tx_index address_index timestamp_index spent_index "
            "filter_index (bytes)\n"
        )
        head = (
            f"# Core {PIN}'s CalculateCacheSizes (node/caches.cpp), from contrib/node/cache-sizes/"
            f"probe.cpp linked against a build tree at {describe}\n"
            f"# (its {', '.join(SOURCES)} equal {PIN}'s). Regenerate with gen.py.\n" + columns
        )
        grid64 = sizes(probe)
        tree = stub_tree(a.core, work)
        stub_probe = work / "stub-probe"
        sh(*stub_build(tree, stub_probe))
        if sizes(stub_probe) != grid64:
            sys.exit("the stub build's grid differs from the full build's")
        head32 = (
            f"# Core {PIN}'s CalculateCacheSizes (node/caches.cpp) on a 32-bit build: "
            "contrib/node/cache-sizes/probe.cpp\n"
            f"# and node/caches.cpp of a build tree at {describe}, built for i386 in {IMAGE_386} "
            "against its node/caches.h,\n"
            "# the constants of its txdb.h and index headers, and stub/util/system.h (the same "
            "stub build for x86_64\n"
            "# reproduces cache_sizes_v24.txt). Regenerate with gen.py.\n" + columns
        )
        files = {
            "cache_sizes_v24.txt": head + grid64,
            "cache_sizes_v24_32bit.txt": head32 + stub_sizes_32bit(tree),
            "cache_log_v24.txt": (
                f"# Core's 'Cache configuration' lines (init.cpp:1986-2023) from {version}\n"
                f"# (dashd sha256 {digest}), on regtest, timestamps stripped. Regenerate with "
                "contrib/node/cache-sizes/gen.py.\n\n" + logged(a.dashd)
            ),
        }
        if a.check:
            bad = [n for n, t in files.items() if (a.out / n).read_text() != t]
            if bad:
                sys.exit(f"differs from the recorded vectors: {', '.join(bad)}")
            print("cache-sizes: the recorded vectors match")
            return
        a.out.mkdir(parents=True, exist_ok=True)
        for name, text in files.items():
            (a.out / name).write_text(text)
            print(f"wrote {a.out / name}")


if __name__ == "__main__":
    main()
