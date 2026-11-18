#!/usr/bin/env python3
"""The global-allocator early checks (SB0 L5; DEC-176, DEC-181 (4), DEC-188).

  link-probe.py <crate> <manifest> [<target>...]
  link-probe.py --check-allow

The proof of the allocator rule is check-products.py's canary link of
every registered product (DEC-181); consumers outside that registry are out
of scope. This checks the S and pure crates themselves, three ways.

Native code (DEC-188 (3)) fails the check. A C library can replace or
interpose malloc and free without any #[global_allocator], which no canary
sees. So every package in the crate's all-features closure (below) with a
`links` key, a build script compiling C or C++ (a cc, cmake or bindgen
build-dependency), or a build script that emitted link libraries or search
paths in the sample builds below needs a layers.toml [[native]] record: its
package, its version and a review note confirming it does not replace or
interpose malloc or free. A native package without one fails. A record names
a package and version, not a source, so a patched fork inherits it.

The source scan is advisory (DEC-181 (4), DEC-188 (4)): its hits are
warnings, and the exit status ignores them. It scans sources for the `global_allocator` token. It takes the crate's
`--all-features` normal closure on each platform (the host and each target;
`cargo metadata --filter-platform`, each declaration's cfg evaluated). A
package in that closure is reported, proc macros included, if the token appears,
whatever cfg guards it, in a file under the directory of one of its library
or build-script targets' `src_path` (`target/` and `.git` aside when that is
the package root), or in an `OUT_DIR` its build script wrote. A `#[path]`
file outside those trees, an attribute a macro assembles, and a dependency
feature only a downstream product enables are the canary's to catch.

layers.toml's [[allocator.exception]] records list exceptions. Each has a
package, a version and a dec, and the cited decision's row on node/integration
must hold the exact token `package@version` (`--check-allow` validates only
these). The token is enough: a row that names it to refuse it would pass too,
so review the cited row.

It also builds the crate's library with cargo in release, with all features,
the default ones and none, on the host and each target. A one-line consumer
binary is then linked against those artifacts through normal linkage:

    extern crate <crate>;
    #[global_allocator] static PROBE: std::alloc::System = std::alloc::System;
    fn main() {}

The link fails if the crate, or anything it links, installs an allocator in
those configurations, and so does the check: that allocator is real, not a
scan's guess. Any other link failure fails too, as do malformed exception or
native records.
"""

import importlib.util
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONFLICT = "conflicts with global allocator in:"
TOKEN = re.compile(r"\bglobal_allocator\b")
NATIVE_BUILD = {"cc", "cmake", "bindgen"}
# out_dirs' key for the package ids whose build scripts linked native code.
LINKS = "native links"
SCANNED_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro", "custom-build"}
_spec = importlib.util.spec_from_file_location("check_layers", ROOT / "contrib" / "node" / "check-layers.py")
layers = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(layers)


def artifacts(manifest, crate, features, target, out_dirs):
    """Cargo builds the library (release, as products ship); returns its
    crate name and rlib, the directories of every unit built (proc macros
    included) and the native -L paths, or None if it does not build. Adds
    each build script's OUT_DIR to `out_dirs[package id]`."""
    argv = ["cargo", "build", "--locked", "-q", "--release", "-p", crate, "--lib", "--message-format=json"]
    if features:
        argv.append(features)
    if target:
        argv += ["--target", target]
    out = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, text=True)
    if out.returncode:
        return None
    lib, deps, native = None, set(), set()
    for line in out.stdout.splitlines():
        msg = json.loads(line)
        if msg.get("reason") == "build-script-executed":
            native.update(msg["linked_paths"])
            out_dirs.setdefault(msg["package_id"], set()).add(msg["out_dir"])
            if msg["linked_libs"] or msg["linked_paths"]:
                out_dirs.setdefault(LINKS, set()).add(msg["package_id"])
        elif msg.get("reason") == "compiler-artifact":
            deps.update(str(Path(f).parent) for f in msg["filenames"])
            if Path(msg["manifest_path"]).resolve() == manifest and {"lib", "rlib"} & set(msg["target"]["kind"]):
                rlib = next((f for f in msg["filenames"] if f.endswith(".rlib")), None)
                lib = (msg["target"]["name"], rlib) if rlib else None
    return lib and (*lib, sorted(deps), sorted(native))


