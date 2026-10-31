#!/usr/bin/env python3
"""The layer rules over the dependency graph (SB0 §2.3; contrib/node/layers.toml).

  check-layers.py                 L1, L2, L3 and [bans] (L8)
  check-layers.py mobile          L6, the mobile closure, and O5, blst's
                                  `no-threads` in no N graph (check-mobile.sh)
  check-layers.py size BYTES --profile P --ndk V --api N [--record [--ruling DEC]]
                                  L7, the stripped size (check-mobile.sh)
  check-layers.py crates LAYER... [pure]
                                  `name manifest` (`-` without a library) for
                                  each member in the layers, with `pure` also
                                  each one marked `pure = true` (check-pure.sh,
                                  check-shared.sh)

Every package Cargo resolves is classified, member or not: by its source (the
base SDK, B), then by name ([crates]), then by name pattern ([patterns], every
`dashd-*` is N). A copy of a layered crate from crates.io, git or a path is
that crate's layer, and is checked as a root like a member.

A violation is an offending edge: from, to (name, and version and source for
a package outside the workspace), kind (normal or build) and target
condition. Each graph rule is a ratchet against layers-baseline.toml, which
lists, per rule and root, the full set of grandfathered edges: an edge not
listed fails, and so does a listed one that no longer occurs.

The graph (L2, L8, the general L3 walk) is the one Cargo resolves for the
whole workspace with every feature on and for every target (`cargo metadata
--all-features`, no platform filter), so a feature some other workspace crate
turns on in a shared dependency counts, as Cargo unifies it in a workspace
build. Normal and build edges are followed; dev edges never reach a
dependent. An optional dependency nothing in the workspace turns on is not in
the resolved graph, yet a consumer outside the workspace (the mobile SDK,
Platform) can turn it on, so every workspace, path or git package also
contributes the normal and build dependencies its manifest declares, matched
by package name. (Registry packages do not: their unused options, such as
bitcoin-internals' bincode, are not in the lockfile at all, so downstream
feature combinations of registry crates are not covered.) A declared registry
package missing from the lockfile is a leaf known by name; a declared path or
git package missing from it is unknown, and reaching one fails. (This walk
replaces EV0's check-layering.py.)

A walk stops at a package whose layer the root's layer allows and whose own
allowed layers are a subset of the root's: that package is a root of its own
check. Every rule is keyed by package name and applies to every resolved
package with that name (DEC-170): each copy of a hot-path crate is walked
through everything, N and allow_w included. allow_w is the one exception, a
permission rather than a rule: it covers members only, so an outside copy is
plain N and fails closed. A feature-qualified allow_w entry (`crate/feature`)
is checked with `cargo tree -p` on the crate's own graph with the feature off
(no W at all). L6 takes each mobile product's graph from Cargo's platform
filter (`cargo tree --target`, `cargo metadata --filter-platform`) with its
default features, never by mapping it back onto the all-target graph. L7
compares with a reference no change can move: the record at the merge base
with node/integration, else layers.toml's [size].
"""

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
import tomllib
from collections import Counter, defaultdict, deque
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NODE = ROOT / "contrib" / "node"
GRAPH_RULES = ("L1", "L2", "L3", "L8")
MOBILE_RULES = ("L6", "O5")
RULES = GRAPH_RULES + MOBILE_RULES
MOBILE_TARGETS = ("aarch64-linux-android", "aarch64-apple-ios")
MOBILE_ROOTS = ("dash-spv-ffi", "key-wallet-ffi")
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def norm(name):
    """A crate name as crates.io compares it: case and `-`/`_` folded."""
    return name.lower().replace("_", "-")


def norm_source(source):
    """A package source or id, reduced to what identifies the repository: the
    `git+`/`path+` kind and URL scheme, user, query, fragment, case, a trailing
    `.git` or `/` dropped; a file path resolved through symlinks."""
    source = re.sub(r"^(git|path|registry|sparse)\+", "", source).split("#")[0].split("?")[0]
    if source.startswith("file://"):
        return str(Path(source[len("file://"):]).resolve())
    source = re.sub(r"^[a-z]+://", "", source.lower())
    source = re.sub(r"^[^@/]+@", "", source)
    if re.match(r"^[^/]+:", source):  # scp-like host:path
        source = source.replace(":", "/", 1)
    return source.removesuffix("/").removesuffix(".git")


def load_toml(name):
    return tomllib.loads((NODE / name).read_text())


def run(*argv, check=True):
    result = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)
    if check and result.returncode != 0:
        sys.exit(f"error: {' '.join(argv)} failed:\n{result.stderr.strip()}")
    return result


def cargo(*args):
    return run("cargo", *args).stdout


