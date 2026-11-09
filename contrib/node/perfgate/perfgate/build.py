"""Checkouts and builds of the revisions under comparison.

Each side gets a persistent detached worktree under $PERFGATE_ROOT/worktrees/<side>, with its own target/
(incremental from the previous gate run; CARGO_TARGET_DIR is never set). Criterion benches are built with
the criterion shim of *this* tool patched in for both supported criterion lines, so base and head run under
the same harness even when the base predates the shim.
"""

import functools
import glob
import hashlib
import json
import os
import re
import shutil
import subprocess
import tomllib

from . import paths, sandbox

SHIM = os.path.join(paths.TOOL_DIR, "criterion-shim")
# (patch key, shim dir): criterion 0.8.x replaces `criterion`, 0.5.x a renamed second patch of the same crate.
SHIM_PATCHES = [("criterion", "v0.8"), ("criterion05", "v0.5")]


class BuildError(RuntimeError):
    pass


@functools.cache
def harness_hash():
    """Hash of the harness as this process found it at first use: its Python sources and the shim."""
    return paths.tree_hash(os.path.join(paths.TOOL_DIR, "perfgate"), SHIM)


@functools.cache
def shim_hash():
    """Hash of the criterion shim alone: what the counts depend on (anchors outlive other harness edits)."""
    return paths.tree_hash(SHIM)


@functools.cache
def shim_snapshot():
    """A copy of the shim taken once per process, so that base and head build the same bytes even when the
    checkout running the tool changes mid-run."""
    dest = os.path.join(paths.sub("shim"), harness_hash())
    if not os.path.exists(dest):
        tmp = f"{dest}.{os.getpid()}"
        shutil.copytree(SHIM, tmp, ignore=shutil.ignore_patterns("target", "Cargo.lock"))
        os.replace(tmp, dest)
    return dest


def git(*args, cwd=None, check=True, env=None):
    p = subprocess.run(["git", *args], cwd=cwd or paths.REPO, capture_output=True, text=True,
                       env=None if env is None else {**os.environ, **env})
    if check and p.returncode:
        raise BuildError(f"git {' '.join(args)}: {p.stderr.strip()}")
    return p.stdout.strip()


def resolve(rev, cwd=None):
    return git("rev-parse", "--verify", f"{rev}^{{commit}}", cwd=cwd)


# A fixed identity and date make the synthesized merge commit (and so its cache keys) reproducible.
MERGE_ENV = {"GIT_AUTHOR_NAME": "perfgate", "GIT_AUTHOR_EMAIL": "perfgate@localhost",
             "GIT_COMMITTER_NAME": "perfgate", "GIT_COMMITTER_EMAIL": "perfgate@localhost",
             "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z"}


def merge_candidate(base, head, cwd=None):
    """The commit the gate measures as the head: the tree the manager's `--no-ff` merge of `head` into
    `base` would produce (B1). A head that already contains the base is itself; otherwise a merge commit is
    synthesized with `git merge-tree` (never on any branch). A conflict raises BuildError."""
    base, head = resolve(base, cwd), resolve(head, cwd)
    if subprocess.run(["git", "merge-base", "--is-ancestor", base, head], cwd=cwd or paths.REPO).returncode == 0:
        return head
    p = subprocess.run(["git", "merge-tree", "--write-tree", "--name-only", base, head], cwd=cwd or paths.REPO,
                       capture_output=True, text=True)
    if p.returncode == 1:
        files = [f for f in p.stdout.splitlines()[1:] if f and not f.startswith(("Auto-merging", "CONFLICT"))]
        raise BuildError(f"{head[:12]} does not merge cleanly into {base[:12]} (conflicts: "
                         f"{', '.join(files[:10]) or 'see git merge-tree'}): rebase the branch")
    if p.returncode:
        raise BuildError(f"git merge-tree failed: {p.stderr.strip()}")
    tree = p.stdout.splitlines()[0].strip()
    return git("commit-tree", tree, "-p", base, "-p", head, "-m", f"perfgate merge candidate {head[:12]} into "
               f"{base[:12]}", cwd=cwd, env=MERGE_ENV)


