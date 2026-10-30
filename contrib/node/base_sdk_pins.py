#!/usr/bin/env python3
"""Reads and moves the workspace's base SDK pin (A1 §1.6, rule R1).

The root `Cargo.toml` declares every base SDK package that members use once in
`[workspace.dependencies]` (upstream git URL, upstream `rev`) and redirects it
through `[patch."<upstream>"]` to one `rev` of the local fork. Packages reached
only through other base SDK packages come from the fork as path dependencies
and are not listed. This script checks that layout and the committed
`Cargo.lock`, that the repository's other workspaces (node-fuzz, cs2-diff, …)
repeat the root's [patch], that Cargo really builds the pinned fork (no path
override or source replacement in a config file or the environment), and
rewrites both `rev`s.

Subcommands:
    check            exit 1 with one line per violation
    show             print `<fork-url> <fork-rev> <upstream-rev>`
    fork             print the [patch] URL (empty without a [patch]); for CI
    set FORK UP      rewrite the fork and upstream revs in the root manifest and
                     in every other workspace with a base SDK [patch]. Expects
                     each entry on one line as `git = "…", rev = "…"`, the
                     layout bump-base-sdk.sh and the guide use.
    nested           print the other workspaces with a base SDK [patch], which
                     bump-base-sdk.sh relocks after `set`
"""

import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

UPSTREAM_URL = "https://github.com/dashpay/base-sdk"
FORK_BRANCH = "node/base-sdk"

# Packages of the base SDK workspace, so a stray copy is caught even when it
# comes from a path, a registry or another git URL. The check adds the members
# of the pinned fork revision, so packages added later are covered too.
BASE_SDK_PACKAGES = {
    "dash-dev",
    "dash-num",
    "dash-p2p-core",
    "dash-params",
    "dash-pkc",
    "dash-pow",
    "dash-primitives",
    "dash-script",
    "dash-types",
    "dash-types-marker",
}

DEP_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
ROOT = Path(__file__).resolve().parents[2]


def load_toml(path: Path) -> dict:
    return tomllib.loads(path.read_text())