def probe(manifest, crate, features, target, out_dirs):
    """None if the consumer links, else the reason it does not."""
    built = artifacts(manifest, crate, features, target, out_dirs)
    if built is None:
        return "the library does not build"
    name, lib, deps, native = built
    out_dir = ROOT / "target" / "node-link-probe" / (target or "host") / crate / (features.strip("-") or "default-features")
    out_dir.mkdir(parents=True, exist_ok=True)
    main = out_dir / "main.rs"
    main.write_text(f"extern crate {name};\n"
                    "#[global_allocator] static PROBE: std::alloc::System = std::alloc::System;\n"
                    "fn main() {}\n")
    argv = ["rustc", "--edition", "2024", "--crate-type", "bin", "-o", str(out_dir / "consumer"),
            str(main), "--extern", f"{name}={lib}"]
    for path in deps:
        argv += ["-L", f"dependency={path}"]
    for path in native:
        argv += ["-L", path]
    if target:
        argv += ["--target", target]
        linker = os.environ.get(f"CARGO_TARGET_{target.upper().replace('-', '_')}_LINKER")
        if linker:
            argv += ["-C", f"linker={linker}"]
    out = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if out.returncode == 0:
        return None
    conflict = next((l for l in out.stdout.splitlines() if CONFLICT in l), None)
    if conflict:
        return "installs a global allocator: " + conflict.split(CONFLICT, 1)[1].strip()
    return "a consumer does not link:\n" + out.stdout


def closure(manifest, triple):
    """The packages of the crate's all-features normal closure on `triple`."""
    meta = json.loads(layers.cargo("metadata", "--locked", "--all-features", "--filter-platform", triple,
                                   "--format-version", "1"))
    cfg = set(layers.run("rustc", "--print", "cfg", "--target", triple).stdout.split())
    edges = layers.resolved_edges(meta, lambda condition: layers.cfg_applies(condition, triple, cfg))
    pkgs = {p["id"]: p for p in meta["packages"]}
    todo = [next(i for i, p in pkgs.items() if Path(p["manifest_path"]).resolve() == manifest)]
    seen = set(todo)
    while todo:
        for to, kind, _ in edges.get(todo.pop(), ()):
            if kind == "normal" and to not in seen:
                seen.add(to)
                todo.append(to)
    return [pkgs[i] for i in sorted(seen)]


def natives(packages, reviewed, linking):
    """`package vX (why)` for each native package of `packages` that no
    [[native]] record reviews; `linking` holds the package ids whose build
    scripts emitted link libraries or search paths."""
    missing = []
    for p in packages:
        why = [f"links = {p['links']!r}"] if p.get("links") else []
        why += ["its build script links native code"] if p["id"] in linking else []
        tools = sorted({d["name"] for d in p["dependencies"] if d["kind"] == "build"} & NATIVE_BUILD)
        why += [f"builds with {', '.join(tools)}"] if tools else []
        if why and (p["name"], p["version"]) not in reviewed:
            missing.append(f"{p['name']} v{p['version']} ({'; '.join(why)})")
    return missing


def native_records():
    """layers.toml's [[native]] records as (package, version) pairs."""
    reviewed = set()
    for record in tomllib.loads((ROOT / "contrib" / "node" / "layers.toml").read_text()).get("native", []):
        if not (isinstance(record, dict) and set(record) == {"package", "version", "note"} and all(
                isinstance(f, str) and f.strip() for f in record.values())):
            sys.exit(f"error: layers.toml's [[native]] {record!r} must give package, version and a review note "
                     f"(DEC-188)")
        reviewed.add((record["package"], record["version"]))
    return reviewed