def tool_tree(rev, cwd=None, rel=None):
    """The git tree of this tool's directory (or of `rel`) at `rev` ('' when absent)."""
    rel = rel or os.path.relpath(paths.TOOL_DIR, paths.REPO)
    out = git("ls-tree", "-d", rev, rel, cwd=cwd, check=False)
    return out.split()[2] if out else ""


TOOL_REL = "contrib/node/perfgate"
NODE_BENCH_REL = "node-bench"  # trusted integration code the node rows run (D1)


def node_bench_override():
    """D2: PERFGATE_NODE_BENCH is for local dev only; a gate that uses it is non-authoritative."""
    d = os.environ.get("PERFGATE_NODE_BENCH")
    return f"node-bench overridden: {d}" if d else None


def harness_problems(base):
    """Why the running harness is not the base's (M3): it must be the committed tool tree of `base`, clean."""
    problems = []
    if os.path.relpath(paths.TOOL_DIR, paths.REPO) != TOOL_REL:
        problems.append(f"the running harness is not {TOOL_REL} of a checkout ({paths.TOOL_DIR})")
    elif not tool_tree(base):
        problems.append(f"the base {base[:12]} has no {TOOL_REL}")
    elif tool_tree("HEAD") != tool_tree(base):
        problems.append(f"the running harness ({paths.TOOL_DIR}, at {resolve('HEAD')[:12]}) is not the base's "
                        f"({base[:12]})")
    if git("status", "--porcelain", "--untracked-files=all", "--", paths.TOOL_DIR, check=False):
        problems.append(f"the running harness has uncommitted changes ({paths.TOOL_DIR})")
    # D1: node-bench drives the node rows, so it is pinned to the base the same way.
    nb = os.path.join(paths.REPO, NODE_BENCH_REL)
    if tool_tree("HEAD", rel=NODE_BENCH_REL) != tool_tree(base, rel=NODE_BENCH_REL):
        problems.append(f"the running node-bench ({nb}, at {resolve('HEAD')[:12]}) is not the base's ({base[:12]})")
    if git("status", "--porcelain", "--untracked-files=all", "--", nb, check=False):
        problems.append(f"node-bench has uncommitted changes ({nb})")
    return problems


INCLUDE = re.compile(r'include(?:_bytes|_str)?!\s*\(\s*(?:"([^"]+)"|concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"'
                     r'\s*\)\s*,\s*"([^"]+)")')


def included_files(wt, pdir, srcs):
    """Files the bench sources pull in with include!/include_bytes!/include_str! (a literal path relative to
    the source, or concat!(env!("CARGO_MANIFEST_DIR"), "...")), relative to the worktree."""
    out = set()
    files = []
    for s in srcs:
        full = os.path.join(wt, s)
        if os.path.isdir(full):
            files += [os.path.join(d, f) for d, _ds, fs in os.walk(full) for f in fs if f.endswith(".rs")]
        elif os.path.isfile(full):
            files.append(full)
    for f in files:
        with open(f, errors="replace") as fh:
            for rel, manifest_rel in INCLUDE.findall(fh.read()):
                target = os.path.normpath(os.path.join(os.path.dirname(f), rel) if rel else
                                          os.path.join(pdir, manifest_rel.lstrip("/")))
                if target.startswith(os.path.realpath(wt) + os.sep) and os.path.exists(target):
                    out.add(os.path.relpath(target, wt))
    return out


def bench_glue(root, cdir):
    """A crate's bench glue: every `bench.rs` and `bench/` under its `src/` (BL-426, DEC-514), library code by
    location that only benches drive (dashd-chainstate's `connect/bench.rs`). Found on disk, so each side's
    boundary has the modules its own tree holds; a product module that happens to be named bench only costs a
    stale anchor."""
    out = set()
    for dirpath, dirnames, filenames in os.walk(os.path.join(cdir, "src")):
        if "bench" in dirnames:
            dirnames.remove("bench")
            out.add(os.path.relpath(os.path.join(dirpath, "bench"), root))
        if "bench.rs" in filenames:
            out.add(os.path.relpath(os.path.join(dirpath, "bench.rs"), root))
    return out


