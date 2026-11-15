#!/usr/bin/env python3
"""A prebuilt RocksDB for selftest-checks.sh's nested trees (BL-133).

Every selftest shard builds the workspace's tests in a fresh nested worktree with a cold target/,
and librocksdb-sys's build script then compiles RocksDB's C++ from nothing: 15-37 min per shard
at -O0 under load, the selftest's long pole. No selftest case's verdict depends on how RocksDB's
C++ was built (no case builds, links or inspects a product that links it; the quirk-register cases
only build and list the workspace's test binaries), so the selftest builds the library once per
key and every shard links it: librocksdb-sys links a static `librocksdb.a` from ROCKSDB_LIB_DIR when
ROCKSDB_STATIC is set, and compiles no C++ of its own. lz4-sys and zstd-sys, which the library
calls, still build in each tree, at the versions the key holds.

    selftest-rocksdb.py <workspace root>

prints the directory holding `librocksdb.a` (set it as ROCKSDB_LIB_DIR) and exits 0, or prints
nothing and exits 0 when Cargo.lock has no librocksdb-sys. On any failure it says why on stderr and
exits 1; the selftest then builds RocksDB from source, as before.

The key is what reaches the library: the lock entries of librocksdb-sys, cc and the -sys crates it
compiles against (lz4-sys, zstd-sys), its features as `cargo test --workspace` resolves them, the
root manifest's [profile] tables, the host, the toolchain and the C/C++ compilers' versions, the
compiler, flag and rustflags environment variables, and the tree's and Cargo home's config files.
The library is built from those inputs in a throwaway package next to the cache (the root's lock,
toolchain file and .cargo/ copied in; no rustc wrapper, so no sccache server is started from a
gate's unit, BL-185), checked against the lock's sources, under a lock per key (a second selftest
waits for it and reuses it), and moved into place in one rename. The cache is
${NODE_SELFTEST_CACHE:-${XDG_CACHE_HOME:-~/.cache}/node-selftest}/rocksdb; it keeps the 3 most
recently used libraries, and any used within a day.
"""

import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

SCHEME = 1
KEEP = 3
STALE = 24 * 3600
WRAPPERS = {"ccache", "sccache", "distcc"}
ENV = re.compile(r"(HOST_|TARGET_)?(CC|CXX|AR|CFLAGS|CXXFLAGS|CPPFLAGS|ARFLAGS|CXXSTDLIB|CRATE_CC_NO_DEFAULTS"
                 r"|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_RUSTFLAGS|CARGO_TARGET_\w+_RUSTFLAGS"
                 r"|CARGO_PROFILE_\w+|ROCKSDB_\w+|LZ4_\w+|ZSTD_\w+|LIBCLANG_\w+|BINDGEN_\w+)([_-][\w-]+)?"
                 r"|CARGO_BUILD_TARGET")
BARE = re.compile(r"[A-Za-z0-9_-]+")


def fail(msg):
    sys.exit(f"selftest-rocksdb: {msg}")


def run(argv, cwd, env=None):
    r = subprocess.run(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True)
    if r.returncode:
        fail(f"`{' '.join(argv)}` exited {r.returncode}: {r.stderr.strip()[-2000:]}")
    return r.stdout


def version(cmd, cwd):
    """The compiler's version; a wrapper (`ccache g++`) is its compiler's."""
    words = cmd.split()
    exe = next((w for w in words if Path(w).name not in WRAPPERS), words[0])
    try:
        r = subprocess.run([exe, "--version"], cwd=cwd, capture_output=True, text=True)
    except OSError as e:
        return f"{exe}: {e}"
    return f"{exe}: {r.returncode} {r.stdout.strip()}"


def tree_files(root):
    """The tree's toolchain file and Cargo config, which the throwaway package gets copies of."""
    return [f for f in (root / "rust-toolchain.toml", root / "rust-toolchain", root / ".cargo/config.toml",
                        root / ".cargo/config") if f.is_file()]