def scan(packages, out_dirs, allowed):
    """`package: file:line` for each global_allocator token in `packages`."""
    hits = []
    for p in packages:
        if f"{p['name']}@{p['version']}" in allowed:
            continue
        package_root = Path(p["manifest_path"]).parent
        dirs = {Path(t["src_path"]).parent for t in p["targets"] if SCANNED_KINDS & set(t["kind"])}
        dirs |= {Path(d) for d in out_dirs.get(p["id"], ())}

        def scanned(f):
            return f.is_file() and not (f.is_relative_to(package_root)
                                        and {"target", ".git"} & set(f.relative_to(package_root).parts[:1]))

        for path in sorted({f for d in dirs for f in d.rglob("*") if scanned(f)}):
            for n, line in enumerate(path.read_bytes().decode(errors="replace").splitlines(), 1):
                if TOKEN.search(line):
                    hits.append(f"{p['name']} v{p['version']}: {path}:{n}")
    return hits


def allow_list():
    """layers.toml's [[allocator.exception]] records as `package@version`
    tokens. Each cites, as `dec`, a decision whose row in the gates' rulings
    (contrib/node/gate-rulings.toml) on node/integration holds that exact
    token (DEC-181 (5), DEC-987)."""
    table = tomllib.loads((ROOT / "contrib" / "node" / "layers.toml").read_text()).get("allocator", {})
    if set(table) - {"exception"}:
        sys.exit(f"error: layers.toml's [allocator] takes only [[allocator.exception]] records, not "
                 f"{sorted(set(table) - {'exception'})} (DEC-181)")
    allowed, rows = set(), None
    for record in table.get("exception", []):
        row = token = None
        if isinstance(record, dict) and set(record) == {"package", "version", "dec"} and all(
                isinstance(f, str) and f for f in record.values()):
            rows = layers.decisions(layers.BASE_REF) if rows is None else rows
            row, token = rows.get(record["dec"]), f"{record['package']}@{record['version']}"
        # Whole tokens only: `bls@0.3.1` is not `blst@0.3.17`.
        if row is None or token not in {t.rstrip(".") for t in re.split(r"[^\w.@+-]+", row)}:
            sys.exit(f"error: layers.toml's [[allocator.exception]] {record!r} must give package, version and dec, "
                     f"a decision whose row in {layers.RULINGS} on node/integration holds the token package@version (DEC-181)")
        allowed.add(token)
    return allowed


def main(argv):
    if argv == ["--check-allow"]:
        print(f"allocator exceptions: {len(allow_list())}")
        return 0
    if len(argv) < 2:
        sys.exit(__doc__)
    crate, manifest, targets = argv[0], Path(argv[1]).resolve(), [None, *argv[2:]]
    host = re.search(r"^host: (\S+)", layers.run("rustc", "-vV").stdout, re.M).group(1)
    allowed, reviewed, failed = allow_list(), native_records(), False
    for target in targets:
        out_dirs = {}
        for features in ("--all-features", "", "--no-default-features"):
            reason = probe(manifest, crate, features, target, out_dirs)
            if reason:
                print(f"error: {crate} ({target or 'host'}, {features or 'default features'}): {reason} (SB0 L5)",
                      file=sys.stderr)
                failed = True
        packages = closure(manifest, target or host)
        for missing in natives(packages, reviewed, out_dirs.get(LINKS, set())):
            print(f"error: {crate} ({target or 'host'}): native package {missing} in its all-features closure is "
                  f"not reviewed in layers.toml's [[native]] (SB0 L5; DEC-188)", file=sys.stderr)
            failed = True
        for hit in scan(packages, out_dirs, allowed):
            print(f"warning: {crate} ({target or 'host'}): global_allocator in its all-features closure: {hit} "
                  f"(advisory; SB0 L5; DEC-176, DEC-188)", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