def bench_boundary(wt, packages):
    """Everything that can change a gated bench's inputs, per bench package, relative to the worktree:

    - `manifest`: the package manifest;
    - `sources`: the bench sources (a bench under `<pkg>/benches/` is that whole directory: it may use sibling
      modules) and the data files they include; the gate overlays these from the base;
    - `build`: the build scripts of every workspace (or path) package in the bench's dependency closure
      (`build.rs`, even where the base has none, and the declared build script with its directory), the whole
      crates of the proc macros among them, of their local build-dependencies and of every local crate those
      use, their `test_utils` input generators and their bench glue (any `bench.rs` or `bench/` under `src/`);
    - `closure`: the manifests of those packages, whose build scripts, build-dependencies, `links` and
      features count too.
    """
    meta = metadata(wt, no_deps=False)
    pkgs = {p["id"]: p for p in meta["packages"]}
    members = set(meta["workspace_members"])
    deps = {n["id"]: n["deps"] for n in meta["resolve"]["nodes"]}
    root = os.path.realpath(wt)
    out = {}
    for pid, pkg in pkgs.items():
        if pkg["name"] not in packages or pid not in members:
            continue
        seen, todo = {pid}, [pid]
        while todo:  # the workspace packages the bench target is built from: every kind of dependency of the
            cur = todo.pop()  # bench's own package, normal and build dependencies further down
            for d in deps.get(cur, []):
                kinds = {k.get("kind") for k in d.get("dep_kinds", [])}
                local = d["pkg"] in members or pkgs[d["pkg"]].get("source") is None  # path deps too
                if local and d["pkg"] not in seen and (cur == pid or kinds - {"dev"}):
                    seen.add(d["pkg"])
                    todo.append(d["pkg"])
        pdir = os.path.dirname(pkg["manifest_path"])
        srcs = set()
        for t in pkg["targets"]:
            if "bench" in t["kind"]:
                rel = os.path.relpath(t["src_path"], pdir)
                srcs.add(os.path.join(pdir, "benches") if rel.startswith("benches" + os.sep) else t["src_path"])
        srcs = {os.path.relpath(x, root) for x in srcs}
        srcs |= included_files(root, pdir, srcs)
        build_paths, closure, generators = set(), set(), set()
        # Code that runs at compile time and can write anything, the bench included (DEC-76 c): proc macros, the
        # build-dependencies of the closure's crates, and every local crate those two use. Whole crate directories.
        todo = [c for c in seen if any("proc-macro" in t["kind"] for t in pkgs[c]["targets"])]
        todo += [d["pkg"] for c in seen for d in deps.get(c, [])
                 if any(k.get("kind") == "build" for k in d.get("dep_kinds", []))]
        compile_time = set()
        while todo:
            c = todo.pop()
            if c in compile_time or (c not in members and pkgs[c].get("source") is not None):
                continue
            compile_time.add(c)
            todo += [d["pkg"] for d in deps.get(c, []) if {k.get("kind") for k in d.get("dep_kinds", [])} - {"dev"}]
        for cid in compile_time:
            cdir = os.path.dirname(pkgs[cid]["manifest_path"])
            if os.path.realpath(cdir).startswith(root + os.sep):
                build_paths.add(os.path.relpath(cdir, root))
        for cid in seen:
            cdir = os.path.dirname(pkgs[cid]["manifest_path"])
            if not os.path.realpath(cdir).startswith(root + os.sep):
                continue  # a path dependency outside the repository: nothing the PR's tree can change
            closure.add(os.path.relpath(pkgs[cid]["manifest_path"], root))
            build_paths.add(os.path.relpath(os.path.join(cdir, "build.rs"), root))
            # Input generators the benches reach through a `test-utils` feature (Block::dummy and friends):
            # library code by location, bench input by use.
            for name in ("src/test_utils", "src/test_utils.rs", "src/test-utils", "src/test-utils.rs"):
                generators.add(os.path.relpath(os.path.join(cdir, name), root))
            generators |= bench_glue(root, cdir)
            for t in pkgs[cid]["targets"]:
                if "custom-build" in t["kind"]:
                    d = os.path.dirname(t["src_path"])
                    build_paths.add(os.path.relpath(t["src_path"] if d == cdir else d, root))
        out[pkg["name"]] = {"manifest": os.path.relpath(pkg["manifest_path"], root), "sources": sorted(srcs),
                            "build": sorted(build_paths | generators), "closure": sorted(closure)}
    return out