def key_inputs(root):
    """(librocksdb-sys version, its features, the key's inputs as one JSON-able dict)."""
    lock = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    sys_pkgs = [p for p in lock if p["name"] == "librocksdb-sys"]
    if not sys_pkgs:
        return None, None, None
    if len(sys_pkgs) > 1:
        fail(f"Cargo.lock holds {len(sys_pkgs)} versions of librocksdb-sys")
    pkg = sys_pkgs[0]
    names = {"librocksdb-sys", "cc"} | {d.split(" ")[0] for d in pkg.get("dependencies", [])
                                        if d.split(" ")[0].endswith("-sys")}
    entries = sorted((p for p in lock if p["name"] in names), key=lambda p: (p["name"], p["version"]))
    # `cargo test --workspace` resolves features for every member, with dev-dependencies, for the host.
    tree = run(["cargo", "tree", "--locked", "--offline", "--workspace", "-e", "features", "--prefix", "none",
                "-i", f"librocksdb-sys@{pkg['version']}"], root)
    features = sorted(set(re.findall(r'^librocksdb-sys feature "([^"]+)"', tree, re.M)))
    profile = tomllib.loads((root / "Cargo.toml").read_text()).get("profile", {})
    if "test" in profile:  # `cargo test` builds dependencies with it; the throwaway build uses dev
        fail("the root manifest has a [profile.test], which the prebuilt library would not follow")
    rustc = run(["rustc", "-vV"], root)
    host = re.search(r"^host: (\S+)", rustc, re.M).group(1)
    under = host.replace("-", "_")
    compilers = {}
    for tool, default in (("CXX", "c++"), ("CC", "cc"), ("AR", "ar")):
        cmd = next((os.environ[v] for v in (f"{tool}_{host}", f"{tool}_{under}", f"HOST_{tool}", tool)
                    if os.environ.get(v)), default)
        compilers[tool] = version(cmd, root)
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    configs = {}  # the tree's own files by relative name, so trees at other paths share a key
    for f in tree_files(root) + [f for f in (home / "config.toml", home / "config") if f.is_file()]:
        rel = str(f.relative_to(root)) if f.is_relative_to(root) else str(f)
        configs[rel] = hashlib.sha256(f.read_bytes()).hexdigest()
    inputs = {
        "scheme": SCHEME,
        "lock": entries,
        "features": features,
        "profile": profile,
        "rustc": rustc,
        "compilers": compilers,
        "env": {k: v for k, v in sorted(os.environ.items()) if ENV.fullmatch(k)},
        "configs": configs,
    }
    return pkg["version"], features, inputs