class Layers:
    """layers.toml, validated."""

    def __init__(self, members):
        conf = load_toml("layers.toml")
        self.allowed = {k: set(v) for k, v in conf["allowed"].items()}
        self.layer, self.pending, self.pure = {}, {}, set()
        self.by_norm = {}
        for name, spec in conf["crates"].items():
            if isinstance(spec, str):
                spec = {"layer": spec}
            unknown = set(spec) - {"layer", "pending", "pure"}
            if unknown:
                sys.exit(f"error: layers.toml: `{name}` has unknown keys {sorted(unknown)}")
            self.layer[name] = self.known(spec.get("layer"), f"`{name}`")
            self.by_norm[norm(name)] = self.layer[name]
            if "pending" in spec:
                self.pending[name] = spec["pending"]
            if spec.get("pure"):
                self.pure.add(name)
        self.patterns = {p: self.known(l, f"pattern `{p}`") for p, l in conf.get("patterns", {}).items()}
        fork = run("python3", str(NODE / "base_sdk_pins.py"), "fork").stdout.strip()
        self.b_sources = {norm_source(s) for s in conf["b"]["sources"] + ([fork] if fork else [])}
        boundary = conf["w-boundary"]
        self.allow_w = {}
        for crate, _, feature in (e.partition("/") for e in boundary["allow_w"]):
            if crate in self.allow_w:
                sys.exit(f"error: layers.toml: allow_w lists {crate} twice")
            self.allow_w[crate] = feature or None
        self.hot_path = boundary["hot_path"]
        self.bans = conf.get("bans", {})
        self.members = members

    def known(self, layer, what):
        if layer not in self.allowed:
            sys.exit(f"error: layers.toml: {what} has layer {layer!r}, not one of {', '.join(self.allowed)}")
        return layer

    def by_name(self, name):
        """A name's layer: listed in [crates], else by [patterns]; None if
        neither. Case and `-`/`_` do not matter."""
        return self.by_norm.get(norm(name)) or self.by_pattern(name)

    def by_pattern(self, name):
        """A name's layer by [patterns]; None if no pattern matches."""
        return next((l for p, l in self.patterns.items() if fnmatch.fnmatchcase(norm(name), norm(p))), None)

    def from_b(self, source):
        source = norm_source(source)
        return any(source == s or source.startswith(s + "/") for s in self.b_sources)

    def of(self, layer):
        return sorted(n for n, l in self.layer.items() if l == layer)

    def check_declared(self, out):
        """L1."""
        for name in sorted(self.members):
            if name not in self.layer:
                out.add("L1", name, "unlisted", f"`{name}` is a workspace crate with no layer in layers.toml")
            elif name in self.pending:
                out.add("L1", name, "pending", f"`{name}` is a workspace crate but layers.toml still marks "
                        f"it pending ({self.pending[name]}); drop the tag")
        for name in sorted(set(self.layer) - self.members - set(self.pending)):
            out.add("L1", name, "stale", f"layers.toml lists `{name}`, which is not a workspace crate; "
                    f"remove it, or mark it `pending` with the row that brings it")
        for name, layer in sorted(self.layer.items()):
            pattern = self.by_pattern(name)
            if pattern and pattern != layer:
                out.add("L1", name, "pattern", f"`{name}` is listed as {layer}, but its name pattern makes it {pattern}")
        for name in sorted({*self.allow_w, *self.hot_path}):
            if self.layer.get(name, "N") != "N":
                out.add("L1", name, "w-boundary", f"`{name}` is in [w-boundary] but its layer is {self.layer[name]}, not N")


class Violations:
    """Offending edges by (rule, root), ratcheted against layers-baseline.toml."""

    def __init__(self):
        self.found = defaultdict(dict)  # (rule, root) -> {edge: message}

    def add(self, rule, root, edge, message):
        self.found[(rule, root)].setdefault(edge, message)

    def ratchet(self, rules):
        """Compares with layers-baseline.toml; prints the outcome; 0 when clean."""
        listed, errors = defaultdict(dict), []
        for entry in load_toml("layers-baseline.toml").get("violation", []):
            rule, root, edges = entry.get("rule"), entry.get("root"), entry.get("edges")
            if (rule not in RULES or not root or not entry.get("closes") or not isinstance(edges, list)
                    or not edges or set(entry) - {"rule", "root", "edges", "closes"}):
                errors.append(f"layers-baseline.toml: an entry needs `rule` (one of {', '.join(RULES)}), "
                              f"`root`, `edges` (a non-empty list) and `closes`, and nothing else: {entry}")
                continue
            for edge in edges:
                if edge in listed[(rule, root)]:
                    errors.append(f"layers-baseline.toml lists {rule} [{root}] {edge} twice")
                elif rule in rules:
                    listed[(rule, root)][edge] = entry["closes"]
        for (rule, root), edges in sorted(self.found.items()):
            for edge, message in sorted(edges.items()):
                closes = listed.get((rule, root), {}).get(edge)
                if closes:
                    print(f"baseline {rule} [{root}] {edge} (closed by {closes}): {message}")
                else:
                    errors.append(f"{rule} [{root}] {edge}: {message}")
        for (rule, root), edges in sorted(listed.items()):
            for edge, closes in sorted(edges.items()):
                if edge not in self.found.get((rule, root), {}):
                    errors.append(f"{rule} [{root}] {edge} is listed in layers-baseline.toml (closed by "
                                  f"{closes}) but no longer occurs; remove it")
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1 if errors else 0


def resolved_edges(meta, applies=lambda condition: True):
    """`cargo metadata`'s resolved normal and build edges: from → {(to,
    kind, target condition)}, for the conditions `applies` keeps."""
    edges = defaultdict(set)
    for node in meta["resolve"]["nodes"]:
        for dep in node["deps"]:
            for k in dep["dep_kinds"]:
                kind = k["kind"] or "normal"
                if kind in ("normal", "build") and (k["target"] is None or applies(k["target"])):
                    edges[node["id"]].add((dep["pkg"], kind, k["target"]))
    return edges