def bench_sources(wt, packages):
    """{package: (manifest path, [bench source paths and included data])}, relative to the worktree."""
    return {k: (v["manifest"], v["sources"]) for k, v in bench_boundary(wt, packages).items()}


def show_toml(rev, path, cwd=None):
    p = subprocess.run(["git", "show", f"{rev}:{path}"], cwd=cwd or paths.REPO, capture_output=True, text=True)
    return tomllib.loads(p.stdout) if p.returncode == 0 else None


BENCH_MANIFEST_KEYS = ("bench", "dev-dependencies", "dev_dependencies")


def dev_dependency_names(toml):
    """The crate names a manifest's dev-dependencies resolve to (following `package = ` renames)."""
    names = set()
    tables = [toml.get(k) or {} for k in ("dev-dependencies", "dev_dependencies")]
    tables += [t.get(k) or {} for t in (toml.get("target") or {}).values() for k in ("dev-dependencies",
                                                                                      "dev_dependencies")]
    for table in tables:
        for key, spec in table.items():
            names.add(spec.get("package", key) if isinstance(spec, dict) else key)
    return names


def lock_closure(rev, names, cwd=None):
    """Every registry or git package `rev`'s Cargo.lock reaches from `names` (the bench's dev-dependencies),
    as (name, version, source, checksum): what the bench's inputs are generated with, transitively."""
    lock = show_toml(rev, "Cargo.lock", cwd) or {}
    by_name = {}
    for p in lock.get("package", []):
        by_name.setdefault(p["name"], []).append(p)
    out, todo = set(), [p for n in names for p in by_name.get(n, [])]
    while todo:
        p = todo.pop()
        key = (p["name"], p["version"], p.get("source"), p.get("checksum"))
        if key in out:
            continue
        out.add(key)
        for dep in p.get("dependencies", []):
            name, _, version = dep.partition(" ")
            todo += [q for q in by_name.get(name, []) if not version or q["version"] == version.split(" ")[0]]
    return {k for k in out if k[2] is not None}  # registry/git only: workspace crates are compared as sources


ROOT_TABLES = ("patch", "replace")


def root_build_tables(root):
    """The workspace manifest's tables that change how every bench is built: the release and bench profiles
    (bench inherits release; package overrides and build-override included), [patch] and [replace]."""
    profile = root.get("profile") or {}
    return {"profile": {k: profile.get(k) for k in ("release", "bench")}, **{k: root.get(k) for k in ROOT_TABLES}}


CLOSURE_KEYS = ("build-dependencies", "build_dependencies", "features")


def build_manifest(toml):
    """The parts of a manifest that shape how a crate is built rather than what it does: its build script and
    `links`, its build-dependencies (also target-specific) and its features."""
    if toml is None:
        return None
    package = toml.get("package") or {}
    out = {k: toml.get(k) for k in CLOSURE_KEYS}
    out.update(build=package.get("build"), links=package.get("links"))
    out["target"] = {t: {k: v.get(k) for k in CLOSURE_KEYS} for t, v in (toml.get("target") or {}).items()}
    return out


def bench_manifest(toml):
    """The parts of a package manifest that define its benches: [[bench]] and dev-dependencies (also
    target-specific ones)."""
    if toml is None:
        return None
    out = {k: toml.get(k) for k in BENCH_MANIFEST_KEYS}
    out["target"] = {t: {k: v.get(k) for k in BENCH_MANIFEST_KEYS} for t, v in (toml.get("target") or {}).items()}
    return out