def build(root, ver, inputs, dest):
    """Builds librocksdb-sys `ver` from `inputs` and moves its librocksdb.a into a staging dir, which it returns."""
    stage = Path(tempfile.mkdtemp(prefix=".stage-", dir=dest.parent))
    tmp = stage / "build"
    try:
        (tmp / "src").mkdir(parents=True)
        (tmp / "src/lib.rs").write_text("")
        feats = ", ".join(json.dumps(f) for f in inputs["features"])
        (tmp / "Cargo.toml").write_text(
            f'[package]\nname = "selftest-rocksdb-prebuilt"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n\n'
            f'[workspace]\n\n[dependencies]\nlibrocksdb-sys = {{ version = "={ver}", default-features = false, '
            f'features = [{feats}] }}\n' + profile_toml(inputs["profile"]))
        # The root lock: cargo keeps its versions, and drops what this package does not use.
        shutil.copyfile(root / "Cargo.lock", tmp / "Cargo.lock")
        for f in tree_files(root):
            (tmp / f.relative_to(root)).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(f, tmp / f.relative_to(root))
        env = {**os.environ, "RUSTC_WRAPPER": "", "CARGO_BUILD_RUSTC_WRAPPER": ""}
        out = run(["cargo", "build", "--offline", "--message-format=json-render-diagnostics",
                   "--target-dir", str(tmp / "target")], tmp, env)
        want = {p["name"]: (p.get("source"), p["version"]) for p in inputs["lock"]}
        lib = None
        for msg in (json.loads(line) for line in out.splitlines() if line.startswith("{")):
            source, _, nv = msg.get("package_id", "").rpartition("#")
            name, _, v = nv.rpartition("@")
            if name in want and (source, v) != want[name]:
                fail(f"the throwaway build resolved {msg['package_id']}, the lock {name} {want[name]}")
            if name != "librocksdb-sys":
                continue
            if msg.get("reason") == "compiler-artifact" and sorted(msg.get("features", [])) != inputs["features"]:
                fail(f"the throwaway build has features {msg.get('features')}, the workspace {inputs['features']}")
            if msg.get("reason") == "build-script-executed":
                lib = Path(msg["out_dir"]) / "librocksdb.a"
        if lib is None or not lib.is_file():
            fail("the throwaway build left no librocksdb.a")
        lib.rename(stage / "librocksdb.a")
        return stage
    except BaseException:
        shutil.rmtree(stage, ignore_errors=True)
        raise
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def profile_toml(profile, prefix="profile"):
    """The [profile] tables as TOML: scalars and arrays, and each sub-table under its quoted name if need be."""
    out = ""
    plain = {k: v for k, v in profile.items() if not isinstance(v, dict)}
    if (plain or not profile) and prefix != "profile":
        out += f"\n[{prefix}]\n" + "".join(f"{json.dumps(k)} = {json.dumps(v)}\n" for k, v in plain.items())
    for k, v in profile.items():
        if isinstance(v, dict):
            out += profile_toml(v, f"{prefix}.{k if BARE.fullmatch(k) else json.dumps(k)}")
    return out


def prune(base, dest):
    """Drops a killed build's leftovers, and libraries beyond the KEEP most recently used and unused for
    a day (a running selftest links its library hours after it took it), each under its own key's
    lock so none goes while it is being taken."""
    now = time.time()
    dirs = [d for d in base.iterdir() if d.is_dir()]
    libs = sorted((d for d in dirs if not d.name.startswith(".")), key=lambda d: d.stat().st_mtime)
    for d in dirs:  # a build's staging dir lives for an hour at most
        if d.name.startswith(".") and now - d.stat().st_mtime > STALE:
            shutil.rmtree(d, ignore_errors=True)
    for d in libs[:-KEEP]:
        if d == dest or now - d.stat().st_mtime <= STALE:
            continue
        # (the empty lock file stays: one a waiter already holds open must stay the key's only lock)
        with open(base / f".lock-{d.name}", "a") as lk:
            try:
                fcntl.flock(lk, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                continue
            if now - d.stat().st_mtime > STALE:
                shutil.rmtree(d, ignore_errors=True)


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: selftest-rocksdb.py <workspace root>")
    # A stopped selftest stops its build here too, and the build's files go with it.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    root = Path(sys.argv[1]).resolve()
    ver, features, inputs = key_inputs(root)
    if ver is None:
        return
    blob = json.dumps(inputs, sort_keys=True).encode()
    key = f"{ver.split('+')[0]}-{hashlib.sha256(blob).hexdigest()[:16]}"
    cache = os.environ.get("NODE_SELFTEST_CACHE") or \
        Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "node-selftest"
    base = Path(cache) / "rocksdb"
    base.mkdir(parents=True, exist_ok=True)
    dest = base / key
    with open(base / f".lock-{key}", "a") as lk:
        fcntl.flock(lk, fcntl.LOCK_EX)
        if not (dest / "librocksdb.a").is_file():
            print(f"selftest-rocksdb: building librocksdb-sys {ver} ({', '.join(features)}) once for key {key}",
                  file=sys.stderr, flush=True)
            t0 = time.monotonic()
            stage = build(root, ver, inputs, dest)
            (stage / "key.json").write_bytes(blob)  # what the key holds, for whoever wonders why it moved
            stage.rename(dest)
            print(f"selftest-rocksdb: built in {time.monotonic() - t0:.0f} s", file=sys.stderr)
        os.utime(dest)
    prune(base, dest)
    print(dest)


if __name__ == "__main__":
    main()