def cfg_applies(condition, triple, cfg):
    """Whether a `[target.…]` condition (a triple or `cfg(…)`) holds for
    `triple`, whose `rustc --print cfg` lines are `cfg`."""
    if not condition.startswith("cfg("):
        return condition == triple
    tokens, pos = re.findall(r'"[^"]*"|[\w-]+|[(),=]', condition[4:-1]), 0

    def predicate():
        nonlocal pos
        name, pos = tokens[pos], pos + 1
        if name in ("all", "any", "not"):
            values, pos = [], pos + 1
            while tokens[pos] != ")":
                values.append(predicate())
                pos += tokens[pos] == ","
            pos += 1
            return {"all": all, "any": any}[name](values) if name != "not" else not values[0]
        if pos < len(tokens) and tokens[pos] == "=":
            name, pos = f"{name}={tokens[pos + 1]}", pos + 2
        return name in cfg

    return predicate()


def platform_edges(triple):
    """The resolved edges Cargo keeps for one platform (`--filter-platform`).
    Cargo keeps every declaration of a dependency it keeps, so those whose
    condition does not hold there are dropped (key-wallet-ffi's clap under
    cfg(target_os = "linux") is no Android edge)."""
    cfg = set(run("rustc", "--print", "cfg", "--target", triple).stdout.split())
    return resolved_edges(json.loads(cargo("metadata", "--locked", "--all-features", "--filter-platform", triple,
                                           "--format-version", "1")),
                          lambda condition: cfg_applies(condition, triple, cfg))


class Graph:
    """The workspace's all-features, all-targets resolved graph, widened by
    declared dependencies; every package classified."""

    def __init__(self, layers):
        meta = json.loads(cargo("metadata", "--locked", "--all-features", "--format-version", "1"))
        self.layers = layers
        self.pkgs = {p["id"]: p for p in meta["packages"]}
        self.member_ids = set(meta["workspace_members"])
        self.name = {i: p["name"] for i, p in self.pkgs.items()}
        self.by_version = defaultdict(list)
        for i, p in self.pkgs.items():
            self.by_version[(p["name"], p["version"])].append(i)
        self.resolved = resolved_edges(meta)
        self.edges = defaultdict(set, {k: set(v) for k, v in self.resolved.items()})
        # The root manifest's [patch] tables: a dependency declared from a
        # patched source resolves to the patch's source.
        self.patched = defaultdict(set)
        for url, entries in tomllib.loads((ROOT / "Cargo.toml").read_text()).get("patch", {}).items():
            for spec in entries.values():
                if isinstance(spec, dict) and (spec.get("git") or spec.get("path")):
                    to = spec.get("git") or "file://" + str((ROOT / spec["path"]).resolve())
                    self.patched[norm_source(CRATES_IO if url == "crates-io" else url)].add(norm_source(to))
        self.leaves, self.opaque = set(), set()
        for pkg in meta["packages"]:
            if (pkg["source"] or "").startswith("registry+"):
                continue  # a registry crate's unused options are its own business
            for dep in pkg["dependencies"]:
                if dep["kind"] not in (None, "build"):
                    continue
                targets = self.declared(dep)
                if not targets:
                    registry = (dep["source"] or "").startswith("registry+")
                    leaf = f"{'declared' if registry else 'unresolved'} {dep['name']}"
                    self.leaves.add(leaf)
                    if not registry:
                        self.opaque.add(leaf)
                    self.name[leaf] = dep["name"]
                    targets = [leaf]
                for t in targets:
                    self.edges[pkg["id"]].add((t, dep["kind"] or "normal", dep["target"]))
        self.members = {self.name[i]: i for i in self.member_ids}
        self.features = {self.name[i]: self.pkgs[i]["features"] for i in self.member_ids}
        self.layer = {i: self.classify(i) for i in self.name}

    def declared(self, dep):
        """The packages a declared dependency can be: the one at its path, or
        those of its name from its git or registry source."""
        if dep.get("path"):
            where = Path(dep["path"]).resolve()
            return [i for i, p in self.pkgs.items()
                    if p["source"] is None and Path(p["manifest_path"]).parent.resolve() == where]
        sources = {norm_source(dep["source"] or CRATES_IO)}
        sources |= self.patched.get(next(iter(sources)), set())
        return [i for i, p in self.pkgs.items()
                if p["name"] == dep["name"] and norm_source(p["source"] or p["id"]) in sources]

    def classify(self, i):
        if i in self.leaves:
            return None if i in self.opaque else self.layers.by_name(self.name[i])
        p = self.pkgs[i]
        if self.layers.from_b(p["source"] or p["id"]):
            return "B"
        return self.layers.by_name(p["name"])

    def roots(self):
        """Every classified package with dependencies of its own to check."""
        return sorted((i for i, l in self.layer.items() if l and i not in self.leaves), key=self.label)

    def label(self, i):
        if i in self.member_ids or i in self.leaves:
            return self.name[i]
        p = self.pkgs[i]
        source = p["source"]
        if source is None:
            where = "path " + str(Path(p["manifest_path"]).parent)
        elif source == CRATES_IO:
            where = "crates.io"
        else:
            where = source.split("#")[0]
        return f"{p['name']} v{p['version']} ({where})"

    def edge_label(self, u, v, kind, target):
        return f"{self.label(u)} -> {self.label(v)} [{kind}{', ' + target if target else ''}]"

    def offending(self, root, visit, edges=None):
        """Breadth first from `root`. `visit(node)` returns "stop" (offending:
        report every edge into it from the expanded part, do not expand),
        "skip" (do not expand) or None (expand). Yields (edge, path)."""
        edges = self.edges if edges is None else edges
        parent, verdict = {root: None}, {}
        queue = deque([root])
        while queue:
            u = queue.popleft()
            for v, kind, target in sorted(edges.get(u, ()), key=lambda e: (e[0], e[1], e[2] or "")):
                if v not in verdict:
                    verdict[v] = visit(v)
                if verdict[v] == "stop":
                    yield (u, v, kind, target), self.path(parent, u) + [v]
                elif verdict[v] is None and v not in parent:
                    parent[v] = u
                    queue.append(v)

    @staticmethod
    def path(parent, at):
        path = []
        while at is not None:
            path.append(at)
            at = parent[at]
        return path[::-1]

    def route(self, path):
        return " → ".join(self.label(p) for p in path)

    def tree(self, package, *args, edges="normal,build", target="all", resolved=None):
        """`cargo tree -p package`: its edges as (from, to, kind, target
        condition) package ids, and the first path to each package. The kinds
        and conditions come from `resolved` (for one platform, cargo's own
        filter: platform_edges), else from the all-targets graph."""
        resolved = self.resolved if resolved is None else resolved
        kinds = set(edges.split(","))
        stack, paths, found = [], {}, set()
        for line in cargo("tree", "--locked", "-q", "-p", package, "-e", edges, "--target", target,
                          "--prefix", "depth", "--format", "{p}", *args).splitlines():
            match = re.match(r"^(\d+)(\S+) v(\S+)(.*)$", line)
            if not match:
                continue
            depth, node = int(match.group(1)), self.lookup(match.group(2), match.group(3), match.group(4))
            del stack[depth:]
            stack.append(node)
            paths.setdefault(node, list(stack))
            if depth:
                u = stack[depth - 1]
                hits = {e for e in resolved.get(u, ()) if e[0] == node and e[1] in kinds}
                found |= {(u, *e) for e in hits} or {(u, node, "?", None)}
        return found, paths

    def lookup(self, name, version, rest):
        """The package id of a `cargo tree` line."""
        ids = self.by_version.get((name, version), [])
        if len(ids) == 1:
            return ids[0]
        sources = [s for s in re.findall(r"\(([^)]*)\)", rest) if s not in ("*", "proc-macro")]
        for i in ids:
            p = self.pkgs[i]
            if not sources:
                if (p["source"] or "").startswith("registry+"):
                    return i
            elif p["source"] is None:
                if Path(p["manifest_path"]).parent == Path(sources[-1]):
                    return i
            elif norm_source(p["source"]) == norm_source(sources[-1]):
                return i
        sys.exit(f"error: cannot place `{name} v{version}{rest}` from cargo tree in the metadata graph")