def bench_changes(base, head, base_wt, packages, cwd=None, head_wt=None, depinfo=()):
    """What `head` changes in the inputs of the benches of `packages` (M2, PF0 r2 ruling 1): (changed files,
    changed manifest parts, the base's bench source paths to overlay). See bench_boundary, computed on both
    sides (a head can add a crate to the closure); the workspace's release/bench profiles, [patch] and
    [replace], the workspace entries of bench dev-dependencies and everything the lockfile resolves them to,
    and `depinfo` (files the base build of the benches read) count too. Toolchain and cargo configuration
    changes are refused before this runs (toolchain_changes)."""
    bounds = [bench_boundary(base_wt, packages)] + ([bench_boundary(head_wt, packages)] if head_wt else [])
    paths_ = sorted({p for b in bounds[0].values() for p in b["sources"]} | set(depinfo))
    watched = sorted(set(paths_) | {p for bs in bounds for b in bs.values() for p in b["sources"] + b["build"]})
    changed = git("diff", "--name-only", base, head, "--", *watched, cwd=cwd).split() if watched else []
    manifest = []
    roots = [show_toml(rev, "Cargo.toml", cwd) or {} for rev in (base, head)]
    dev_names = set()
    for b in bounds[0].values():
        tomls = [show_toml(rev, b["manifest"], cwd) for rev in (base, head)]
        if bench_manifest(tomls[0]) != bench_manifest(tomls[1]):
            manifest.append(f"{b['manifest']} ([[bench]] / dev-dependencies)")
        for t in tomls:
            dev_names |= dev_dependency_names(t or {})
    for mpath in sorted({m for bs in bounds for b in bs.values() for m in b["closure"]}):
        if build_manifest(show_toml(base, mpath, cwd)) != build_manifest(show_toml(head, mpath, cwd)):
            manifest.append(f"{mpath} (build script, links, build-dependencies or features)")
    tables = [root_build_tables(r) for r in roots]
    for k in tables[0]:
        if tables[0][k] != tables[1][k]:
            manifest.append(f"Cargo.toml [{k}]" if k != "profile" else "Cargo.toml [profile.release]/[profile.bench]")
    # Dev-dependencies inherited from the workspace, and every package the lockfile resolves them to: a `rand`
    # (or rand_core) bump, or the same version from another source, changes every seeded input.
    wsdeps = [{n: (r.get("workspace") or {}).get("dependencies", {}).get(n) for n in sorted(dev_names)}
              for r in roots]
    if wsdeps[0] != wsdeps[1]:
        manifest.append("Cargo.toml [workspace.dependencies] of bench dev-dependencies")
    locks = [lock_closure(rev, dev_names, cwd) for rev in (base, head)]
    if locks[0] != locks[1]:
        moved = sorted({k[0] for k in locks[0] ^ locks[1]})
        manifest.append(f"Cargo.lock: {', '.join(moved[:8])}{' …' if len(moved) > 8 else ''} (bench dev-dependency "
                        "closure)")
    return changed, manifest, paths_


def depinfo_files(target, bench_targets, wt):
    """Files the last build of `bench_targets` read (rustc dep-info: sources, #[path] modules, included data),
    relative to the worktree, excluding library `src/` trees (those are the product, measured as such)."""
    root = os.path.realpath(wt)
    out = set()
    for name in bench_targets:
        infos = sorted(glob.glob(os.path.join(target, "release", "deps", f"{name}-*.d")), key=os.path.getmtime)
        if not infos:
            continue
        with open(infos[-1]) as f:
            for line in f:
                head, sep, _rest = line.partition(":")
                if not sep or head.endswith(".d") or not head.startswith(root + os.sep):
                    continue
                rel = os.path.relpath(head, root)
                if "/src/" not in "/" + rel:
                    out.add(rel)
    return sorted(out)


def bench_inputs(rev, wt, packages, cwd=None):
    """{package: hash of everything that defines its benches at `rev`} (the bench_boundary files and manifest
    parts, [profile.bench], the workspace entries and locked versions of its dev-dependencies). An anchor
    records it, so a later bench-input change makes the anchor stale for that package."""
    out = {}
    root = show_toml(rev, "Cargo.toml", cwd) or {}
    for pkg, b in bench_boundary(wt, packages).items():
        toml = show_toml(rev, b["manifest"], cwd) or {}
        names = dev_dependency_names(toml)
        files = sorted(set(b["sources"]) | set(b["build"]))
        h = hashlib.sha256(git("ls-tree", "-r", rev, "--", *files, cwd=cwd).encode() if files else b"")
        h.update(json.dumps([bench_manifest(toml), root_build_tables(root),
                             {n: (root.get("workspace") or {}).get("dependencies", {}).get(n) for n in sorted(names)},
                             sorted(lock_closure(rev, names, cwd)),
                             [build_manifest(show_toml(rev, m, cwd)) for m in b["closure"]]],
                            sort_keys=True, default=str).encode())
        out[pkg] = h.hexdigest()[:16]
    return out


