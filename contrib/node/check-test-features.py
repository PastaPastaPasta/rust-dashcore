#!/usr/bin/env python3
"""D50 (O2b-4 §6.5): no shipping product resolves a test-only feature, and no test target
that links the oracle leaves node-gate's local oracle lane.

  check-test-features.py [--root DIR] [--offline] | --selftest

For each layer-A product in contrib/node/layers.toml's [crates], with its
default features and every feature set its [[product]] rows ship, cargo
resolves the product alone, as its binaries and libraries build:

    cargo tree --locked -p <pkg> [--features ...] -e normal,build,features \
        --target all --prefix none --format '{p} {f}'

so the libraries a product ships come with it, every target platform counts
(a `cfg(target_os = "ios")` dependency's features too), and dev-dependency
edges do not. Never workspace all-features (check-layers.py's discipline:
one package at a time, `--target all`, normal and build edges). A cargo error
fails closed.

A resolved package is banned with a feature by its identity, its name and a
path source inside the checked workspace, so a same-named feature on an
unrelated crate does not match: BANNED's (package, feature) pairs, and every
package of BANNED_PACKAGES with any features.

A `cargo tree` line that is neither a package nor a feature node is a problem
too: a line the check cannot read could hide a banned package.

--selftest runs the nine controls of contrib/node/check-test-features/
fixtures/, each a small workspace resolved --offline: (i) a banned feature on
a normal edge fails, (ii) the same feature on a dev edge only passes, (iii) a
banned feature on a `cfg(target_os = "ios")` normal edge fails, (iv) one on a
normal edge to a proc-macro crate fails, (v) a path cargo prints with
parentheses, no feature banned, fails as a line not read, (vi) dashd-chainstate's
`planted-bugs` on a normal edge fails (O2b-4 4b's planted searches), (vii) the
same reached through dashd-node's `planted-bugs` fails on both, (viii) the
package node-l1 on a normal edge, no features, fails, (ix) a test target whose
required features turn the oracle on only through another feature, or through
`dash-oracle-sys/oracle`, fails, while one that also names a local-lane
feature, one with `test = false` and one that never reaches the oracle pass.
The selftest also checks LOCAL_LANE against node-gate.yml's LOCAL.

Planner lanes: node-gate's feature-gated test planner (BL-608) keeps a test
target in the local oracle lane only when its *declared* required-features name
one of LOCAL_LANE (the oracle archives stay local, DEC-650/BL-563). It does not
expand features. So a workspace test target whose required features, expanded
through its package's own [features], turn the oracle on (a LOCAL_LANE feature
or `dash-oracle-sys/oracle`) must declare a LOCAL_LANE feature itself, or the
planner does not keep it local and it cannot link. Like the planner, a target
with `test = false` is skipped. Only the target's own package's [features] are
followed: a feature of another workspace crate (`dep:X`, `X/f`) and a
dependency's declared `features = [...]` are not (BL-794), so such a target
still needs its LOCAL_LANE feature named by hand.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FIXTURES = os.path.join(ROOT, "contrib", "node", "check-test-features", "fixtures")

# Test-only features of node crates: the harnesses' seams, never a product's.
BANNED = {
    ("dash-chain", "test-util"),
    ("dash-kv", "image"),
    ("dashd-chainstate", "test-support"),
    ("dashd-mempool", "test-support"),
    ("dashd-chainstate", "planted-bugs"),
    ("dashd-node", "test-support"),
    ("dashd-node", "planted-bugs"),
    ("dashd-node", "test-hooks"),
}
# Packages no product may resolve at all.
BANNED_PACKAGES = {"node-l1"}

# node-gate.yml's planner LOCAL: the features that keep a test target in the local oracle lane.
LOCAL_LANE = {"oracle", "sut", "l3"}
# The entries that link the Core oracle archive (dash-oracle-sys's build.rs links nothing without it).
ORACLE_ENTRIES = {"dash-oracle-sys/oracle", "dash-oracle-sys?/oracle"}

# `{p} {f}`: `name vVERSION [(proc-macro)] [(source)] features`, a repeated subtree ending ` (*)`.
# Packages from a registry print no source. A path crate's source is its absolute directory; a
# git or file URL is another identity.
LINE = re.compile(
    r"^(?P<name>\S+) v(?P<version>\S+)(?: \(proc-macro\))?(?: \((?P<source>[^()]*)\))?"
    r"(?: (?P<features>[^ ()]*))?(?: \(\*\))?$"
)
# The features edges' nodes: `name feature "f"`, a repeated one ending ` (*)`. Their packages'
# lines carry the same features, so these are read only to be recognised.
FEATURE_NODE = re.compile(r'^\S+ feature "[^"]*"(?: \(\*\))?$')


def products(root):
    """[(package, features)] to resolve: each layer-A crate with its default features, and each
    [[product]] row's feature set for it."""
    with open(os.path.join(root, "contrib", "node", "layers.toml"), "rb") as f:
        layers = tomllib.load(f)
    names = sorted(n for n, layer in layers.get("crates", {}).items() if layer == "A")
    out = {(n, ()) for n in names}
    for row in layers.get("product", []):
        if row.get("package") in names:
            out.add((row["package"], tuple(sorted(row.get("features", [])))))
    return sorted(out)


def resolve(root, package, features, offline):
    """The `{p} {f}` lines of `package`'s own resolution; dies on a cargo error."""
    cmd = ["cargo", "tree", "--locked", "-q", "-p", package, "-e", "normal,build,features", "--target", "all",
           "--prefix", "none", "--format", "{p} {f}"]
    if features:
        cmd += ["--features", ",".join(features)]
    if offline:
        cmd.append("--offline")
    r = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"error: {' '.join(cmd)} failed in {root}:\n{r.stderr.strip()}")
    return r.stdout.splitlines()