def git(repo: Path, *args: str) -> subprocess.CompletedProcess:
    # Replace refs would let Git show other trees than the commits hold.
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True,
                          env={**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"})


def only(values: set, default):
    return next(iter(values)) if len(values) == 1 else default


def report(errors: list) -> bool:
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    return bool(errors)


def is_base_sdk_dep(name: str, spec, packages: set) -> bool:
    if not isinstance(spec, dict):
        return name in packages
    if spec.get("package", name) in packages:
        return True
    return "base-sdk" in spec.get("git", "") or "base-sdk" in spec.get("path", "")


def dep_tables(manifest: dict):
    """Yields (label, table) for every dependency table of a manifest."""
    scopes = [("", manifest)] + [(f"target.{t}.", body) for t, body in manifest.get("target", {}).items()]
    for prefix, body in scopes:
        for key in DEP_TABLES:
            if key in body:
                yield prefix + key, body[key]


def pins(manifest: dict, errors: list) -> tuple[str, str, str]:
    """Returns (fork_url, fork_rev, upstream_rev) and records layout errors."""
    declared = {
        name: spec
        for name, spec in manifest.get("workspace", {}).get("dependencies", {}).items()
        if is_base_sdk_dep(name, spec, BASE_SDK_PACKAGES)
    }
    patched = manifest.get("patch", {}).get(UPSTREAM_URL, {})

    for table, body in manifest.get("patch", {}).items():
        if table == UPSTREAM_URL:
            continue
        for name, spec in body.items():
            if is_base_sdk_dep(name, spec, BASE_SDK_PACKAGES):
                errors.append(f"[patch.{table!r}] redirects base SDK package `{name}`; only "
                              f"[patch.\"{UPSTREAM_URL}\"] may")

    if not patched:
        errors.append(f"root Cargo.toml has no [patch.\"{UPSTREAM_URL}\"] table")
    if set(declared) != set(patched):
        errors.append(f"[workspace.dependencies] declares {sorted(declared)} but "
                      f"[patch] redirects {sorted(patched)}; they must list the same packages")

    upstream_revs = set()
    for name, spec in declared.items():
        if not isinstance(spec, dict) or spec.get("git") != UPSTREAM_URL or "rev" not in spec:
            errors.append(f"[workspace.dependencies] `{name}` must be `git = \"{UPSTREAM_URL}\"` "
                          f"with a `rev`")
            continue
        upstream_revs.add(spec["rev"])

    fork = set()
    for name, spec in patched.items():
        if not isinstance(spec, dict) or set(spec) != {"git", "rev"}:
            errors.append(f"[patch] `{name}` must be exactly `{{ git = \"file://…\", rev = \"<sha>\" }}`")
            continue
        if not spec["git"].startswith("file://"):
            errors.append(f"[patch] `{name}` points at {spec['git']}, not the local fork (file://)")
        fork.add((spec["git"], spec["rev"]))

    for label, revs in (("upstream", upstream_revs), ("fork", {rev for _, rev in fork})):
        for rev in revs:
            if not SHA_RE.match(rev):
                errors.append(f"{label} rev `{rev}` is not a full 40-hex commit id")
    if len(upstream_revs) > 1:
        errors.append(f"[workspace.dependencies] uses several upstream revs: {sorted(upstream_revs)}")
    if len(fork) > 1:
        errors.append(f"[patch] uses several fork sources: {sorted(fork)}")

    url, rev = only(fork, ("", ""))
    return url, rev, only(upstream_revs, "")


def check_fork(repo: Path, rev: str, upstream: str, errors: list) -> set:
    """Checks the fork revision and returns the package names it contains."""
    if not repo.is_dir():
        errors.append(f"fork repository {repo} does not exist")
        return set()
    # Older rust-dashcore commits pin revisions that a rebase moved off the
    # branch; the tags bump-base-sdk.sh creates keep those valid.
    on_branch = git(repo, "merge-base", "--is-ancestor", rev, FORK_BRANCH).returncode == 0
    if not on_branch and not git(repo, "tag", "--contains", rev, "-l", "node-base-sdk-*").stdout.strip():
        errors.append(f"fork rev {rev} is not on `{FORK_BRANCH}` or a node-base-sdk-* tag in {repo}; "
                      f"pin only merged revisions")
    if upstream and git(repo, "merge-base", "--is-ancestor", upstream, rev).returncode != 0:
        errors.append(f"upstream rev {upstream} is not an ancestor of fork rev {rev}; re-pin with "
                      f"contrib/node/bump-base-sdk.sh")

    return fork_packages(repo, rev, errors)


def fork_packages(repo: Path, rev: str, errors: list) -> set:
    """Returns the package names of the fork's workspace at `rev`."""
    packages = set()
    root = git(repo, "show", f"{rev}:Cargo.toml")
    if root.returncode != 0:
        errors.append(f"cannot read Cargo.toml at fork rev {rev}: {root.stderr.strip()}")
        return packages
    for member in tomllib.loads(root.stdout).get("workspace", {}).get("members", []):
        manifest = git(repo, "show", f"{rev}:{member}/Cargo.toml")
        if manifest.returncode != 0:
            errors.append(f"cannot read {member}/Cargo.toml at fork rev {rev}: {manifest.stderr.strip()}")
            continue
        packages.add(tomllib.loads(manifest.stdout)["package"]["name"])
    return packages


def check_members(packages: set, errors: list) -> None:
    meta = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                          cwd=ROOT, capture_output=True, text=True)
    if meta.returncode != 0:
        errors.append(f"cargo metadata failed:\n{meta.stderr.strip()}")
        return
    for package in json.loads(meta.stdout)["packages"]:
        manifest_path = Path(package["manifest_path"])
        rel = manifest_path.relative_to(ROOT)
        for label, table in dep_tables(load_toml(manifest_path)):
            for name, spec in table.items():
                if is_base_sdk_dep(name, spec, packages) and not (isinstance(spec, dict) and spec.get("workspace")):
                    errors.append(f"{rel} [{label}] `{name}` pins base SDK itself; use "
                                  f"`{name} = {{ workspace = true, … }}`")