def overlay_bench_sources(wt, base, paths_):
    """Check the base's bench sources out over the head worktree, so the head's library code is measured with
    the base's inputs and harness code (M2). Paths the base lacks are left alone."""
    present = [p for p in paths_ if git("ls-tree", base, "--", p, check=False)]
    if present:
        git("checkout", base, "--", *present, cwd=wt)
    return present


def prepare_worktree(side, sha, lock_from=None):
    """A clean detached checkout of `sha` that keeps its target/ (and an untracked Cargo.lock) between runs.

    When the revision does not track Cargo.lock, `lock_from` (the base's resolved lockfile) is copied in,
    so that both sides build against the same dependency versions.
    """
    if not re.fullmatch(r"[A-Za-z0-9_-]+", side):
        raise BuildError(f"worktree label {side!r}: use letters, digits, '-' and '_' only")
    parent = os.path.realpath(paths.sub("worktrees"))
    wt = os.path.join(parent, side)
    if os.path.dirname(os.path.realpath(wt)) != parent:
        raise BuildError(f"refusing worktree {wt}: it resolves outside {parent}")
    if os.path.exists(os.path.join(wt, ".git")):
        git("checkout", "--detach", "--force", sha, cwd=wt)
        git("clean", "-ffdxq", "-e", "/target/", "-e", "/Cargo.lock", cwd=wt)
    else:
        if os.path.lexists(wt):
            shutil.rmtree(wt)
        git("worktree", "add", "--detach", "--force", wt, sha)
    tracked = git("ls-files", "--", "Cargo.lock", cwd=wt)
    if not tracked and lock_from and os.path.exists(lock_from):
        shutil.copyfile(lock_from, os.path.join(wt, "Cargo.lock"))
    return wt


def shim_config():
    args = []
    for key, sub in SHIM_PATCHES:
        args += ["--config", f'patch.crates-io.{key}.path="{os.path.join(shim_snapshot(), sub)}"']
        if key != "criterion":
            args += ["--config", f'patch.crates-io.{key}.package="criterion"']
    return args


def metadata(wt, no_deps=True):
    cmd = ["cargo", "metadata", "--format-version", "1", *shim_config()] + (["--no-deps"] if no_deps else [])
    p = subprocess.run(cmd, cwd=wt, env=cargo_env(), capture_output=True, text=True)
    if p.returncode:
        raise BuildError(f"cargo metadata failed in {wt}: {p.stderr.strip()[-2000:]}")
    return json.loads(p.stdout)


SHIM_MARKER = b"perfgate criterion shim: "


def registry_criterion(wt):
    """Versions of criterion that Cargo.lock still takes from crates.io rather than from the shim."""
    with open(os.path.join(wt, "Cargo.lock"), "rb") as f:
        lock = tomllib.load(f)
    return sorted(p["version"] for p in lock.get("package", []) if p["name"] == "criterion" and p.get("source"))


def lock_dependencies(wt, log=print):
    """Resolve Cargo.lock with the shim patched in, before anything is built, so that it can be fingerprinted
    and handed to the head even when every base result comes from the cache.

    A tracked lockfile pins criterion from crates.io, and cargo keeps a locked version over a `--config`
    patch; `cargo update -p criterion@<locked>` moves it to the shim (offline first: the shim only needs
    crates already in the local registry).
    """
    metadata(wt, no_deps=False)
    locked, left = registry_criterion(wt), []
    if locked:
        cmd = ["cargo", "update", *shim_config(), *(a for v in locked for a in ("-p", f"criterion@{v}"))]
        for extra in (["--offline"], []):
            p = subprocess.run([*cmd, *extra], cwd=wt, env=cargo_env(), capture_output=True, text=True)
            if p.returncode == 0:
                break
        left = registry_criterion(wt)
    # Everything the sandboxed, offline build will need, fetched here (cargo runs no package code to fetch).
    p = subprocess.run(["cargo", "fetch", "--locked", *shim_config()], cwd=wt, env=cargo_env(), capture_output=True,
                       text=True)
    if p.returncode:
        raise BuildError(f"cargo fetch failed in {wt}: {p.stderr.strip()[-2000:]}")
    if left:
        log(f"warning: Cargo.lock in {wt} keeps criterion {', '.join(left)} from crates.io (the shim covers "
            "0.5 and 0.8); benches built against it are reported as errors")