def check_outside(graph, out):
    """L1: L4 and L5 lint workspace members, so a P, Pn or S package, or one
    of a name marked `pure = true` whatever its layer (DEC-170), from outside
    the workspace (a copy of a listed crate, say) is unchecked."""
    pure = {norm(n) for n in graph.layers.pure}
    for i in graph.roots():
        if i in graph.member_ids:
            continue
        marked = norm(graph.name[i]) in pure
        if graph.layer[i] in ("P", "Pn", "S") or marked:
            out.add("L1", graph.label(i), "outside",
                    f"{graph.label(i)} is a {graph.layer[i]} package{' marked pure' if marked else ''} from "
                    f"outside the workspace, which the "
                    f"purity and shared checks (L4, L5) cannot lint; use the workspace crate")


def check_direction(layers, graph, out):
    """L2 and the general L3 walk, for every classified package."""
    for root in graph.roots():
        own, name = graph.layer[root], graph.name[root]
        # FP1: allow_w names workspace crates; a copy of the name elsewhere is plain N.
        allow_w = name in layers.allow_w and root in graph.member_ids

        def visit(node):
            if node in graph.opaque:
                return None if own in ("A", "T") else "stop"
            dep = graph.layer[node]
            if dep is None:
                return None  # outside every layer, or unlisted (L1 reports a member)
            if own == "N" and dep == "W":
                return None if allow_w else "stop"
            if dep not in layers.allowed[own]:
                return "stop"
            # A package whose allowed layers are a subset of the root's is a
            # root of its own check, so it answers for its graph.
            return "skip" if layers.allowed[dep] <= layers.allowed[own] else None

        for (u, v, kind, target), path in graph.offending(root, visit):
            edge = graph.edge_label(u, v, kind, target)
            if v in graph.opaque:
                out.add("L2", graph.label(root), edge,
                        f"{graph.label(v)} is a path or git dependency no build resolves, so its layer "
                        f"and dependencies are unknown: {graph.route(path)}")
            elif own == "N" and graph.layer[v] == "W":
                out.add("L3", graph.label(root), edge,
                        f"{name} (N) reaches W outside [w-boundary] allow_w: {graph.route(path)}")
            else:
                out.add("L2", graph.label(root), edge,
                        f"{name} ({own}) reaches {graph.name[v]} ({graph.layer[v]}); {own} may depend only "
                        f"on {', '.join(sorted(layers.allowed[own]))}: {graph.route(path)}")