def banned(root, lines):
    """The banned (package, feature) identities `lines` resolve, sorted, and the lines neither
    shape reads (each a problem: a line it cannot read could be a banned package)."""
    root = os.path.realpath(root)
    hits, unread = set(), []
    for line in lines:
        line = line.rstrip()
        if not line or FEATURE_NODE.match(line):
            continue
        m = LINE.match(line)
        if not m:
            unread.append(line)
            continue
        if not m.group("source") or not os.path.isabs(m.group("source")):
            continue  # a registry, git or file URL source: not this workspace's crate
        source = os.path.realpath(m.group("source"))
        if os.path.commonpath([root, source]) != root:
            continue  # not this workspace's crate: another identity
        name, features = m.group("name"), [f for f in (m.group("features") or "").split(",") if f]
        if name in BANNED_PACKAGES:
            hits.add((name, "*"))
        hits |= {(name, f) for f in features if (name, f) in BANNED}
    return sorted(hits), unread


def lanes(root, offline):
    """Problems, one per workspace test target the planner would not keep in the local oracle
    lane although its required features turn the oracle on."""
    cmd = ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"] + (["--offline"] if offline else [])
    r = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"error: {' '.join(cmd)} failed in {root}:\n{r.stderr.strip()}")
    meta = json.loads(r.stdout)
    problems = []
    for p in sorted((p for p in meta["packages"] if p["id"] in meta["workspace_members"]), key=lambda p: p["name"]):
        table = p["features"]
        for t in sorted(p["targets"], key=lambda t: t["name"]):
            need = set(t.get("required-features") or ())
            if "test" not in t["kind"] or t.get("test") is False or not need or need & LOCAL_LANE:
                continue
            seen, todo, oracle = set(), list(need), False
            while todo:
                f = todo.pop()
                if f in seen:
                    continue
                seen.add(f)
                oracle |= f in LOCAL_LANE
                for e in table.get(f, ()):
                    if e in table:
                        todo.append(e)
                    oracle |= e in ORACLE_ENTRIES
            if oracle:
                problems.append(f"{p['name']} test {t['name']}: required-features [{','.join(sorted(need))}] turn the "
                                f"oracle on but name none of {','.join(sorted(LOCAL_LANE))}, so node-gate's planner "
                                "does not keep it in the local oracle lane")
    return problems


def check(root, offline=False):
    """Problems, one per product configuration and banned identity, then one per test target
    off its oracle lane (empty when none)."""
    problems = []
    for package, features in products(root):
        label = package + (f" --features {','.join(features)}" if features else "")
        hits, unread = banned(root, resolve(root, package, features, offline))
        for name, feature in hits:
            what = f"package {name}" if feature == "*" else f"{name} feature \"{feature}\""
            problems.append(f"{label} resolves {what}")
        problems += [f"{label}: cargo tree line not read: {line!r}" for line in unread]
    return problems + lanes(root, offline)


def selftest():
    banned_feature = ["prod resolves dashd-node feature \"test-support\""]
    unread = os.path.join(FIXTURES, "paren-path", "dep(x)")
    expect = {
        "normal-edge": banned_feature,
        "dev-edge": [],
        "ios-edge": banned_feature,
        "proc-macro-edge": banned_feature,
        "paren-path": [f"prod: cargo tree line not read: 'dep v0.1.0 ({unread})'"],
        "planted-edge": ["prod resolves dashd-chainstate feature \"planted-bugs\""],
        "planted-transitive": ["prod resolves dashd-chainstate feature \"planted-bugs\"",
                               "prod resolves dashd-node feature \"planted-bugs\""],
        "node-l1-package": ["prod resolves package node-l1"],
        "planner-lane": [f"node-l1 test {name}: required-features [{need}] turn the oracle on but name none of "
                         "l3,oracle,sut, so node-gate's planner does not keep it in the local oracle lane"
                         for name, need in (("planted_only", "planted"), ("shim_only", "shim"))],
    }
    with open(os.path.join(ROOT, ".github", "workflows", "node-gate.yml")) as f:
        local = re.search(r"^\s*LOCAL = \{(.*?)\}", f.read(), re.M | re.S)
    if not local or set(re.findall(r'"([^"]+)":', local.group(1))) != LOCAL_LANE:
        sys.exit(f"selftest: node-gate.yml's LOCAL is not LOCAL_LANE {sorted(LOCAL_LANE)}")
    seen = sorted(os.listdir(FIXTURES))
    if seen != sorted(expect):
        sys.exit(f"selftest: fixtures {seen}, expected {sorted(expect)}")
    for name, want in sorted(expect.items()):
        problems = check(os.path.join(FIXTURES, name), offline=True)
        if problems != want:
            sys.exit(f"selftest: {name}: problems {problems}, expected {want}")
    print(f"check-test-features selftest: ok ({len(expect)} controls)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", default=ROOT, help="the workspace to check (default: this repository)")
    ap.add_argument("--offline", action="store_true", help="resolve without the network")
    ap.add_argument("--selftest", action="store_true", help="run the fixture controls")
    args = ap.parse_args()
    if args.selftest:
        selftest()
        return
    problems = check(args.root, args.offline)
    for p in problems:
        print(f"D50: {p}")
    if problems:
        sys.exit(1)
    print(f"check-test-features: ok ({len(products(args.root))} product configurations, test-target lanes)")


if __name__ == "__main__":
    main()