def targets(meta):
    """{(package, kind, target name): manifest dir}."""
    out = {}
    for pkg in meta["packages"]:
        for t in pkg["targets"]:
            for kind in t["kind"]:
                out[(pkg["name"], kind, t["name"])] = os.path.dirname(pkg["manifest_path"])
    return out


def cargo_env():
    """The environment for cargo runs that execute no code under test (metadata, update, fetch): the same
    allowlist as sandboxed builds, so nothing can redirect target/ or leak in unrecorded."""
    return sandbox.build_env(os.environ.get("TMPDIR", "/tmp"))


# Files that choose the toolchain or reconfigure cargo: a head that changes one could make even the gate's
# own `cargo metadata` or `rustc -vV` run its binaries, so such a head is refused before any cargo runs in it.
TOOLCHAIN_FILES = re.compile(r"(^|/)(rust-toolchain(\.toml)?|\.cargo/config(\.toml)?)$")


def toolchain_changes(base, head, cwd=None):
    """Tracked files of `head` the gate refuses to run cargo over: toolchain and cargo configuration files that
    differ from `base`, and a symlinked Cargo.lock (cargo update, outside the sandbox, would write through it)."""
    changed = git("diff", "--name-only", base, head, cwd=cwd).split()
    out = [f for f in changed if TOOLCHAIN_FILES.search(f)]
    lock = git("ls-tree", head, "--", "Cargo.lock", cwd=cwd, check=False)
    if lock.startswith("120000"):
        out.append("Cargo.lock (a symlink)")
    return out


def target_dir(wt, fresh_from=None):
    """The target directory of a worktree slot: $ROOT/targets/<label>, outside the worktree, so nothing the
    PR tracks (a `target` symlink, say) can redirect it. `fresh_from` (a label) starts it as a reflinked copy
    of that slot's target, so a head never builds on a previous head's artifacts but still reuses the base's
    compiled registry crates."""
    parent = os.path.realpath(paths.sub("targets"))
    target = os.path.join(parent, os.path.basename(wt))
    if fresh_from is not None:
        if os.path.lexists(target):
            shutil.rmtree(target)
        seed = os.path.join(parent, fresh_from)
        if os.path.isdir(seed) and not os.path.islink(seed):
            subprocess.run(["cp", "-a", "--reflink=auto", seed, target], check=True)
    os.makedirs(target, exist_ok=True)
    if os.path.islink(target) or os.path.dirname(os.path.realpath(target)) != parent:
        raise BuildError(f"refusing target directory {target}: not a real directory under {parent}")
    return target


def cargo_json(cmd, wt, log):
    """Run a cargo build of `wt` into its target directory (target_dir) with JSON messages, inside the sandbox
    (sandbox.py): build scripts, proc macros and the compiler can write only that and a scratch directory.
    Returns the compiler-artifact messages, whose executables must be inside the target directory."""
    target = target_dir(wt)
    tmp = sandbox.scratch("build", os.path.basename(wt))
    cmd = [*cmd, "--locked", "--offline", "--target-dir", target]
    with open(log, "a") as lf:
        lf.write(f"$ {' '.join(cmd)}\n")
        lf.flush()
        p = subprocess.run(sandbox.wrap(cmd, writable=[target, tmp], cwd=wt, env=sandbox.build_env(tmp)), cwd=wt,
                           stdout=subprocess.PIPE, stderr=lf, text=True)
    if p.returncode:
        with open(log) as lf:
            tail = lf.read()[-3000:]
        raise BuildError(f"build failed in {wt} (log {log}):\n{tail}")
    msgs = [json.loads(line) for line in p.stdout.splitlines() if line.startswith("{")]
    arts = [m for m in msgs if m.get("reason") == "compiler-artifact" and m.get("executable")]
    inside = os.path.realpath(target) + os.sep
    for m in arts:
        if not os.path.realpath(m["executable"]).startswith(inside):
            raise BuildError(f"cargo put {m['executable']} outside the target directory {target}")
    return arts