def check_bans(layers, graph, out):
    """[bans] over the closure of each classified package of a covered
    layer. The walk stops at packages of covered layers, which are roots of
    the same check, and at packages of the layers the ban `exempts` (bincode
    inside W behind allow_w); it passes through every other package."""
    for banned, ban in sorted(layers.bans.items()):
        covered = set(ban["layers"])
        packages = {norm(n) for n in (banned, *ban.get("packages", ()))}
        rule = ban.get("rule", "L8")
        exempt = set(ban.get("exempts", ()))

        def visit(node):
            if norm(graph.name[node]) in packages:
                return "stop"
            return "skip" if graph.layer[node] in covered | exempt else None

        for root in graph.roots():
            if graph.layer[root] not in covered:
                continue
            for (u, v, kind, target), path in graph.offending(root, visit):
                out.add(rule, graph.label(root), graph.edge_label(u, v, kind, target),
                        f"{graph.name[root]} ({graph.layer[root]}) reaches {graph.name[v]}: "
                        f"{graph.route(path)} ({ban['reason']})")


def enables(features, start, target):
    """Whether feature `start` turns on feature `target` of the same package."""
    todo, done = [start], set()
    while todo:
        feature = todo.pop()
        if feature == target:
            return True
        if feature in done:
            continue
        done.add(feature)
        todo.extend(f for f in features.get(feature, []) if f in features)
    return False


def check_w_boundary(layers, graph, out):
    """L3's feature-off graph for `crate/feature` entries, on the crate's own
    graph: a feature that names an optional dependency without `dep:` is an
    implicit feature anyone can turn on, so it lands in the off list (write
    `dep:` for the gated edges). Then the hot path: each crate's whole closure
    in the resolved workspace graph, so features other crates unify onto it
    count, through N and allow_w alike."""
    for crate, feature in sorted(layers.allow_w.items()):
        if crate not in graph.members or feature is None:
            continue
        own = graph.features[crate]
        off = [f for f in own if not enables(own, f, feature)]
        edges, paths = graph.tree(crate, "--no-default-features", "--features", ",".join(off))
        for u, v, kind, target in sorted(e for e in edges if graph.layer[e[1]] == "W"):
            out.add("L3", f"{crate} without {feature}", graph.edge_label(u, v, kind, target),
                    f"{crate} reaches W with its `{feature}` feature off; allow_w admits only the edges "
                    f"that feature adds: {graph.route(paths[u] + [v])}")
    # Every resolved package of a hot-path name, from any source (DEC-170).
    names = {norm(c) for c in layers.hot_path}
    hot = sorted((i for i in graph.pkgs if norm(graph.name[i]) in names), key=graph.label)
    for root in hot:
        visit = lambda node: "stop" if graph.layer[node] == "W" else None  # noqa: E731
        for (u, v, kind, target), path in graph.offending(root, visit, graph.resolved):
            out.add("L3", f"{graph.label(root)} (hot path)", graph.edge_label(u, v, kind, target),
                    f"hot-path crate {graph.label(root)} reaches W in the workspace build: {graph.route(path)}")


def graph_rules():
    layers = Layers(set())
    graph = Graph(layers)
    layers.members = set(graph.members)
    out = Violations()
    layers.check_declared(out)
    check_outside(graph, out)
    check_direction(layers, graph, out)
    check_w_boundary(layers, graph, out)
    check_bans(layers, graph, out)
    rc = out.ratchet(GRAPH_RULES)
    if rc:
        print("layer check failed (contrib/node/layers.toml, SB0 §2.3)", file=sys.stderr)
    else:
        counts = Counter(graph.layer[i] for i in graph.roots())
        print("layers ok: L1 L2 L3 L8 over " + ", ".join(f"{l} {counts[l]}" for l in layers.allowed if counts[l]))
    return rc


def workspace_packages():
    """The members' packages, from `cargo metadata --no-deps`."""
    return json.loads(cargo("metadata", "--locked", "--no-deps", "--format-version", "1"))["packages"]


def mobile_rules():
    """L6: the default-feature normal closure of each mobile product, per
    target, holds no mobile-deny.toml package and no Pn or N package (by
    name, so an unlisted `dashd-*` counts)."""
    layers = Layers(set())
    graph = Graph(layers)
    deny = load_toml("mobile-deny.toml")
    banned = {norm(e["crate"].split("@")[0]): e.get("reason", "") for e in deny["bans"]["deny"]}

    def reason(node):
        name = norm(graph.name[node])
        if name in banned:
            return banned[name]
        if graph.layer[node] in ("Pn", "N"):
            return f"a {graph.layer[node]} crate (node-only)"
        return None

    targets = deny.get("graph", {}).get("targets", MOBILE_TARGETS)
    platform = {target: platform_edges(target) for target in targets}
    out = Violations()
    for root in MOBILE_ROOTS:
        for target in targets:
            edges, paths = graph.tree(root, edges="normal", target=target, resolved=platform[target])
            for u, v, kind, cond in sorted(edges, key=lambda e: (e[0], e[1], e[2], e[3] or "")):
                if reason(v):
                    out.add("L6", f"{root} [{target}]", graph.edge_label(u, v, kind, cond),
                            f"{graph.name[v]} is in the mobile closure: {graph.route(paths[u] + [v])} ({reason(v)})")
    checked = no_threads_rule(layers, graph, out)
    rc = out.ratchet(MOBILE_RULES)
    if rc:
        print("mobile closure check failed (contrib/node/mobile-deny.toml, SB0 L6 and O5)", file=sys.stderr)
    else:
        print(f"mobile closure ok: L6 for {', '.join(MOBILE_ROOTS)} on {', '.join(targets)}; "
              f"O5 over {checked} N packages")
    return rc