def check_lock(url: str, rev: str, packages: set, errors: list) -> None:
    locked = subprocess.run(["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"],
                            cwd=ROOT, capture_output=True, text=True)
    if locked.returncode != 0:
        errors.append("cargo metadata --locked failed: Cargo.lock is missing or stale (run "
                      "`cargo metadata` and commit it), or a git source could not be fetched:\n"
                      + locked.stderr.strip())
        return
    want = f"git+{url}?rev={rev}#{rev}"
    check_resolved(json.loads(locked.stdout), want, rev, packages, errors)

    lock = load_toml(ROOT / "Cargo.lock")
    found = False
    for package in lock.get("package", []):
        source = package.get("source", "path")
        if is_base_sdk_package(package["name"], source, packages):
            found = True
            if source != want:
                errors.append(f"Cargo.lock: `{package['name']}` comes from {source}, not the "
                              f"pinned fork {want}")
    if not found:
        errors.append("Cargo.lock has no base SDK package")
    for unused in lock.get("patch", {}).get("unused", []):
        errors.append(f"Cargo.lock: patch for `{unused['name']}` is unused; remove it from "
                      f"[workspace.dependencies] and [patch]")


def nested_workspaces(errors: list) -> list:
    """Returns (path, manifest) for the repository's other workspaces (a
    `[workspace]` table of their own, such as node-fuzz and cs2-diff), new ones
    included."""
    listed = git(ROOT, "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "*Cargo.toml")
    if listed.returncode != 0:
        errors.append(f"git ls-files failed: {listed.stderr.strip()}")
        return []
    found = []
    for rel in sorted(set(listed.stdout.split("\0")) - {"", "Cargo.toml"}):
        path = ROOT / rel
        if path.name != "Cargo.toml" or not path.is_file():
            continue
        try:
            manifest = load_toml(path)
        except (OSError, ValueError) as error:  # TOML or UTF-8
            errors.append(f"{rel} cannot be read: {error}")
            continue
        if "workspace" in manifest:
            found.append((path, manifest))
    return found


def pin_text(spec) -> str:
    if isinstance(spec, dict) and set(spec) == {"git", "rev"}:
        return f"{spec['git']}@{spec['rev']}"
    return "missing" if spec is None else repr(spec)


def same_url(a: str, b: str) -> bool:
    """Compares git URLs as Cargo does: without a trailing `/` or `.git`, and
    GitHub URLs case-insensitively."""
    def canonical(url: str) -> str:
        url = url.rstrip("/").removesuffix(".git")
        return url.lower() if url.lower().startswith("https://github.com/") else url
    return canonical(a) == canonical(b)


def patched_nested(errors: list) -> list:
    """Returns the manifests of the other workspaces with a base SDK [patch]."""
    return [path for path, manifest in nested_workspaces(errors) if UPSTREAM_URL in manifest.get("patch", {})]


def check_nested(root_patch: dict, url: str, rev: str, packages: set, errors: list) -> list:
    """Checks that the other workspaces build base SDK only from the root's pin
    or from fork revisions they name, and returns their directories. A
    workspace with a base SDK [patch] must list every root entry unchanged,
    point an entry for a package only it uses at the same fork source, declare
    its base SDK dependencies from upstream so the [patch] applies, and lock
    only that source. A workspace without one may pin fork revisions directly
    (dde-probe compares two) and must lock nothing else."""
    want = f"git+{url}?rev={rev}#{rev}"
    nested = nested_workspaces(errors)
    for path, manifest in nested:
        rel = path.relative_to(ROOT)
        patch = None
        for key, body in manifest.get("patch", {}).items():
            if key == UPSTREAM_URL:
                patch = body
            elif same_url(key, UPSTREAM_URL):
                errors.append(f"{rel} [patch.{key!r}] patches base SDK; spell its key \"{UPSTREAM_URL}\" as the "
                              f"root does")
            else:
                errors.extend(f"{rel} [patch.{key!r}] redirects base SDK package `{name}`; only "
                              f"[patch.\"{UPSTREAM_URL}\"] may"
                              for name, spec in body.items() if is_base_sdk_dep(name, spec, packages))
        if patch is not None:
            for name, spec in root_patch.items():
                if patch.get(name) != spec:
                    errors.append(f"{rel} [patch] `{name}` is {pin_text(patch.get(name))}, the root's is "
                                  f"{pin_text(spec)}; repeat the root's [patch]")
            for name, spec in patch.items():
                if name not in root_patch and spec != {"git": url, "rev": rev}:
                    errors.append(f"{rel} [patch] `{name}` is {pin_text(spec)}; an entry the root lacks "
                                  f"must point at the root's fork source {url}@{rev}")
        declared = ("workspace.dependencies", manifest["workspace"].get("dependencies", {}))
        for label, table in [*dep_tables(manifest), declared]:
            for name, spec in table.items():
                if not is_base_sdk_dep(name, spec, packages) or (isinstance(spec, dict) and spec.get("workspace")):
                    continue
                source = spec.get("git") if isinstance(spec, dict) else None
                if patch is not None and source != UPSTREAM_URL:
                    errors.append(f"{rel} [{label}] `{name}` is not declared from {UPSTREAM_URL}, so the "
                                  f"[patch] does not apply to it")
                elif patch is None and source == UPSTREAM_URL:
                    errors.append(f"{rel} [{label}] `{name}` comes from {UPSTREAM_URL} without a base SDK "
                                  f"[patch]; repeat the root's")
                elif patch is None and (source != url or "rev" not in spec):
                    errors.append(f"{rel} [{label}] `{name}` must name a fork revision (`git = \"{url}\", "
                                  f"rev = …`) or come through the root's [patch]")
        lock = path.with_name("Cargo.lock")
        try:
            locked = load_toml(lock).get("package", []) if lock.is_file() else []
        except (OSError, ValueError) as error:  # TOML or UTF-8
            errors.append(f"{lock.relative_to(ROOT)} cannot be read: {error}")
            locked = []
        for package in locked:
            source = package.get("source", "path")
            if not is_base_sdk_package(package["name"], source, packages):
                continue
            if patch is not None and source != want:
                errors.append(f"{lock.relative_to(ROOT)}: `{package['name']}` comes from {source}, not the "
                              f"pinned fork {want}; relock with `cargo metadata --manifest-path {rel}`")
            elif patch is None and not source.startswith(f"git+{url}?rev="):
                errors.append(f"{lock.relative_to(ROOT)}: `{package['name']}` comes from {source}, not a "
                              f"revision of the fork {url}")
    return [path.parent.relative_to(ROOT) for path, _ in nested]


def cargo_home() -> Path:
    return Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")


def is_base_sdk_package(name: str, source, packages: set) -> bool:
    return name in packages or "base-sdk" in (source or "")


def check_resolved(meta: dict, want: str, rev: str, packages: set, errors: list) -> None:
    """Checks the source Cargo builds, which Cargo.lock does not show: a `paths`
    override or a source replacement leaves the lockfile as it is but swaps the
    code. Every base SDK package must come from Cargo's own checkout of the
    pinned revision (source `want`), unmodified."""
    checkouts = (cargo_home() / "git" / "checkouts").resolve()
    seen = set()
    for package in meta["packages"]:
        source = package["source"]
        if not is_base_sdk_package(package["name"], source, packages):
            continue
        name, manifest = package["name"], Path(package["manifest_path"]).resolve()
        if source != want:
            errors.append(f"`{name}` is built from {source or manifest.parent} (cargo metadata), "
                          f"not the pinned fork {want}; look for a path override or [patch]")
            continue
        if not manifest.is_relative_to(checkouts):
            errors.append(f"`{name}` is built from {manifest.parent}, not Cargo's checkout of the "
                          f"pinned fork under {checkouts}; look for a source replacement")
            continue
        # checkouts/<repo>-<hash>/<short rev>/…
        checkout = checkouts.joinpath(*manifest.relative_to(checkouts).parts[:2])
        if checkout in seen:
            continue
        seen.add(checkout)
        queries = [git(checkout, "rev-parse", "HEAD"),
                   # Untracked and ignored files and index flags that hide edits count
                   # too; explicit flags, so `status.showUntrackedFiles` cannot hide them.
                   git(checkout, "--no-optional-locks", "status", "--porcelain",
                       "--untracked-files=all", "--ignored=traditional"),
                   git(checkout, "ls-files", "-v")]
        if failed := next((q for q in queries if q.returncode != 0), None):
            errors.append(f"cannot inspect Cargo's checkout {checkout} (`git {' '.join(failed.args[3:])}`: "
                          f"{failed.stderr.strip()}); delete it so Cargo checks it out again")
            continue
        head, status, files = queries[0].stdout.strip(), queries[1].stdout, queries[2].stdout
        dirty = [line for line in status.splitlines() if line[3:] != ".cargo-ok"]
        dirty += [line for line in files.splitlines() if line[:1].islower() or line[:1] == "S"]
        if head != rev or dirty:
            errors.append(f"Cargo's checkout {checkout} is not a clean checkout of {rev} "
                          f"(HEAD {head}, {len(dirty)} changed file(s)); delete it so "
                          f"Cargo checks it out again")


def table(config: dict, key: str) -> dict:
    value = config.get(key, {})
    return value if isinstance(value, dict) else {}


# Config keys that can make Cargo build other code for a package than the
# lockfile names. `--config` arguments are invisible here; aliases can carry them.
def config_overrides(config: dict) -> list:
    found = []
    if config.get("paths"):
        found.append(f"`paths` overrides {config['paths']}")
    for name, body in table(config, "patch").items():
        names = [dep for dep, spec in (body.items() if isinstance(body, dict) else [])
                 if is_base_sdk_dep(dep, spec, BASE_SDK_PACKAGES)]
        if "base-sdk" in name or names:
            found.append(f"[patch.{name!r}] redirects {names or 'base SDK'}")
    for name, spec in table(config, "source").items():
        if "base-sdk" in name or "base-sdk" in str(spec):
            found.append(f"[source.{name}] replaces the base SDK source")
    for name, expansion in table(config, "alias").items():
        if "--config" in str(expansion):
            found.append(f"alias `{name}` passes --config")
    if config.get("include"):
        found.append("`include` pulls in further config files")
    return found


def config_files() -> list:
    """Every config file Cargo may load for this workspace: `.cargo/config[.toml]`
    in the workspace root and its ancestors, in any directory of the workspace
    (Cargo searches from the current directory), and in $CARGO_HOME."""
    dirs = [ROOT, *ROOT.parents]
    for parent, subdirs, _ in os.walk(ROOT):
        subdirs[:] = [d for d in subdirs if d not in ("target", ".git")]
        dirs.append(Path(parent))
    dirs = [d / ".cargo" for d in dirs] + [cargo_home()]
    files = [d / name for d in dirs for name in ("config.toml", "config")]
    return list(dict.fromkeys(f.resolve() for f in files if f.is_file()))


# Environment variables Cargo reads as config that can change resolution.
ENV_OVERRIDES = re.compile(r"^(CARGO_(PATHS|PATCH|SOURCE|UNSTABLE)|__CARGO_)")


def check_overrides(errors: list) -> None:
    for path in config_files():
        try:
            found = config_overrides(load_toml(path))
        except (OSError, tomllib.TOMLDecodeError) as error:
            found = [f"cannot be read: {error}"]
        errors.extend(f"Cargo config {path}: {override}" for override in found)
    for name, value in sorted(os.environ.items()):
        if ENV_OVERRIDES.match(name) or (name.startswith("CARGO_ALIAS_") and "--config" in value):
            errors.append(f"environment variable {name} can change what Cargo builds; unset it")


def check() -> int:
    errors = []
    root = load_toml(ROOT / "Cargo.toml")
    url, rev, upstream = pins(root, errors)
    packages = set(BASE_SDK_PACKAGES)
    nested = []
    if url and rev:
        packages |= check_fork(Path(url.removeprefix("file://")), rev, upstream, errors)
        nested = check_nested(root["patch"][UPSTREAM_URL], url, rev, packages, errors)
    check_members(packages, errors)
    check_overrides(errors)
    if url and rev:
        check_lock(url, rev, packages, errors)
    if report(errors):
        print("base SDK pin check failed (docs/node-program/BASE-SDK-FORK.md)", file=sys.stderr)
        return 1
    print(f"base SDK pin ok: every package is built from {url}@{rev}")
    print(f"nested workspaces checked: {', '.join(map(str, nested)) or 'none'}")
    return 0


def show() -> int:
    errors = []
    pinned = pins(load_toml(ROOT / "Cargo.toml"), errors)
    if report(errors):
        return 1
    print(*pinned)
    return 0


def fork() -> int:
    patch = load_toml(ROOT / "Cargo.toml").get("patch", {}).get(UPSTREAM_URL, {})
    urls = sorted({spec.get("git", "") for spec in patch.values() if isinstance(spec, dict)} - {""})
    print(urls[0] if urls else "")
    return 0


def set_revs(fork_rev: str, upstream_rev: str) -> int:
    if report([f"`{rev}` is not a full 40-hex commit id" for rev in (fork_rev, upstream_rev)
               if not SHA_RE.match(rev)]):
        return 1

    def repin(text: str, git_re: str, rev: str) -> tuple[str, int]:
        return re.subn(rf'(git = "{git_re}", rev = ")[0-9a-f]{{40}}"', rf'\g<1>{rev}"', text)

    # `git = "<upstream>", rev = "…"` lines are the declarations; `file://`
    # lines are the patch. Both keep their formatting.
    fork_re = r'file://[^"]*base-sdk[^"]*'
    path = ROOT / "Cargo.toml"
    text, n_up = repin(path.read_text(), re.escape(UPSTREAM_URL), upstream_rev)
    text, n_fork = repin(text, fork_re, fork_rev)
    if n_up == 0 or n_up != n_fork:
        report([f"rewrote {n_up} upstream and {n_fork} fork revs; expected the same non-zero count"])
        return 1
    rewritten = [(path, text)]
    # The other workspaces with a base SDK [patch] (node-fuzz, cs2-diff) follow
    # the root. They may patch packages the root does not, so every entry of
    # their [patch] must be rewritten, whatever the root has.
    errors = []
    for nested in patched_nested(errors):
        nested_text, _ = repin(nested.read_text(), re.escape(UPSTREAM_URL), upstream_rev)
        nested_text, n = repin(nested_text, fork_re, fork_rev)
        entries = len(load_toml(nested)["patch"][UPSTREAM_URL])
        if n != entries:
            errors.append(f"{nested.relative_to(ROOT)}: rewrote {n} of its {entries} [patch] entries; write "
                          f"each as `git = \"file://…\", rev = \"…\"` on one line")
        rewritten.append((nested, nested_text))
    if report(errors):
        return 1
    for target, new_text in rewritten:
        target.write_text(new_text)
    print(f"set fork rev {fork_rev} and upstream rev {upstream_rev} on {n_up} packages, and in "
          f"{len(rewritten) - 1} other workspaces")
    return 0


def nested() -> int:
    errors = []
    for path in patched_nested(errors):
        print(path.parent.relative_to(ROOT))
    return 1 if report(errors) else 0


def main() -> int:
    args = sys.argv[1:]
    if args == ["check"]:
        return check()
    if args == ["show"]:
        return show()
    if args == ["fork"]:
        return fork()
    if args == ["nested"]:
        return nested()
    if len(args) == 3 and args[0] == "set":
        return set_revs(args[1], args[2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