def build_benches(wt, wanted, log):
    """Build the (package, bench target) pairs that exist at this revision, one cargo invocation per package.

    One package per invocation matters: cargo unifies dependency features across the packages selected
    together, so building dash-spv alongside dashcore compiles different code than building it alone, and
    a bench's counts would then depend on which other workloads happened to be rebuilt (this moved
    storage/disk/get by 15% between a full and a partial base build). Built alone, a package's benches
    are what `cargo bench -p <package>` runs.

    Returns ({(package, target): (executable, package dir)}, {(package, target): reason it is missing}).
    """
    tmap = targets(metadata(wt))
    present = {(p, t) for (p, kind, t) in tmap if kind == "bench"}
    missing = {pt: "bench target not in this revision" for pt in wanted if pt not in present}
    exes = {}
    for pkg in sorted({p for p, t in wanted if (p, t) not in missing}):
        names = sorted(t for p, t in wanted if p == pkg and (p, t) not in missing)
        cmd = ["cargo", "bench", "--no-run", "--message-format=json-render-diagnostics", *shim_config(), "-p", pkg]
        for t in names:
            cmd += ["--bench", t]
        for m in cargo_json(cmd, wt, log):
            if "bench" in m["target"]["kind"] and m["target"]["name"] in names:
                exes[(pkg, m["target"]["name"])] = (m["executable"], os.path.dirname(m["manifest_path"]))
    for pt in wanted:
        if pt not in missing and pt not in exes:
            missing[pt] = "cargo produced no executable"
    return exes, missing


def uses_shim(exe):
    """Whether a bench executable was linked against the shim rather than crates.io criterion (which would
    ignore the shim's protocol and benchmark everything for minutes)."""
    with open(exe, "rb") as f:
        return SHIM_MARKER in f.read()


def build_bins(wt, names, log):
    """Release builds of the named binary targets that exist at this revision: {name: path}.

    One invocation per package, for the same feature-unification reason as build_benches. The shim stays
    patched in (harmless: it only replaces criterion, a dev-dependency): lock_dependencies resolved Cargo.lock
    with it, and `--locked` refuses a lockfile that disagrees with the build's patches.
    """
    tmap = targets(metadata(wt))
    found = {t: p for (p, kind, t) in tmap if kind == "bin" and t in names}
    out = {}
    for t, p in sorted(found.items()):
        cmd = ["cargo", "build", "--release", "--message-format=json-render-diagnostics", *shim_config(), "-p", p,
               "--bin", t]
        out.update({m["target"]["name"]: m["executable"] for m in cargo_json(cmd, wt, log)
                    if "bin" in m["target"]["kind"] and m["target"]["name"] == t})
    return out


def toolchain(wt):
    """The compilers and cargo configuration a build in `wt` uses (cargo config is searched upwards from the
    worktree and in CARGO_HOME, so the agentbox linker and rustflags settings are covered)."""

    def out(*cmd):
        p = subprocess.run(cmd, cwd=wt, capture_output=True, text=True)
        return p.stdout.strip() if p.returncode == 0 else None

    h = hashlib.sha256()
    root = os.path.realpath(wt)
    dirs, d = [], root
    while True:
        dirs.append(os.path.join(d, ".cargo"))
        if d == os.path.dirname(d):
            break
        d = os.path.dirname(d)
    dirs.append(os.environ.get("CARGO_HOME") or os.path.expanduser("~/.cargo"))
    for d in dirs:
        for name in ("config", "config.toml"):
            p = os.path.join(d, name)
            if os.path.isfile(p):
                # Paths inside the worktree by their relative name: base and head live in different dirs.
                h.update((os.path.relpath(p, root) if p.startswith(root + os.sep) else p).encode())
                with open(p, "rb") as f:
                    h.update(f.read())
    cc = out("cc", "--version")
    return {"rustc": out("rustc", "-vV"), "cargo": out("cargo", "-vV"), "cc": cc.splitlines()[0] if cc else None,
            "cargo_config": h.hexdigest()[:16]}