# DEC-112 O5: blst's feature that runs every pooled call inline. Only
# dash-spv-ffi's manifest may turn it on.
NO_THREADS = ("blst", "no-threads")


def no_threads_rule(layers, graph, out):
    """O5 (SB0 §7, DEC-112 O5): no N member's own graph turns on blst's
    `no-threads`, which DEC-79 rejected for the node (Cargo would unify it
    into the node's blst, making it serial). Each N member is resolved on its
    own (`cargo tree -p`, default features, every target, normal and build
    edges), as its binaries build. Returns how many members it checked."""
    crate, feature = NO_THREADS
    versions = sorted({p["version"] for p in graph.pkgs.values() if norm(p["name"]) == crate})
    members = sorted(graph.name[i] for i in graph.member_ids if layers.by_name(graph.name[i]) == "N")
    for member in members:
        for version in versions:
            result = run("cargo", "tree", "--locked", "-q", "-p", member, "-e", "normal,build,features",
                         "--target", "all", "-i", f"{crate}@{version}", "--prefix", "depth", check=False)
            if result.returncode != 0:
                if "did not match any packages" in result.stderr:
                    continue  # this blst is not in the member's graph
                sys.exit(f"error: cargo tree -p {member} -i {crate}@{version} failed:\n{result.stderr.strip()}")
            route = no_threads_route(result.stdout, crate, feature)
            if route is not None:
                out.add("O5", member, f"{crate} v{version} feature \"{feature}\"",
                        f"{member} (N) turns on {crate}'s `{feature}`, which only dash-spv-ffi may "
                        f"(DEC-112 O5, DEC-79): {' ← '.join(route)}")
    return len(members)


def no_threads_route(tree, crate, feature):
    """In an inverted `cargo tree --prefix depth` of `crate`, the first chain
    from its `feature` down to a root, or None when nothing enables it."""
    lines = [re.match(r"^(\d+)(.*)$", l) for l in tree.splitlines()]
    lines = [(int(m.group(1)), m.group(2).strip()) for m in lines if m]
    want = f'{crate} feature "{feature}"'
    for i, (depth, text) in enumerate(lines):
        if text == want:
            route, at = [text], depth
            for d, t in lines[i + 1:]:
                if d != at + 1:
                    break
                route.append(t.removesuffix(" (*)"))
                at = d
            return route
    return None


SIZE_PATH = "contrib/node/mobile-size.txt"
SIZE_FILE = ROOT / SIZE_PATH
# What the size depends on besides the code: a change of any of them is a
# baseline migration, which needs a ruling.
ENV_KEYS = ("library", "target", "profile", "ndk", "api")
SIZE_KEYS = ENV_KEYS + ("bytes", "ruling")


def parse_size(text):
    record = {}
    for line in text.splitlines():
        if line.strip() and not line.startswith("#"):
            key, _, value = line.partition("=")
            record[key.strip()] = value.strip()
    missing = [k for k in SIZE_KEYS if k not in record]
    if missing:
        sys.exit(f"error: {SIZE_PATH} lacks {', '.join(missing)}")
    return record


RULINGS = "contrib/node/gate-rulings.toml"
# Where the rulings were before the register (DEC-987): read only at a tree or
# revision that has no register, so a change based before it is judged as then.
LEGACY_DECISIONS = "docs/node-program/DECISIONS.md"
# The program's decision log, outside this repository (DEC-986), where L7's
# size rule runs: a register row is new only as a copy of a log row the log
# did not have at the merge base (DEC-987).
DECISIONS_LOG = Path(os.environ.get("NODE_PROGRAM_DECISIONS")
                     or Path.home() / "workspace/node-program/DECISIONS.md")


# The branch whose history and decision log L7 answers to; nothing a change
# or its environment controls redirects it (DEC-170). Fully qualified, so a
# tag of that name cannot stand in for it.
BASE_REF = "refs/heads/node/integration"


def show(rev, path):
    """`path` at `rev`, or None where it does not exist."""
    out = run("git", "show", f"{rev}:{path}", check=False)
    return out.stdout if out.returncode == 0 else None


def read(rev, path):
    """`path` in the working tree (`rev` None) or at `rev`; None where it does not exist."""
    if rev is None:
        return (ROOT / path).read_text() if (ROOT / path).exists() else None
    return show(rev, path)


def decisions(rev=None):
    """The gates' rulings by DEC id (RULINGS), in the working tree or at
    `rev`; where that has no register, the rows of the decision log it had."""
    text = read(rev, RULINGS)
    if text is None:
        return log_rows(read(rev, LEGACY_DECISIONS))
    where = rev or "the working tree"
    try:
        doc = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        sys.exit(f"error: {RULINGS} at {where}: {e}")
    rulings = doc.get("rulings")
    if (set(doc) != {"rulings"} or not isinstance(rulings, dict)
            or not all(re.fullmatch(r"DEC-\d+", k) and isinstance(v, str) for k, v in rulings.items())):
        sys.exit(f"error: {RULINGS} at {where}: it is one table, [rulings], mapping DEC-<n> to the row's text")
    return rulings


def log_rows(text):
    """A decision log's rows by DEC id: the text after the id's cell."""
    return dict(re.findall(r"^\| (DEC-\d+) \|(.*)$", text or "", re.M))


def log_at(base):
    """The decision log's rows when `base` was made: `base`'s own copy where it
    has one, else the log's last commit not after `base`'s; None where the log
    has no such history."""
    if (legacy := show(base, LEGACY_DECISIONS)) is not None:
        return log_rows(legacy)
    path = DECISIONS_LOG.resolve()
    when = run("git", "show", "-s", "--format=%ct", base).stdout.strip()
    # The log's own repository, whatever GIT_DIR and the like (a hook's) name.
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}

    def log_git(*argv):
        return subprocess.run(("git", "-C", str(path.parent), *argv), env=env,
                              capture_output=True, text=True).stdout.strip()

    rev = log_git("rev-list", "-1", f"--before=@{when}", "HEAD", "--", path.name)
    return log_rows(rev and log_git("show", f"{rev}:./{path.name}")) or None


def logged():
    """The rulings, which are kept on the base branch: its rows there win
    over this tree's."""
    return decisions() | decisions(BASE_REF)


def ruling_row(ruling):
    """`ruling`'s row in the log; None if it has none."""
    return logged().get(ruling)


def unstated(ruling, record, keys):
    """The values of `record` under `keys` that `ruling`'s row does not state
    (bytes plain or with thousands separators): a ruling covers only what it
    states (DEC-170)."""
    row = ruling_row(ruling) or ""

    def stated(key):
        value = record[key]
        forms = (value, f"{int(value):,}") if key == "bytes" else (value,)
        return any(re.search(rf"(?<![\w.]){re.escape(f)}(?![\w]|[.,]\d)", row) for f in forms)

    return [f"{key} {record[key]}" for key in keys if not stated(key)]


def new_rulings(base):
    """The decisions recorded since `base`, in this tree or on the base
    branch: the ones a change may cite to move L7. Where the decision log is
    present, only copies of its rows that it did not have at `base` (DEC-987)."""
    rows = logged()
    fresh = set(rows) - set(decisions(base))
    if not DECISIONS_LOG.is_file():
        print(f"L7: decision-log check SKIPPED: no decision log at {DECISIONS_LOG} (DEC-987)")
        return fresh
    log, then = log_rows(DECISIONS_LOG.read_text()), log_at(base)
    if then is None:
        print(f"L7: decision-log age check SKIPPED: no history of {DECISIONS_LOG} at {base[:9]} (DEC-987)")
    for ruling in sorted(fresh):
        why = (f"is not a row of {DECISIONS_LOG}" if ruling not in log
               else f"differs from its row in {DECISIONS_LOG}" if rows[ruling].strip() != log[ruling].strip()
               else f"was a row of {DECISIONS_LOG} at the merge base" if then and ruling in then else None)
        if why:
            print(f"L7: {ruling} in {RULINGS} {why}: not a new ruling", file=sys.stderr)
            fresh.discard(ruling)
    return fresh


def size_constant(text):
    size = tomllib.loads(text).get("size") if text else None
    return {k: str(v) for k, v in size.items()} if size else None


def reference_size():
    """The record L7 compares with, which no change on this branch can move
    (DEC-170): the one committed at the merge base with node/integration
    (HEAD's first parent when HEAD is node/integration itself), or,
    while that has none, the constant in layers.toml's [size]; with where it
    came from and the decisions new to this change. A change to the constant
    needs one of those."""
    ref = BASE_REF
    base = run("git", "merge-base", "HEAD", ref, check=False)
    if base.returncode != 0:
        sys.exit(f"error: L7: no merge base with {ref}; update the local branch: "
                 f"git fetch origin node/integration:node/integration")
    base = base.stdout.strip()
    what = f"the merge base with {ref}"
    if run("git", "rev-parse", "HEAD").stdout.strip() == run("git", "rev-parse", ref).stdout.strip():
        parent = run("git", "rev-parse", "--verify", "-q", "HEAD^1", check=False)
        if parent.returncode != 0:
            sys.exit(f"error: L7: HEAD is {ref} and has no first parent to compare with")
        base, what = parent.stdout.strip(), "HEAD's first parent"
    fresh = new_rulings(base)
    record = show(base, SIZE_PATH)
    if record is not None:
        return parse_size(record), what, fresh
    constant = size_constant((NODE / "layers.toml").read_text())
    if constant is None or set(constant) != set(SIZE_KEYS):
        sys.exit(f"error: L7: {what} has no {SIZE_PATH}, and layers.toml's [size] does not give "
                 f"{', '.join(SIZE_KEYS)} (DEC-170)")
    before = size_constant(show(base, "contrib/node/layers.toml"))
    ruling = constant["ruling"]
    if before is not None and constant != before and (ruling == before["ruling"] or ruling not in fresh):
        sys.exit(f"error: L7: layers.toml's [size] moved from {what} without a decision new to this change; "
                 f"record one in {RULINGS} and cite it there (DEC-170)")
    if ruling_row(ruling) is None:
        sys.exit(f"error: L7: layers.toml's [size] cites {ruling}, which names no decision")
    # The decision fixes the constant: a new one needs a new decision stating it.
    missing = unstated(ruling, constant, ENV_KEYS + ("bytes",))
    if missing:
        sys.exit(f"error: L7: layers.toml's [size] gives {', '.join(missing)}, which {ruling} does not state; "
                 f"record a decision that does in {RULINGS} and cite it there (DEC-170)")
    return constant, f"layers.toml's [size], {constant['ruling']}", fresh


def size_rule(argv):
    """L7: growth over the reference record at most min(2 %, 256 KiB), in
    the same build environment, unless this change records a new ruling."""
    parser = argparse.ArgumentParser(prog="check-layers.py size")
    parser.add_argument("bytes", type=int)
    for key in ("profile", "ndk", "api"):
        parser.add_argument(f"--{key}", required=True)
    parser.add_argument("--record", action="store_true")
    parser.add_argument("--ruling")
    args = parser.parse_args(argv)
    if args.ruling is not None and not args.record:
        sys.exit("error: --ruling goes with --record")
    reference, where, fresh = reference_size()
    working = parse_size(SIZE_FILE.read_text())
    measured = {**working, "profile": args.profile, "ndk": args.ndk, "api": args.api, "bytes": str(args.bytes)}
    base = int(reference["bytes"])
    allowance = min(base * 2 // 100, 256 * 1024)

    def moved_keys(old, new):
        return [k for k in ENV_KEYS if new[k] != old[k]]

    def moves(old, new):
        return [f"{k} {old[k]} → {new[k]}" for k in moved_keys(old, new)]

    def change(record):
        """What `record` changes relative to the reference (the environment
        keys moved, else the byte delta), the delta, and whether that needs
        a ruling."""
        moved, delta = moves(reference, record), int(record["bytes"]) - base
        return ", ".join(moved) or f"{delta:+} B", delta, bool(moved) or delta > allowance

    def ruled(record):
        # A decision new to this change, stating the size and any environment
        # it moves to; the reference's own covers nothing new.
        return (record["ruling"] != reference["ruling"] and record["ruling"] in fresh
                and not unstated(record["ruling"], record, moved_keys(reference, record) + ["bytes"]))

    what, delta, needs = change(measured)
    print(f"mobile size: {working['library']} {working['target']} profile {args.profile} NDK {args.ndk} "
          f"API {args.api}: {args.bytes} B; reference {base} B ({where}), delta {delta:+} B, "
          f"allowance {allowance} B")
    if args.record:
        measured["ruling"] = args.ruling or "-"
        if args.ruling and ruling_row(args.ruling) is None:
            sys.exit(f"error: L7: --ruling {args.ruling} names no decision in {RULINGS}")
        if needs and not ruled(measured):
            sys.exit(f"error: L7: {what} relative to the reference needs a new ruling: --record --ruling DEC-…")
        header = [l for l in SIZE_FILE.read_text().splitlines() if l.startswith("#")]
        SIZE_FILE.write_text("\n".join(header + [f"{k} = {measured[k]}" for k in SIZE_KEYS]) + "\n")
        print(f"mobile size: recorded {args.bytes} B")
        return 0
    errors = []
    work_what, _, work_needs = change(working)
    if work_needs and not ruled(working):
        errors.append(f"{SIZE_PATH} moved from the reference ({work_what}) without a new ruling")
    env = moves(working, measured)
    if env:
        errors.append(f"measured with {', '.join(env)}; a build environment change is a baseline "
                      f"migration: check-mobile.sh --record --ruling DEC-…")
    elif needs and not ruled(working):
        errors.append(f"{working['library']} grew by {delta} B, over min(2 %, 256 KiB) = {allowance} B, "
                      f"without a ruling recorded in this change (SB0 §2.3 L7)")
    elif needs and args.bytes > int(working["bytes"]):
        errors.append(f"{args.bytes} B is over the {working['bytes']} B that ruling {working['ruling']} "
                      f"covers; re-record it with the ruling for the whole step")
    for error in errors:
        print(f"error: L7: {error}", file=sys.stderr)
    return 1 if errors else 0


def crates(selected):
    """`name manifest` for each member in the selected layers (`pure`: also
    each one marked `pure = true`); `-` for a member without a library."""
    packages = workspace_packages()
    layers = Layers({p["name"] for p in packages})
    for p in sorted(packages, key=lambda p: p["name"]):
        name = p["name"]
        if layers.layer.get(name) in selected or ("pure" in selected and name in layers.pure):
            lib = any({"lib", "rlib"} & set(t["kind"]) for t in p["targets"])
            print(name, p["manifest_path"] if lib else "-")
    return 0


def main(argv):
    if not argv:
        return graph_rules()
    if argv[0] == "crates" and len(argv) > 1:
        return crates(set(argv[1:]))
    if argv[0] == "mobile" and len(argv) == 1:
        return mobile_rules()
    if argv[0] == "size":
        return size_rule(argv[1:])
    sys.exit(__doc__)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
