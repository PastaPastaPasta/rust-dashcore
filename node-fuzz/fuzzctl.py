#!/usr/bin/env python3
# EMBARGO-1
"""fuzzctl: the node-fuzz farm around run.sh (node program O3; CS0 §9.3; PLAN §7 handling).

    python3 -I node-fuzz/fuzzctl.py check                 the registry agrees with the code
    python3 -I node-fuzz/fuzzctl.py smoke [--parallel N]  the per-PR smoke (fixed CPU budget)
    python3 -I node-fuzz/fuzzctl.py triage [--recheck]    cluster, minimise, regression vectors
    python3 -I node-fuzz/fuzzctl.py status-line           the STATUS-ready summary
    python3 -I node-fuzz/fuzzctl.py export-public DEST    public-route regression vectors
    python3 -I node-fuzz/fuzzctl.py farm launch|status|stop [--night ID]
        [--only TARGET...] [--plateau-minutes M]       (launch) stop legs on a suspected plateau
    python3 -I node-fuzz/fuzzctl.py certify TARGET@PIN [--seconds S] [--launch]
                                                       certify a leg's plateau (DEC-186)
    python3 -I node-fuzz/fuzzctl.py selftest              this tool's own tests
    python3 -I node-fuzz/fuzzctl.py under-test TARGET     the crates run.sh's under-test build instruments
    python3 -I node-fuzz/fuzzctl.py harness-smoke --bin DIR [--sequences N] [--seed S]
                                                          Core-judged random op sequences

Every fuzz process runs through run.sh, which keeps inputs, corpora, crashes and logs in the
private tree ($DASH_EMBARGO_DIR/fuzz/) and prints one summary line. This tool never sets
DASH_EMBARGO_DIR and never prints an input, a path inside the private tree or, for a
private-route target (targets.toml), anything about a finding beyond aggregate counts.
"""

import argparse
import bisect
import collections
import contextlib
import copy
import fcntl
import getpass
import hashlib
import importlib.util
import json
import math
import os
import re
import secrets
import shutil
import signal
import struct
import subprocess
import sys
import threading
import time
import tomllib
import weakref
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
RUN_SH = os.path.join(HERE, "run.sh")
# HX1 X4: the sb4-core-scenarios harness's random operation sequences.
REGEN = os.path.join(ROOT, "contrib", "node", "sb4-core-scenarios", "regen.py")
REGISTRY = os.path.join(HERE, "targets.toml")
PINS = ("v24.0",)
# Pins that triage state written before DEC-82 (v24 only) may still name: such a state stays
# valid, but no leg runs them and a finding from one gets no new vector.
LEGACY_PINS = ("v23.1",)
KINDS = ("l1", "model", "selftest")
ROUTES = ("private", "public")
# A planted search's feature (O2b-4 D41): its node side runs an armed bug, so its rows are
# O2B4-RUN only, never in the smoke, the farm or a certificate.
PLANTED = "planted"
# Each planted search's plant field (PLAN §3): only a kept divergence there is the plant found.
PLANT_FIELDS = {"diff_chain_par_p3a": "reply", "diff_chain_p3b": "post.persisted"}
REGISTRY_KEYS = {"name", "owner", "kind", "pins", "route", "encoding", "features", "smoke", "nightly", "public_seeds", "honggfuzz",
                 "min_certify_execs", "plateau_cov_slack", "plateau_ft", "replay_cov_slack",
                 "replay_ft_slack", "smoke_corpus_max", "smoke_corpus_max_bytes", "under_test"}
# The workspace fuzz/ crate's directories of upstream's honggfuzz targets (outside the registry).
UPSTREAM_FUZZ_DIRS = ("dash", "hashes")
# A chain target's corpus encoding (O2b-4 §3a.2, D55), as targets.toml and node-l1/chain-encodings.toml
# name it.
ENCODING_NAME = re.compile(r"chain-v[1-9][0-9]*")
# The node-fuzz feature a chain-v2 target's bin is built with (node-l1/sut; O2b-4 4a, D40).
CHAIN_V2_FEATURE = "chain-diff"
# Each chain target's encoding, compiled into node-l1 (D55); the registry's rows must agree.
CHAIN_ENCODINGS = os.path.join(ROOT, "node-l1", "chain-encodings.toml")
# The census floors a chain-v2 target's certify run is gated on (O2b-4 §4.5, D46).
CENSUS_DIR = os.path.join(ROOT, "node-l1", "census")
# BL-812: the Core side, never in an under-test build (Core's coverage is the oracle flavor's).
NOT_UNDER_TEST = ("dash-oracle-sys",)
# BL-812: run.sh's opt-in builds; a leg fuzzctl runs (smoke, farm) is always the default build.
OPT_IN_BUILD_VARS = ("NODE_FUZZ_COVERAGE", "NODE_FUZZ_TRACE_CMP")


def load_sibling(name):
    """node-fuzz/<name>.py as a module: fuzzctl runs as `python3 -I`, which keeps its directory off
    sys.path."""
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


census_floor = load_sibling("census_floor")
tree_encoding = load_sibling("tree_encoding")


def encoding_problem(fdir, target, encoding, run_dir=None):
    """Why `target`'s private tree, or its run `run_dir`, is not of `encoding` (the registry's,
    O2b-4 §3a.3), None when it is or `encoding` is empty: the tree's ENCODING marker, and the
    run's record of the encoding its binary was built for, must both name it. Triage moves no
    vector of a chain target across encodings."""
    if not encoding:
        return None
    try:
        marked = tree_encoding.read_marker(os.path.join(fdir, target))
    except tree_encoding.Refused as e:
        return str(e)
    if marked != encoding:
        return f"encoding-mismatch: the tree is {marked!r}, the registry {encoding}"
    if run_dir is not None:
        lines = log_lines(os.path.join(run_dir, "encoding"))
        recorded = lines[0].decode(errors="replace") if lines is not None and len(lines) == 1 else None
        if recorded != encoding:
            return f"encoding-mismatch: the run is {recorded!r}, the registry {encoding}"
    return None


def chain_encoding(target):
    """`target`'s encoding in node-l1/chain-encodings.toml, "" for a target it does not name, None
    when the file cannot be read."""
    try:
        with open(CHAIN_ENCODINGS, "rb") as f:
            encoding = tomllib.load(f).get(target, "")
    except (OSError, tomllib.TOMLDecodeError):
        return None
    return encoding if isinstance(encoding, str) else None
# A divergence record: its input's sha256.
SHA_NAME = re.compile(r"[0-9a-f]{64}")
# A regression vector: `<cluster id>-<generation>`, owned by one cluster.
VECTOR_NAME = re.compile(r"[0-9a-f]{12}-[0-9]+")
# Counts a run's verdict carries, and the CPU-hours below which a leg's share counts as spent.
FINDING_KEYS = ("divergences", "crashes", "sink_failures")
DONE_EPS = 1e-3
# Where the manager reserves wall-clock bench windows (DEC-23(b)): lines `<start> <end> [id]`,
# ISO 8601 with a UTC offset. A missing file reserves nothing. (/work/bench is read-only for
# agents.)
DEFAULT_WINDOWS = os.path.expanduser("~/.config/node-program/bench-windows")
# node-bench marks every process of a run with this variable (node-bench/nodebench/noise.py).
BENCH_MARK_VAR = "NODE_BENCH_MARK"
UNIT_PREFIX = "node-fuzz-farm-"


def die(msg):
    print(f"fuzzctl: {msg}", file=sys.stderr)
    sys.exit(2)


def total(rows, key):
    return sum(r.get(key, 0) for r in rows)


def today():
    return datetime.now().strftime("%Y-%m-%d")


# ---------------------------------------------------------------------------------------------
# Registry

# The smoke fuzzes from a sample of each target's working corpus and public seeds, never all of
# them (BL-333): libFuzzer reads its stop file only once its corpus is loaded, and a farmed corpus
# of thousands of inputs at 1-9 executions a second outlived the budget. Sized for the slowest
# targets: at the 90th percentile of their measured CPU-s per execution (diff_dml_block 0.30,
# diff_p2p_inv 0.14, diff_p2p_addr* 0.09-0.10) a leg's private seeds, fixed vectors and 64
# sampled inputs load within about a third of its budget. targets.toml overrides either cap; its
# rule is half the budget, which a leg whose private seeds and fixed vectors take about that
# much already cannot meet by any cap.
SMOKE_CORPUS_MAX = 64
SMOKE_CORPUS_MAX_BYTES = 4 << 20


@dataclass(frozen=True)
class Target:
    name: str
    kind: str
    pins: tuple
    route: str
    smoke: float
    nightly: float
    public_seeds: tuple = ()
    owner: str = ""
    honggfuzz: str = ""
    min_certify_execs: int = 0  # 0: the default floor (CERTIFY_EXECS)
    plateau_cov_slack: int = 0  # cov a window may gain and still be flat (DEC-260 (4))
    plateau_ft: bool = True  # whether ft counts in the plateau tests (DEC-260 (2))
    replay_cov_slack: int = 0  # cov two processes' replays of one corpus may differ by (DEC-271)
    replay_ft_slack: int = 0  # ft they may differ by, when more than PLATEAU_FT of it (DEC-271)
    smoke_corpus_max: int = SMOKE_CORPUS_MAX  # files of the smoke's sample of the working corpus (BL-333)
    smoke_corpus_max_bytes: int = SMOKE_CORPUS_MAX_BYTES  # and its bytes
    features: tuple = ()  # node-fuzz cargo features run.sh builds the bin with: its required-features (D40)
    encoding: str = ""  # a chain target's corpus encoding (D55), node-l1/chain-encodings.toml's; else ""
    under_test: tuple = ()  # the packages run.sh's NODE_FUZZ_COVERAGE=under-test build instruments (BL-812)

    @property
    def private(self):
        return self.route == "private"

    @property
    def plateau(self):
        return Plateau(self.plateau_cov_slack, self.plateau_ft)

    @property
    def replay(self):
        return Replay(self.replay_cov_slack, self.replay_ft_slack)


@dataclass(frozen=True)
class Plateau:
    """What the plateau tests (the farm's suspicion and `certify`'s window) count as growth for a
    target within its processes: more than `cov_slack` cov, or, when `ft` holds, more than
    PLATEAU_FT of ft or any discovery line. A target whose replays drift without mutation sets
    them in targets.toml (DEC-260). The slack is a leg's, summed over its processes, as the farm's
    history is. Comparisons between processes are `Replay`'s (DEC-271)."""

    cov_slack: int = 0
    ft: bool = True

    def strictest(self, other):
        """The rule that counts as growth whatever either rule does."""
        return Plateau(min(self.cov_slack, other.cov_slack), self.ft or other.ft)

    def record(self):
        return {"cov_slack": self.cov_slack, "ft": self.ft}

    @staticmethod
    def from_record(r):
        """The rule a certificate recorded, or None when `r` is not one."""
        ok = (isinstance(r, dict) and set(r) == {"cov_slack", "ft"} and is_count(r["cov_slack"])
              and r["cov_slack"] <= MAX_COV_SLACK and isinstance(r["ft"], bool))
        return Plateau(r["cov_slack"], r["ft"]) if ok else None


@dataclass(frozen=True)
class Replay:
    """How far `certify`'s two replays of one corpus, each in a process of its own, may differ
    before they count as growth or as not reproducing (DEC-271): `cov_slack` cov, and ft by the
    larger of PLATEAU_FT of the earlier replay's ft and `ft_slack`. Set in targets.toml from a
    measurement of the target's replays."""

    cov_slack: int = 0
    ft_slack: int = 0

    def ft_allowance(self, ft):
        return max(PLATEAU_FT * ft, self.ft_slack)

    def strictest(self, other):
        """The rule that allows no more than either rule does."""
        return Replay(min(self.cov_slack, other.cov_slack), min(self.ft_slack, other.ft_slack))

    def record(self):
        return {"cov_slack": self.cov_slack, "ft_slack": self.ft_slack}

    @staticmethod
    def from_record(r):
        """The rule a certificate recorded, or None when `r` is not one."""
        ok = (isinstance(r, dict) and set(r) == {"cov_slack", "ft_slack"} and is_count(r["cov_slack"])
              and r["cov_slack"] <= MAX_COV_SLACK and is_count(r["ft_slack"]) and r["ft_slack"] <= MAX_FT_SLACK)
        return Replay(r["cov_slack"], r["ft_slack"]) if ok else None


MAX_COV_SLACK = 64  # a larger cov slack is a typo, not a measured drift
# Absolute, like the slack: about 1 % of diff_chain's ft, but a quarter of a small target's (ft
# ~4000). A guard against a typo, not a bound; a target that measures more raises it.
MAX_FT_SLACK = 1000


def load_registry(path=REGISTRY):
    """The registry's targets; dies on a malformed entry."""
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    targets, seen = [], set()
    for t in raw.get("target", []):
        name = t.get("name", "")
        if not re.fullmatch(r"[a-z0-9_]+", name) or name in seen:
            die(f"targets.toml: bad or repeated name {name!r}")
        seen.add(name)
        kind, route = t.get("kind"), t.get("route", "private")
        pins = tuple(t.get("pins", []))
        smoke, nightly = t.get("smoke", 0), t.get("nightly", 0)
        if kind not in KINDS or route not in ROUTES or any(p not in PINS for p in pins):
            die(f"targets.toml: {name}: bad kind, route or pins")
        if len(set(pins)) != len(pins) or not all(
            isinstance(x, (int, float)) and x >= 0 for x in (smoke, nightly)
        ):
            die(f"targets.toml: {name}: repeated pins or a negative budget")
        if (kind == "selftest") != (not pins) or (kind == "selftest" and (smoke or nightly)):
            die(f"targets.toml: {name}: only the self-test has no pins, and it has no budgets")
        if kind == "l1" and route != "private":
            die(f"targets.toml: {name}: L1 targets compare consensus codecs and are private-route")
        unknown = set(t) - REGISTRY_KEYS
        if unknown:
            die(f"targets.toml: {name}: unknown keys {sorted(unknown)}")
        owner, twin = t.get("owner"), t.get("honggfuzz", "")
        if not isinstance(owner, str) or not re.fullmatch(r"[a-z0-9_-]+", owner) or not isinstance(twin, str):
            die(f"targets.toml: {name}: a missing or bad owner, or a bad honggfuzz path")
        floor = t.get("min_certify_execs", 0)
        if not is_count(floor, 1) and "min_certify_execs" in t:
            die(f"targets.toml: {name}: min_certify_execs must be a positive whole number")
        slack, ft = t.get("plateau_cov_slack", 0), t.get("plateau_ft", True)
        if not is_count(slack) or slack > MAX_COV_SLACK or not isinstance(ft, bool):
            die(f"targets.toml: {name}: plateau_cov_slack must be a whole number up to {MAX_COV_SLACK} and "
                "plateau_ft a boolean")
        replay = Replay.from_record({"cov_slack": t.get("replay_cov_slack", 0),
                                     "ft_slack": t.get("replay_ft_slack", 0)})
        if replay is None:
            die(f"targets.toml: {name}: replay_cov_slack must be a whole number up to {MAX_COV_SLACK} and "
                f"replay_ft_slack one up to {MAX_FT_SLACK}")
        features = t.get("features", [])
        if (not isinstance(features, list)
                or not all(isinstance(f, str) and re.fullmatch(r"[a-z0-9_][a-z0-9_-]*", f) for f in features)
                or len(set(features)) != len(features)):
            die(f"targets.toml: {name}: features must be a list of distinct cargo feature names")
        encoding = t.get("encoding", "")
        if "encoding" in t and not (isinstance(encoding, str) and ENCODING_NAME.fullmatch(encoding)):
            die(f"targets.toml: {name}: encoding must name a chain encoding (chain-v<n>)")
        under_test = t.get("under_test", [])
        if "under_test" in t and (
                not isinstance(under_test, list) or not under_test
                or not all(isinstance(c, str) and re.fullmatch(r"[a-z0-9_][a-z0-9_-]*", c) for c in under_test)
                or len(set(under_test)) != len(under_test)):
            die(f"targets.toml: {name}: under_test must be a non-empty list of distinct cargo package names")
        sample = {k: t.get(k, default) for k, default in (("smoke_corpus_max", SMOKE_CORPUS_MAX),
                                                           ("smoke_corpus_max_bytes", SMOKE_CORPUS_MAX_BYTES))}
        if not all(is_count(x) for x in sample.values()):
            die(f"targets.toml: {name}: smoke_corpus_max and smoke_corpus_max_bytes must be whole numbers")
        targets.append(
            Target(name, kind, pins, route, smoke, nightly, tuple(t.get("public_seeds", [])), owner, twin, floor,
                   slack, ft, replay.cov_slack, replay.ft_slack, **sample, features=tuple(features),
                   encoding=encoding, under_test=tuple(under_test))
        )
    return targets


def registered_names(source, const):
    """The `name: "…"` entries of the Rust slice constant `const` in `source`."""
    m = re.search(rf"pub const {const}: &\[\w+\] = &\[(.*?)\];", source, re.S)
    return re.findall(r'name: "([a-z0-9_]+)"', m.group(1)) if m else None


def planted_names(source):
    """node-l1's `PLANTED_CHAIN_TARGETS` (the planted searches), or None if it cannot be found."""
    m = re.search(r"pub const PLANTED_CHAIN_TARGETS: \[&str; \d+\] = \[(.*?)\];", source, re.S)
    return re.findall(r'"([a-z0-9_]+)"', m.group(1)) if m else None


def registered_pins(source, const):
    """{name: pins} of the entries of `const` that carry a `pins:` field, in any field order: a
    literal (`&Pin::ALL`, `&[Pin::V24_0, …]`) or the name of a `const NAME: &[Pin] = …;`."""
    m = re.search(rf"pub const {const}: &\[\w+\] = &\[(.*?)\n\];", source, re.S)
    if not m:
        return None

    def parse(spec):
        return PINS if spec == "&Pin::ALL" else tuple(
            sorted(f"v{major}.{minor}" for major, minor in re.findall(r"Pin::V(\d+)_(\d+)", spec)))

    consts = {n: parse(v) for n, v in re.findall(r"const (\w+): &\[Pin\] = (&Pin::ALL|&\[[^\]]*\]);", source)}
    out = {}
    for entry in re.findall(r"Target \{(.*?)\n    \},?", m.group(1), re.S):
        entry = re.sub(r"//[^\n]*", "", entry)
        name = re.search(r'name: "([a-z0-9_]+)"', entry)
        pins = re.search(r"\bpins: (&Pin::ALL|&\[[^\]]*\]|\w+),", entry)
        if not name or not pins:
            continue
        spec = pins.group(1)
        if spec in consts:
            out[name.group(1)] = consts[spec]
        elif spec.startswith("&"):
            out[name.group(1)] = parse(spec)
    return out


def feature_closure(features, enables):
    """The node-fuzz features a build with `features` turns on: those, and each one a feature
    turns on in turn (`enables`, Cargo.toml's [features]; `dep:` and `crate/feature` entries are
    another crate's)."""
    on, todo = set(), list(features)
    while todo:
        f = todo.pop()
        if f in on:
            continue
        on.add(f)
        entries = enables.get(f, [])
        todo += [e for e in (entries if isinstance(entries, list) else [])
                 if isinstance(e, str) and "/" not in e and not e.startswith("dep:")]
    return on


def path_crates(root=ROOT):
    """This repository's crates node-fuzz's bins can link: node-fuzz and every crate it reaches
    through `path` dependencies (normal ones, optional included, in any target table; an entry
    with `workspace = true` takes its path from the root's [workspace.dependencies]) that its
    Cargo.lock builds from a path. Maps each package name to its rustc crate name (its [lib]
    name, else the package name with `_`)."""
    with open(os.path.join(root, "Cargo.toml"), "rb") as f:
        shared = tomllib.load(f).get("workspace", {}).get("dependencies", {})
    crates, todo = {}, [os.path.join(root, "node-fuzz")]
    while todo:
        d = todo.pop()
        with open(os.path.join(d, "Cargo.toml"), "rb") as f:
            m = tomllib.load(f)
        name = m["package"]["name"]
        if name in crates:
            continue
        crates[name] = m.get("lib", {}).get("name", name.replace("-", "_"))
        tables = [m.get("dependencies", {})] + [t.get("dependencies", {}) for t in m.get("target", {}).values()]
        for key, spec in ((k, s) for t in tables for k, s in t.items()):
            if isinstance(spec, dict) and spec.get("workspace") is True:
                spec, base = shared.get(key, {}), root
            else:
                base = d
            if isinstance(spec, dict) and isinstance(spec.get("path"), str):
                todo.append(os.path.normpath(os.path.join(base, spec["path"])))
    with open(os.path.join(root, "node-fuzz", "Cargo.lock"), "rb") as f:
        locked = {p["name"] for p in tomllib.load(f).get("package", []) if "source" not in p}
    return {name: crate for name, crate in crates.items() if name in locked}


def under_test_problems(target, crates):
    """Why `target`'s `under_test` cannot be built (`crates` is [`path_crates`]): a row whose
    system under test is Core (a model row, or an l1 row other than the chain Diff rows), a package
    node-fuzz does not link from this repository, or the Core side (BL-812). The plain oracle of
    an under-test build would leave libFuzzer blind to a Core system under test."""
    if target.under_test and not (target.kind == "selftest" or target.encoding == "chain-v2"):
        yield (f"{target.name}: under_test is for the chain Diff rows (chain-v2) and the selftest; "
               f"this {target.kind} row's system under test is Core")
    for c in target.under_test:
        if c not in crates:
            yield f"{target.name}: under_test names {c}, which is not one of node-fuzz's path crates"
        elif c in NOT_UNDER_TEST:
            yield f"{target.name}: under_test names {c}, the Core side, which is never under test"


def check_registry(root=ROOT):
    """Problems that make the registry disagree with the code (empty when it agrees)."""
    here = os.path.join(root, "node-fuzz")
    targets = {t.name: t for t in load_registry(os.path.join(here, "targets.toml"))}
    with open(os.path.join(here, "Cargo.toml"), "rb") as f:
        manifest = tomllib.load(f)
    bins = {b["name"]: b["path"] for b in manifest.get("bin", [])}
    required = {b["name"]: sorted(b.get("required-features", [])) for b in manifest.get("bin", [])}
    declared = set(manifest.get("features", {}))
    enables = manifest.get("features", {})
    problems = []
    files = {f"fuzz_targets/{f}" for f in os.listdir(os.path.join(here, "fuzz_targets")) if f.endswith(".rs")}
    for f in sorted(files - set(bins.values())):
        problems.append(f"{f} is not a [[bin]] of node-fuzz/Cargo.toml")
    for name in sorted(set(bins) - set(targets)):
        problems.append(f"the [[bin]] {name} is not in targets.toml")
    for name in sorted(set(targets) - set(bins)):
        problems.append(f"targets.toml lists {name}, which is not a [[bin]]")
    for name, path in sorted(bins.items()):
        t = targets.get(name)
        if t is None:
            continue
        try:
            src = open(os.path.join(here, path)).read()
        except OSError:
            problems.append(f"{name}: {path} is missing")
            continue
        macro = {
            "l1": rf'l1_target!\("{name}"\)',
            "selftest": rf'l1_target!\("{name}",',
            "model": rf'model_target!\("{name}"\)',
        }[t.kind]
        if len(re.findall(r"(?:l1|model)_target!\(", src)) != 1 or not re.search(macro, src):
            problems.append(f"{name}: {path} does not define one {t.kind} target of that name")
        # run.sh builds a bin with its registry features, which must be exactly its
        # required-features, each a feature node-fuzz declares (D40).
        if sorted(t.features) != required[name]:
            problems.append(f"{name}: its features {sorted(t.features)} are not its [[bin]]'s "
                            f"required-features {required[name]}")
        for f in sorted(set(t.features) - declared):
            problems.append(f"{name}: feature {f} is not one of node-fuzz/Cargo.toml's [features]")
    for kind, path, const in (
        ("l1", "node-l1/src/lib.rs", "TARGETS"),
        ("model", "node-fuzz/src/models.rs", "MODELS"),
    ):
        names = registered_names(open(os.path.join(root, path)).read(), const)
        if names is None:
            problems.append(f"{path}: cannot find {const}")
            continue
        listed = {n for n, t in targets.items() if t.kind == kind}
        for n in sorted(set(names) - listed):
            problems.append(f"{n} is in {const} ({path}) but not a {kind} target in targets.toml")
        for n in sorted(listed - set(names)):
            problems.append(f"{n} is a {kind} target in targets.toml but not in {const} ({path})")
        if kind == "l1":
            pins = registered_pins(open(os.path.join(root, path)).read(), const)
            for n in sorted(listed & set(names)):
                if pins.get(n) != tuple(sorted(targets[n].pins)):
                    problems.append(f"{n}: the pins of its {const} entry ({path}) are not those of its targets.toml entry")
    # A chain target's public seeds are its own encoding's compile alone (D55, OD-L3-95 (1)): a
    # directory of another encoding's bytes would mean other programs under its decoder.
    encodings = os.path.join(root, "node-l1", "chain-encodings.toml")
    try:
        with open(encodings, "rb") as f:
            chain = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        problems.append("node-l1/chain-encodings.toml is missing or unreadable")
        chain = {}
    for name in sorted(chain):
        t = targets.get(name)
        if t is None:
            problems.append(f"{name} is in node-l1/chain-encodings.toml but not in targets.toml")
            continue
        if list(t.public_seeds) != [f"node-fuzz/public-seeds/{name}"]:
            problems.append(f"{name}: a chain target's public seeds are node-fuzz/public-seeds/{name} alone")
        # Every chain target's row names its encoding, the one its binary is compiled with (run.sh
        # passes it to the target, which refuses any other, and marks the tree with it; §3a.3).
        if t.encoding != chain[name]:
            problems.append(f"{name}: its encoding {t.encoding!r} is not node-l1/chain-encodings.toml's {chain[name]!r}")
        # A chain-v2 target's bin compares Core with the node, which only chain-diff builds.
        if chain[name] == "chain-v2" and CHAIN_V2_FEATURE not in feature_closure(t.features, enables):
            problems.append(f"{name}: a chain-v2 target is built with the {CHAIN_V2_FEATURE} feature")
    for name, t in sorted(targets.items()):
        if name not in chain and (t.encoding or name.startswith("diff_chain")):
            problems.append(f"{name} is a chain target in targets.toml but not in node-l1/chain-encodings.toml")
    # A planted search (D41) has no budget and no certify metadata, and is one node-l1 knows.
    planted = planted_names(open(os.path.join(root, "node-l1", "src", "chain", "intent.rs")).read())
    if planted is None:
        problems.append("node-l1/src/chain/intent.rs: cannot find PLANTED_CHAIN_TARGETS")
    for name, t in sorted(targets.items()):
        if PLANTED not in t.features:
            if planted is not None and name in planted:
                problems.append(f"{name} is in node-l1's PLANTED_CHAIN_TARGETS but not built with {PLANTED}")
            continue
        if t.smoke or t.nightly:
            problems.append(f"{name}: a planted search has smoke 0 and nightly 0 (O2B4-RUN only)")
        if t.min_certify_execs:
            problems.append(f"{name}: a planted search has no min_certify_execs (never certified)")
        if t.plateau_cov_slack or t.replay_cov_slack or t.replay_ft_slack or not t.plateau_ft:
            problems.append(f"{name}: a planted search has no plateau or replay slacks (never certified)")
        if planted is not None and name not in planted:
            problems.append(f"{name}: built with {PLANTED} but not in node-l1's PLANTED_CHAIN_TARGETS")
        if name not in PLANT_FIELDS:
            problems.append(f"{name}: built with {PLANTED} but has no plant field in fuzzctl's PLANT_FIELDS")
    for name in sorted(set(PLANT_FIELDS) - {n for n, t in targets.items() if PLANTED in t.features}):
        problems.append(f"{name} is in fuzzctl's PLANT_FIELDS but is not a planted row")
    # An under-test build (BL-812) instruments only crates node-fuzz links from this repository,
    # never the Core side.
    if any(t.under_test for t in targets.values()):
        try:
            linkable = path_crates(root)
        except (OSError, KeyError, tomllib.TOMLDecodeError):
            problems.append("node-fuzz's path dependencies cannot be read")
        else:
            for name in sorted(targets):
                problems.extend(under_test_problems(targets[name], linkable))
    for t in targets.values():
        for d in t.public_seeds:
            if not os.path.isdir(os.path.join(root, d)):
                problems.append(f"{t.name}: public seed directory {d} is missing")
        if not os.path.isfile(os.path.join(root, t.owner, "Cargo.toml")):
            problems.append(f"{t.name}: its owner crate {t.owner} is not in the repository")
        if t.honggfuzz and not os.path.isfile(os.path.join(root, t.honggfuzz)):
            problems.append(f"{t.name}: its honggfuzz twin {t.honggfuzz} is missing")
    # Every honggfuzz target of a node crate in the workspace fuzz/ crate is registered here.
    claimed = {t.honggfuzz for t in targets.values() if t.honggfuzz}
    hfuzz = os.path.join(root, "fuzz", "fuzz_targets")
    for d in sorted(set(listdir_or_empty(hfuzz)) - set(UPSTREAM_FUZZ_DIRS)):
        for f in sorted(listdir_or_empty(os.path.join(hfuzz, d))):
            path = f"fuzz/fuzz_targets/{d}/{f}"
            if f.endswith(".rs") and path not in claimed:
                problems.append(f"{path} is a node crate's fuzz target that no targets.toml entry names")
    return problems


def legs(targets, budget):
    """(target, pin) for every target whose `budget` attribute is positive. A planted search is
    never one, whatever its budget (D41): a timer's `farm work` plans without `check_registry`."""
    return [(t, p) for t in targets if getattr(t, budget) > 0 and PLANTED not in t.features for p in t.pins]


# ---------------------------------------------------------------------------------------------
# The private tree and run.sh


def fuzz_dir():
    root = os.environ.get("DASH_EMBARGO_DIR", "")
    if not root:
        die("DASH_EMBARGO_DIR is unset; fuzz targets run only with the private sink (CS0 §9.3)")
    if not os.path.isabs(root):
        die("DASH_EMBARGO_DIR must be an absolute path")
    d = os.path.join(root, "fuzz")
    if not os.path.isdir(d) or os.stat(d).st_mode & 0o777 != 0o700:
        die("the private fuzz directory is missing or not mode 0700")
    return d


def private_mkdir(*paths):
    """Creates each directory, in order, with mode 0700 whatever the umask (list parents
    first: `os.makedirs` gives intermediate directories the umask's mode)."""
    for path in paths:
        os.makedirs(path, mode=0o700, exist_ok=True)
        os.chmod(path, 0o700)


def write_private(path, data):
    """Writes `data` (str or bytes) to `path` with mode 0600, atomically."""
    tmp = f"{path}.tmp{os.getpid()}"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(data.encode() if isinstance(data, str) else data)
    os.replace(tmp, path)


INVALID = object()  # read_json's default where an unreadable file must not pass for a missing one


def read_json(path, default=None):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError, RecursionError):
        return default


@dataclass
class Leg:
    """One run.sh invocation and its outcome."""

    target: str
    pin: str
    line: str = ""  # run.sh's summary line (or its fixed diagnostic)
    code: int = -1
    run_dir: str = ""
    verdict: dict = field(default_factory=dict)
    interrupted: bool = False  # the farm interrupted it

    @property
    def passed(self):
        return self.code == 0

    @property
    def has_verdict(self):
        """Whether run.sh ran to its verdict (pass or fail), not a timeout, a cancel, a refusal,
        a failed build or a kill: only a verdict says whether a replay still diverges."""
        return self.code in (0, 1) and bool(self.verdict)

    @property
    def shows_finding(self):
        """Whether run.sh's verdict failed on a divergence, a crash or a sink failure: a run
        fails as well on a kill, a refusal or an interrupt, which say nothing of the input."""
        v = self.verdict
        return self.code == 1 and any(is_count(v.get(k), 1) for k in ("divergences", "crashes", "sink_failures"))


def find_run(fdir, target, pin, pid, since):
    """The run directory run.sh (process `pid`, started at `since`) created: its id ends in
    `-<pin>-<pid>`, or `-<pin>-<pid>-<tag>`; a reused pid's older runs are told apart by their
    creation time."""
    runs = os.path.join(fdir, target, "runs")
    ours = re.compile(rf"-{re.escape(pin)}-{pid}(-[0-9a-f]{{16}})?$")
    found = []
    with contextlib.suppress(OSError):
        for name in os.listdir(runs):
            path = os.path.join(runs, name)
            if ours.search(name) and os.stat(path).st_mtime >= since - 2:
                found.append((os.stat(path).st_mtime, path))
    return max(found)[1] if found else ""


def tagged_run(fdir, target, tag):
    """The run directory whose id ends in `-<tag>` (run.sh --run-tag), or ""."""
    runs = os.path.join(fdir, target, "runs")
    with contextlib.suppress(OSError):
        for name in sorted(os.listdir(runs)):
            if name.endswith("-" + tag):
                return os.path.join(runs, name)
    return ""


def segment_cpu_hours(run_dir):
    """The CPU time of a run's finished segments (`/usr/bin/time`'s user and system seconds)."""
    seconds = 0.0
    with contextlib.suppress(OSError):
        for seg in os.listdir(os.path.join(run_dir, "logs")):
            with contextlib.suppress(OSError, ValueError):
                seconds += sum(float(x) for x in open(os.path.join(run_dir, "logs", seg, "cpu")).read().split()[-2:])
    return round(seconds / 3600, 6)


def run_leg(target, pin, args, on_start=None, timeout=None, cancel=None):
    """Runs `run.sh <target> --pin <pin> <args…>` in a process group of its own. Its stdout and
    stderr carry only the summary line or a fixed diagnostic; nothing else is read. After
    `timeout` seconds, or as soon as `cancel()` (checked every second while it runs) is true,
    the group is interrupted (then killed) and the leg fails. The timeout starts once run.sh has
    built the target (it writes the run's `built` file then): the build has `BUILD_TIMEOUT` of
    its own (BL-174), so a cold tree's build never uses up a job's time."""
    fdir = fuzz_dir()
    since = time.time()
    proc = subprocess.Popen(
        [RUN_SH, target, "--pin", pin, *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
        env={k: v for k, v in os.environ.items() if k not in OPT_IN_BUILD_VARS},
    )
    if on_start:
        on_start(proc)
    timed_out = cancelled = False
    built = timeout is None
    deadline = None if timeout is None else time.monotonic() + BUILD_TIMEOUT
    while True:
        # With a timeout the leg is polled every second for the end of its build.
        waits = ([1.0] if cancel or not built else []) + ([max(0.0, deadline - time.monotonic())] if deadline else [])
        wait = min(waits) if waits else None
        try:
            out, _ = proc.communicate(timeout=wait)
            break
        except subprocess.TimeoutExpired:
            if not built:
                run_dir = find_run(fdir, target, pin, proc.pid, since)
                if run_dir and os.path.exists(os.path.join(run_dir, BUILT_FILE)):
                    built, deadline = True, time.monotonic() + timeout
            cancelled = cancel is not None and cancel()
            timed_out = deadline is not None and time.monotonic() >= deadline
            if cancelled or timed_out:
                break
    if timed_out or cancelled:
        for sig, grace in ((signal.SIGINT, 30), (signal.SIGKILL, None)):
            with contextlib.suppress(ProcessLookupError):
                os.killpg(proc.pid, sig)
            try:
                out, _ = proc.communicate(timeout=grace)
                break
            except subprocess.TimeoutExpired:
                continue
    lines = [x for x in out.splitlines() if x.startswith(("node-fuzz ", "run.sh: "))]
    line = lines[-1] if lines else f"run.sh exited {proc.returncode} with no summary"
    run_dir = find_run(fdir, target, pin, proc.pid, since)
    verdict = read_json(os.path.join(run_dir, "verdict.json"), {}) if run_dir else {}
    if cancelled:
        return Leg(target, pin, "run.sh: cancelled", 130, run_dir, verdict)
    if timed_out:
        return Leg(target, pin, "run.sh: timed out", 124, run_dir, verdict)
    return Leg(target, pin, line, proc.returncode, run_dir, verdict)


# Seconds a triage job may take, from the end of its build, before it is stopped (and the finding
# kept unminimised).
JOB_TIMEOUT = {"minimize": 900, "minimize-divergence": 900, "replay": 300}
# Seconds a job's build (and the wait for another run's build, which holds the lock) may take.
BUILD_TIMEOUT = 3600
# The file run.sh creates in a run's directory when its build is done.
BUILT_FILE = "built"


def stage_job(fdir, target, mode, path):
    """Writes a run.sh job, `fuzz/<target>/jobs/job-<random>` (0600), and returns its name. A
    finding's path (its name may be its hash) and class never go on a command line or into an
    environment, which `ps` and /proc show (PLAN §7.5): run.sh reads them from here."""
    jobs = os.path.join(fdir, target, "jobs")
    private_mkdir(os.path.join(fdir, target), jobs)
    name = "job-" + secrets.token_hex(16)
    write_private(os.path.join(jobs, name), f"mode {mode}\ninput {path}\n")
    return name


# ---------------------------------------------------------------------------------------------
# Triage

CRASH_PREFIXES = {"crash-": "crash", "timeout-": "timeout", "oom-": "oom", "leak-": "leak"}
# libFuzzer's slow-unit artifacts (`-report_slow_units`): at load 150-250 every input is slow, so
# they are counted (`slow_units`) but are not findings (BL-83). Timeouts still are.
SLOW_UNIT_PREFIX = "slow-unit-"
# Frames that say nothing about where a crash is (panic machinery, libFuzzer, std).
NOISE_FRAME = re.compile(
    r"^(std|core|alloc|libfuzzer_sys|__rust|rust_begin_unwind|rust_panic|fuzzer::|"
    r"node_fuzz::Harness|node_fuzz::Check|<|_start|__libc|main$|LLVMFuzzer|rust_fuzzer_test_input|"
    r".*__libfuzzer_sys_run|"
    r"panic_unwind|backtrace|\{\{closure\}\}$)"
)
# The harness's own stops (node-fuzz/src/lib.rs): libFuzzer reports them as crashes, but they
# are not findings (the divergences and the sink failure are counted already).
HARNESS_STOPS = ("divergence limit reached", "sink write failed")
# What libFuzzer prints when SIGINT stops it, before it exits (or hangs: BL-173).
INTERRUPTED_MARK = re.compile(r"==\d+== libFuzzer: run interrupted")
# libFuzzer's status line once it has loaded its corpus: it reads a stop file only from then on.
INITED_LINE = re.compile(r"^#\d+\s+INITED\b", re.M)
# Minimisation attempts per cluster before its unminimised vector is kept for good.
MAX_ATTEMPTS = 2
# Labelled hits whose evidence a cluster keeps (their count is `interrupts`).
MAX_EVIDENCE = 20
# Triage passes that may fail on one run before it is kept as a persistent error.
RUN_RETRIES = 3


def short_location(path):
    """A panic location without the checkout it was built in: `<crate dir>/src/…`."""
    m = re.search(r"([^/]+/src/.*)$", path)
    return m.group(1) if m else os.path.basename(path)


def crash_signatures(log_text):
    """Maps each crash file's basename to its class: `panic <file:line> < frame < frame`, or
    libFuzzer's reason with numbers removed; None for the harness's own stops. Only code
    locations and libFuzzer's fixed text are kept, never message text (which may carry
    input-derived values)."""
    out, block = {}, []
    for line in log_text.splitlines():
        m = re.search(r"Test unit written to (\S+)", line)
        if not m:
            block.append(line)
            continue
        out[os.path.basename(m.group(1))] = signature(block)
        block = []
    return out


def worker_logs(seg):
    """A segment's libFuzzer logs, one per fuzzing process: `fuzz-<i>.log` under `-jobs` (the
    parent's fuzz.out then repeats each worker's log as the worker exits, BL-86), else the one
    process's fuzz.out."""
    files = listdir_or_empty(seg)
    logs = sorted((f for f in files if re.fullmatch(r"fuzz-\d+\.log", f)),
                  key=lambda f: int(f[5:-4]))
    return [os.path.join(seg, f) for f in logs or [f for f in files if f == "fuzz.out"]]


def segments(run_dir):
    """A run's segment log directories, in order."""
    logs = os.path.join(run_dir, "logs")
    segs = [s for s in listdir_or_empty(logs) if re.fullmatch(r"segment-\d+", s)]
    return [os.path.join(logs, s) for s in sorted(segs, key=lambda s: int(s[8:]))]


def named_logs(run_dir):
    """A run's libFuzzer logs (`worker_logs` of every segment) as (name under logs/, text)."""
    for seg in segments(run_dir):
        for path in worker_logs(seg):
            with contextlib.suppress(OSError):
                yield os.path.relpath(path, os.path.join(run_dir, "logs")), open(path, errors="replace").read()


def run_logs(run_dir):
    """The texts of a run's libFuzzer logs."""
    for _, text in named_logs(run_dir):
        yield text


def artifact_writes(log_text):
    """One libFuzzer log's artifact writes, (basename, the process's `run interrupted` line if it
    had printed one before the write, else None) in order. An interrupted process exits at once, so
    a write after that line is the ALARM of one that hung in the interrupt path (BL-173). The line
    is libFuzzer's own, at the start of a line; `log_text` is one process's (a -jobs worker's)."""
    writes, interrupt = [], None
    for line in log_text.splitlines():
        if interrupt is None and INTERRUPTED_MARK.match(line):
            interrupt = line.strip()
        m = re.search(r"Test unit written to (\S+)", line)
        if m:
            writes.append((os.path.basename(m.group(1)), interrupt))
    return writes


def suspected_interrupts(run_dir):
    """{basename: evidence} for each empty `timeout-*` artifact of a run whose logs show every write
    of that name after the writer's own `run interrupted` line: a worker that hung in the interrupt
    path until its ALARM (BL-173). It is a label for a human, never a verdict: the artifact stays a
    finding (D18). The evidence is the segment, the worker (None for a lone process) and the first
    such interrupt line. libFuzzer names an artifact by the hash of its input, so a write of the
    same name before an interrupt (a real timeout of the empty input) withholds the label."""
    cdir = os.path.join(run_dir, "crashes")
    empty = set()
    for name in listdir_or_empty(cdir):
        with contextlib.suppress(OSError):
            if name.startswith("timeout-") and os.stat(os.path.join(cdir, name)).st_size == 0:
                empty.add(name)
    if not empty:
        return {}
    writes = {}
    for log, text in named_logs(run_dir):
        for name, line in artifact_writes(text):
            if name in empty:
                writes.setdefault(name, []).append((log, line))
    labels = {}
    for name, seen in writes.items():
        if all(line for _, line in seen):
            m = re.fullmatch(r"segment-(\d+)/(?:fuzz-(\d+)\.log|fuzz\.out)", seen[0][0])
            labels[name] = {"segment": int(m[1]), "worker": None if m[2] is None else int(m[2]),
                            "interrupt": seen[0][1][:200]}
    return labels


def crash_files(run_dir):
    """A fuzzing run's crash files, without libFuzzer's slow-unit artifacts (BL-83). A timeout
    written after an interrupt is one (D18)."""
    return sorted(f for f in listdir_or_empty(os.path.join(run_dir, "crashes"))
                  if not f.startswith(SLOW_UNIT_PREFIX))


def log_tail(text):
    """(the basename of the last artifact a libFuzzer log says it wrote, or ""; the lines after
    that write, or all of them without one): where an unsaved crash's report would be."""
    written = list(re.finditer(r"[^\n]*Test unit written to (\S+)", text))
    if not written:
        return "", text.splitlines()
    return os.path.basename(written[-1].group(1)), text[written[-1].end():].splitlines()


# A merge's next child process starts with these (libFuzzer's MERGE-OUTER attempt line, then its
# MERGE-INNER lines), into the same log as the failed one.
MERGE_BOUNDARY = re.compile(r"MERGE-(OUTER|INNER):")


def without_timeout_report(tail):
    """`tail` (the lines after a saved `timeout-` write) without that timeout's own report: its
    `ERROR: libFuzzer: timeout after` line and what follows it up to its `SUMMARY: libFuzzer: timeout`
    (BL-173 D11, D12). The report ends early at a merge's next attempt or at any other
    `ERROR: libFuzzer` line (a torn report, killed before its SUMMARY), and what follows still counts:
    skipping to the end would hide a later attempt's unsaved crash, and so would skipping a
    later attempt's own timeout report. A report that never began before a merge's next attempt
    (the child died between the write and its ERROR) never will: a timeout ERROR after that
    boundary is that attempt's own (D14)."""
    kept, skipping, reported = [], False, False
    for line in tail:
        if not reported and MERGE_BOUNDARY.match(line):
            reported = True
        elif not reported and re.search(r"ERROR: libFuzzer: timeout after", line):
            skipping = reported = True  # the first one only: a later attempt's timeout is its own
            continue
        if skipping:
            if line.startswith("SUMMARY: libFuzzer: timeout"):
                skipping = False
                continue
            if MERGE_BOUNDARY.match(line) or "ERROR: libFuzzer" in line:
                skipping = False
            else:
                continue
        kept.append(line)
    return kept


def log_evidence(run_dir):
    """What a run's logs say happened, by run.sh's rules: sink failures (`sink write failed`),
    refusals (`refusing to run`), and the classes of crashes libFuzzer reported but never saved
    an input for (a kill, or a sink that could not keep it)."""
    sink = refused = 0
    unsaved = []
    for _, text in named_logs(run_dir):
        sink += text.count("sink write failed")
        refused += text.count("refusing to run")
        last, tail = log_tail(text)
        # libFuzzer's ALARM writes the `timeout-` artifact before its `ERROR: libFuzzer: timeout`
        # line, which, with its SUMMARY, is that artifact's own report, not a crash it never saved
        # (BL-173 D11). Only when the artifact was saved: a write that failed leaves the report as
        # the only evidence. What follows the report (a merge's later attempt) is still scanned.
        if last.startswith("timeout-") and os.path.exists(os.path.join(run_dir, "crashes", last)):
            tail = without_timeout_report(tail)
        if any("ERROR: libFuzzer" in line for line in tail):
            klass = signature(tail)
            if klass is not None:
                unsaved.append(klass)
    return {"sink_failures": sink, "refused": refused, "unsaved_crashes": unsaved}


def signature(lines):
    location, frames, reason = None, [], None
    in_backtrace = False
    for line in lines:
        if any(stop in line for stop in HARNESS_STOPS):
            return None
        m = re.search(r"panicked at ([^:\s]+):(\d+)(?::\d+)?:?", line)
        if m:
            location, frames, in_backtrace = f"{short_location(m.group(1))}:{m.group(2)}", [], False
            continue
        if line.startswith("stack backtrace:"):
            in_backtrace = True
            continue
        m = re.match(r"\s+\d+: (?:0x[0-9a-f]+ - )?(\S+)", line) if in_backtrace else None
        if m:
            name = re.sub(r"::h[0-9a-f]{16}$", "", m.group(1))
            if not NOISE_FRAME.search(name) and len(frames) < 3:
                frames.append(name)
            continue
        m = re.search(r"==\d+== ERROR: libFuzzer: (.*)", line)
        if m:
            reason = re.sub(r"\d+", "N", m.group(1).split("(")[0]).strip()
    if location:
        return " < ".join([f"panic {location}", *frames])
    return reason or "unknown"


def cluster_id(target, kind, klass):
    return hashlib.sha256(f"{target}\0{kind}\0{klass}".encode()).hexdigest()[:12]


def provenance(verdict):
    return {k: verdict.get(k, "-") for k in (
        "rust_dashcore", "rust_dashcore_dirty", "base_sdk", "oracle_commit", "oracle_flavor",
        "toolchain")}


# What findings_of appends to a model divergence's field to name its command.
CMD_MARK = " cmd="


def findings_of(run_dir, with_cmd=False):
    """The findings of one run: (kind, class, pin, input path). With `with_cmd` (a model target's
    run) a divergence whose sidecar names a `cmd` line has it in its class, so divergences in
    one field on different commands are different clusters (BL-174)."""
    found = []
    ddir = os.path.join(run_dir, "divergences")
    with contextlib.suppress(OSError):
        for name in sorted(os.listdir(ddir)):
            if not SHA_NAME.fullmatch(name):
                continue
            side = {}
            with contextlib.suppress(OSError):
                for row in open(os.path.join(ddir, name + ".txt")):
                    k, _, v = row.strip().partition(" ")
                    side[k] = v
            klass = side.get("field", "unknown")
            if with_cmd and side.get("cmd"):
                klass += CMD_MARK + re.sub(r"[^A-Za-z0-9_.-]", "?", side["cmd"])[:24]
            found.append(("divergence", klass, side.get("pin", ""), os.path.join(ddir, name)))
    cdir = os.path.join(run_dir, "crashes")
    crashed = crash_files(run_dir)
    pin = run_pin(run_dir)
    if crashed:
        sigs = {}
        for text in run_logs(run_dir):
            sigs.update(crash_signatures(text))
        for name in crashed:
            kind = next((k for p, k in CRASH_PREFIXES.items() if name.startswith(p)), None)
            klass = sigs.get(name, kind)
            if kind and klass is not None:
                found.append((kind, klass, pin, os.path.join(cdir, name)))
    # Failures the logs record without an input to keep: no vector (None).
    evidence = log_evidence(run_dir)
    if evidence["sink_failures"]:
        found.append(("sink", "sink write failed", pin, None))
    found += [("crash", klass, pin, None) for klass in evidence["unsaved_crashes"]]
    return found


def listdir_or_empty(path):
    try:
        return os.listdir(path)
    except OSError:
        return []


def run_pin(run_dir):
    """A run's pin: its verdict's, or the one in its id (`<time>-<pin>-<pid>`)."""
    pin = read_json(os.path.join(run_dir, "verdict.json"), {}).get("pin")
    parts = os.path.basename(run_dir).split("-")
    return pin or (parts[1] if len(parts) >= 3 and parts[1] in PINS + LEGACY_PINS else "")


def run_stamp():
    """The current time as run ids begin (`run.sh`: `date -u +%Y%m%dT%H%M%SZ`)."""
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def triage_path(fdir, target):
    return os.path.join(fdir, target, "triage.json")


def load_triage(fdir, target):
    """A target's triage state. Only a missing file is an empty state; an existing one must be
    readable and of exactly the expected shape (`state_problem`), or ValueError."""
    path = triage_path(fdir, target)
    if not os.path.lexists(path):
        return {"clusters": {}, "runs": [], "run_errors": {}}
    try:
        with open(path) as f:
            state = json.load(f)
    except (OSError, ValueError):
        raise ValueError("triage.json is unreadable") from None
    problem = state_problem(target, state)
    if problem:
        raise ValueError(problem)
    state.setdefault("run_errors", {})
    return state


CLUSTER_TYPES = {"kind": str, "class": str, "pins": list, "hits": int, "first_seen": str,
                 "state": str, "minimized": bool, "attempts": int}


def is_count(x, least=0):
    return isinstance(x, int) and not isinstance(x, bool) and x >= least


def cluster_problem(target, cid, c):
    """Why cluster `cid` of `target` is not a valid cluster, or None: its fields and types, its
    id (the hash of its target, kind and class), its vector's name (owned by the cluster), and
    a fixed cluster's promotion record."""
    if not isinstance(c, dict) or not all(isinstance(c.get(k), t) for k, t in CLUSTER_TYPES.items()):
        return "a cluster has missing or mistyped fields"
    if cid != cluster_id(target, c["kind"], c["class"]):
        return "a cluster is filed under another cluster's id"
    if c["state"] not in ("open", "fixed") or not all(p in PINS + LEGACY_PINS for p in c["pins"]):
        return "a cluster has an unknown state or pin"
    if not is_count(c["hits"]) or not is_count(c["attempts"]) or not is_count(c.get("interrupts", 0)):
        return "a cluster has bad counts"
    if not isinstance(c.get("interrupt_evidence", []), list):
        return "a cluster has bad interrupt evidence"
    vector = c.get("vector")
    if vector is not None and (not isinstance(vector, str) or not VECTOR_NAME.fullmatch(vector)
                               or not vector.startswith(cid + "-")):
        return "a cluster's vector is not its own"
    if c["state"] == "fixed" and not (isinstance(c.get("fixed_stamp"), str)
                                      and isinstance(c.get("fixed_at"), str)):
        return "a fixed cluster has no promotion record"
    return None


def state_problem(target, state):
    """Why a decoded triage.json is not valid state, or None."""
    if not isinstance(state, dict) or not {"clusters", "runs"} <= set(state) <= {
            "clusters", "runs", "run_errors"}:
        return "triage.json has the wrong shape"
    if not isinstance(state["runs"], list) or not all(isinstance(r, str) for r in state["runs"]):
        return "triage.json has a bad run list"
    errors = state.get("run_errors", {})
    if not isinstance(errors, dict) or not all(isinstance(k, str) and is_count(v, 1) for k, v in errors.items()):
        return "triage.json has bad run errors"
    if not isinstance(state["clusters"], dict):
        return "triage.json has a bad cluster index"
    for cid, c in state["clusters"].items():
        problem = cluster_problem(target, cid, c)
        if problem:
            return problem
    return None


def vector_problem(fdir, target, cid, c):
    """Why cluster `cid`'s vector is not where its state says, with provenance naming the
    cluster, or None (also when it has no vector)."""
    if not c.get("vector"):
        return None
    sub = "open" if c["state"] == "open" else "fixed"
    if not os.path.isfile(os.path.join(fdir, target, "regress", sub, c["vector"])):
        return "a vector is missing"
    meta = read_json(os.path.join(fdir, target, "regress", "meta", c["vector"] + ".json"))
    if not isinstance(meta, dict) or meta.get("cluster") != cid or meta.get("target") != target:
        return "a vector's provenance is missing or names another cluster"
    return None


# The run.sh modes that fuzz, so their findings are new findings (a minimisation's merge too, BL-841).
FUZZING_MODES = ("run", "merge", "certify", "minimize-corpus")


def is_job_run(run_dir):
    """A run.sh job (a minimisation or replay), not a fuzzing run: its findings are expected
    (a minimisation aborts on purpose), so triage never treats them as new findings."""
    try:
        mode = open(os.path.join(run_dir, "mode")).read().strip()
    except OSError:
        mode = None
    return (mode not in (None, *FUZZING_MODES) or os.path.isdir(os.path.join(run_dir, "input"))
            or os.path.exists(os.path.join(run_dir, "abort-field")))


def run_live(run_dir):
    """Whether a run is still running (BL-841 r2 N1): run.sh holds an exclusive flock on the run's
    `mode` file from before it is in place to the end, and the run's fuzzing processes inherit it.
    `live` is a second name for that file, made only by a run.sh that holds the lock (r3 R3-1).
    None when there is no `live` (a run from an older run.sh, or one killed before it wrote its
    mode)."""
    try:
        fd = os.open(os.path.join(run_dir, "live"), os.O_RDONLY)
    except OSError:
        return None
    try:
        fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
        return False
    except BlockingIOError:
        return True
    except OSError:
        return None
    finally:
        os.close(fd)


class Triage:
    """The clusters of every target, kept in `fuzz/<target>/triage.json` (private).

    Regression vectors are `fuzz/<target>/regress/{open,fixed}/<cluster id>-<generation>`: each
    file belongs to one cluster, so clusters that share an input never share (or delete, or
    promote) each other's file. Provenance is `regress/meta/<name>.json`, outside the directories
    libFuzzer reads."""

    # A run without a verdict and without a `live` file to lock (`run_live`: a run from an older
    # run.sh, or one killed before it wrote its mode) is triaged once it is this old.
    stale_s = 3600

    def __init__(self, fdir, targets, runner=run_leg, minimize=True, stop=None, ready=()):
        self.fdir, self.runner, self.minimize, self.ready = fdir, runner, minimize, set(ready)
        self.yielded = False  # `stop` can flip back (a bench run ends): once true it stays true

        def latched():
            self.yielded = self.yielded or bool(stop and stop())
            return self.yielded
        self.stop = latched
        self.targets = {t.name: t for t in targets}
        self.attempted = set()  # clusters minimised in this pass
        self.stats = {"runs": 0, "findings": 0, "new_clusters": 0, "minimized": 0,
                      "minimize_failed": 0, "fixed_now": 0, "repaired": 0, "errors": 0,
                      "suspected_interrupts": 0, "recheck_unfinished": 0, "migrated": 0}

    def save(self, target, state):
        write_private(triage_path(self.fdir, target), json.dumps(state, indent=1, sort_keys=True))

    def job(self, target, pin, mode, path):
        """Runs a run.sh job (a minimisation or replay of `path`) and removes its file."""
        name = stage_job(self.fdir, target, mode, path)
        try:
            return self.runner(target, pin, ["--job", name], timeout=JOB_TIMEOUT[mode], cancel=self.stop)
        finally:
            with contextlib.suppress(OSError):
                os.remove(os.path.join(self.fdir, target, "jobs", name))

    def regress(self, target, sub):
        """`fuzz/<target>/regress/<sub>/`, created 0700."""
        base = os.path.join(self.fdir, target, "regress")
        private_mkdir(base, os.path.join(base, sub))
        return os.path.join(base, sub)

    def triage_target(self, target):
        """Triages every finished fuzzing run of `target` not triaged yet. A run whose triage
        fails is counted in `errors` and stays pending; after RUN_RETRIES failed passes it is a
        persistent error (`run_errors`), counted on every pass until someone resolves it."""
        runs_dir = os.path.join(self.fdir, target, "runs")
        if not os.path.isdir(runs_dir):
            return
        try:
            state = load_triage(self.fdir, target)
        except ValueError:
            self.stats["errors"] += 1
            return
        if self.reconcile(target, state):
            self.save(target, state)
        for run_id in sorted(set(os.listdir(runs_dir)) - set(state["runs"])):
            if self.stop():
                return
            if state["run_errors"].get(run_id, 0) >= RUN_RETRIES:
                self.stats["errors"] += 1
                continue
            run_dir = os.path.join(runs_dir, run_id)
            if is_job_run(run_dir):
                state["runs"].append(run_id)
                self.save(target, state)
                continue
            verdict, live = read_json(os.path.join(run_dir, "verdict.json")), None
            if verdict is None:
                live = run_live(run_dir)
                verdict = read_json(os.path.join(run_dir, "verdict.json"))  # it may have ended since
                if verdict is None and live:
                    # Still running, `ready` or not (a -jobs worker that outlived run.sh holds the
                    # lock too): neither triaged nor recorded (BL-841 r2 N1, BL-841b §4.2).
                    continue
            if verdict is None:
                with contextlib.suppress(OSError):
                    if run_id in self.ready or live is False or time.time() - os.stat(run_dir).st_mtime > self.stale_s:
                        verdict = {"mode": "run", "pin": run_pin(run_dir)}
            if verdict is None or verdict.get("mode") not in FUZZING_MODES:
                continue
            before = copy.deepcopy(state)
            try:
                self.triage_run(target, state, run_id, verdict)
            except (OSError, ValueError, KeyError, TypeError):
                # Nothing of a failed run's triage is kept, so a retry counts it once.
                state.clear()
                state.update(before)
                self.stats["errors"] += 1
                state["run_errors"][run_id] = state["run_errors"].get(run_id, 0) + 1
            else:
                state["run_errors"].pop(run_id, None)
                state["runs"].append(run_id)
            self.save(target, state)

    def findings(self, target, run_dir):
        return findings_of(run_dir, with_cmd=self.targets[target].kind == "model")

    def triage_run(self, target, state, run_id, verdict):
        self.stats["runs"] += 1
        run_dir = os.path.join(self.fdir, target, "runs", run_id)
        labels = suspected_interrupts(run_dir)
        for kind, klass, pin, path in self.findings(target, run_dir):
            self.stats["findings"] += 1
            # A timeout written after an interrupt is a finding like any other, with a label (D18).
            label = labels.get(os.path.basename(path)) if path else None
            run_pin = pin or verdict.get("pin", "")
            if run_pin in LEGACY_PINS:
                # A run of a pin DEC-82 dropped: counted, never clustered, so it cannot open or
                # reopen a cluster that no vector can close.
                continue
            cid = cluster_id(target, kind, klass)
            c = state["clusters"].get(cid)
            if c is None:
                self.stats["new_clusters"] += 1
                c = state["clusters"][cid] = {
                    "kind": kind, "class": klass, "pins": [], "hits": 0, "first_seen": run_id,
                    "state": "open", "vector": None, "minimized": False, "attempts": 0,
                }
            elif c["state"] == "fixed" and run_id > c.get("fixed_stamp", ""):
                # A regression of a fixed finding, in a run started after the fix: the old
                # vector passes and stays in fixed/; the new finding becomes the open vector.
                c.update(state="open", reopened=run_id, vector=None, minimized=False, attempts=0)
            if label:  # `interrupts` counts the labelled hits: all of the cluster's when it equals `hits`
                self.stats["suspected_interrupts"] += 1
                c["interrupts"] = c.get("interrupts", 0) + 1
                if len(c.setdefault("interrupt_evidence", [])) < MAX_EVIDENCE:
                    c["interrupt_evidence"].append({"run": run_id, **label})
            c["hits"] += 1
            c["last_seen"] = run_id
            if pin and pin not in c["pins"]:
                c["pins"].append(pin)
            if c["state"] == "open" and path is not None and self.wants_vector(cid, c):
                self.make_vector(target, cid, c, run_pin, path, verdict)

    def wants_vector(self, cid, c):
        if c["vector"] is None:
            return True
        return (self.minimize and not c["minimized"] and c["attempts"] < MAX_ATTEMPTS
                and c["kind"] in ("divergence", "crash") and cid not in self.attempted)

    def make_vector(self, target, cid, c, pin, path, verdict):
        """Minimises the finding through run.sh, checks by a replay that the result still has
        the cluster's class, and keeps it (or, failing that, the finding itself) as the
        cluster's open regression vector. A chain target's vector is made only within its
        encoding (`encoding_problem`): a refusal fails the run's triage (a run error)."""
        why = encoding_problem(self.fdir, target, self.targets[target].encoding,
                               os.path.join(self.fdir, target, "runs", c["last_seen"]))
        if why:
            raise ValueError(why)
        best, minimized = path, False
        if self.minimize and c["kind"] in ("divergence", "crash") and not self.stop():
            self.attempted.add(cid)
            c["attempts"] = c.get("attempts", 0) + 1
            leg = self.job(target, pin, "minimize-divergence" if c["kind"] == "divergence"
                           else "minimize", path)
            if not leg.has_verdict:  # timed out, cancelled or killed: not a failed attempt (BL-174)
                c["attempts"] -= 1
            out = os.path.join(leg.run_dir, "crashes", "minimized")
            if leg.passed and leg.run_dir and os.path.isfile(out) and self.same_class(target, pin, out, c):
                best, minimized = out, True
                self.stats["minimized"] += 1
            else:
                self.stats["minimize_failed"] += 1
        with open(best, "rb") as f:
            data = f.read()
        vdir, meta = self.regress(target, "open"), self.regress(target, "meta")
        c["generation"] = c.get("generation", 0) + 1
        name = f"{cid}-{c['generation']}"
        write_private(os.path.join(vdir, name), data)
        write_private(os.path.join(meta, name + ".json"), json.dumps({
            "target": target, "cluster": cid, "kind": c["kind"], "pin": pin,
            "from_run": c["last_seen"], "minimized": minimized, "size": len(data),
            "sha256": hashlib.sha256(data).hexdigest(), "provenance": provenance(verdict),
            "produced_by": "node-fuzz fuzzctl triage (O3)",
        }, indent=1, sort_keys=True))
        if c["vector"] and c["vector"] != name:  # this cluster's earlier open vector
            with contextlib.suppress(OSError):
                os.remove(os.path.join(vdir, c["vector"]))
        c["vector"], c["minimized"] = name, minimized

    def same_class(self, target, pin, path, c):
        """Whether replaying `path` gives a finding of the cluster's kind and class (libFuzzer's
        crash minimisation keeps any crash, not necessarily the one it started from)."""
        leg = self.job(target, pin, "replay", path)
        if not leg.run_dir:
            return False
        found = {(k, cl) for k, cl, _, _ in self.findings(target, leg.run_dir)}
        # A field-only model cluster (opened before the sidecars named commands) is still its
        # class on whichever command the input diverges on (BL-267).
        return (c["kind"], c["class"]) in found or any(
            k == c["kind"] and cl.startswith(c["class"] + CMD_MARK) for k, cl in found)

    def reconcile(self, target, state):
        """Puts each cluster's vector where its state says (open/ or fixed/), finishing a
        promotion that was interrupted between saving the state and moving the file; a vector
        in neither is an error. True when a file moved."""
        moved = False
        base = os.path.join(self.fdir, target, "regress")
        for cid, c in state["clusters"].items():
            if not c["vector"]:
                continue
            want = "open" if c["state"] == "open" else "fixed"
            other = "fixed" if want == "open" else "open"
            if os.path.isfile(os.path.join(base, want, c["vector"])):
                continue
            # A chain target's vector moves to fixed/, which every fuzzing run replays, only
            # within its encoding (§3a.3): misplaced, it stays an error.
            if want == "fixed" and encoding_problem(self.fdir, target, getattr(self.targets.get(target), "encoding", "")):
                self.stats["errors"] += 1
                continue
            newer = sorted((int(n.rsplit("-", 1)[1]), n) for n in listdir_or_empty(os.path.join(base, want))
                           if VECTOR_NAME.fullmatch(n) and n.startswith(cid + "-"))
            if os.path.isfile(os.path.join(base, other, c["vector"])):
                os.replace(os.path.join(base, other, c["vector"]),
                           os.path.join(self.regress(target, want), c["vector"]))
            elif newer:  # killed between writing a new generation and saving the state
                c["vector"], c["generation"] = newer[-1][1], newer[-1][0]
            else:
                self.stats["errors"] += 1
                continue
            self.stats["repaired"] += 1
            moved = True
        # A migration killed between saving the state and removing the old cluster's files leaves
        # them with no owner (and `open/` is exported): remove them (BL-267).
        gone = {c["migrated_from"] for c in state["clusters"].values()
                if c.get("migrated_from") and c["migrated_from"] not in state["clusters"]}
        for sub in ("open", "meta"):
            for n in listdir_or_empty(os.path.join(base, sub)):
                if any(n.startswith(g + "-") for g in gone) and VECTOR_NAME.fullmatch(n.removesuffix(".json")):
                    with contextlib.suppress(OSError):
                        os.remove(os.path.join(base, sub, n))
        self.stats["errors"] += sum(
            vector_problem(self.fdir, target, cid, c) is not None
            for cid, c in state["clusters"].items()
            if c["vector"] and os.path.isfile(os.path.join(
                base, "open" if c["state"] == "open" else "fixed", c["vector"])))
        return moved

    def recheck(self, target):
        """Replays each open cluster's vector on every pin it was seen on; a cluster whose vector
        no longer diverges or crashes on any of them is fixed (saved first), and its vector moves
        to regress/fixed/, which every fuzzing run replays. An open cluster without its vector is
        an error (counted by `reconcile`)."""
        try:
            state = load_triage(self.fdir, target)
        except ValueError:
            self.stats["errors"] += 1
            return
        if self.reconcile(target, state):
            self.save(target, state)
        t = self.targets[target]
        migrations = []
        for cid, c in list(state["clusters"].items()):
            if self.stop():
                break
            vec = os.path.join(self.fdir, target, "regress", "open", c["vector"] or "-")
            if c["state"] != "open" or not os.path.isfile(vec):
                continue
            pins = [p for p in c["pins"] if p in t.pins] or list(t.pins)
            stamp = run_stamp()
            results = []
            for p in pins:
                results.append(self.job(target, p, "replay", vec))
                if self.stop():
                    break
            diverging = any(r.shows_finding for r in results)
            if not diverging and (len(results) < len(pins) or not all(r.passed and r.has_verdict for r in results)):
                if self.stop():  # the farm yielded (a bench run, SIGTERM): the cluster stays open, no error
                    break
                # A replay that timed out, was cancelled or ended without a verdict (or failed
                # without a finding: killed, refused) says nothing about the vector: the cluster
                # stays open and the pass has an error (BL-174).
                self.stats["errors"] += 1
                self.stats["recheck_unfinished"] += 1
                continue
            if not diverging and encoding_problem(self.fdir, target, t.encoding):
                # A chain target's vector is fixed only within its encoding (§3a.3).
                self.stats["errors"] += 1
                continue
            if not diverging:
                c.update(state="fixed", fixed_stamp=stamp,
                         fixed_at=results[0].verdict.get("rust_dashcore", "-"))
                self.save(target, state)
                os.replace(vec, os.path.join(self.regress(target, "fixed"), c["vector"]))
                self.stats["fixed_now"] += 1
            else:
                complete = len(results) == len(pins) and all(r.has_verdict for r in results)
                if complete and t.kind == "model" and c["kind"] == "divergence" and CMD_MARK not in c["class"]:
                    klass = self.replayed_class(c, results)
                    if klass:
                        migrations.append((cid, klass))
        for cid, klass in migrations:
            try:
                self.migrate_cmd(target, state, cid, klass)
            except OSError:  # a file error costs this migration, not the pass: it is retried next time
                self.stats["errors"] += 1

    def replayed_class(self, c, results):
        """The class (`<field> cmd=<command>`) the replays of an open model cluster's field-only
        vector gave it, or None when they gave none or several."""
        found = {cl for r in results if r.shows_finding and r.run_dir
                 for k, cl, _, _ in findings_of(r.run_dir, with_cmd=True)
                 if k == "divergence" and cl.startswith(c["class"] + CMD_MARK)}
        return found.pop() if len(found) == 1 else None

    def migrate_cmd(self, target, state, cid, klass):
        """Re-files the open field-only model cluster `cid` under `klass`, its class with the
        command its vector diverges on (BL-267), so it and the `cmd=` clusters the new sidecars
        open are not duplicates. Its vector and provenance move to the new cluster's name; if
        that cluster exists the two merge (its open vector stays; if it has none, the
        migrated one reopens it).
        The state is saved before the old files are removed: a pass killed between leaves
        orphans in regress/open/ and regress/meta/, and the migration is not redone (the old
        cluster is gone)."""
        clusters = state["clusters"]
        old = clusters[cid]
        new_cid = cluster_id(target, "divergence", klass)
        vdir, mdir = self.regress(target, "open"), self.regress(target, "meta")
        dest = clusters.get(new_cid)
        keeps_dest_vector = bool(dest and dest["state"] == "open" and dest["vector"]
                                 and os.path.isfile(os.path.join(vdir, dest["vector"])))
        old_vector, new_vector = old["vector"], None
        if not keeps_dest_vector:
            generation = (dest or old).get("generation", 0) + (1 if dest else 0)
            new_vector = f"{new_cid}-{generation}"
            with open(os.path.join(vdir, old_vector), "rb") as f:
                data = f.read()
            meta = read_json(os.path.join(mdir, old_vector + ".json"), {})
            write_private(os.path.join(mdir, new_vector + ".json"), json.dumps(
                {**meta, "cluster": new_cid}, indent=1, sort_keys=True))
            write_private(os.path.join(vdir, new_vector), data)  # last: reconcile adopts a vector, so its meta comes first
        if dest is None:
            dest = clusters[new_cid] = dict(old, **{"class": klass})
            dest["vector"] = new_vector
            dest["generation"] = generation
            dest["migrated_from"] = cid
        else:
            dest["migrated_from"] = cid
            dest["hits"] += old["hits"]
            if "interrupts" in old or "interrupts" in dest:
                dest["interrupts"] = dest.get("interrupts", 0) + old.get("interrupts", 0)
            dest["pins"] += [p for p in old["pins"] if p not in dest["pins"]]
            dest["first_seen"] = min(dest["first_seen"], old["first_seen"])
            dest["last_seen"] = max(dest.get("last_seen", ""), old.get("last_seen", ""))
            if new_vector:  # the destination had no open vector (fixed, or lost): it reopens with this one
                dest.update(state="open", vector=new_vector, generation=generation, reopened="migrated",
                            minimized=old["minimized"], attempts=old["attempts"])
        del clusters[cid]
        self.save(target, state)
        for name in (os.path.join(vdir, old_vector), os.path.join(mdir, old_vector + ".json")):
            with contextlib.suppress(OSError):
                os.remove(name)
        self.stats["migrated"] += 1

    def ack_sink_failures(self, target):
        """Closes the target's open sink-failure clusters (once their cause, a full disk say,
        is fixed): sink failures have no input to recheck."""
        try:
            state = load_triage(self.fdir, target)
        except ValueError:
            self.stats["errors"] += 1
            return
        sinks = [c for c in state["clusters"].values() if c["kind"] == "sink" and c["state"] == "open"]
        for c in sinks:
            c.update(state="fixed", fixed_stamp=run_stamp(), fixed_at="acknowledged")
            self.stats["fixed_now"] += 1
        if sinks:
            self.save(target, state)

    def summary(self):
        return "node-fuzz triage " + " ".join(f"{k}={v}" for k, v in self.stats.items())


@contextlib.contextmanager
def triage_lock(fdir):
    fd = os.open(os.path.join(fdir, ".triage.lock"), os.O_WRONLY | os.O_CREAT, 0o600)
    with os.fdopen(fd, "w") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        yield


def plant_found(fdir, t, cid, c):
    """Whether cluster `cid` of planted search `t` is its plant, found (D41, PLAN §3): an open
    divergence at the target's plant field, with its input kept, and a hit that is not a
    suspected interrupt."""
    return (c["kind"] == "divergence" and c["class"] == PLANT_FIELDS.get(t.name) and c["state"] == "open"
            and bool(c.get("vector")) and vector_problem(fdir, t.name, cid, c) is None
            and c.get("interrupts", 0) < c["hits"])


def status_lines(fdir, targets):
    """The STATUS-ready summary and the number of triage errors behind it. The lines: one
    count line, one task line per open public cluster, and one aggregate task line for every
    open private finding (PLAN §7.5: counts only). Unreadable state, runs that could not be
    triaged and missing vectors are errors: the count is then UNKNOWN, never a known zero.
    A planted search's plant, found (`plant_found`), is counted on its own line and never
    open; its other clusters count as any target's, and a divergence at its plant field with
    no input kept is an error. A planted search not found whose state is unreadable, that has
    runs it could not triage, or whose plant-field divergence has no sound vector makes the
    planted line UNKNOWN, never a known zero."""
    open_pub, open_priv, fixed, errors = [], 0, 0, 0
    planted = [t for t in targets if PLANTED in t.features and t.kind != "selftest"]
    found, planted_unknown = 0, False
    for t in targets:
        if t.kind == "selftest":
            continue
        try:
            state = load_triage(fdir, t.name)
        except ValueError:
            errors += 1
            planted_unknown = planted_unknown or t in planted
            continue
        errors += len(state["run_errors"])
        plant, unknown = False, bool(state["run_errors"])
        for cid, c in sorted(state["clusters"].items()):
            problem = vector_problem(fdir, t.name, cid, c) is not None
            errors += problem
            if t in planted:
                if plant_found(fdir, t, cid, c):
                    plant = True
                    continue
                if c["kind"] == "divergence" and c["class"] == PLANT_FIELDS.get(t.name) and c["state"] == "open":
                    errors += not c.get("vector")
                    unknown = unknown or problem or not c.get("vector")
            if c["state"] == "fixed":
                fixed += 1
            elif t.private:
                open_priv += 1
            else:
                open_pub.append((t.name, cid, c))
        found += plant
        planted_unknown = planted_unknown or (t in planted and not plant and unknown)
    known = (f"{open_priv + len(open_pub)} (private {open_priv}, public {len(open_pub)}); "
             f"fixed {fixed}")
    lines = [f"SW-FUZZ open findings: UNKNOWN (triage errors: {errors}); known open {known}"
             if errors else f"SW-FUZZ open findings: {known}"]
    if planted:
        lines.append("SW-FUZZ planted searches: UNKNOWN" if planted_unknown
                     else f"SW-FUZZ planted searches: {found}/{len(planted)} found")
    for name, cid, c in open_pub:
        vec = (f"vector {c['vector'][:12]}" + (" (minimised)" if c["minimized"] else "")
               if c["vector"] else "no vector")
        lines.append(f"TASK fuzz-{cid}: {name} {c['kind']} [{c['class']}] on "
                     f"{','.join(c['pins']) or '-'}, {c['hits']} hits, {vec}")
    if open_priv:
        lines.append(f"TASK fuzz-private: {open_priv} open private-route findings; a designated "
                     "reader triages them in the private tree (PLAN §7.5)")
    if errors:
        lines.append(f"TASK fuzz-triage-errors: {errors} triage errors (unreadable state, runs "
                     "not triaged, missing vectors); see `fuzzctl triage` in the private tree")
    return lines, errors


def run_triage(targets, only=None, recheck=False, minimize=True, runner=run_leg, stop=None,
               ready=(), ack_sink_failures=False):
    """Triages (and with `recheck`, rechecks) every target, or those in `only`. Runs named in
    `ready` are triaged even without a verdict (the farm recovered them after a kill). Its
    summary aggregates over all of them, so it never singles out a private-route target."""
    fdir = fuzz_dir()
    tri = Triage(fdir, targets, runner=runner, minimize=minimize, stop=stop, ready=ready)
    with triage_lock(fdir):
        for t in targets:
            if t.kind == "selftest" or (only and t.name not in only):
                continue
            tri.triage_target(t.name)
            if recheck:
                tri.recheck(t.name)
            if ack_sink_failures:
                tri.ack_sink_failures(t.name)
    return tri


# ---------------------------------------------------------------------------------------------
# Closing a cluster of timeouts written after an interrupt, by hand (BL-173 D13, D18)

AUDIT_NAME = "triage-audit.jsonl"


def close_phantom(targets, cluster, reason, by, log=print):
    """Closes the one open cluster `cluster` (its id) as `fixed` with `fixed_at="close-phantom"`,
    on a human's written `reason` and name `by`: the manual end of D18's label. It refuses without
    either, and a cluster that is unknown, is not open, or has a hit that carries no
    `suspected_interrupt` label (`interrupts` is not `hits`: a real timeout, a divergence, a
    hit from before the label). Nothing runs it but the CLI; no farm or nightly path does. The row
    (who, the OS user, when, why, the cluster and its labelled evidence) goes to the target's audit
    log (`triage-audit.jsonl`, written and synced first), then the state is saved, then a vector
    moves to regress/fixed/ as `recheck` does (triage's `reconcile` finishes a move that dies).
    The `fixed_stamp` is empty: no code fixed anything, so any run triaged later that finds the
    class again reopens it. Private-route targets are never named (PLAN \u00a77.5). True when closed;
    a refusal changes nothing."""
    reason, by = (reason or "").strip(), (by or "").strip()
    if not reason or not by:
        log("node-fuzz close-phantom refused: a written reason and who is closing it are required")
        return False
    fdir = fuzz_dir()
    with triage_lock(fdir):
        for t in targets:
            if t.kind == "selftest":
                continue
            try:
                state = load_triage(fdir, t.name)
            except ValueError:
                log("node-fuzz close-phantom refused: a target's triage state is unreadable")
                return False
            c = state["clusters"].get(cluster)
            if c is None:
                continue
            name = "a private-route cluster" if t.private else f"{t.name} {cluster} {c['kind']} [{c['class']}]"
            if c["state"] != "open":
                why = "it is not open"
            elif not c["hits"] or c.get("interrupts") != c["hits"]:
                why = f"only {c.get('interrupts', 0)} of its {c['hits']} hits carry a suspected_interrupt label"
            else:
                why = None
            if why:
                log(f"node-fuzz close-phantom refused: {name}: {why}")
                return False
            append_row(os.path.join(fdir, t.name), {
                "event": "close-phantom", "at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "by": by, "user": getpass.getuser(), "reason": reason, "target": t.name, "cluster": cluster,
                "kind": c["kind"], "class": c["class"], "pins": c["pins"], "hits": c["hits"],
                "first_seen": c["first_seen"], "last_seen": c.get("last_seen"),
                "interrupt_evidence": c.get("interrupt_evidence", []), "rule": "BL-173 D18"}, AUDIT_NAME)
            c.update(state="fixed", fixed_stamp="", fixed_at="close-phantom", closed_by=by, closed_reason=reason)
            write_private(triage_path(fdir, t.name), json.dumps(state, indent=1, sort_keys=True))
            base = os.path.join(fdir, t.name, "regress")
            if c.get("vector"):
                with contextlib.suppress(OSError):
                    private_mkdir(base, os.path.join(base, "fixed"))
                    os.replace(os.path.join(base, "open", c["vector"]), os.path.join(base, "fixed", c["vector"]))
            log(f"node-fuzz close-phantom closed: {name} hits={c['hits']} by={by}")
            return True
    log("node-fuzz close-phantom refused: no such cluster")
    return False


# ---------------------------------------------------------------------------------------------
# Smoke


def smoke(targets, parallel, selftest=True, triage=True, only=None):
    """Every leg with a smoke budget, through run.sh, on a fixed CPU budget; then triage of any
    findings. Prints run.sh's line for each public-route leg and one aggregate line (the lines of
    private-route legs go to `fuzz/.smoke/<time>.log` only: PLAN §7.5); True when all passed."""
    fdir = fuzz_dir()
    started = time.time()
    unknown = sorted(set(only or ()) - {t.name for t in targets})
    selected = [(t, p) for t, p in legs(targets, "smoke") if not only or t.name in only]
    if unknown or not selected:
        print(f"node-fuzz smoke legs=0 unknown_targets={len(unknown)} status=fail "
              "(the selection names no smoke leg)", flush=True)
        return False
    budget = sum(t.smoke for t, _ in selected)
    selftest_ok = "skipped"
    if selftest:
        r = subprocess.run([RUN_SH, "--selftest"], capture_output=True, text=True)
        selftest_ok = "ok" if r.returncode == 0 and "node-fuzz selftest: ok" in r.stdout else "fail"
        if selftest_ok == "fail":
            diag = [x for x in (r.stdout + r.stderr).splitlines() if x.startswith("run.sh: ")]
            print(diag[-1] if diag else "run.sh --selftest failed", flush=True)
    done, private = [], {t.name for t in targets if t.private}

    def one(leg):
        t, p = leg
        return run_leg(t.name, p, ["--cpu-hours", f"{t.smoke / 3600:.6f}", "--jobs", "1",
                                   "--corpus-sample", str(t.smoke_corpus_max),
                                   "--corpus-sample-bytes", str(t.smoke_corpus_max_bytes)])

    with ThreadPoolExecutor(max_workers=max(1, parallel)) as pool:
        for leg in pool.map(one, selected):
            if leg.target not in private:
                print(leg.line, flush=True)
            done.append(leg)
    logs = os.path.join(fdir, ".smoke")
    private_mkdir(logs)
    write_private(os.path.join(logs, run_stamp() + ".log"), "".join(leg.line + "\n" for leg in done))
    tri = None
    if triage and any(not leg.passed for leg in done):
        tri = run_triage(targets, only={leg.target for leg in done if not leg.passed})
        print(tri.summary(), flush=True)
    v = [leg.verdict for leg in done]
    passed = sum(leg.passed for leg in done)
    triage_errors = tri.stats["errors"] if tri is not None else 0
    ok = passed == len(done) and selftest_ok != "fail" and not triage_errors
    counts = " ".join(f"{k}={total(v, k)}" for k in FINDING_KEYS)
    # Why the legs failed, counted by reason (run.sh's verdict says, BL-333); never which legs.
    reasons = collections.Counter(leg.verdict.get("reason") or f"no verdict, exit {leg.code}"
                                  for leg in done if not leg.passed)
    why = f" ({'; '.join(f'{r}: {n}' for r, n in sorted(reasons.items()))})" if reasons else ""
    print(
        f"node-fuzz smoke legs={len(done)} private_legs={sum(leg.target in private for leg in done)} "
        f"pass={passed} fail={len(done) - passed} "
        f"cpu_floor_seconds={budget:g} cpu_seconds={total(v, 'cpu_hours') * 3600:.0f} "
        f"wall_seconds={time.time() - started:.0f} "
        f"{counts} triage_errors={triage_errors} selftest={selftest_ok} status={'pass' if ok else 'fail'}{why}",
        flush=True,
    )
    if tri is not None:
        print("\n".join(status_lines(fdir, targets)[0]), flush=True)
    return ok


# ---------------------------------------------------------------------------------------------
# The harness smoke (HX1 X4, designs/CH1-node.md §11)


def harness_smoke(bin_dir, sequences, seed, out, jobs, random_delivery, runner=subprocess.run):
    """`sequences` random operation sequences judged by Dash Core through the sb4-core-scenarios
    harness (`regen.py --random`, outside run.sh: no libFuzzer, and the sequences come from a
    seed, not from inputs), then the same seed again with `--check`, which must reproduce every
    file. Public route: nothing here is private. Prints one line, then the sequences that failed,
    differed or hit a known finding; True when none failed or differed. A sequence the harness
    fails on (Core asserting, say) is a finding, its stderr beside the outputs in `out` as
    <name>.err; one failing the way a recorded finding does (sb4-core-scenarios/findings/: same
    kind, and its terminal stderr line contains the signature) counts as known and does not
    fail the smoke."""
    started = time.time()
    cmd = [sys.executable, "-I", REGEN, "--bin", bin_dir, "--random", str(sequences), "--seed", str(seed),
           "--random-out", out, "--jobs", str(jobs)] + (["--random-delivery"] if random_delivery else [])
    summaries, failures = [], []
    for phase, extra in (("generate", []), ("check", ["--check"])):
        r = runner(cmd + extra, capture_output=True, text=True)
        failures += [f"{phase}: {x}" for x in r.stdout.splitlines() if x.startswith(("FAIL ", "KNOWN "))]
        lines = [x for x in r.stdout.splitlines() if x.startswith("random seed=")]
        if not lines:
            diag = (r.stdout + r.stderr).strip().splitlines()
            print(f"node-fuzz harness-smoke status=fail (regen.py exited {r.returncode}: "
                  f"{diag[-1] if diag else 'no output'})", flush=True)
            return False
        summaries.append(dict(kv.split("=", 1) for kv in lines[-1].split()[1:]))
    gen, check = summaries
    ok = gen["fail"] == check["fail"] == check["differ"] == "0"
    known = gen.get("known", "0")
    print(f"node-fuzz harness-smoke sequences={sequences} seed={seed} delivery={gen['delivery']} "
          f"ok={gen['ok']} cut={gen['cut']} known={known} fail={gen['fail']} differ={check['differ']} "
          f"wall_seconds={time.time() - started:.0f} status={'pass' if ok else 'fail'}", flush=True)
    for line in failures:
        print(f"  {line}", flush=True)
    return ok


# ---------------------------------------------------------------------------------------------
# Bench windows


def parse_windows(text):
    """`<start> <end> [id]` rows (ISO 8601; a time without an offset is UTC) as (start, end)
    epoch pairs; malformed rows raise ValueError, so a typo never silently frees a window."""
    out = []
    for n, row in enumerate(text.splitlines(), 1):
        row = row.split("#", 1)[0].strip()
        if not row:
            continue
        parts = row.split()
        if len(parts) not in (2, 3):
            raise ValueError(f"windows line {n}: expected `<start> <end> [id]`")
        start, end = (datetime.fromisoformat(p) for p in parts[:2])
        start, end = (d if d.tzinfo else d.replace(tzinfo=timezone.utc) for d in (start, end))
        if end <= start:
            raise ValueError(f"windows line {n}: the window ends before it starts")
        out.append((start.timestamp(), end.timestamp()))
    return out


def next_window(windows, now, horizon):
    """The first window overlapping [now, now + horizon): (start, end), or None."""
    hits = [w for w in windows if w[0] < now + horizon and w[1] > now]
    return min(hits) if hits else None


def live_bench_pids(proc="/proc"):
    """Processes carrying node-bench's run mark (a benchmark is running now)."""
    pids, needle = [], (BENCH_MARK_VAR + "=").encode()
    for pid in os.listdir(proc):
        if not pid.isdigit():
            continue
        with contextlib.suppress(OSError):
            with open(os.path.join(proc, pid, "environ"), "rb") as f:
                if any(e.startswith(needle) for e in f.read().split(b"\0")):
                    pids.append(int(pid))
    return pids


class Bench:
    """Whether the farm must yield: a reserved window overlaps the horizon, or node-bench is
    running."""

    def __init__(self, windows_path, proc="/proc", clock=time.time):
        self.path, self.proc, self.clock = windows_path, proc, clock

    def windows(self):
        try:
            text = open(self.path).read()
        except FileNotFoundError:
            return []
        return parse_windows(text)

    def blocked(self, horizon):
        """None when the farm may run for `horizon` seconds, otherwise the reason and, for a
        window, how many seconds until it starts."""
        now = self.clock()
        try:
            w = next_window(self.windows(), now, horizon)
        except (OSError, ValueError) as e:
            return ("the bench windows file is unreadable: " + str(e).split(":")[0], 0)
        if w:
            return ("a reserved bench window", max(0.0, w[0] - now))
        if live_bench_pids(self.proc):
            return ("node-bench is running", 0)
        return None


# ---------------------------------------------------------------------------------------------
# Nightly farm


def plan_legs(targets, cpu_hours):
    """Each farmed leg's share of `cpu_hours`, by its target's nightly weight."""
    farmed = legs(targets, "nightly")
    total = sum(t.nightly for t, _ in farmed)
    return {f"{t.name}@{p}": round(cpu_hours * t.nightly / total, 4) for t, p in farmed} if total else {}


def credit_by_leg(records):
    """Each leg's CPU-hours that count toward its share: those of its clean slices (passed or
    interrupted) since its last finding. A finding voids the leg's earlier credit, so after the
    fix the leg runs its whole share afresh (CS0 §9.3); failed and lost slices earn none. The
    ledger keeps every slice, so totals still report all the time spent. A corpus minimisation
    (`kind` cmin, BL-841) counts in the cap but earns no credit: it fuzzes nothing."""
    credit = {}
    for r in records:
        if r["status"] == "finding":
            credit[r["leg"]] = 0.0
        elif r["status"] in CLEAN and r.get("kind") != CMIN:
            credit[r["leg"]] = credit.get(r["leg"], 0.0) + r["cpu_hours"]
    return credit


def next_leg(plan, spent, running):
    """The leg furthest below its budget that is not running, or None."""
    left = {leg: budget - spent.get(leg, 0) for leg, budget in plan.items()}
    open_legs = [leg for leg, rest in left.items() if rest > DONE_EPS and leg not in running]
    return max(open_legs, key=lambda leg: (left[leg] / plan[leg], leg)) if open_legs else None


def launch_shares(plan, cpu_hours, selected):
    """Each selected leg's share for a launch: the night's cap split by the plan's weights among
    the selected legs only (DEC-186 (4)); without --only, the plan's own shares. Nothing is
    redistributed: a leg that stops early leaves its share unspent."""
    weight = sum(plan[leg] for leg in selected)
    return {leg: cpu_hours * plan[leg] / weight for leg in selected} if weight else {}


# ---------------------------------------------------------------------------------------------
# Live plateau suspicion (DEC-184 (1), DEC-186 (1)): a stop heuristic that saves CPU. It never
# certifies anything; only `certify` does.

# A leg is suspected of a plateau when, over its last window of fuzzing time, its libFuzzer
# processes gained no cov and at most this fraction of ft.
PLATEAU_FT = 0.001
MIN_PLATEAU_MINUTES = 15
COVERAGE = "coverage.jsonl"
COV_STATUS = re.compile(rb"\bcov: (\d+) ft: (\d+)")


class LogScan:
    """What a run's libFuzzer processes report, read incrementally from their logs
    (`worker_logs`). Each process is measured against itself from its own `INITED` line (its
    corpus loaded), and the run's growth is the sum of the processes' growths: a process that lags
    another, or restarts on a smaller corpus, still shows its growth (Sol r1 B1), and a corpus
    load is never growth."""

    def __init__(self):
        self.files = {}  # path -> [offset, cov and ft at INITED or None, latest cov and ft]

    def __call__(self, run_dir):
        """(cov growth, ft growth, highest ft, epoch). The epoch names the latest segment and its
        processes, and is None until every one of them has INITED; an interval whose two ends
        do not share one epoch spans a corpus load and is not fuzzing time (Sol r1 B2)."""
        dcov = dft = top = 0
        epoch = None
        for seg in segments(run_dir):
            paths, ready = worker_logs(seg), True
            for path in paths:
                state = self.files.setdefault(path, [0, None, None])
                with contextlib.suppress(OSError), open(path, "rb") as fh:
                    fh.seek(state[0])
                    data = fh.read()
                    end = data.rfind(b"\n") + 1  # a line still being written waits
                    for line in data[:end].splitlines():
                        m = COV_STATUS.search(line)
                        if not m:
                            continue
                        now = (int(m.group(1)), int(m.group(2)))
                        if state[1] is None and b"INITED" in line:
                            state[1] = now
                        if state[1] is not None:
                            state[2] = (max(state[2][0], now[0]), max(state[2][1], now[1])) if state[2] else now
                    state[0] += end
                if state[1] is None:
                    ready = False
                    continue
                dcov += state[2][0] - state[1][0]
                dft += state[2][1] - state[1][1]
                top = max(top, state[2][1])
            epoch = (seg, tuple(paths)) if ready and paths else None
        return dcov, dft, top, epoch

    def forget(self, run_dir):
        prefix = os.path.join(run_dir, "")
        self.files = {p: v for p, v in self.files.items() if not p.startswith(prefix)}


FEATURE_SET_SIZE = 1 << 21  # libFuzzer's kFeatureSetSize: its feature ids fold modulo it
UNION_SIGNAL = "union-ft"  # a coverage-history row's signal under shards (BL-841b)


class UnionScan:
    """A shard run's plateau signal (BL-841b §2.2): the union of its workers' feature sets. Each
    worker writes, for every input it adds to its corpus (loaded, reloaded or found), the features
    that were new to it (`-features_dir`, `features/segment-<s>/<k>/<sha1>`), so the union of every
    worker's files is the run's feature set, as one process would count it. Read incrementally: a
    file is read once its size is a whole number of uint32s and unchanged since the last call (one
    still being written waits a call), and only once. Returns LogScan's tuple: no cov growth (the
    union has no PC count), the union's growth since U₀, its size, and LogScan's epoch for the
    latest segment. U₀ is the union of the files present when the run's first epoch was seen (the
    shard loads); until all of them are read the epoch is None, so no interval is fuzzing time and
    no load is ever taken for growth. One renamed (a REDUCE) or removed before it was read counts
    as read, so the epoch still comes (BL-841b r1 M2); under its new name it is growth, which only
    delays a stop. Once U₀ is read, the growth is reported even while a later segment loads (its
    epoch None), so a sample's growth is never counted twice (r1 L1)."""

    def __init__(self):
        self.read = {}  # run_dir -> {path: size seen last, or None once read}
        self.union = {}  # run_dir -> the features read so far
        self.base = {}  # run_dir -> [U₀'s paths not read yet, U₀'s features]
        self.logs = LogScan()

    def __call__(self, run_dir):
        epoch = self.logs(run_dir)[3]
        seen = self.read.setdefault(run_dir, {})
        union = self.union.setdefault(run_dir, set())
        root = os.path.join(run_dir, "features")
        paths = [os.path.join(root, seg, k, f) for seg in listdir_or_empty(root)
                 for k in listdir_or_empty(os.path.join(root, seg)) for f in listdir_or_empty(os.path.join(root, seg, k))]
        if epoch is not None and run_dir not in self.base:
            # Every file read so far is one of U₀'s: U₀ is what is read, and what is still to read.
            self.base[run_dir] = [{p for p in paths if seen.get(p, 0) is not None}, set(union)]
        base = self.base.get(run_dir)
        if base:
            base[0].intersection_update(paths)
        for path in paths:
            if path in seen and seen[path] is None:
                continue
            try:
                size = os.stat(path).st_size
            except OSError:
                continue
            if size % 4 or seen.get(path) != size:
                seen[path] = size
                continue
            try:
                data = open(path, "rb").read(size)
            except OSError:
                continue
            got = {f % FEATURE_SET_SIZE for f in struct.unpack(f"<{size // 4}I", data)}
            union |= got
            seen[path] = None
            if base and path in base[0]:
                base[0].discard(path)
                base[1] |= got
        if base is None or base[0]:
            return 0, 0, len(union), None
        return 0, len(union) - len(base[1]), len(union), epoch

    def forget(self, run_dir):
        for d in (self.read, self.union, self.base):
            d.pop(run_dir, None)
        self.logs.forget(run_dir)


def suspected(history, window_s, rule=Plateau(), union=False):
    """Whether a leg is suspected of a plateau: its `history` is (fuzzing seconds, cov growth, ft
    growth, highest ft) rows, each cumulative over the leg's processes, starting at 0 seconds.
    Over the last `window_s` seconds of that fuzzing time (a full window must have run) it gained
    no cov (at most the target's `rule.cov_slack`) and, unless its `rule` leaves ft out, at most
    PLATEAU_FT of the ft it had at the window's start, which is the last row at or before the
    window's nominal start. Returns {window_minutes, fuzz_hours, ft, ft_gain}, or None. A stop
    heuristic only: certification is `certify`'s (DEC-186). A `union` history (a shard leg's,
    `UnionScan`) has no cov: a new PC is always a new feature, so its ft test is the whole test, and
    a rule that leaves ft out never suspects it (BL-841b §2.3)."""
    if not history or history[-1][0] < window_s or (union and not rule.ft):
        return None
    end_s, cov, ft_gain, ft = history[-1]
    _, cov0, ft_gain0, ft0 = history[bisect.bisect_right(history, end_s - window_s, key=lambda h: h[0]) - 1]
    if (ft0 == 0 or (not union and cov - cov0 > rule.cov_slack)
            or (rule.ft and ft_gain - ft_gain0 > PLATEAU_FT * ft0)):
        return None
    return {"window_minutes": window_s / 60, "fuzz_hours": round(end_s / 3600, 4), "ft": ft,
            "ft_gain": ft_gain - ft_gain0}


HISTORY_KEYS = {"leg", "fuzz_s", "cov_gain", "ft_gain", "ft"}  # and `signal` on a shard leg's rows


def load_coverage(state_dir, legs_):
    """Each leg's coverage history (`suspected`) and its recorded suspicion, from the night's
    `coverage.jsonl`. Rows: `{leg, fuzz_s, cov_gain, ft_gain, ft}` (cumulative; a leg's history
    starts at 0 seconds), `{leg, suspected: {…}}` when the farm stopped the leg, and
    `{leg, reset: true}` after a finding, which starts the leg's history afresh (CS0 §9.3). A shard
    leg's rows also carry `signal: union-ft` (BL-841b), and a leg's history holds one signal (each
    leg's is the third value returned: `union-ft` or None) unless a reset separates them. A missing
    file is no history; a malformed row, a history without its 0-second start, one that goes
    backwards or mixes signals is FarmStateError; a torn final row is ignored (`repair_ledger`)."""
    try:
        data = open(os.path.join(state_dir, COVERAGE), "rb").read()
    except FileNotFoundError:
        return {}, {}, {}
    history, marks, signals = {}, {}, {}
    for raw in data.split(b"\n")[:-1]:
        try:
            row = json.loads(raw)
        except ValueError:
            raise FarmStateError("the night's coverage history has a malformed row") from None
        leg = row.get("leg") if isinstance(row, dict) else None
        if not isinstance(leg, str) or leg not in legs_:
            raise FarmStateError("the night's coverage history has an invalid row")
        if row == {"leg": leg, "reset": True}:
            history[leg] = []
            marks.pop(leg, None)
            signals.pop(leg, None)
            continue
        if set(row) == {"leg", "suspected"} and suspicion_problem(row["suspected"]) is None:
            marks[leg] = row["suspected"]
            continue
        signal_ = row.get("signal")
        if (set(row) - {"signal"} != HISTORY_KEYS or signal_ not in (None, UNION_SIGNAL) or not finite(row["fuzz_s"])
                or not all(is_count(row[k]) for k in ("cov_gain", "ft_gain", "ft"))):
            raise FarmStateError("the night's coverage history has an invalid row")
        rows = history.setdefault(leg, [])
        if rows and signals.get(leg) != signal_:
            raise FarmStateError("the night's coverage history mixes signals without a reset")
        signals[leg] = signal_
        new = (row["fuzz_s"], row["cov_gain"], row["ft_gain"], row["ft"])
        if (not rows and new[0] != 0) or (rows and any(a < b for a, b in zip(new, rows[-1]))):
            raise FarmStateError("the night's coverage history has no start or goes backwards")
        rows.append(new)
    return history, marks, signals


def suspicion_problem(p):
    keys = ("window_minutes", "fuzz_hours", "ft", "ft_gain")
    if not isinstance(p, dict) or set(p) != set(keys) or not all(finite(p[k]) for k in keys):
        return "a malformed plateau suspicion"
    return None


def legs_named(legs_, targets):
    """The legs whose target is positively public in the current registry; every other leg,
    a private-route or removed or renamed target's, is counted, never named (Sol r1 M3)."""
    public = {t.name for t in targets if t.route == "public"}
    return [leg for leg in legs_ if leg.split("@")[0] in public]


# ---------------------------------------------------------------------------------------------
# Certification (DEC-186 (2)): `certify <target>@<pin>`, the only path that certifies a leg

CERTIFY_SECONDS = 3600  # of the process's CPU time after INITED
CERTIFY_WALL_HOURS = 12.0  # the default wall-clock cap on a window (`starved` when it is hit)
CERTIFY_EXECS = 10**6  # the default floor on executions in the window (targets.toml min_certify_execs)
# The longest a window may run on after its stop (run.sh's $stop_grace): a later exit fails the
# run, killed or not, whatever its status (DEC-186 amendment 3 (a), Sol r4 B1).
STOP_GRACE = 1500
CERTIFICATE = "certificate.json"
INITED_EXECS = re.compile(rb"^#(\d+)\s+INITED\b")
# The window log's grammar (DEC-186 amendment 3 (c)), libFuzzer 0.4.13's own formats. A status
# line (PrintStats): `#<execs>\t<label> cov: … ft: … corp: <n>[/<size>] [focus: …] [lim: …]
# exec/s: … rss: <n>Mb`; a NEW or REDUCE line (PrintStatusForNewUnit) adds ` L: <size>/<max> MS:
# <n> ` and min(n, 10) mutator names, each followed by `-`, then maybe ` DE: ` and up to 10
# dictionary words, each quoted by PrintASCII (\\, \", printable bytes, else \ooo) and followed
# by `-`.
STATUS_FORM = re.compile(
    rb"#(\d+)\t(NEW   |REDUCE|pulse |RELOAD|INITED|DONE  ) cov: (\d+) ft: (\d+) corp: \d+(?:/\d+(?:b|Kb|Mb))?"
    rb"(?: focus: \d+)?(?: lim: \d+)? exec/s: \d+ rss: \d+Mb(.*)")
ASCII_WORD = rb'"(?:[ !#-\[\]-~]|\\\\|\\"|\\[0-7]{3})*"'
UNIT_TAIL = re.compile(rb" L: \d+/\d+ MS: (\d+) ((?:[A-Za-z]+-)*)(?: DE: ((?:" + ASCII_WORD + rb"-)+))?")
# The other lines of a fuzzing loop: new functions of a discovery (-print_funcs), a slow unit, and
# the harness's one line of a clean run (node-fuzz `Harness::run`: its count of the inputs of the
# declared strictness, at each power of two). libFuzzer prints the artifact prefix as it is, so
# the prefix runs to the fixed `'; Test unit written to ` (a sink path may hold a `'`; Sol r4 m1).
LOOP_LINES = [re.compile(x) for x in (rb"\tNEW_FUNC\[\d+/\d+\]: \S.*", rb"Slowest unit: \d+ s:",
                                      rb"artifact_prefix='.*'; Test unit written to \S.*",
                                      rb"Base64: [A-Za-z0-9+/]*=*",
                                      rb"[a-z0-9_]+: \d+ inputs Core reads with a flag byte the node refuses \(A-12, DEC-98\)")]
# After the DONE line (Loop's end, then FuzzerDriver's): a recommended dictionary when there is one,
# `Done N runs …`, and the five final stats (-print_final_stats), each once, in this order.
DICTIONARY = (b"###### Recommended dictionary. ######", re.compile(ASCII_WORD + rb" # Uses: \d+"),
              b"###### End of recommended dictionary. ######")
DONE_RUNS = re.compile(rb"Done (\d+) runs in \d+ second\(s\)")
FINAL_STATS = [re.compile(x) for x in (
    rb"stat::number_of_executed_units: (\d+)", rb"stat::average_exec_per_sec:     (\d+)",
    rb"stat::new_units_added:          (\d+)", rb"stat::slowest_unit_time_sec:    (\d+)",
    rb"stat::peak_rss_mb:              (\d+)")]
MAX_PRINTED = 10  # libFuzzer's kMaxMutationsToPrint
# A certify run's segments (run.sh): the merge, the replay before the window, the window, the
# replay after it.
REPLAY_BEFORE, WINDOW, REPLAY_AFTER = "segment-2", "segment-3", "segment-4"
# window.json as run.sh's driver writes it: each key and the test its value must pass.
WINDOW_KEYS = {
    "target": lambda x: isinstance(x, str), "pin": lambda x: isinstance(x, str),
    "run": lambda x: isinstance(x, str), "cpu_window": lambda x: is_count(x, 1),
    "wall_cap": lambda x: is_count(x, 1), "elapsed": lambda x: finite(x),
    "load_seconds": lambda x: x is None or finite(x), "wall_seconds": lambda x: x is None or finite(x),
    "cpu_at_inited": lambda x: x is None or finite(x), "cpu_at_stop": lambda x: x is None or finite(x),
    "stopped_by": lambda x: x in (None, "cpu-window", "wall-cap"), "killed": lambda x: isinstance(x, bool),
    "status": lambda x: isinstance(x, int) and not isinstance(x, bool),
    "stop_to_exit": lambda x: x is None or finite(x),
}
# segment.json, a farm segment's record from the same driver (BL-228; BL-820, BL-826): window.json's
# keys with the INITED fields null (a segment's allotment counts from its start), the farm's stop,
# the source of its cpu file and the CPU of each part, the processes it killed after libFuzzer's
# exit, and each fuzzing process's INITED. Only the selftest reads it whole; run.sh reads stopped_by.
PROCESS_INITED_KEYS = {"log": lambda x: isinstance(x, str), "pid": lambda x: x is None or is_count(x, 1),
                       "seconds_to_inited": lambda x: x is None or finite(x),
                       "cpu_at_inited": lambda x: x is None or finite(x)}
SEGMENT_KEYS = {
    **WINDOW_KEYS,
    **dict.fromkeys(("load_seconds", "wall_seconds", "cpu_at_inited"), lambda x: x is None),
    "stopped_by": lambda x: x in (None, "cpu-window", "wall-cap", "run-stop"),
    "cpu_source": lambda x: x in ("wait4", "tree"), "cpu_wait4": lambda x: finite(x),
    "cpu_adopted": lambda x: finite(x), "cpu_tree": lambda x: finite(x),
    "orphans_killed": is_count,
    "process_inited": lambda x: isinstance(x, list) and all(
        isinstance(p, dict) and set(p) - {"loaded"} == set(PROCESS_INITED_KEYS)
        and all(ok(p[k]) for k, ok in PROCESS_INITED_KEYS.items())
        and (p.get("loaded") is None or is_count(p["loaded"], 1))
        and (p["seconds_to_inited"] is not None or p["pid"] is None)
        and (p["pid"] is None) == (p["cpu_at_inited"] is None) for p in x),
}
# The flags libFuzzer's -fork mode gives its parent and children. The harness's start-up check
# (node-sink `check_fuzz_args`) refuses them all but `stop_file` (which ends a certify window), and
# refuses the children's temporary first corpus directory, so certify runs one process, which is
# a true union by itself.
FORK_FLAGS = ("fork", "reload", "print_funcs", "stop_file", "features_dir", "seed_inputs")
SINK_SOURCE = os.path.join(ROOT, "node-sink", "src", "sink.rs")


def certify_engine(sink_source):
    """("single", 1) while the harness refuses libFuzzer's fork-mode flags; ("fork", None) once
    its ALLOWED_FLAGS admit them all, which `certify` refuses until a fork engine (and its
    sink, oracle and artifact behaviour under fork) is built and verified."""
    m = re.search(r"const ALLOWED_FLAGS: &\[&str\] = &\[(.*?)\];", sink_source, re.S)
    flags = set(re.findall(r'"([a-z_]+)"', m.group(1))) if m else set()
    return ("fork", None) if set(FORK_FLAGS) <= flags else ("single", 1)


def log_lines(path):
    with contextlib.suppress(OSError):
        return open(path, "rb").read().splitlines()
    return None


def replay_cov(run_dir, seg):
    """The (cov, ft) of a replay pass (`-runs=0` over the working corpus): its one INITED line's, or
    None without exactly one, or with one that is not a whole status line."""
    lines = log_lines(os.path.join(run_dir, "logs", seg, "fuzz.out")) or []
    found = [STATUS_FORM.fullmatch(line) for line in lines if INITED_EXECS.match(line)]
    if len(found) != 1 or not found[0] or found[0].group(5):
        return None
    return int(found[0].group(3)), int(found[0].group(4))


def run_identity(run_dir):
    """(target, pin, run) as the run's place in the tree says: fuzz/<target>/runs/<time>-<pin>-…"""
    run = os.path.basename(run_dir)
    parts = run.split("-")
    return os.path.basename(os.path.dirname(os.path.dirname(run_dir))), parts[1] if len(parts) >= 3 else "", run


def run_findings(run_dir):
    """What a run left, read by run.sh's rules: its divergences, its crashes (saved, or reported
    by libFuzzer without an input), sink failures and refusals in its logs."""
    evidence = log_evidence(run_dir)
    divergences = listdir_or_empty(os.path.join(run_dir, "divergences"))
    return {"divergences": sum(SHA_NAME.fullmatch(n) is not None for n in divergences),
            "crashes": len([f for f in crash_files(run_dir) if f != "minimized"]) + len(evidence["unsaved_crashes"]),
            "sink_failures": evidence["sink_failures"], "refused": evidence["refused"]}


def window_log(data):
    """The window log (`data`, its bytes) read by its grammar (DEC-186 amendment 3 (c)): a dict of
    `inited` and `done` ((cov, ft) of its one INITED line and of its DONE line), `inited_execs`,
    `executed` (its final stats' count), `units_added` (theirs), `unit_lines` (its NEW and REDUCE
    lines after INITED), `new_lines` (its lines after INITED that show a discovery: a NEW or
    NEW_FUNC line, or a status line whose cov or ft is above every earlier line's, INITED's
    included, as libFuzzer labels a discovery REDUCE when its parent input was ever reduced, and
    RELOAD when it re-reads the corpus) and `problems`. A problem is any line after INITED that is
    torn (no newline), unknown or out of place, counts that go backwards, a DONE line, `Done N
    runs` or final stat missing or repeated, content after the final stats, final counts that
    disagree, or a DONE line above every earlier line. Lines before INITED (the corpus load) are
    not read."""
    lines = data.split(b"\n")
    problems = [] if lines[-1] == b"" else ["the log ends in a torn line"]
    lines = lines[:-1]
    status = [STATUS_FORM.fullmatch(line) for line in lines]
    inits = [n for n, m in enumerate(status) if m and m.group(2) == b"INITED" and not m.group(5)]
    out = {"inited": None, "done": None, "inited_execs": None, "executed": None, "units_added": None,
           "unit_lines": 0, "new_lines": 0, "problems": problems}
    if len(inits) != 1:
        problems.append("no single complete INITED line")
        return out
    first = status[inits[0]]
    out["inited"], out["inited_execs"] = (int(first.group(3)), int(first.group(4))), int(first.group(1))
    last, best = (out["inited_execs"], *out["inited"]), out["inited"]
    phase, stat, unknown = "loop", 0, 0  # loop, done, dictionary, runs, stats, end
    finals = {}
    for line, m in zip(lines[inits[0] + 1:], status[inits[0] + 1:]):
        if phase == "loop" and m:
            label, counts = m.group(2).strip(), tuple(int(m.group(i)) for i in (1, 3, 4))
            if label == b"INITED" or any(x < y for x, y in zip(counts, last)):
                problems.append("a second INITED line, or counts that go backwards")
            last = counts
            if label in (b"NEW", b"REDUCE"):
                tail = UNIT_TAIL.fullmatch(m.group(5))
                if not tail or len(tail.group(2).split(b"-")) - 1 != min(int(tail.group(1)), MAX_PRINTED) or (
                        tail.group(3) and len(re.findall(ASCII_WORD + rb"-", tail.group(3))) > MAX_PRINTED):
                    problems.append("a malformed NEW or REDUCE line")
                out["unit_lines"] += 1
            elif m.group(5):
                problems.append("a status line with trailing content")
            if label == b"DONE":
                # Coverage and features grow only through a discovery, and libFuzzer prints a line
                # for each (ReportNewCoverage, RELOAD), so its DONE line repeats the highest cov
                # and ft printed since INITED (244 of 244 real single-process logs).
                if counts[1:] != best:
                    problems.append("the DONE line shows coverage no earlier line printed: a line was lost")
                out["done"], phase = counts[1:], "done"
                continue
            out["new_lines"] += label == b"NEW" or counts[1] > best[0] or counts[2] > best[1]
            best = (max(best[0], counts[1]), max(best[1], counts[2]))
        elif phase == "loop" and any(x.fullmatch(line) for x in LOOP_LINES):
            out["new_lines"] += line.startswith(b"\tNEW_FUNC")
        elif phase == "done" and line == DICTIONARY[0]:
            phase = "dictionary"
        elif phase == "dictionary" and (line == DICTIONARY[2] or DICTIONARY[1].fullmatch(line)):
            phase = "runs" if line == DICTIONARY[2] else phase
        elif phase in ("done", "runs") and DONE_RUNS.fullmatch(line):
            finals["runs"], phase = int(DONE_RUNS.fullmatch(line).group(1)), "stats"
        elif phase == "stats" and FINAL_STATS[stat].fullmatch(line):
            finals[stat] = int(FINAL_STATS[stat].fullmatch(line).group(1))
            stat += 1
            phase = "end" if stat == len(FINAL_STATS) else phase
        else:
            unknown += 1
    if unknown:
        problems.append(f"{unknown} unknown, torn or misplaced lines after INITED")
    if phase != "end":
        problems.append("no DONE line, `Done N runs` line and complete final stats")
    else:
        out["executed"], out["units_added"] = finals[0], finals[2]
        if not last[0] == finals["runs"] == finals[0]:
            problems.append("the DONE line, `Done N runs` and the final stats count different executions")
    out["problems"] = sorted(set(problems))
    return out


def certify_evidence(run_dir):
    """The evidence of a certify run, read from what run.sh left, failing closed: None when its
    verdict is missing, torn or not a certify run's (Sol r1 B4). Findings are the most of what the
    verdict says and what the run's divergences, crashes and logs show. Coverage is the replay
    passes' (DEC-186 amendment 2), never the window's: `baseline` before the window, `final` after
    it. The window log is read by its grammar (`window_log`, amendment 3 (c)): `log_problems`,
    `window_inited` (its INITED's cov and ft, the start of its discoveries), `window_done` (its DONE
    line's, measured in the process at its end: amendment 3 (b)), `new_lines`, and `unit_lines`
    (its NEW and REDUCE lines after INITED) against `units_added` (its final stats' count of the
    units those lines report: libFuzzer ignores its write errors, so a log that lost lines shows
    fewer). The window is window.json, kept whole, and its log's final executions less its INITED
    count. `source` is run.sh's record of the build, kept whole."""
    v = read_json(os.path.join(run_dir, "verdict.json"))
    if not isinstance(v, dict) or v.get("mode") != "certify" or not isinstance(v.get("certify"), dict):
        return None
    c = v["certify"]
    counts = ("divergences", "crashes", "sink_failures", "refused", "libfuzzer_status")
    if not all(isinstance(v.get(k), int) and not isinstance(v.get(k), bool) for k in counts):
        return None
    seg = os.path.join(run_dir, "logs", WINDOW)
    window = read_json(os.path.join(seg, "window.json"))
    logs = worker_logs(seg)
    data = None
    if len(logs) == 1 and logs[0].endswith("fuzz.out"):
        with contextlib.suppress(OSError):
            data = open(logs[0], "rb").read()
    log = window_log(data) if data is not None else None
    source = log_lines(os.path.join(run_dir, "source"))
    found = run_findings(run_dir)
    cpu0, cpu1 = (window.get("cpu_at_inited"), window.get("cpu_at_stop")) if isinstance(window, dict) else (None, None)
    # BL-812: a build other than the default adds its settings; the default's key set is unchanged,
    # so certificates written before it still match their evidence.
    build = {k: v[k] for k in ("coverage", "rust_trace_cmp") if k in v}
    default_build = build.get("coverage", "all") == "all" and build.get("rust_trace_cmp", True) is True
    return {**chain_evidence(run_dir, v.get("target")), **({} if default_build else {"build": build}),
        "target": v.get("target"), "pin": v.get("pin"), "run": v.get("run"),
        "rev": v.get("rust_dashcore"), "dirty": v.get("rust_dashcore_dirty"), "flavor": v.get("oracle_flavor"),
        "engine": c.get("engine"), "jobs": c.get("jobs"), "window_seconds": c.get("seconds"),
        "wall_cap": c.get("wall_cap"), "merge_status": c.get("merge_status"),
        "replay_before_status": c.get("replay_before_status"), "replay_after_status": c.get("replay_after_status"),
        "libfuzzer_status": v["libfuzzer_status"], "verdict_status": v.get("status"), "processes": len(logs),
        "baseline": replay_cov(run_dir, REPLAY_BEFORE), "final": replay_cov(run_dir, REPLAY_AFTER),
        "log_problems": log["problems"] if log else ["no window log"],
        "window_inited": log and log["inited"], "window_done": log and log["done"],
        "new_lines": log["new_lines"] if log and log["inited"] else None,
        "units_added": log and log["units_added"], "unit_lines": log["unit_lines"] if log and log["inited"] else None,
        "executions": log["executed"] - log["inited_execs"] if log and log["executed"] is not None else None,
        "cpu_seconds": round(cpu1 - cpu0, 3) if finite(cpu0) and finite(cpu1) else None,
        "window": window,
        "source": [line.decode(errors="replace") for line in source] if source is not None else None,
        **{k: max(v[k], found[k]) for k in found},
    }


def chain_evidence(run_dir, target):
    """A chain target's run's evidence beyond DEC-186's (O2b-4 §4.6), {} for any other target's run
    (whose evidence keeps its keys, F10). A chain target is one chain-encodings.toml names now, or
    one whose run recorded an encoding. `encoding` is the run's record of the encoding its binary
    was built for (None when absent or unreadable). A chain-v2 run's `census_floor` is its census
    floor gate's record: the sha256 of the floor copy it judged by, and the result it wrote."""
    lines = log_lines(os.path.join(run_dir, "encoding"))
    recorded = lines[0].decode(errors="replace") if lines is not None and len(lines) == 1 else None
    now = chain_encoding(target) if isinstance(target, str) else None
    if lines is None and not now:
        return {}
    ev = {"encoding": recorded}
    if "chain-v2" in (recorded, now):
        seg = os.path.join(run_dir, "logs", REPLAY_BEFORE)
        copy = os.path.join(seg, census_floor.FLOOR_COPY)
        result = read_json(os.path.join(seg, census_floor.RESULT))
        result = result if isinstance(result, dict) else {}
        sha = None
        with contextlib.suppress(OSError):
            sha = census_floor.sha256_file(copy)
        ev["census_floor"] = {"floor_sha256": sha, "floor_version": result.get("floor_version"),
                              "result": result.get("result"), "missing": result.get("missing")}
    return ev


def census_rejudge(run_dir, ev, encoding):
    """A chain target's run's census, judged again from what the run stored (O2b-4 §4.6), None for
    a run of no chain target (the registry's `encoding` empty and none in the evidence):
    {"encoding": the registry's, "failed": [...], "changed": [...]}. `failed` holds why the run's
    census does not pass by its own record: the gate's result is not `met`, the floor copy is not
    the one its binary was built with, or the census does not meet that copy. `changed` holds why
    it does not meet the target's floor now (CENSUS_DIR): a floor changed since the run, which the
    run cannot have been measured against."""
    if ev is None or (not encoding and "encoding" not in ev):
        return None
    census = {"encoding": encoding, "failed": [], "changed": []}
    if "chain-v2" not in (encoding, ev.get("encoding")):
        return census
    try:
        rejudge_into(census, run_dir, ev, encoding)
    except Exception as e:  # noqa: BLE001 (a run that cannot be judged again never certifies)
        census["failed"].append(f"the census could not be judged again ({type(e).__name__})")
    return census


def rejudge_into(census, run_dir, ev, encoding):
    """`census_rejudge`'s judgement of a chain-v2 run, into `census`'s `failed` and `changed`."""
    gate = ev.get("census_floor") or {}
    if gate.get("result") != "met":
        census["failed"].append(f"the census floor gate did not pass ({gate.get('result')})")
    built = log_lines(os.path.join(run_dir, census_floor.FLOOR_SHA))
    if not (built and len(built) == 1 and built[0].decode(errors="replace") == gate.get("floor_sha256")):
        census["failed"].append("the recorded floor is not the one the run was built with")
    seg = os.path.join(run_dir, "logs", REPLAY_BEFORE)
    for floor, into, what in ((os.path.join(seg, census_floor.FLOOR_COPY), census["failed"], "the floor the run recorded"),
                              (os.path.join(CENSUS_DIR, f"{ev['target']}.floor.json"), census["changed"],
                               "the target's floor now: the floor changed since the run")):
        result, report = census_floor.judge_run(run_dir, target=ev["target"], pin=ev["pin"], encoding=encoding,
                                                floor_path=floor)
        if result != "met":
            why = report["problems"] + report["missing"] + report["bounds"]
            into.append(f"the census does not meet {what} ({result}: {'; '.join(why[:3])})")


def window_problems(ev, identity):
    """How a certify run's evidence contradicts itself or its place in the tree (Sol r2 M3): the
    verdict, window.json and the source record must name the run's own target, pin and run; the
    window's facts must agree with the verdict's and with what stopped it. The source record is
    four lines: the commit, `clean` or `dirty` (as the verdict says), the run, and the sha256 of
    the binary the run executed."""
    target, pin, run = identity
    w, src = ev["window"], ev["source"]
    problems = [why for bad, why in (
        ((ev["target"], ev["pin"], ev["run"]) != identity, "the verdict is another leg's or run's"),
        (src is not None and (len(src) != 4 or src[0] != ev["rev"] or src[2] != run
                              or src[1] != ("clean" if ev["dirty"] is False else "dirty")
                              or not SHA_NAME.fullmatch(src[3])),
         "the source record is another run's or disagrees with the verdict"),
        (w is not None and not (isinstance(w, dict) and set(w) == set(WINDOW_KEYS)
                                and all(ok(w[k]) for k, ok in WINDOW_KEYS.items())), "window.json is malformed"),
    ) if bad]
    if problems or w is None:
        return problems
    stop_cpu = ev["cpu_seconds"]
    exit_status = w["status"] if w["status"] >= 0 else 128 - w["status"]
    return [why for bad, why in (
        ((w["target"], w["pin"], w["run"]) != identity, "window.json is another leg's or run's"),
        ((w["cpu_window"], w["wall_cap"]) != (ev["window_seconds"], ev["wall_cap"]),
         "window.json's window or wall cap differs from the verdict's"),
        (exit_status != ev["libfuzzer_status"], "window.json's exit status differs from the verdict's"),
        (w["killed"] and w["stopped_by"] is None, "a kill without a stop"),
        ((w["stopped_by"] is None) != (w["stop_to_exit"] is None),
         "a stop without its time to the exit, or that time without a stop"),
        (w["stopped_by"] == "cpu-window" and (
            stop_cpu is None or stop_cpu < w["cpu_window"] or w["elapsed"] >= w["wall_cap"]
            or w["wall_seconds"] is None),
         "stopped for its CPU window without the measurements that prove it"),
        (w["stopped_by"] == "wall-cap" and w["elapsed"] < w["wall_cap"], "stopped for the wall cap before it"),
        (stop_cpu is not None and w["wall_seconds"] is not None and stop_cpu > w["wall_seconds"] * 1.1 + 5,
         "more CPU time than wall time allows"),
        (ev["executions"] is not None and ev["executions"] < 0, "executions went backwards"),
    ) if bad]


def listed(target, targets):
    """The registry's entry for `target`, None for a target it no longer lists."""
    return next((t for t in targets if t.name == target), None)


def certify_floor(target, targets):
    """The executions a window of `target` needs: its min_certify_execs, else CERTIFY_EXECS (a
    target the registry no longer lists gets the default)."""
    t = listed(target, targets)
    return (t.min_certify_execs if t else 0) or CERTIFY_EXECS


def plateau_rule(target, targets):
    """The plateau tests' rule for `target` (`Plateau`; a target the registry no longer lists gets
    the default)."""
    t = listed(target, targets)
    return t.plateau if t else Plateau()


def registry_encoding(target, targets):
    """`target`'s encoding in the registry ("" for a target that is no chain target, or that the
    registry no longer lists)."""
    t = listed(target, targets)
    return t.encoding if t else ""


def replay_rule(target, targets):
    """`certify`'s replay comparisons' rule for `target` (`Replay`; the default for a target the
    registry no longer lists)."""
    t = listed(target, targets)
    return t.replay if t else Replay()


def judge(ev, floor=CERTIFY_EXECS, identity=None, rule=Plateau(), replay=Replay(), census=None):
    """A certify run's status, derived from its evidence and nothing else (DEC-186: never a
    stored boolean): (status, reasons). Statuses, the first that applies: `no-evidence`,
    `finding`, `invalid` (evidence that contradicts itself or its run's place, `identity`, in the
    tree), `failed` (not a clean, complete, single-process window between two replays, ended at
    its stop file with exit 0: a SIGINT or a kill never certifies), `starved` (the wall-clock cap
    stopped it before its CPU window: rerun it), `failed` again (a window log that breaks its
    grammar or lost lines, or replays that do not reproduce), `growing` (the window printed a
    discovery, its DONE line's cov differs from its INITED's or its ft is above INITED's by more
    than 0.1 % (DEC-186 amendment 3 (b)), or the replays' coverage differs), `insufficient-progress`
    (fewer than `floor` executions in the window), `uncertified` (a clean flat run that cannot
    certify: a window under 3600 CPU seconds, a tree dirty at build time, no source record or
    known commit, or the plain oracle), `certified`. The two replays run in processes of their
    own, so the target's `replay` rule (`Replay`, DEC-271) lets them differ, both ways, by its
    `cov_slack` and `ft_allowance` (never under 0.1 %): the replay after the window must not be
    lower than the one before it by more (by default any cov, or ft by more than 0.1 %). Its `rule` (`Plateau`,
    DEC-260) lets the window's DONE line's cov differ from its INITED's by its `cov_slack`, and,
    when it leaves ft out, drops the growth tests' ft comparisons and the discovery lines (cov
    found in the window is then bounded by DONE against INITED); the replay after the window
    still may not lose ft, which is reproducibility, not growth. Loads of one corpus
    vary by a few features (a smoke's replays: ft 3927-3930; its windows' INITED differed from
    their replays by ft 2, then cov 1), so the window's INITED is evidence, not a test: the
    leg's lock and the binary's copy make the window's corpus and binary the replays'. A chain
    target's run is also judged by its `census` (`census_rejudge`, O2b-4 §4.6): evidence of
    another encoding than the registry's is `invalid`, a census that fails by the run's own record
    is `failed`, and one that fails only against the target's floor now is `uncertified`."""
    if ev is None:
        return "no-evidence", ["no readable certify verdict"]
    if sum(ev[k] for k in ("divergences", "crashes", "sink_failures")):
        return "finding", ["divergences, crashes or sink failures"]
    problems = window_problems(ev, identity or (ev["target"], ev["pin"], ev["run"]))
    if census is not None and ev.get("encoding") != census["encoding"]:
        problems.append(f"the run's encoding {ev.get('encoding')!r} is not the registry's {census['encoding']!r}")
    if problems:
        return "invalid", problems
    w, base, final = ev["window"] or {}, ev["baseline"], ev["final"]
    stopped = w.get("stopped_by")
    failed = [why for bad, why in (
        (ev["engine"] != "single" or ev["jobs"] != 1 or ev["processes"] != 1, "not one libFuzzer process"),
        (ev["merge_status"] != 0, "the corpus merge failed"),
        (ev["replay_before_status"] != 0 or base is None or base[0] == 0, "no replay before the window"),
        (ev["refused"], "the target refused to run"),
        (ev["verdict_status"] != "pass", "run.sh's verdict is not a pass"),
        (not ((stopped == "cpu-window" and ev["libfuzzer_status"] == 0 and not w["killed"])
              or (stopped == "wall-cap" and (ev["libfuzzer_status"] == 72 or w["killed"]))),
         "libFuzzer did not end at its stop file after its CPU window (exit 0, not killed), or at the wall cap"),
        (stopped is not None and w["stop_to_exit"] > STOP_GRACE,
         f"libFuzzer exited {w.get('stop_to_exit')} s after its stop, more than {STOP_GRACE} s"),
        *((True, why) for why in (census or {}).get("failed", ())),
    ) if bad]
    if failed:
        return "failed", failed
    if stopped == "wall-cap":
        return "starved", [f"the {ev['wall_cap']} s wall-clock cap came before {ev['window_seconds']} CPU-s"]
    failed = [why for bad, why in (
        (ev["replay_after_status"] != 0 or final is None, "no replay after the window"),
        *((True, "the window log breaks its grammar: " + why) for why in ev["log_problems"]),
        (ev["executions"] is None or ev["new_lines"] is None or ev["window_done"] is None,
         "no window log with a DONE line and final stats"),
        # libFuzzer prints each unit it adds (a NEW or REDUCE line) before it counts it, and the
        # window ends between two units.
        (ev["units_added"] is None or ev["unit_lines"] != ev["units_added"],
         "the window log's NEW and REDUCE lines differ from the units libFuzzer added: it lost lines"),
        (final is not None and (final[0] < base[0] - replay.cov_slack
                                or final[1] < base[1] - replay.ft_allowance(base[1])),
         "the replay after the window has less coverage than the one before it: not reproducible"),
    ) if bad]
    if failed:
        return "failed", failed
    cov_gain, ft_gain = final[0] - base[0], final[1] - base[1]
    (cov0, ft0), (cov1, ft1) = ev["window_inited"], ev["window_done"]
    if (cov_gain > replay.cov_slack or abs(cov1 - cov0) > rule.cov_slack
            or (rule.ft and (ev["new_lines"] or ft_gain > replay.ft_allowance(base[1])
                             or ft1 - ft0 > PLATEAU_FT * ft0))):
        return "growing", [f"{ev['new_lines']} discovery lines in the window; in the process cov {cov0}->{cov1}, "
                           f"ft {ft0}->{ft1}; replayed cov +{cov_gain}, ft +{ft_gain}"]
    if ev["executions"] < floor:
        return "insufficient-progress", [f"{ev['executions']} executions in the window; certification needs {floor}"]
    if ev["window_seconds"] < CERTIFY_SECONDS:
        return "uncertified", [f"a {ev['window_seconds']} CPU-s window: certification needs {CERTIFY_SECONDS}"]
    if ev["source"] is None or ev["dirty"] is not False or not re.fullmatch(r"[0-9a-f]{12}", str(ev["rev"])):
        return "uncertified", ["no source record, the tree was dirty or changed during the build, or its commit "
                               "is unknown"]
    if ev["flavor"] != "fuzz":
        return "uncertified", ["not the coverage-instrumented (fuzz) oracle"]
    if "build" in ev:
        return "uncertified", ["not the default coverage build (BL-812's NODE_FUZZ_COVERAGE or NODE_FUZZ_TRACE_CMP)"]
    if census is not None and census["changed"]:
        return "uncertified", census["changed"]
    return "certified", []


UNREAD = object()  # read_certificate's default: read the run's certificate.json itself


def certificate_record(run_dir):
    """A run's certificate.json, read once: None when it has none, INVALID when it is unreadable or
    not of its shape, else the dict. Its shape: {"evidence": …, "cutoff": …, "eligibility": …,
    "plateau": …, "replay": …}, the cutoff a run stamp (`certify`'s time once run.sh exited) not
    before the run's start, eligibility `certify`'s reasons at that cutoff (`eligibility`; a list
    of strings, empty when it found none), and plateau and replay the target's rules that
    `certify` judged by (`Plateau.record`, `Replay.record`; a certificate written before DEC-260
    or DEC-271 lacks them, and is judged by the defaults, no looser than what judged it then)."""
    path = os.path.join(run_dir, CERTIFICATE)
    if not os.path.lexists(path):
        return None
    cert = read_json(path, INVALID)
    if not (isinstance(cert, dict) and set(cert) - {"plateau", "replay"} == {"evidence", "cutoff", "eligibility"}
            and None not in recorded_rules(cert)
            and isinstance(cert["cutoff"], str) and RUN_STAMP.fullmatch(cert["cutoff"])
            and cert["cutoff"] >= os.path.basename(run_dir).split("-")[0]
            and isinstance(cert["eligibility"], list) and all(isinstance(r, str) for r in cert["eligibility"])):
        return INVALID
    return cert


def recorded_rules(cert):
    """The (`Plateau`, `Replay`) a certificate's dict recorded, the defaults for those it lacks;
    None for one that is not a rule."""
    return (Plateau.from_record(cert.get("plateau", Plateau().record())),
            Replay.from_record(cert.get("replay", Replay().record())))


def read_certificate(run_dir, floor=CERTIFY_EXECS, cert=UNREAD, rule=Plateau(), replay=Replay(), encoding=UNREAD):
    """(status, reasons, evidence) of a certify run: derived from the run's own evidence, bound to
    the run's place in the tree, which its certificate's evidence, when it has a certificate, must
    equal (a certificate that is unreadable, malformed or disagrees with its run is invalid; Sol r2
    m1). `cert` is the run's `certificate_record`, when the caller has read it. A certificate's run
    is judged by the stricter of `rule` and `replay` (the target's now) and the rules it recorded,
    so changing the registry never certifies an earlier run. A chain target's run is re-judged
    by its census (`census_rejudge`) under `encoding`, the registry's encoding of its target
    (by default chain-encodings.toml's; when that cannot be read, the run of a `diff_chain*` target,
    or of one that recorded an encoding, is invalid). Whether the leg may be certified at all is
    `eligibility`'s (`certificates`)."""
    ev = certify_evidence(run_dir)
    cert = certificate_record(run_dir) if cert is UNREAD else cert
    if isinstance(cert, dict):
        recorded = recorded_rules(cert)
        if cert["evidence"] != json.loads(json.dumps(ev)) or None in recorded:
            cert = INVALID
        else:
            rule, replay = rule.strictest(recorded[0]), replay.strictest(recorded[1])
    if cert is INVALID:
        return "invalid", ["certificate.json is unreadable or does not match the run's evidence"], ev
    if encoding is UNREAD:
        encoding = chain_encoding(ev["target"]) if ev and isinstance(ev["target"], str) else ""
    if encoding is None and ev and (str(ev["target"]).startswith("diff_chain") or "encoding" in ev):
        return "invalid", ["node-l1/chain-encodings.toml cannot be read: the chain target's encoding is unknown"], ev
    census = census_rejudge(run_dir, ev, encoding)
    return (*judge(ev, floor, run_identity(run_dir), rule, replay, census=census), ev)


RUN_STAMP = re.compile(r"\d{8}T\d{6}Z")


def eligibility(fdir, target, pin, cutoff=None):
    """Why the leg cannot be certified, from its target's triage state, failing closed (DEC-186
    amendment 3 (d)): [] when it can. `certify` asks before its window and after it (with its
    completion cutoff), and the status reader asks with the cutoff its certificate records, so the
    two agree. Reasons: the state cannot be read (a torn file or a malformed cluster); an open
    cluster seen on the pin or on no pin in particular, whenever it was found (CS0 §9.3); any
    cluster whose regression vector or its provenance is missing or unreadable; and any run of
    the target, of whatever pin, whose triage failed or that is not triaged yet (pending, or never
    triageable: its findings are unknown), or the runs cannot be listed. With `cutoff` (a run
    stamp), a run that started after it does not count: run.sh released the leg's lock by then,
    so it cannot have touched the window, and it is the farm's to triage."""
    try:
        state = load_triage(fdir, target)
    except ValueError:
        return ["the triage state is unreadable"]
    reasons = []
    if any(c["state"] == "open" and (not c["pins"] or pin in c["pins"]) for c in state["clusters"].values()):
        reasons.append("an open finding")
    if any(vector_problem(fdir, target, cid, c) for cid, c in state["clusters"].items()):
        reasons.append("a regression vector or its provenance is missing or unreadable")
    try:
        runs = os.listdir(os.path.join(fdir, target, "runs"))
    except FileNotFoundError:
        runs = []
    except OSError:
        return reasons + ["the runs cannot be listed"]
    counted = lambda r: not (cutoff and RUN_STAMP.fullmatch(r.split("-")[0]) and r.split("-")[0] > cutoff)
    if any(counted(r) for r in state["run_errors"]):
        reasons.append("a run whose triage failed")
    if any(counted(r) for r in set(runs) - set(state["runs"]) - set(state["run_errors"])):
        reasons.append("a run not triaged yet")
    return reasons


def certificates(fdir, targets):
    """The latest certify run of every leg in the private tree (any target directory, so a
    removed target still reports): {leg: (status, reasons, evidence)}. A leg that `eligibility`
    refuses at its certificate's recorded cutoff (now, without one), or that `certify` found
    ineligible when it completed (the reasons its certificate records), is `finding`, whatever its
    run's status: the status reader reports what the command did, and never upgrades a refusal
    once pending runs triage clean (Sol r3 M1). A run without a certificate (certify did not
    complete) is at most `uncertified`."""
    latest = {}
    for target in sorted(listdir_or_empty(fdir)):
        runs = os.path.join(fdir, target, "runs")
        for run in listdir_or_empty(runs):
            path = os.path.join(runs, run, "mode")
            try:
                mode, made = open(path).read().strip(), os.stat(path).st_mtime_ns
            except OSError:
                continue
            pin = run_pin(os.path.join(runs, run))
            # The latest by the run id's start time, then by when the run wrote its mode file
            # (its pid, which follows the time in the id, is no order).
            key = (run.split("-")[0], made)
            if mode == "certify" and pin and key > latest.get(f"{target}@{pin}", ((), ""))[0]:
                latest[f"{target}@{pin}"] = (key, os.path.join(runs, run))
    certs = {}
    for leg, (_, run) in sorted(latest.items()):
        cert = certificate_record(run)
        name = leg.split("@")[0]
        if PLANTED in getattr(listed(name, targets), "features", ()):
            continue  # a planted search is never certified (D41), so never a leg here
        status, reasons, ev = read_certificate(run, certify_floor(name, targets), cert, plateau_rule(name, targets),
                                               replay_rule(name, targets), registry_encoding(name, targets))
        if cert is None and status == "certified":
            status, reasons = "uncertified", ["no certificate.json: certify did not complete"]
        recorded = cert["eligibility"] if isinstance(cert, dict) else []
        now = eligibility(fdir, *leg.split("@"), cert["cutoff"] if isinstance(cert, dict) else None)
        blocked = recorded + [r for r in now if r not in recorded]
        certs[leg] = ("finding", blocked, ev) if blocked else (status, reasons, ev)
    return certs


def certificate_token(status, ev):
    rev = (ev or {}).get("rev") or "-"
    return f"{status}@{rev}"


def farm_state_dir(fdir, night):
    return os.path.join(fdir, ".farm", night)


class FarmBusy(Exception):
    """Another worker holds the night."""


class FarmStateError(Exception):
    """A night's plan or ledger that cannot be trusted: the farm refuses to run or report on it
    rather than guess (the message names no target)."""


SLICE_ID = re.compile(r"[0-9a-f]{16}")
STATUSES = ("pass", "finding", "interrupted", "fail", "lost")
# Slices whose CPU time counts toward their leg's share.
CLEAN = ("pass", "interrupted")


def finite(x, positive=False):
    return (isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x)
            and (x > 0 if positive else x >= 0))


def registry_digest(targets):
    """Identifies the farmed legs and their weights a plan was made for."""
    farmed = sorted((t.name, p, t.nightly) for t, p in legs(targets, "nightly"))
    return hashlib.sha256(json.dumps(farmed).encode()).hexdigest()[:16]


def load_plan(path, night, targets):
    """The night's plan, None when there is none yet; FarmStateError when it exists but is
    unreadable or is not a plan for this night and registry (fail closed: an empty or foreign
    plan must never let a night count as complete)."""
    if not os.path.exists(path):
        return None
    try:
        with open(path) as f:
            plan = json.load(f)
    except (OSError, ValueError):
        raise FarmStateError("the night's plan is unreadable") from None
    want = set(plan_legs(targets, 1))
    if (not isinstance(plan, dict) or set(plan) != {"night", "cpu_hours", "legs", "registry"}
            or plan["night"] != night or not finite(plan["cpu_hours"], positive=True)
            or not isinstance(plan["legs"], dict) or not plan["legs"]):
        raise FarmStateError("the night's plan is malformed")
    if set(plan["legs"]) != want or plan["registry"] != registry_digest(targets):
        raise FarmStateError("the night's plan does not match the registry; start a new night")
    split = plan_legs(targets, plan["cpu_hours"])
    if not all(finite(b) and abs(b - split[leg]) <= 1e-3 for leg, b in plan["legs"].items()):
        raise FarmStateError("the night's plan has bad shares")
    return plan


# A ledger row's kind when it is not a fuzzing slice: the leg's corpus minimisation (BL-841).
CMIN = "cmin"
# A fuzzing slice's replay, from its farm segments' records (`replay_cpu`); absent on older rows
# and on slices whose replay is unknown.
REPLAY_KEYS = ("replay_cpu_s", "replay_max_s", "workers", "inited_workers")
# With them, when every process's record has its loaded count: the inputs the processes loaded, in
# all (BL-841b; a slice's replay cost per input, `c`, is replay_cpu_s over it).
REPLAY_INPUTS = "replay_inputs"
# A shard slice's done row (BL-841b): `replay_mode` shard (absent: jobs) and, with the replay keys,
# `replay_imbalance`, its slowest worker's replay over the median worker's. Every fuzzing slice the
# farm ran records `slice_s`, its planned length in seconds (§4.1), and a failed one with the replay
# keys `ran_out` true when run.sh's reason is BEFORE_INITED (BL-841b r2 R2-1).
SHARD = "shard"
# A minimisation's done row (BL-841b, Q13): `cmin_files`, [files before, files kept], when it
# swapped its corpus in (it passed, or was killed after the swap, C-841-F13).
CMIN_FILES = "cmin_files"


def valid_row(row, legs_):
    if not isinstance(row, dict) or row.get("event") not in ("start", "done"):
        return False
    if row.get("kind", CMIN) != CMIN:
        return False
    if not isinstance(row.get("slice"), str) or not SLICE_ID.fullmatch(row["slice"]):
        return False
    if not isinstance(row.get("leg"), str) or row["leg"] not in legs_:
        return False
    if row["event"] == "start":
        return finite(row.get("time"))
    counts = ("executions", *FINDING_KEYS)
    replay = [k for k in REPLAY_KEYS if k in row]
    if replay and (len(replay) != len(REPLAY_KEYS) or not (
            finite(row["replay_cpu_s"]) and finite(row["replay_max_s"]) and is_count(row["workers"])
            and is_count(row["inited_workers"]) and row["inited_workers"] <= row["workers"])):
        return False
    if REPLAY_INPUTS in row and not (replay and is_count(row[REPLAY_INPUTS], 1)):
        return False
    if row.get("replay_mode", SHARD) != SHARD:
        return False
    if "replay_imbalance" in row and not (replay and finite(row["replay_imbalance"]) and row["replay_imbalance"] >= 1):
        return False
    if "slice_s" in row and not finite(row["slice_s"], positive=True):
        return False
    if RAN_OUT in row and not (row[RAN_OUT] is True and row.get("status") == "fail" and replay):
        return False
    if CMIN_FILES in row and not (row.get("kind") == CMIN and isinstance(row[CMIN_FILES], list)
                                  and len(row[CMIN_FILES]) == 2 and all(is_count(x, 1) for x in row[CMIN_FILES])
                                  and row[CMIN_FILES][1] <= row[CMIN_FILES][0]):
        return False
    if ("jobs" in row and not is_count(row["jobs"], 1)) or ("wall_s" in row and not finite(row["wall_s"])):
        return False
    return (row.get("status") in STATUSES and finite(row.get("cpu_hours"))
            and all(isinstance(row.get(k), int) and not isinstance(row.get(k), bool) and row[k] >= 0
                    for k in counts)
            and isinstance(row.get("recovered"), bool))


def ledger_path(state_dir):
    return os.path.join(state_dir, "slices.jsonl")


def repair_ledger(state_dir, name="slices.jsonl"):
    """Drops a torn final row of the ledger or another of the night's row files (a kill during
    an append leaves a line without its newline) by truncating to the last whole row. Returns 1
    when it dropped one."""
    path = os.path.join(state_dir, name)
    try:
        data = open(path, "rb").read()
    except FileNotFoundError:
        return 0
    if not data or data.endswith(b"\n"):
        return 0
    os.truncate(path, data.rfind(b"\n") + 1)
    return 1


def load_ledger(state_dir, legs_):
    """The night's ledger rows: a `start` row when a slice is launched, and a `done` row with its
    outcome once it is accounted for (a corpus minimisation's both carry `kind` cmin). Every whole
    row must be a valid row for one of `legs_`, or FarmStateError; a torn final row (no newline) is
    ignored here and dropped by `repair_ledger`."""
    try:
        data = open(ledger_path(state_dir), "rb").read()
    except FileNotFoundError:
        return []
    rows, started, done = [], {}, set()
    for raw in data.split(b"\n")[:-1]:
        try:
            row = json.loads(raw)
        except ValueError:
            raise FarmStateError("the night's ledger has a malformed row") from None
        if not valid_row(row, legs_):
            raise FarmStateError("the night's ledger has an invalid row")
        sid = row["slice"]
        if row["event"] == "start":
            if sid in started:
                raise FarmStateError("the night's ledger starts a slice twice")
            started[sid] = (row["leg"], row.get("kind"))
        elif started.get(sid) != (row["leg"], row.get("kind")) or sid in done:
            raise FarmStateError("the night's ledger has a done row without its start, or twice")
        else:
            done.add(sid)
        rows.append(row)
    return rows


def load_records(state_dir, legs_=None):
    """The `done` rows: one per slice accounted for. Without `legs_`, the night's own plan
    names the legs."""
    if legs_ is None:
        legs_ = read_json(os.path.join(state_dir, "plan.json"), {}).get("legs", {})
    return [r for r in load_ledger(state_dir, legs_) if r["event"] == "done"]


def append_row(state_dir, row, name="slices.jsonl"):
    """Appends one ledger (or coverage) row with a single write and an fsync: once this returns
    the row survives a kill, and a kill during it leaves at most a torn final line."""
    fd = os.open(os.path.join(state_dir, name), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        os.write(fd, (json.dumps(row, sort_keys=True) + "\n").encode())
        os.fsync(fd)
    finally:
        os.close(fd)


def replay_cpu(run_dir):
    """A run's replay (BL-841): libFuzzer's -jobs workers each load the whole corpus before they
    fuzz, up to their INITED line, and each farm segment's record has every fuzzing process's
    own CPU at its INITED (window_driver.py, BL-826). Returns the sum and the largest of those
    figures over the run's segments, with the processes counted and those with a figure. None
    when a segment has no record (it ended without one, or ran outside the driver): its processes
    are unknown. ValueError when a record is malformed, which is never read as a replay."""
    logs = os.path.join(run_dir, "logs")
    try:
        segments = sorted(e for e in os.listdir(logs) if e.startswith("segment-"))
    except OSError:
        return None
    if not segments:
        return None
    replay = top = 0.0
    workers = inited = inputs = 0
    loaded = True
    for seg in segments:
        path = os.path.join(logs, seg, "segment.json")
        if not os.path.lexists(path):
            return None
        try:
            with open(path) as f:
                procs = json.load(f).get("process_inited")
        except (OSError, ValueError, RecursionError, AttributeError):
            raise ValueError(f"{seg}'s record is unreadable") from None
        if not procs or not SEGMENT_KEYS["process_inited"](procs):
            raise ValueError(f"{seg}'s process_inited is malformed")
        for p in procs:
            workers += 1
            if p.get("loaded") is None:
                loaded = False
            else:
                inputs += p["loaded"]
            if p["cpu_at_inited"] is not None:
                inited += 1
                replay += p["cpu_at_inited"]
                top = max(top, p["cpu_at_inited"])
    return {"replay_cpu_s": round(replay, 3), "replay_max_s": round(top, 3), "workers": workers,
            "inited_workers": inited, **({REPLAY_INPUTS: inputs} if loaded else {})}


def replay_imbalance(run_dir):
    """A run's slowest replay over its median one (each fuzzing process's CPU at INITED, every
    segment's), or None when fewer than one process has a figure or a record is unreadable."""
    figures = []
    for seg in segments(run_dir):
        procs = read_json(os.path.join(seg, "segment.json"), {}).get("process_inited")
        if not isinstance(procs, list) or not SEGMENT_KEYS["process_inited"](procs):
            return None
        figures += [p["cpu_at_inited"] for p in procs if p["cpu_at_inited"] is not None]
    figures.sort()
    if not figures:
        return None
    mid = len(figures) // 2
    median = figures[mid] if len(figures) % 2 else (figures[mid - 1] + figures[mid]) / 2
    return round(figures[-1] / median, 3) if median > 0 else None


def shard_record(run_dir):
    """A shard run's shards.json (run.sh --shard), or None for any other run."""
    record = read_json(os.path.join(run_dir, "shards.json")) if run_dir else None
    return record if isinstance(record, dict) else None


def replay_counted(r):
    """Whether a done row's replay counts: a fuzzing slice that passed (it ran its budget out,
    neither interrupted nor ended during its replay) and whose every process reached INITED with
    a known CPU figure. Every other slice's replay is unknown."""
    return (r.get("kind") != CMIN and r["status"] == "pass" and all(k in r for k in REPLAY_KEYS)
            and r["workers"] > 0 and r["inited_workers"] == r["workers"] and r["cpu_hours"] > 0)


def replay_floor(r):
    """A lower bound on a leg's replay, in CPU-s per worker, from a fuzzing slice whose replay is
    not counted (BL-841 r1 F1): the slowest worker that reached INITED, or, when none did, every
    worker's mean CPU, all of it spent replaying. A slice that ran out of its budget before every
    worker reached INITED (`ran_out`) floors at the larger of the two: its workers still loading
    spent their budget on it (C-841b-R3N1). 0 from any other row."""
    if r.get("kind") == CMIN or replay_counted(r) or not all(k in r for k in REPLAY_KEYS) or not r["workers"]:
        return 0.0
    mean = r["cpu_hours"] * 3600 / r["workers"]
    if r.get(RAN_OUT):
        return max(r["replay_max_s"], mean)
    return r["replay_max_s"] if r["inited_workers"] else mean


def replay_share(rows):
    """The replay share of a set of done rows: the counted slices' replay CPU, and every corpus
    minimisation's CPU (it is all replay, so it can't hide its own cost), over the CPU of both;
    the worst counted slice's own share; and the fuzzing slices left out. The shares are None
    when no slice counts."""
    counted = [r for r in rows if replay_counted(r)]
    cmin = sum(r["cpu_hours"] for r in rows if r.get("kind") == CMIN) * 3600
    cpu = sum(r["cpu_hours"] for r in counted) * 3600
    share = (sum(r["replay_cpu_s"] for r in counted) + cmin) / (cpu + cmin) if counted else None
    worst = max((r["replay_cpu_s"] / (r["cpu_hours"] * 3600) for r in counted), default=None)
    unknown = sum(r.get("kind") != CMIN for r in rows) - len(counted)
    return {"share": share, "max": worst, "counted": len(counted), "unknown": unknown, "cmin_cpu_s": cmin}


def share_text(x):
    return "-" if x is None else f"{x:.3f}"


def replay_cost(rows):
    """The replay cost of a set of done rows (BL-841b): over the counted slices that know their
    inputs, `c` the replay CPU per loaded input, `n` the inputs each process loaded and `replay_s`
    each process's replay, both on average; None when no slice knows them."""
    known = [r for r in rows if replay_counted(r) and REPLAY_INPUTS in r]
    cpu, inputs, procs = (sum(r[k] for r in known) for k in ("replay_cpu_s", REPLAY_INPUTS, "workers"))
    if cpu <= 0:
        return None
    return {"c": cpu / inputs, "n": inputs / procs, "replay_s": cpu / procs}


def worker_rate(r):
    """A slice's CPU-s per wall second per worker, or None (a recovered row has no wall time)."""
    return r["cpu_hours"] * 3600 / (r["jobs"] * r["wall_s"]) if r.get("jobs") and r.get("wall_s") else None


# The replay trend's alarm (BL-841b, OD-L3-161 Q10): over the last TREND_NIGHTS nights with a known
# replay cost, c grew by C_GROWTH_ALARM or more while the inputs grew by less than N_FLAT; or the
# next leg's projected share is over --replay-target.
TREND_NIGHTS = 5
C_GROWTH_ALARM = 0.20
N_FLAT = 0.05


def replay_points(fdir, leg):
    """The replay cost of `leg` per night and corpus epoch, oldest first by its first counted slice's
    start: {night, c, n, replay_s, rate, share, mode, cmins, time}. `cmins` is the count of
    minimisations of the leg's target that changed its corpus (`cmin_resets`) started before the
    point. Each done slice belongs to the epoch its start falls in. Starts are rounded to the
    second (BL-841c r2), so a minimisation and a slice that start in the same second are ordered
    by position only within one night's ledger. A night's name is not its time (BL-841c r3): a
    slice whose start shares its second with another night's minimisation has no known epoch,
    and when two nights' points of one epoch would start in the same second their order is
    unknown. Exclusion is per slice (BL-841c r4): such a slice is left out, the points it
    touched are rebuilt from their other slices, and that repeats while rebuilt points still
    start in one second. The slices left out are counted in the third value. The fourth is the
    newest epoch, the count of the minimisations seen, whether or not any of its points is left
    (`replay_projection` reads only it). A minimisation within a night splits it into one point
    per epoch, each costed, timed and rated from its own epoch's slices only, so no point mixes
    a corpus before a minimisation with the one after it (BL-841c r1). Points without a known
    cost are left out. A night whose plan is unreadable, or whose plan names this leg and whose
    ledger is unusable, is counted in the second value, never read. A plan is read without the
    registry check (as `Farm.earlier_records` does, BL-841b r1 M1): the trend reads counts only
    and gates nothing, so a target added or a weight changed since must not hide the nights
    before."""
    farm = os.path.join(fdir, ".farm")
    nights, unusable, resets, name = [], 0, [], leg.split("@")[0]
    for night in sorted(os.listdir(farm)) if os.path.isdir(farm) else []:
        d = farm_state_dir(fdir, night)
        path = os.path.join(d, "plan.json")
        if not os.path.exists(path):
            continue
        plan = read_json(path)
        legs_ = plan.get("legs") if isinstance(plan, dict) else None
        if not isinstance(legs_, dict):
            unusable += 1
            continue
        # Any leg of the target is read for its minimisations; only this leg's plan counts a night
        # this leg can't read.
        if not any(x.split("@")[0] == name for x in legs_):
            continue
        try:
            ledger = load_ledger(d, legs_)
        except (FarmStateError, OSError):
            unusable += leg in legs_
            continue
        order = {r["slice"]: (r["time"], night, i) for i, r in enumerate(ledger) if r["event"] == "start"}
        started = {k: v[0] for k, v in order.items()}
        resets += [order[r["slice"]] for r in ledger if r["event"] == "done" and cmin_resets(r, name)]
        nights.append((night, started, order, [r for r in ledger if r["event"] == "done" and r["leg"] == leg]))

    def before(k, s):
        """Whether start `k` came before start `s`: True, False, or None when they share a second
        in two nights' ledgers."""
        if k[0] != s[0]:
            return k[0] < s[0]
        return k[2] < s[2] if k[1] == s[1] else None

    groups, starts, ambiguous = {}, {}, 0
    for night, started, order, rows in nights:
        starts[night] = started
        for r in rows:
            seen = [before(k, order[r["slice"]]) for k in resets]
            if None in seen:
                ambiguous += 1
                continue
            groups.setdefault((night, sum(seen)), []).append(r)
    while True:
        points = {key: p for key, rows in groups.items() if (p := replay_point(*key, rows, starts[key[0]]))}
        tied = collections.Counter((p["time"], p["cmins"]) for p in points.values())
        hit = [key for key, p in points.items() if tied[p["time"], p["cmins"]] > 1]
        if not hit:
            break
        for key in hit:
            t, started = points[key]["time"], starts[key[0]]
            kept = [r for r in groups[key] if started[r["slice"]] != t]
            ambiguous += len(groups[key]) - len(kept)
            groups[key] = kept
    return sorted(points.values(), key=lambda p: (p["time"], p["cmins"])), unusable, ambiguous, len(resets)


def replay_point(night, cmins, rows, started):
    """One night's point of one epoch from its done rows, or None without a known cost."""
    cost = replay_cost(rows)
    if cost is None:
        return None
    known = sorted((r for r in rows if replay_counted(r) and REPLAY_INPUTS in r), key=lambda r: started[r["slice"]])
    rates = [worker_rate(r) for r in known]
    modes = {r.get("replay_mode", "jobs") for r in known}
    return {"night": night, "time": started[known[0]["slice"]], "cmins": cmins,
            "mode": modes.pop() if len(modes) == 1 else "mixed",
            "rate": next((x for x in reversed(rates) if x), None),
            "share": replay_share(rows)["share"], **cost}


def cmin_resets(r, name):
    """Whether a done row is a minimisation of target `name` that changed the corpus the next slice
    loads (as `Farm.replay_estimate` reads one, OD-L3-161 Q13): one that removed inputs, or a
    passing one without its counts. One that kept every input changes nothing."""
    if r.get("kind") != CMIN or r["leg"].split("@")[0] != name:
        return False
    kept = r.get(CMIN_FILES)
    return kept[1] < kept[0] if kept else r["status"] == "pass"


def replay_projection(points, replay_target, slice_minutes, max_slice_minutes, *, epoch):
    """The next leg's replay, from the last TREND_NIGHTS points of the newest epoch `epoch`
    (`replay_points`' fourth value), never an older one's (BL-841c r4): the inputs per process grow by
    their mean step, c by its mean (geometric) growth, not below 0; the slice is the length the
    farm would give it (`Farm.slice_seconds`) at the last point's rate. Only the points of the last
    point's replay mode count (a shard worker loads 1/J of a jobs worker's inputs, BL-841b), since
    its target's last minimisation that changed its corpus (its `cmins`: a minimisation cuts the
    inputs and keeps the costlier ones, which is no growth, C-841b-L2). None with fewer than two
    such points or no rate. `closing` is the alarm."""
    points = [p for p in points if p.get("cmins", 0) == epoch]
    key = points[-1].get("mode") if points else None
    same = len(points) - next((i for i, p in enumerate(reversed(points)) if p.get("mode") != key), len(points))
    window = points[same:][-TREND_NIGHTS:]
    if len(window) < 2 or not window[-1]["rate"]:
        return None
    first, last, steps = window[0], window[-1], len(window) - 1
    n = max(0.0, last["n"] + (last["n"] - first["n"]) / steps)
    c = last["c"] * max(1.0, (last["c"] / first["c"]) ** (1 / steps))
    replay, rate = n * c, last["rate"]
    slice_s = min(max(replay / (replay_target * rate), slice_minutes * 60), max_slice_minutes * 60)
    share = replay / (rate * slice_s)
    c_growth, n_growth = last["c"] / first["c"] - 1, last["n"] / first["n"] - 1
    # The farm sizes a slice so that its replay is exactly --replay-target when it can: only a slice
    # held at its cap (or a rounding error's width over) projects over it.
    closing = (share > replay_target * (1 + 1e-9)
               or (c_growth >= C_GROWTH_ALARM * (1 - 1e-9) and n_growth < N_FLAT * (1 - 1e-9)))
    return {"nights": len(window), "n": n, "c": c, "replay_s": replay, "slice_s": slice_s, "share": share,
            "c_growth": c_growth, "n_growth": n_growth, "closing": closing}


def floor_gated(t):
    """A target whose certify run is gated on its census floor (O2b-4 4e2): a minimisation could
    drop an input that reaches a census cell without adding a libFuzzer feature, so it gets none
    (BL-841, OD-L3-157 (3))."""
    return t.encoding == "chain-v2"


CMIN_HISTORY = ".cmin.jsonl"  # in fuzz/: each passing minimisation's corpus count, across nights
RM_IF_IDLE = "/work/scratch/mgr/rm-if-idle.sh"  # NODE_FUZZ_RM_IF_IDLE overrides it
# run.sh's verdict reason for a slice whose budget ran out while it loaded its corpus, and the done
# row's field that records it (BL-841b r2 R2-1).
BEFORE_INITED = "budget spent before INITED (corpus load)"
RAN_OUT = "ran_out"
# A minimisation's CPU-s per corpus input when no replay of the leg measured one (C-841-F7, BL-841b
# r1 L3: DESIGN §5.1's figure), and a shard-mode leg's least --cmin-growth (OD-L3-161 Q8).
CMIN_INPUT_CPU_S = 2.27
SHARD_CMIN_GROWTH = 0.5


def load_cmin_history(fdir):
    """Each target's corpus file count after its latest passing minimisation (`load_cmin_rows`)."""
    return {t: row["files_after"] for t, row in load_cmin_rows(fdir).items()}


def load_cmin_rows(fdir):
    """Each target's latest passing minimisation, from `fuzz/.cmin.jsonl` (a torn final row is
    dropped first): its row, with `files_after`, the corpus's file count after it, and (since
    BL-841b) `files_before`. A malformed row: FarmStateError."""
    repair_ledger(fdir, CMIN_HISTORY)
    history = {}
    try:
        data = open(os.path.join(fdir, CMIN_HISTORY), "rb").read()
    except FileNotFoundError:
        return history
    for raw in data.split(b"\n")[:-1]:
        try:
            row = json.loads(raw)
        except ValueError:
            raise FarmStateError("the minimisation history has a malformed row") from None
        if not (isinstance(row, dict) and isinstance(row.get("target"), str) and is_count(row.get("files_after"), 1)
                and ("files_before" not in row or is_count(row["files_before"], 1))):
            raise FarmStateError("the minimisation history has an invalid row")
        history[row["target"]] = row
    return history


def cmin_swapped(run_dir):
    """A minimisation run that was killed after its swap (C-841-F13): its verdict-less run has its
    counts (run.sh writes them before the renames) and no cmin/ (the second rename took it). Its
    `cmin` counts, or None."""
    if not run_dir or os.path.exists(os.path.join(run_dir, "verdict.json")) or os.path.lexists(os.path.join(run_dir, "cmin")):
        return None
    try:
        counts = [int(x) for x in open(os.path.join(run_dir, "cmin.counts")).read().split()]
    except (OSError, ValueError):
        return None
    if len(counts) != 5 or counts[2] < 1:
        return None
    return dict(zip(("files_before", "bytes_before", "files_after", "bytes_after"), counts[:4]))


def cmin_row(status, verdict, run_dir):
    """A minimisation's done-row counts (`cmin_files`): a passing one's verdict's, or a swapped one's
    that was killed (`cmin_swapped`); {} otherwise."""
    counts = (verdict.get("cmin") or {}) if status == "pass" else (cmin_swapped(run_dir) or {})
    before, after = counts.get("files_before"), counts.get("files_after")
    ok = is_count(before, 1) and is_count(after, 1) and after <= before
    return {CMIN_FILES: [before, after]} if ok else {}


def corpus_files(tdir, sub="corpus"):
    try:
        with os.scandir(os.path.join(tdir, sub)) as it:
            return sum(e.is_file(follow_symlinks=False) for e in it)
    except OSError:
        return 0


class Farm:
    """The worker inside the systemd unit: slices of run.sh legs until every leg of the launch
    is suspected of a plateau or has spent its share, the night's cap is spent, the deadline
    passes, or the unit is stopped. Resumable: progress is `.farm/<night>/slices.jsonl` and each
    leg's coverage history `.farm/<night>/coverage.jsonl`, and a new launch for the same night
    continues them. Durations use `clock`, a monotonic clock (Sol r1 B3)."""

    margin = 60  # seconds before the deadline at which slices are interrupted
    lead = 120  # seconds between a slice's planned end and a reserved window
    imminent = 180  # a window this close interrupts running slices
    # Seconds a slice may outlive its stop file before the SIGINT, and its SIGINT before the
    # SIGKILL: well inside libFuzzer's 1200 s -timeout, whose ALARM files the phantom (BL-173), and
    # (together) inside `imminent` and the unit's TimeoutStopSec of 180.
    stop_wait = 90
    int_wait = 30
    escalate_poll = 0.5

    def __init__(self, args, targets, bench, runner=run_leg, clock=time.monotonic, log=print):
        self.a, self.targets, self.bench = args, targets, bench
        self.runner, self.clock, self.log = runner, clock, log
        self.fdir = fuzz_dir()
        self.dir = farm_state_dir(self.fdir, args.night)
        private_mkdir(os.path.dirname(self.dir), self.dir)
        # One worker per night, from before the plan is made or the ledger repaired until
        # `release` (or the object's end).
        lock = os.open(os.path.join(self.dir, "lock"), os.O_WRONLY | os.O_CREAT, 0o600)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            os.close(lock)
            raise FarmBusy(f"night {args.night} is already being worked") from None
        self.release = weakref.finalize(self, os.close, lock)
        self.ready_runs = set()  # recovered runs without a verdict: triage them now
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.procs = {}  # leg (or "triage") -> Popen of a running run.sh
        self.interrupted = set()
        # leg -> CPU-h given to its running slice; it leaves with the slice's done row, under
        # `lock`, so that the cap counts a slice once (`work`).
        self.given = {}
        self.triage_errors = 0
        # Plateau suspicion: the running slice of each leg, the runs found for slices, each
        # slice's last sample, and each leg's history (`suspected`).
        self.cov_lock = threading.Lock()
        self.window_s = args.plateau_minutes * 60
        self.scan, self.union_scan = LogScan(), UnionScan()
        self.slice_modes = {}  # sid -> SHARD for a shard slice (BL-841b)
        self.slice_ids, self.slice_runs, self.sampled = {}, {}, {}  # sampled: sid -> (time, growth, epoch)
        self.fuzz_s = {}  # leg -> its fuzzing seconds unrounded; rows keep 0.1 s (BL-134)
        plan_path = os.path.join(self.dir, "plan.json")
        self.plan = load_plan(plan_path, args.night, targets)
        if self.plan is None:
            budget = args.hours * args.cores
            if not finite(budget, positive=True) or not plan_legs(targets, budget):
                raise FarmStateError("nothing to farm: no budget or no farmed legs")
            self.plan = {"night": args.night, "cpu_hours": budget, "legs": plan_legs(targets, budget),
                         "registry": registry_digest(targets)}
            write_private(plan_path, json.dumps(self.plan, indent=1, sort_keys=True))
        self.ledger_repaired = repair_ledger(self.dir) + repair_ledger(self.dir, COVERAGE)
        self.records()  # validates every row before anything is scheduled
        self.coverage, self.suspicions, self.signals = load_coverage(self.dir, self.plan["legs"])
        only = set(args.only or ())
        self.selected = {leg for leg in self.plan["legs"] if not only or leg.split("@")[0] in only}
        self.replay_bound = set()  # legs logged as replay-bound in this launch (BL-841)
        self.replay_alarms = set()  # legs whose replay trend is closing at this launch (BL-841b)
        self.cmin_checked = set()  # legs whose minimisation was considered in this launch
        self.cmin_deferred = set()  # legs whose minimisation waited for time or cap, logged once
        self.held_bound = set()  # legs held as replay-bound at the cap in this launch (BL-841b §4.1)
        self.jobs_mode = set()  # legs run in jobs mode under --replay-mode shard, logged once (Q6)
        self.fold_lost = 0  # new inputs of shard slices recovered after a kill, never folded (Q5)
        self.history = self.earlier_records()

    def earlier_records(self):
        """Every earlier night's done rows, in start order: a leg's slice length carries across
        nights (`replay_estimate`, BL-841 r1 F1). A night whose plan or ledger can't be read is
        skipped and counted, never fatal."""
        farm, rows, skipped = os.path.dirname(self.dir), [], 0
        for night in sorted(os.listdir(farm)):
            d = os.path.join(farm, night)
            if night == self.a.night or not os.path.isdir(d):
                continue
            plan = read_json(os.path.join(d, "plan.json"))
            legs_ = plan.get("legs") if isinstance(plan, dict) else None
            try:
                if not isinstance(legs_, dict):
                    raise FarmStateError("no plan")
                ledger = load_ledger(d, legs_)
            except (FarmStateError, OSError):
                skipped += 1
                continue
            started = {r["slice"]: r["time"] for r in ledger if r["event"] == "start"}
            rows += [(started[r["slice"]], r) for r in ledger if r["event"] == "done"]
        if skipped:
            self.log(f"node-fuzz farm: {skipped} earlier nights are unreadable; their slices size none of this one's")
        return [r for _, r in sorted(rows, key=lambda x: x[0])]

    def records(self):
        return load_records(self.dir, self.plan["legs"])

    def record(self, row, settles=None):
        with self.lock:
            append_row(self.dir, row)
            self.given.pop(settles, None)

    def done_row(self, sid, leg, status, verdict, run_dir, recovered, **extra):
        """A slice's done row. A fuzzing slice's carries its replay when its run's segment
        records give it (`replay_cpu`; a malformed record leaves it unknown)."""
        replay = {}
        if run_dir and extra.get("kind") != CMIN:
            try:
                replay = replay_cpu(run_dir) or {}
            except ValueError:
                self.log("node-fuzz farm: a slice's segment record is malformed; its replay is unknown")
        shard = {}
        if extra.get("kind") != CMIN and shard_record(run_dir) is not None:
            imbalance = replay_imbalance(run_dir) if replay else None
            shard = {"replay_mode": SHARD, **({"replay_imbalance": imbalance} if imbalance else {})}
        return {"event": "done", "slice": sid, "leg": leg, "status": status,
                "run": os.path.basename(run_dir), "rev": verdict.get("rust_dashcore", "-"),
                "recovered": recovered, **replay, **shard, **extra,
                **{k: verdict.get(k, 0) for k in ("cpu_hours", "executions", "slow_units",
                                                  *FINDING_KEYS)}}

    def sample(self, leg):
        """Adds what the logs of the leg's running slice report (`LogScan`) to the leg's coverage
        history, persisted before it counts. The interval since the slice's last sample is
        fuzzing time only when both ends saw the same epoch: one segment, its processes all past
        INITED. Growth always counts, whether or not its interval is fuzzing time."""
        with self.cov_lock:
            sid = self.slice_ids.get(leg)
            if sid is None:
                return
            run = self.slice_runs.get(sid) or tagged_run(self.fdir, leg.split("@")[0], sid)
            self.slice_runs[sid] = run
            if not run:
                return
            union = self.slice_modes.get(sid) == SHARD
            dcov, dft, top, epoch = (self.union_scan if union else self.scan)(run)
            now = self.clock()
            last = self.sampled.get(sid)
            if last is None and epoch is None:
                # Nothing is fuzzing time yet, and growth so far counts at the next sample.
                self.sampled[sid] = (now, (0, 0), None)
                return
            self.sampled[sid] = (now, (dcov, dft), epoch)
            seen = last[1] if last else (0, 0)
            fuzzed = max(0.0, now - last[0]) if last and epoch is not None and last[2] == epoch else 0
            signal_ = UNION_SIGNAL if union else None
            if self.coverage.get(leg) and self.signals.get(leg) != signal_:
                # A launch of the other replay mode: the leg's history starts afresh (BL-841b §2.2).
                append_row(self.dir, {"leg": leg, "reset": True}, COVERAGE)
                self.coverage[leg] = []
                self.fuzz_s.pop(leg, None)
            self.signals[leg] = signal_
            history = self.coverage.setdefault(leg, [])
            prev = history[-1] if history else (0, 0, 0, 0)
            # A poll shorter than a row's 0.1 s still counts: the sum is kept unrounded (BL-134).
            fuzz_s = self.fuzz_s[leg] = self.fuzz_s.get(leg, prev[0]) + fuzzed
            row = (round(fuzz_s, 1), prev[1] + max(0, dcov - seen[0]),
                   prev[2] + max(0, dft - seen[1]), max(prev[3], top))
            if history and row == prev:
                return
            append_row(self.dir, dict(zip(("fuzz_s", "cov_gain", "ft_gain", "ft"), row), leg=leg,
                                      **({"signal": signal_} if signal_ else {})), COVERAGE)
            history.append(row)

    def suspicion(self, leg):
        with self.cov_lock:
            rule = plateau_rule(leg.split("@")[0], self.targets)
            return suspected(self.coverage.get(leg, []), self.window_s, rule, self.signals.get(leg) == UNION_SIGNAL)

    def suspect(self, leg, p):
        """Records that the farm stopped the leg on a suspected plateau (not a certificate)."""
        with self.cov_lock:
            append_row(self.dir, {"leg": leg, "suspected": p}, COVERAGE)
            self.suspicions[leg] = p

    def reset_coverage(self, leg):
        """A finding voids the leg's history as it voids its credit (CS0 §9.3)."""
        with self.cov_lock:
            append_row(self.dir, {"leg": leg, "reset": True}, COVERAGE)
            self.coverage[leg] = []
            self.fuzz_s.pop(leg, None)
            self.suspicions.pop(leg, None)
            self.signals.pop(leg, None)

    def accounted(self, run, verdict, interrupted, live=True):
        """A slice's status and the verdict to record it by, one rule for live and recovered
        slices (Sol r1 B4): run.sh's verdict; when it has none or an unreadable one, what the run
        left (its finished segments' CPU time, its divergences, crashes and logs, read by
        run.sh's rules; such a run is triaged at once), which is a finding if it shows one. A slice
        without a run is lost when recovered, and failed (a broken build, say) or interrupted
        when live. A recovered slice without a verdict was interrupted by the kill."""
        killed = not isinstance(verdict, dict) or "status" not in verdict
        if killed and not run and live:
            return ("interrupted" if interrupted else "fail"), {}
        if killed and run:
            self.ready_runs.add(os.path.basename(run))
            verdict = {"cpu_hours": segment_cpu_hours(run), "rust_dashcore": "-", **run_findings(run)}
        elif killed:
            return "lost", {}
        elif run:  # what the run left counts even beside a verdict (an unsaved crash, say)
            found = run_findings(run)
            verdict = {**verdict, **{k: max(verdict.get(k, 0), found[k]) for k in found}}
        if sum(verdict.get(k, 0) for k in FINDING_KEYS):
            return "finding", verdict
        if verdict.get("status") == "pass":
            return "pass", verdict
        if verdict.get("refused"):
            return "fail", verdict
        stopped = bool(run) and os.path.exists(os.path.join(run, "stop"))  # the farm's stop file
        if (killed and not live) or interrupted or stopped or verdict.get("libfuzzer_status") == 72:  # SIGINT's exit
            return "interrupted", verdict
        return "fail", verdict  # a failure the farm did not cause

    def reconcile(self):
        """Accounts, exactly once, for every slice that was started but not recorded done (the
        worker was killed), by `accounted`. Returns how many it recovered."""
        rows = load_ledger(self.dir, self.plan["legs"])
        done = {r["slice"] for r in rows if r.get("event") == "done"}
        recovered = 0
        for start in (r for r in rows if r.get("event") == "start" and r["slice"] not in done):
            target, _ = start["leg"].split("@")
            run = tagged_run(self.fdir, target, start["slice"])
            verdict = read_json(os.path.join(run, "verdict.json")) if run else None
            status, verdict = self.accounted(run, verdict, False, live=False)
            if status == "finding":
                self.reset_coverage(start["leg"])
            kind = {"kind": start["kind"]} if "kind" in start else {}
            self.record(self.done_row(start["slice"], start["leg"], status, verdict, run, True, **kind,
                                      **(cmin_row(status, verdict, run) if kind else {})))
            if kind and status == "pass":
                self.cmin_passed(target, verdict)
            elif kind and (counts := cmin_swapped(run)):
                self.cmin_passed(target, {"cmin": counts})  # killed after its swap (C-841-F13)
            record = None if kind else shard_record(run)
            if record is not None and "folded" not in record:
                # A shard slice killed before its fold: its new inputs are lost (OD-L3-161 Q5).
                self.fold_lost += sum(1 for _ in os.scandir(os.path.join(run, "new"))) if os.path.isdir(
                    os.path.join(run, "new")) else 0
            recovered += 1
        return recovered

    def tracked(self, key):
        """`self.runner`, with the run.sh process registered under `key` so that
        `interrupt_all` reaches it; the result says whether it was interrupted."""

        def runner(target, pin, args, on_start=None, **kw):
            def started(p):
                with self.lock:
                    self.procs[key] = p
                if on_start:
                    on_start(p)

            try:
                result = self.runner(target, pin, args, on_start=started, **kw)
            finally:
                with self.lock:
                    self.procs.pop(key, None)
                    interrupted = key in self.interrupted
                    self.interrupted.discard(key)
            result.interrupted = interrupted
            return result

        return runner

    def triage(self, **kw):
        """Triage through tracked run.sh processes, stopping when the unit stops. Logs only
        the aggregate STATUS line: a farm's journal never singles out a private-route target."""
        if self.stop.is_set():
            return

        def stop():  # the unit stops, a bench window is about to start, or node-bench runs
            return self.stop.is_set() or self.bench.blocked(self.lead) is not None

        try:
            # A planted search's triage and recheck run a planted build: O2B4-RUN only (D41).
            tri = run_triage([t for t in self.targets if PLANTED not in t.features],
                             runner=self.tracked("triage"), stop=stop,
                             ready=self.ready_runs, **kw)
            lines, errors = status_lines(self.fdir, self.targets)
            self.triage_errors = max(self.triage_errors, tri.stats["errors"], errors)
            self.log(f"node-fuzz farm: triage errors={tri.stats['errors']}; " + lines[0])
        except Exception as e:  # noqa: BLE001 -- the farm keeps running; the type names no input
            self.triage_errors = max(self.triage_errors, 1)
            self.log(f"node-fuzz farm: triage failed: {type(e).__name__}")

    def request_stop(self, leg):
        """Writes the stop file of the leg's running slice (BL-173), which every libFuzzer worker
        reads at the top of its loop: it prints its final stats and exits 0, and run.sh writes the
        verdict, and starts no more segments. True when a worker of the slice's latest segment is
        past INITED, so that it will read the file soon (libFuzzer reads it only once fuzzing). False
        when the slice has no run directory yet, is building or loading its corpus, or the file
        cannot be written: it needs the SIGINT now."""
        sid = self.slice_ids.get(leg)
        run = (self.slice_runs.get(sid) or tagged_run(self.fdir, leg.split("@")[0], sid)) if sid else ""
        if not run:
            return False
        try:
            write_private(os.path.join(run, "stop"), b"stop\n")  # libFuzzer ignores an empty one
        except OSError:
            return False
        segs = segments(run)
        return bool(segs) and any(INITED_LINE.search(open(path, errors="replace").read())
                                  for path in worker_logs(segs[-1]))

    def escalate(self, leg, p, steps):
        """Signals the process group of `p` (the run.sh process of `leg`) at each step of `steps`,
        `(wait, signal)`, that it outlives: a worker can hang in libFuzzer's interrupt path until
        its ALARM, which writes an empty timeout artifact (BL-173). Ends when the process does."""
        for wait, sig in steps:
            end = time.monotonic() + wait
            while time.monotonic() < end and self.procs.get(leg) is p:
                time.sleep(self.escalate_poll)
            # run_leg reaps the process before it leaves `procs`: never signal a reaped group.
            if self.procs.get(leg) is not p or getattr(p, "returncode", None) is not None:
                return
            self.log(f"node-fuzz farm: a stopped slice is still running: {sig.name}")
            with contextlib.suppress(ProcessLookupError):
                os.killpg(p.pid, sig)

    def interrupt_all(self, why, legs=None):
        """Stops every running slice (or those of `legs`) once: through its stop file (BL-173), or
        with a SIGINT when it is not fuzzing yet (`request_stop`), which libFuzzer answers with its
        final stats. A slice that outlives `stop_wait` after its stop file gets the SIGINT; one that
        outlives `int_wait` after its SIGINT gets a SIGKILL."""
        with self.lock:
            fresh = [(leg, p) for leg, p in self.procs.items()
                     if leg not in self.interrupted and (legs is None or leg in legs)]
            for leg, p in fresh:
                self.interrupted.add(leg)
                if self.request_stop(leg):
                    steps = [(self.stop_wait, signal.SIGINT), (self.int_wait, signal.SIGKILL)]
                else:
                    steps = [(self.int_wait, signal.SIGKILL)]
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(p.pid, signal.SIGINT)
                threading.Thread(target=self.escalate, args=(leg, p, steps), daemon=True).start()
        if fresh:
            self.log(f"node-fuzz farm: interrupting {len(fresh)} slices: {why}")

    def slice(self, leg, cpu_hours):
        target, pin = leg.split("@")
        start = self.clock()
        sid = secrets.token_hex(8)
        self.record({"event": "start", "slice": sid, "leg": leg, "time": round(time.time())})
        shard = self.mode(leg) == SHARD
        with self.cov_lock:
            self.slice_ids[leg] = sid
            if shard:
                self.slice_modes[sid] = SHARD
        try:
            result = self.tracked(leg)(target, pin, ["--cpu-hours", f"{cpu_hours:.4f}", "--jobs",
                                                     str(self.a.jobs), "--run-tag", sid, *(["--shard"] if shard else [])])
            self.sample(leg)  # up to the slice's end
        finally:
            with self.cov_lock:
                self.slice_ids.pop(leg, None)
                self.sampled.pop(sid, None)
                self.slice_modes.pop(sid, None)
                run = self.slice_runs.pop(sid, None)
                if run:
                    self.scan.forget(run)
                    self.union_scan.forget(run)
        status, v = self.accounted(result.run_dir, result.verdict, result.interrupted)
        if labelled := len(suspected_interrupts(result.run_dir)) if result.run_dir else 0:
            self.log(f"node-fuzz farm: {labelled} timeouts written after an interrupt are findings labelled "
                     "suspected_interrupt (BL-173 D18)")
        if status == "finding":
            self.reset_coverage(leg)
        ran_out = status == "fail" and v.get("reason") == BEFORE_INITED
        # Recorded before the slice is acknowledged: a kill after this keeps it accounted for.
        row = self.done_row(sid, leg, status, v, result.run_dir, False,
                            wall_s=round(self.clock() - start), jobs=self.a.jobs,
                            slice_s=round(cpu_hours * 3600 / self.a.jobs, 1))
        if ran_out and all(k in row for k in REPLAY_KEYS):
            row[RAN_OUT] = True
        self.record(row, settles=leg)
        private = not legs_named([leg], self.targets)
        self.log(f"node-fuzz farm: a private-route slice ended: {status}" if private else result.line)
        if status == "pass":
            self.drop_pre_cmin(target)
        if status == "finding":
            self.triage(only={target})
        return BEFORE_INITED if ran_out else status

    def cmin(self, leg):
        """Minimises the leg's working corpus (run.sh --minimize-corpus, BL-841), recorded as a
        `cmin` row: its CPU counts in the cap and in the replay share, and it earns no credit.
        Returns "finding" when the merge found one, else "cmin"."""
        target, pin = leg.split("@")
        start = self.clock()
        sid = secrets.token_hex(8)
        self.record({"event": "start", "slice": sid, "leg": leg, "time": round(time.time()), "kind": CMIN})
        result = self.tracked(leg)(target, pin, ["--minimize-corpus", "--run-tag", sid])
        status, v = self.accounted(result.run_dir, result.verdict, result.interrupted)
        self.record(self.done_row(sid, leg, status, v, result.run_dir, False, kind=CMIN,
                                  wall_s=round(self.clock() - start), **cmin_row(status, v, result.run_dir)),
                    settles=leg)
        private = not legs_named([leg], self.targets)
        self.log(f"node-fuzz farm: a private-route leg's minimisation ended: {status}" if private else result.line)
        if status == "pass":
            self.cmin_passed(target, v)
        elif counts := cmin_swapped(result.run_dir):
            self.cmin_passed(target, {"cmin": counts})  # killed after its swap (C-841-F13)
        if status == "finding":
            self.reset_coverage(leg)
            self.triage(only={target})
            return "finding"
        return CMIN

    def cmin_passed(self, target, verdict):
        """Records a passing minimisation's corpus counts, after it (from which the next one is due)
        and before it."""
        counts = verdict.get("cmin") or {}
        after, before = counts.get("files_after"), counts.get("files_before")
        if is_count(after, 1):
            append_row(self.fdir, {"target": target, "files_after": after, "night": self.a.night,
                                   **({"files_before": before} if is_count(before, 1) else {}),
                                   "time": round(time.time())}, name=CMIN_HISTORY)

    def cmin_estimate(self, leg, records):
        """A minimisation of the leg's target in CPU-h (C-841-F7) and its wall seconds (F8): its
        target's last one's CPU, else one pass over the target's corpus (its old one, which run.sh
        rolls back and merges, when a minimisation cut between its renames left no corpus/; BL-841b
        r2 R2-2) at the leg's latest measured replay CPU per input (`replay_cost`), or
        CMIN_INPUT_CPU_S without one (r1 L3); its wall time at the leg's last rate (one process),
        never under --cmin-minutes."""
        name, rows = leg.split("@")[0], [*self.history, *records]
        cpu = next((r["cpu_hours"] for r in reversed(rows)
                    if r.get("kind") == CMIN and r["leg"].split("@")[0] == name and r["cpu_hours"] > 0), None)
        if cpu is None:
            cost = next((c for r in reversed(rows) if r["leg"] == leg and (c := replay_cost([r]))), None)
            tdir = os.path.join(self.fdir, name)
            cut = not os.path.isdir(os.path.join(tdir, "corpus"))
            files = corpus_files(tdir, "corpus.pre-cmin" if cut else "corpus")
            cpu = files * (cost["c"] if cost else CMIN_INPUT_CPU_S) / 3600
        est = self.replay_estimate(leg, records)
        rate = est[1] if est and est[1] > 0 else 1.0
        return cpu, max(self.a.cmin_minutes * 60, cpu * 3600 / min(rate, 1.0))

    def mode(self, leg):
        """The leg's replay mode in this launch (BL-841b): shard under --replay-mode shard, but jobs
        for a target whose plateau rule leaves ft out, which the shard signal (ft only) can't serve
        (OD-L3-161 Q6; logged once)."""
        if getattr(self.a, "replay_mode", "jobs") != SHARD:
            return "jobs"
        if not plateau_rule(leg.split("@")[0], self.targets).ft:
            if leg not in self.jobs_mode:
                self.jobs_mode.add(leg)
                who = leg if legs_named([leg], self.targets) else "a private-route leg"
                self.log(f"node-fuzz farm: {who} runs in jobs mode: its plateau rule leaves ft out, and a shard "
                         "run's signal is ft only (plateau_ft=false)")
            return "jobs"
        return SHARD

    def replay_held(self, leg, records):
        """Whether the leg is held for the launch as replay-bound at the cap (BL-841b §4.1,
        C-841-N2): its estimate is a lower bound from a slice that already had the current
        --max-slice-minutes and ran out of its budget before every worker reached INITED (its done
        row's `ran_out`, from run.sh's BEFORE_INITED reason; BL-841b r2 R2-1), with no counted slice
        after it. Logged once per launch, with the minimisation's advice, and that a sharded run or
        a longer cap is the remedy when its last minimisation removed nothing."""
        est = self.replay_estimate(leg, records)
        bound = est[2] if est else None
        if not bound or bound.get("slice_s", 0) < self.a.max_slice_minutes * 60 * (1 - 1e-9):
            return False
        if leg not in self.held_bound:
            self.held_bound.add(leg)
            who = leg if legs_named([leg], self.targets) else "a private-route leg"
            last = load_cmin_rows(self.fdir).get(leg.split("@")[0]) or {}
            nothing = last.get("files_before") is not None and last["files_before"] == last["files_after"]
            self.log(f"node-fuzz farm: {who} is replay-bound; minimise its corpus (--cmin)"
                     + ("; its last minimisation removed nothing: run it sharded (--replay-mode shard) or "
                        "raise --max-slice-minutes" if nothing else ""))
        return True

    def cmin_due(self, leg):
        """Whether the leg's target is minimised before the leg's first slice of the launch: never
        a floor-gated target (OD-L3-157 (3)) or one whose last minimisation's old corpus is still
        kept; one whose minimisation was cut between its two renames (no corpus beside the old
        one: run.sh's mode rolls it back); one with no minimisation yet, or whose working corpus
        has grown by --cmin-growth since its last (a shard-mode leg's by at least SHARD_CMIN_GROWTH,
        OD-L3-161 Q8)."""
        name = leg.split("@")[0]
        t = next((t for t in self.targets if t.name == name), None)
        if t is None or floor_gated(t):
            return False
        tdir = os.path.join(self.fdir, name)
        if os.path.isdir(os.path.join(tdir, "corpus.pre-cmin")):
            return not os.path.isdir(os.path.join(tdir, "corpus"))
        files = corpus_files(tdir)
        last = load_cmin_history(self.fdir).get(name)
        growth = max(self.a.cmin_growth, SHARD_CMIN_GROWTH) if self.mode(leg) == SHARD else self.a.cmin_growth
        return files > 0 and (last is None or files >= last * (1 + growth))

    def drop_pre_cmin(self, target):
        """After a passing slice, the old corpus a minimisation kept goes, through rm-if-idle
        (the farm is its only remover; run.sh never deletes it)."""
        tdir = os.path.join(self.fdir, target)
        pre = os.path.join(tdir, "corpus.pre-cmin")
        if not (os.path.isdir(pre) and corpus_files(tdir) > 0):  # never the only copy (BL-841 r1 F5)
            return
        try:
            r = subprocess.run([os.environ.get("NODE_FUZZ_RM_IF_IDLE") or RM_IF_IDLE, pre],
                               capture_output=True, timeout=600)
            ok = r.returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            ok = False
        if not ok:
            self.log("node-fuzz farm: a minimisation's old corpus could not be removed; it is kept")

    def replay_trend(self):
        """Logs each launched leg whose replay trend over the nights so far is closing
        (`replay_projection`, BL-841b), once per launch."""
        a = self.a
        for leg in sorted(self.selected):
            points, _, _, epoch = replay_points(self.fdir, leg)
            p = replay_projection(points, a.replay_target, a.slice_minutes, a.max_slice_minutes, epoch=epoch)
            if p and p["closing"]:
                self.replay_alarms.add(leg)
                who = leg if legs_named([leg], self.targets) else "a private-route leg"
                self.log(f"node-fuzz farm: {who} replay margin closing: projected share {p['share']:.3f} at the next "
                         f"leg (target {a.replay_target:g}); c {p['c_growth']:+.0%}, inputs {p['n_growth']:+.0%} "
                         f"over {p['nights']} nights")

    def replay_estimate(self, leg, records):
        """The leg's replay (CPU-s per worker) and rate (CPU-s per wall second per worker), from its
        slices of the launch's replay mode (a shard worker loads 1/J of what a jobs worker does,
        BL-841b) since its target's latest passing minimisation (whose replay is the corpus the next
        slice loads), over the earlier nights' and this night's rows in order: a counted slice's
        measured replay, raised by any later slice's lower bound (`replay_floor`: a slice that ran
        out during its replay, BL-841 r1 F1), each with its own slice's rate; and the row of that
        bound when its slice ran out of its budget before every worker reached INITED (`ran_out`,
        §4.1, BL-841b r2 R2-1), or None otherwise: a counted slice's measured replay, one whose
        workers all reached INITED before it was interrupted or ended in a finding (r1 H1), or the
        floor of a slice cut short before that, or of a row without `ran_out` (an older ledger). A
        minimisation of the leg's target scales it by the fraction of the corpus it kept, and one
        without counts clears it (Q13). None without either."""
        name, est, mode = leg.split("@")[0], None, self.mode(leg)
        for r in (*self.history, *records):
            if r.get("kind") == CMIN:
                if r["leg"].split("@")[0] == name and (r["status"] == "pass" or CMIN_FILES in r):
                    # Its corpus is the next slice's (OD-L3-161 Q13): the kept fraction scales the
                    # estimate, one that kept every input leaves it and its bound, and a row without
                    # the counts (phase 1's) clears it.
                    kept = r.get(CMIN_FILES)
                    if kept is None:
                        est = None
                    elif est and kept[1] < kept[0]:
                        est = (est[0] * kept[1] / kept[0], est[1], None)
            elif (r["leg"] == leg and r.get("replay_mode", "jobs") == mode and r.get("jobs") and r.get("wall_s")
                  and r["cpu_hours"] > 0):
                rate = r["cpu_hours"] * 3600 / (r["jobs"] * r["wall_s"])
                if replay_counted(r):
                    est = (r["replay_max_s"], rate, None)
                elif replay_floor(r) > (est[0] if est else 0):
                    est = (replay_floor(r), rate, r if r.get(RAN_OUT) else None)
        return est

    def slice_seconds(self, leg, records):
        """The leg's slice length (BL-841): long enough that the replay is at most
        --replay-target of what each worker spends (`replay_estimate`), between --slice-minutes
        and --max-slice-minutes; --slice-minutes without an estimate. At the cap the leg is
        replay-bound, logged once per launch."""
        a, est = self.a, self.replay_estimate(leg, records)
        floor, cap = a.slice_minutes * 60, a.max_slice_minutes * 60
        if est is None:
            return floor
        replay, rate, _ = est
        want = replay / (a.replay_target * rate)
        if want > cap and leg not in self.replay_bound:
            self.replay_bound.add(leg)
            who = leg if legs_named([leg], self.targets) else "a private-route leg"
            self.log(f"node-fuzz farm: {who} is replay-bound: replay {replay:.0f} CPU-s needs a "
                     f"{want / 60:.0f} min slice, cap {a.max_slice_minutes:g}")
        return min(max(want, floor), cap)

    def work(self):
        """Runs slices until every leg of the launch (`--only`'s, or all) is suspected of a
        plateau or has spent its share (`complete`), the night's cap is spent (`cap`), the
        launch's hours are up (`deadline`), or the unit stops (`stopped`). Shares are the cap
        split among the launch's legs (`launch_shares`); nothing is redistributed, and no slice is
        given more than what is left of the cap after all the CPU time of the night's slices
        (credited or not) and the slices running, so no sequence of launches exceeds it (Sol r1
        M4); a corpus minimisation (--cmin) reserves its estimate (`cmin_estimate`, C-841-F7), so only
        one that runs over it can pass the cap. A leg that restarts its share after a
        finding gets what the cap still allows. A leg suspected of a plateau (`suspected`; checked on every poll, so its running slice is cut within a poll)
        stops for the night; that is a heuristic that saves CPU, and only `certify` certifies a plateau
        (DEC-186). Yields to bench windows and node-bench runs: no slice starts that would overlap
        a reserved window, and running slices are interrupted when one is about to start or
        node-bench is running. A leg retires for the rest of the launch after a finding (CS0
        §9.3: the leg has failed; the finding is triaged) or after two failures without one (a
        broken build, say). A slice that ran out during its corpus load sizes the leg's next
        (`replay_estimate`) and is no failure while it was below --max-slice-minutes and longer
        than the leg's last one that did (BL-841 r1 F1). A leg whose estimate is a bound at the
        current cap is held instead (`replay_held`, BL-841b §4.1)."""
        a = self.a
        deadline = self.clock() + a.hours * 3600
        recovered = self.reconcile()
        self.log(f"node-fuzz farm: night {a.night}, {len(self.records())} slices "
                 f"done ({recovered} recovered after a kill), cores {a.cores}, jobs {a.jobs} per slice, "
                 f"{len(self.selected)}/{len(self.plan['legs'])} legs, plateau window "
                 f"{a.plateau_minutes:g} min")
        if not self.bench.blocked(3600):
            self.triage(recheck=True)  # the open vectors, replayed against today's code
        held = self.legs_with_open_findings() & self.selected
        if held:
            self.log(f"node-fuzz farm: {len(held)} legs held by open findings")
        suspects = {leg for leg in self.selected - held if self.suspicion(leg)}
        if suspects:
            self.log(f"node-fuzz farm: {len(suspects)} legs suspected of a plateau earlier in the night")
        self.replay_trend()
        plan = launch_shares(self.plan["legs"], self.plan["cpu_hours"], self.selected)
        parallel = max(1, a.cores // a.jobs)
        min_slice = a.min_slice_minutes * 60
        running, retired, failures = {}, set(held), {}
        planned, in_replay = {}, {}  # each leg's running slice's seconds; its longest that ran out replaying
        waiting = None
        with ThreadPoolExecutor(max_workers=parallel) as pool:
            while True:
                for leg, fut in list(running.items()):
                    if not fut.done():
                        continue
                    running.pop(leg)
                    with self.lock:  # a slice that raised before its done row
                        self.given.pop(leg, None)
                    status = fut.result() if fut.exception() is None else "fail"
                    if status == CMIN:  # a minimisation, done or not, is no slice of the leg
                        continue
                    if status == BEFORE_INITED:
                        s = planned.get(leg, 0)
                        grew = in_replay.get(leg, 0) < s < a.max_slice_minutes * 60 * (1 - 1e-9)
                        in_replay[leg] = max(in_replay.get(leg, 0), s)
                        if grew:
                            continue
                        status = "fail"
                    failures[leg] = (failures.get(leg, 0) + 1) if status == "fail" else 0
                    if status == "finding" or (status == "fail" and failures[leg] >= 2):
                        retired.add(leg)
                for leg in self.selected - retired - suspects:
                    if leg in running:
                        self.sample(leg)
                    p = self.suspicion(leg)
                    if not p:
                        continue
                    suspects.add(leg)
                    self.suspect(leg, p)
                    if leg in running:  # cut within a poll; the slice ends as interrupted
                        self.interrupt_all("a private-route leg is suspected of a plateau"
                                           if not legs_named([leg], self.targets) else
                                           f"{leg} is suspected of a plateau at ft {p['ft']}", legs={leg})
                if self.stop.is_set():
                    outcome = "stopped"
                    break
                left = deadline - self.clock()
                if left <= self.margin:
                    self.interrupt_all("the deadline")
                    outcome = "deadline"
                    break
                with self.lock:  # a slice's done row and its `given` entry, never both or neither
                    records = self.records()
                    pending = sum(self.given.values())
                spent = credit_by_leg(records)
                cap_left = self.plan["cpu_hours"] - total(records, "cpu_hours") - pending
                open_plan = {leg: b for leg, b in plan.items() if leg not in retired | suspects | self.held_bound}
                # Each leg's slice length (BL-841), capped by the deadline and a reserved window.
                want = {leg: self.slice_seconds(leg, records) for leg in open_plan}
                slice_s = left - self.margin
                blocked = self.bench.blocked(min(max(want.values(), default=a.slice_minutes * 60), slice_s)
                                             + self.lead)
                if blocked:
                    reason, until = blocked
                    if until <= self.imminent:
                        self.interrupt_all(reason)
                    slice_s = min(slice_s, until - self.lead)
                hold = blocked[0] if blocked and slice_s < min_slice else None
                if hold and hold != waiting:
                    self.log(f"node-fuzz farm: waiting: {hold}")
                waiting = hold
                while len(running) < parallel and slice_s >= min_slice and cap_left > DONE_EPS:
                    leg = next_leg(open_plan, spent, running)
                    if leg is None:
                        break
                    if a.cmin and leg not in self.cmin_checked:
                        # Only with --cmin (BL-841 r1 F3), before the leg's first slice of the
                        # launch, under the leg's run lock (run.sh's), counted in the cap by its done
                        # row. It reserves its estimated CPU in the cap (C-841-F7) and starts only
                        # when the time to the deadline or a window covers its estimated wall time
                        # (F8); otherwise it waits, unchecked, and the leg's slices go on meanwhile.
                        if not self.cmin_due(leg):
                            self.cmin_checked.add(leg)
                        else:
                            cmin_cpu, cmin_wall = self.cmin_estimate(leg, records)
                            if cmin_cpu <= cap_left and cmin_wall <= slice_s:
                                self.cmin_checked.add(leg)
                                cap_left -= cmin_cpu
                                self.given[leg] = cmin_cpu
                                running[leg] = pool.submit(self.cmin, leg)
                                continue
                            if leg not in self.cmin_deferred:
                                self.cmin_deferred.add(leg)
                                who = leg if legs_named([leg], self.targets) else "a private-route leg"
                                self.log(f"node-fuzz farm: {who}'s minimisation waits: its estimate "
                                         f"({cmin_cpu:.2f} CPU-h, {cmin_wall / 60:.0f} min) is over what the "
                                         f"cap or the time left allows")
                    if self.replay_held(leg, records):
                        open_plan.pop(leg, None)
                        continue
                    cpu = min(open_plan[leg] - spent.get(leg, 0), a.jobs * min(want[leg], slice_s) / 3600, cap_left)
                    cap_left -= cpu
                    self.given[leg] = cpu
                    planned[leg] = cpu * 3600 / a.jobs
                    running[leg] = pool.submit(self.slice, leg, cpu)
                if not running and waiting is None:
                    # Nothing left to schedule: the cap spent, every leg suspected or its share
                    # spent, every remaining leg retired, or too little time for a slice.
                    if next_leg(open_plan, spent, {}) is not None:
                        outcome = "cap" if cap_left <= DONE_EPS else "deadline"
                    elif next_leg({leg: plan[leg] for leg in retired | self.held_bound if leg in plan}, spent, {}) is not None:
                        outcome = "legs-retired"
                    else:
                        outcome = "complete"
                    break
                self.stop.wait(a.poll)
            if outcome == "stopped":
                self.interrupt_all("the unit is stopping")
        self.log(self.summary(outcome, len(retired), len(suspects)))
        self.release()
        return outcome

    def legs_with_open_findings(self):
        """The plan's legs held for the launch because they have failed (CS0 §9.3): a leg whose
        target has an open triage cluster seen on its pin; a leg whose last slice found
        something in a run not triaged yet; every leg of a target whose triage state cannot be
        read. Once a recheck fixes the cluster the leg runs its whole share afresh
        (`credit_by_leg`)."""
        held, last = set(), {}
        for r in self.records():
            last[r["leg"]] = r
        for t in self.targets:
            try:
                state = load_triage(self.fdir, t.name)
            except ValueError:
                held |= {f"{t.name}@{p}" for p in t.pins}
                continue
            for c in state["clusters"].values():
                if c["state"] == "open":
                    held |= {f"{t.name}@{p}" for p in (c["pins"] or t.pins)}
            for p in t.pins:
                r = last.get(f"{t.name}@{p}")
                if r and r["status"] == "finding" and r["run"] not in state["runs"]:
                    held.add(f"{t.name}@{p}")
        return held & set(self.plan["legs"])

    def summary(self, outcome, retired=0, suspected_=0):
        """The launch's line. Its credit is each selected leg's, capped by its share, out of the
        launch's cap. Under --only the shares are the launch's (`launch_shares`, as the launch
        scheduled them); otherwise they are the plan's own, whose 4-dp rounding `launch_shares`
        would renormalise into a figure 0.01 off."""
        records = self.records()
        replay = replay_share(records)
        slices = [r for r in records if r.get("kind") != CMIN]
        costs = [cost["c"] for leg in {r["leg"] for r in slices}
                 if (cost := replay_cost([r for r in slices if r["leg"] == leg]))]
        if self.selected == set(self.plan["legs"]):
            shares, cap = self.plan["legs"], self.plan["cpu_hours"]
        else:
            shares = launch_shares(self.plan["legs"], self.plan["cpu_hours"], self.selected)
            cap = sum(shares.values())
        credited = sum(min(c, shares[leg]) for leg, c in credit_by_leg(records).items() if leg in shares)
        line = (f"node-fuzz farm night={self.a.night} slices={len(slices)} cmin={len(records) - len(slices)} "
                f"cpu_hours={total(records, 'cpu_hours'):.2f} credited_cpu_hours={credited:.2f}/{cap:g} "
                f"executions={total(records, 'executions')} "
                + " ".join(f"{k}={total(records, k)}" for k in FINDING_KEYS)
                + f" slow_units={total(records, 'slow_units')}"
                + f" interrupted={interrupted_count(slices)} retired_legs={retired}"
                + f" suspected_legs={suspected_}"
                + f" replay_share={share_text(replay['share'])} replay_share_max={share_text(replay['max'])}"
                + f" replay_unknown={replay['unknown']}"
                + f" replay_c_max={share_text(max(costs, default=None))} replay_alarm={len(self.replay_alarms)}"
                + f" replay_held={len(self.held_bound)} fold_lost={self.fold_lost}"
                + f" triage_errors={self.triage_errors} ledger_repaired={self.ledger_repaired}"
                + f" outcome={outcome}")
        write_private(os.path.join(self.dir, "summary"), line + "\n")
        return line


def interrupted_count(records):
    return sum(r["status"] == "interrupted" for r in records)


def unit_name(night):
    return UNIT_PREFIX + night


def unit_state(night):
    """`systemctl --user is-active` of the night's service: active, inactive, failed, …"""
    return subprocess.run(["systemctl", "--user", "is-active", unit_name(night) + ".service"],
                          capture_output=True, text=True).stdout.strip() or "-"


def farm_units(active_only=True):
    """The farm's loaded services and timers (`node-fuzz-farm-*`), by name."""
    out = subprocess.run(["systemctl", "--user", "list-units", "--plain", "--no-legend",
                          *([] if active_only else ["--all"]), UNIT_PREFIX + "*"],
                         capture_output=True, text=True).stdout
    return sorted(row.split()[0] for row in out.splitlines() if row.split())


def systemd_launch(unit, description, quota, worker, print_only, extra=(), env=None):
    """Starts `worker` in a transient `systemd-run --user` unit at the lowest priority (DEC-184
    (4): it takes only what nothing else on the box wants); with `print_only`, prints the command
    instead and returns False. The description is fixed: systemd's default, the command line,
    would put a target's name in the journal (PLAN §7.5). The caller's TMPDIR is left out: it
    may be a session's own directory that outlives nothing; run.sh points each target's TMPDIR
    into its run."""
    env = {**{k: os.environ[k] for k in ("PATH", "HOME", "DASH_EMBARGO_DIR", "CARGO_BUILD_JOBS",
                                         "CARGO_HOME", "RUSTUP_HOME", "RUSTC_WRAPPER", "NODE_FUZZ_RM_IF_IDLE")
              if k in os.environ}, **(env or {})}
    cmd = ["systemd-run", "--user", f"--unit={unit}", f"--description={description}", "--collect",
           f"--working-directory={ROOT}", "-p", quota, "-p", "KillMode=mixed",
           "-p", "TimeoutStopSec=180", "-p", "Nice=19", "-p", "IOSchedulingClass=idle",
           "-p", "CPUWeight=10", *[f"--setenv={k}={v}" for k, v in env.items()], *extra]
    if print_only:
        print(" ".join(cmd + worker).replace(os.environ["DASH_EMBARGO_DIR"], "$DASH_EMBARGO_DIR"))
        return False
    r = subprocess.run(cmd + worker, capture_output=True, text=True)
    if r.returncode:
        last = r.stderr.strip().splitlines()[-1] if r.stderr.strip() else r.returncode
        die(f"systemd-run failed: {last}")
    return True


def farm_launch(a):
    if a.cores < 1 or a.jobs < 1 or a.jobs > a.cores:
        die("--cores and --jobs must be positive, with --jobs <= --cores")
    fuzz_dir()
    problems = check_registry()
    if problems:
        die(f"the registry is inconsistent: {problems[0]}")
    unit = unit_name(a.night)
    running = [u for u in farm_units() if u.endswith(".service")]
    if running:
        die(f"a farm is already running ({', '.join(running)}); stop it first")
    worker = [sys.executable, "-I", os.path.abspath(__file__), "farm", "work", "--night", a.night,
              "--hours", str(a.hours), "--cores", str(a.cores), "--jobs", str(a.jobs),
              "--slice-minutes", str(a.slice_minutes), "--min-slice-minutes", str(a.min_slice_minutes),
              "--max-slice-minutes", str(a.max_slice_minutes), "--replay-target", str(a.replay_target),
              "--cmin-growth", str(a.cmin_growth), *(["--cmin"] if a.cmin else []),
              "--cmin-minutes", str(a.cmin_minutes), "--replay-mode", a.replay_mode,
              "--poll", str(a.poll), "--windows", a.windows, "--plateau-minutes", str(a.plateau_minutes),
              *(["--only", *a.only] if a.only else [])]
    quota = f"CPUQuota={a.cores * 100}%"
    timer = [f"--on-calendar={a.on_calendar}", "--timer-property=AccuracySec=1min"] if a.on_calendar else []
    if not systemd_launch(unit, "node-fuzz farm", quota, worker, a.print_only, timer):
        return
    on = f", on {a.on_calendar}" if a.on_calendar else ""
    print(f"node-fuzz farm: launched {unit} ({quota}, {a.hours:g} h{on})")


def cmin_ready(fdir, target):
    """run.sh --minimize-corpus's check, under the triage lock it holds: 0 when the target has no
    open triage cluster and no job file; 3 an open cluster, 4 a job file, 5 an unreadable triage
    state (OD-L3-157 (c)). Counts nothing and names nothing."""
    try:
        state = load_triage(fdir, target)
    except ValueError:
        return 5
    if any(c["state"] == "open" for c in state["clusters"].values()):
        return 3
    jobs = os.path.join(fdir, target, "jobs")
    try:
        if os.listdir(jobs):
            return 4
    except FileNotFoundError:
        pass
    except OSError:
        return 5
    return 0


def replay_share_report(a, targets):
    """`fuzzctl.py replay-share <night> [--leg L]` (BL-841 §5): each leg's replay share, its
    worst counted slice and its slices left out, then a line per slice (counts and CPU only;
    another leg than a public one is counted, never named, unless --leg names it). Exits 0 when
    every counted slice and each leg's share (its minimisations' CPU included) are under
    --target, 1 when one is not, and 2 when a leg has no counted slice."""
    fdir = fuzz_dir()
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*(\.[A-Za-z0-9_-]+)*", a.night):
        die("the night must be a plain name")
    if not 0 < a.target < 1:
        die("--target must be above 0 and below 1")
    d = farm_state_dir(fdir, a.night)
    try:
        plan = load_plan(os.path.join(d, "plan.json"), a.night, targets)
        records = load_records(d, plan["legs"]) if plan else []
    except FarmStateError as e:
        die(f"night {a.night}: unusable state: {e}")
    if plan is None:
        die(f"night {a.night} has no plan")
    if a.leg is not None and a.leg not in plan["legs"]:
        die("--leg is not one of the night's legs")
    legs_ = [a.leg] if a.leg else sorted({r["leg"] for r in records})
    named = set(legs_named(legs_, targets)) | ({a.leg} if a.leg else set())
    rc = 0 if legs_ else 2
    for leg in legs_:
        rows = [r for r in records if r["leg"] == leg]
        rep = replay_share(rows)
        over = (rep["share"] is not None and rep["share"] >= a.target) or (rep["max"] or 0) >= a.target
        result = "over" if over else "unknown" if rep["share"] is None else "pass"
        if over:
            rc = 1
        elif rep["share"] is None and rc == 0:
            rc = 2
        cost = replay_cost(rows)
        modes = {r.get("replay_mode", "jobs") for r in rows if r.get("kind") != CMIN}
        print(f"node-fuzz replay-share night={a.night} leg={leg if leg in named else 'private-route'} "
              f"mode={modes.pop() if len(modes) == 1 else 'mixed' if modes else '-'} "
              f"slices={len(rows) - sum(r.get('kind') == CMIN for r in rows)} counted={rep['counted']} "
              f"replay_share={share_text(rep['share'])} replay_share_max={share_text(rep['max'])} "
              f"replay_unknown={rep['unknown']} cmin_cpu_s={rep['cmin_cpu_s']:.0f} "
              f"c={share_text(cost and cost['c'])} target={a.target:g} result={result}")
        for r in rows:
            cpu = r["cpu_hours"] * 3600
            share = share_text(r["replay_cpu_s"] / cpu) if replay_counted(r) else "unknown"
            c = share_text(r["replay_cpu_s"] / r[REPLAY_INPUTS]) if REPLAY_INPUTS in r else "-"
            mode = "-" if r.get("kind") == CMIN else r.get("replay_mode", "jobs")
            print(f"node-fuzz replay-share slice={r['slice']} kind={r.get('kind', 'fuzz')} status={r['status']} "
                  f"mode={mode} cpu_s={cpu:.0f} replay_s={r.get('replay_cpu_s', '-')} "
                  f"workers={r.get('inited_workers', '-')}/{r.get('workers', '-')} inputs={r.get(REPLAY_INPUTS, '-')} "
                  f"c={c} imbalance={r.get('replay_imbalance', '-')} share={share}")
    return rc


def replay_trend_report(a):
    """`fuzzctl.py replay-trend <target>@<pin>` (BL-841b): the leg's replay cost per night (counts
    and CPU only), and the next leg's projection against --target. Exits 0 when the margin holds,
    1 when it is closing (`replay_projection`), and 2 with fewer than two nights of the newest epoch
    (`cmins`, the minimisations seen) to read."""
    fdir = fuzz_dir()
    if not 0 < a.target < 1 or not 0 < a.replay_target < 1 or not 0 < a.slice_minutes <= a.max_slice_minutes:
        die("--target and --replay-target must be above 0 and below 1, and --slice-minutes at most "
            "--max-slice-minutes")
    points, unusable, ambiguous, epoch = replay_points(fdir, a.leg)
    for p in points:
        print(f"node-fuzz replay-trend leg={a.leg} night={p['night']} inputs={p['n']:.0f} c={p['c']:.3f} "
              f"replay_s={p['replay_s']:.0f} rate={share_text(p['rate'])} share={share_text(p['share'])} "
              f"mode={p['mode']} cmins={p['cmins']}")
    proj = replay_projection(points, a.replay_target, a.slice_minutes, a.max_slice_minutes, epoch=epoch)
    if proj is None:
        print(f"node-fuzz replay-trend leg={a.leg} cmins={epoch} nights={len(points)} unusable={unusable} "
              f"ambiguous={ambiguous} result=unknown")
        return 2
    print(f"node-fuzz replay-trend leg={a.leg} cmins={epoch} nights={proj['nights']} unusable={unusable} "
          f"ambiguous={ambiguous} "
          f"projected_inputs={proj['n']:.0f} projected_c={proj['c']:.3f} projected_replay_s={proj['replay_s']:.0f} "
          f"slice_minutes={proj['slice_s'] / 60:.0f} projected_share={proj['share']:.3f} target={a.target:g} "
          f"margin={a.target - proj['share']:.3f} c_growth={proj['c_growth']:.3f} n_growth={proj['n_growth']:.3f} "
          f"result={'closing' if proj['closing'] else 'ok'}")
    return 1 if proj["closing"] else 0


def farm_status(a, targets):
    fdir = fuzz_dir()
    failed = False
    farm = os.path.join(fdir, ".farm")
    nights = sorted(os.listdir(farm)) if os.path.isdir(farm) else []
    print("node-fuzz farm units: " + (" ".join(farm_units()) or "none active"))
    # The old corpora minimisations keep until the farm removes them, and swaps cut between their
    # renames: counts only (BL-841 r1 F10).
    kept = [os.path.isdir(os.path.join(fdir, t.name, "corpus")) for t in targets
            if os.path.isdir(os.path.join(fdir, t.name, "corpus.pre-cmin"))]
    print(f"node-fuzz farm minimisations: old_corpora_kept={sum(kept)} cut_swaps={len(kept) - sum(kept)}")
    for night in ([a.night] if a.night else nights):
        d = farm_state_dir(fdir, night)
        try:
            plan = load_plan(os.path.join(d, "plan.json"), night, targets)
            records = load_records(d, plan["legs"]) if plan else []
        except FarmStateError as e:
            print(f"node-fuzz farm night={night}: unusable state: {e}")
            failed = True
            continue
        if plan is None:
            print(f"node-fuzz farm night={night}: no plan")
            continue
        try:
            suspicions = standing(*load_coverage(d, plan["legs"]), targets)
        except FarmStateError as e:
            print(f"node-fuzz farm night={night}: unusable state: {e}")
            failed = True
            continue
        spent = credit_by_leg(records)
        done = sum(min(spent.get(leg, 0), b) for leg, b in plan["legs"].items())
        complete = sum(spent.get(leg, 0) >= b - DONE_EPS for leg, b in plan["legs"].items())
        slices = [r for r in records if r.get("kind") != CMIN]
        replay = replay_share(records)
        print(f"node-fuzz farm night={night} unit={unit_state(night)} slices={len(slices)} "
              f"cmin={len(records) - len(slices)} "
              f"cpu_hours={total(records, 'cpu_hours'):.2f} credited_cpu_hours={done:.2f}/{plan['cpu_hours']:g} "
              f"legs_complete={complete}/{len(plan['legs'])} legs_suspected={len(suspicions)}/{len(plan['legs'])} "
              + " ".join(f"{k}={total(records, k)}" for k in FINDING_KEYS)
              + f" slow_units={total(records, 'slow_units')} interrupted={interrupted_count(slices)}"
              + f" replay_share={share_text(replay['share'])} replay_unknown={replay['unknown']}")
        named = legs_named(sorted(suspicions), targets)
        if named:
            print(f"node-fuzz farm night={night} " + " ".join(
                suspicion_token(leg, suspicions[leg]) for leg in named))
    certs = certificates(fdir, targets)
    named = legs_named(certs, targets)
    print(f"node-fuzz certify: {len(certs)} legs with a certify run, "
          f"{sum(c[0] == 'certified' for c in certs.values())} certified"
          + "".join(f" {leg}={certificate_token(certs[leg][0], certs[leg][2])}" for leg in named)
          + (f"; {len(certs) - len(named)} other legs (counts only)" if len(certs) > len(named) else ""))
    return not failed


def standing(history, marks, signals, targets):
    """The legs the farm stopped on a suspected plateau whose history still shows it over the
    suspicion's own window, under its target's rule and its history's signal: a later launch with a
    longer window may have run the leg again."""
    return {leg: p for leg, p in marks.items()
            if suspected(history.get(leg, []), p["window_minutes"] * 60, plateau_rule(leg.split("@")[0], targets),
                         signals.get(leg) == UNION_SIGNAL)}


def suspicion_token(leg, p):
    """A suspected plateau: what the farm stopped on, never a certificate (DEC-186)."""
    return f"{leg}=plateau-suspected@ft{p['ft']}({p['window_minutes']:g}m)"


def farm_plateau_line(fdir, targets):
    """The STATUS line of the latest farm night (by its ledger's last write): how many legs the
    farm stopped on a suspected plateau, with each positively public leg named and every other
    leg counted only (PLAN §7.5, Sol r1 M3); None when there is no farm night. The night's own
    plan names its legs, so a registry changed since still reports. A suspicion is a heuristic;
    `certify_line` reports certification."""
    farm = os.path.join(fdir, ".farm")
    nights = [(os.stat(ledger_path(farm_state_dir(fdir, n))).st_mtime, n) for n in listdir_or_empty(farm)
              if os.path.isfile(ledger_path(farm_state_dir(fdir, n)))]
    if not nights:
        return None
    night = max(nights)[1]
    d = farm_state_dir(fdir, night)
    plan = read_json(os.path.join(d, "plan.json"))
    try:
        if not isinstance(plan, dict) or not isinstance(plan.get("legs"), dict):
            raise FarmStateError("no plan")
        load_records(d, plan["legs"])
        suspicions = standing(*load_coverage(d, plan["legs"]), targets)
    except FarmStateError:
        return f"SW-FUZZ farm night={night}: unusable state (see `fuzzctl farm status`)"
    named = legs_named(sorted(suspicions), targets)
    hidden = len(suspicions) - len(named)
    return (f"SW-FUZZ farm night={night}: {len(suspicions)}/{len(plan['legs'])} legs stopped on a "
            f"suspected plateau (not certified; see `fuzzctl certify`)"
            + (": " + " ".join(suspicion_token(leg, suspicions[leg]) for leg in named) if named else "")
            + (f"; {hidden} other legs" if hidden else ""))


def certify_line(fdir, targets):
    """The STATUS line of the certify runs (DEC-186): each leg's latest status, the positively
    public legs named and the others counted; None when no leg has a certify run."""
    certs = certificates(fdir, targets)
    if not certs:
        return None
    named = legs_named(certs, targets)
    hidden = [c[0] for leg, c in certs.items() if leg not in named]
    return (f"SW-FUZZ certify (DEC-186): {sum(c[0] == 'certified' for c in certs.values())}/{len(certs)} "
            "legs certified"
            + (": " + " ".join(f"{leg}={certificate_token(certs[leg][0], certs[leg][2])}" for leg in named)
               if named else "")
            + (f"; {len(hidden)} other legs, {hidden.count('certified')} certified" if hidden else ""))


def farm_stop(a):
    """Stops the night's unit and its timer, or without --night every farm unit and timer; the
    worker interrupts its slices and records them, so a later launch resumes."""
    ok = True
    names = {unit_name(a.night) + s for s in (".timer", ".service")} if a.night else None
    for unit in farm_units(active_only=False):
        if names is not None and unit not in names:
            continue
        r = subprocess.run(["systemctl", "--user", "stop", unit], capture_output=True, text=True)
        print(f"node-fuzz farm: stopped {unit}" if r.returncode == 0 else
              f"node-fuzz farm: could not stop {unit}")
        ok &= r.returncode == 0
    return ok


def farm_work(a, targets):
    if a.night == "auto":
        a.night = today()
    try:
        farm = Farm(a, targets, Bench(a.windows), log=lambda line: print(line, flush=True))
    except (FarmStateError, FarmBusy) as e:
        print(f"node-fuzz farm: refusing night {a.night}: {e}", flush=True)
        return 2
    signal.signal(signal.SIGTERM, lambda *_: farm.stop.set())
    signal.signal(signal.SIGINT, lambda *_: farm.stop.set())
    farm.work()
    return 1 if farm.triage_errors else 0


def certify_unit(leg, targets):
    """The certify unit's name: the leg for a public leg, else a random tag (unit names reach
    the journal and `systemctl`: PLAN §7.5)."""
    tag = leg if legs_named([leg], targets) else secrets.token_hex(8)
    return "node-fuzz-certify-" + tag.replace("@", "-")


def certify_launch(a, leg, targets):
    """`certify --launch`: the run in a transient unit at the farm's priority with one core's
    quota, as it is one libFuzzer process. A leg that is not public is passed in the unit's
    environment, not its command line, and its unit name is random (PLAN §7.5)."""
    unit = certify_unit(leg, targets)
    public = bool(legs_named([leg], targets))
    worker = [sys.executable, "-I", os.path.abspath(__file__), "certify", leg if public else "-",
              "--seconds", str(a.seconds), "--wall-cap-hours", str(a.wall_cap_hours)]
    env = {} if public else {"NODE_FUZZ_CERTIFY_LEG": leg}
    if systemd_launch(unit, "node-fuzz certify", "CPUQuota=100%", worker, a.print_only, env=env):
        print(f"node-fuzz certify: launched {unit}")
    return 0


def wall_cap(a):
    return round(a.wall_cap_hours * 3600)


def certify(a, targets, runner=run_leg):
    """`certify <target>@<pin>` (DEC-186 (2), amendment 2): run.sh's certify mode merges the leg's
    corpora into its working corpus, measures it with a replay pass, fuzzes it in one libFuzzer
    process until it has used a window of its own CPU seconds after INITED (or the wall-clock cap
    passes), and measures it again; the certificate is derived from what the run left
    (`read_certificate`) and saved as the run's certificate.json. The leg's runs are triaged
    (without minimising) before the run and after it, so a finding that any run of the leg made,
    the farm's or this one's, blocks success (Sol r2 B2). Exits 0 only when the leg is certified
    and, once the run ended, `eligibility` finds nothing at its completion cutoff, which the
    certificate records (DEC-186 amendment 3 (d))."""
    target, _, pin = a.leg.partition("@")
    t = listed(target, targets)
    if t is None or t.kind == "selftest" or pin not in t.pins:
        die("certify needs a registry leg <target>@<pin>")
    if PLANTED in t.features:
        die("certify refuses a planted search (O2b-4 D41): its node side runs an armed bug")
    try:
        with open(SINK_SOURCE) as f:
            engine = certify_engine(f.read())
    except OSError:
        die("cannot read the harness source to choose the certify engine")
    if engine[0] != "single":
        die("the harness admits libFuzzer's fork mode; certify runs one process until a fork "
            "engine is built and verified (DEC-186 (2))")
    fdir = fuzz_dir()
    run_triage(targets, only=[target], minimize=False)
    blocked = eligibility(fdir, target, pin)
    if blocked:
        die("the leg cannot be certified now: " + "; ".join(blocked))
    if a.launch:
        return certify_launch(a, a.leg, targets)
    with contextlib.suppress(OSError):
        os.nice(19)
    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    result = runner(target, pin, ["--certify", "--seconds", str(a.seconds), "--wall-cap", str(wall_cap(a)),
                                  "--run-tag", secrets.token_hex(8)], cancel=stop.is_set)
    # run.sh released the leg's lock when it exited: a run that started after that (a farm slice)
    # cannot have touched the window, and is the farm's to triage.
    ended = run_stamp()
    if result.run_dir:  # run.sh's copy of the binary, should run.sh have been killed
        with contextlib.suppress(OSError):
            os.remove(os.path.join(result.run_dir, "bin"))
    if not result.run_dir:
        print(f"node-fuzz certify status=no-evidence ({result.line if t.route == 'public' else 'no run'})")
        return 1
    rule, replay = plateau_rule(target, targets), replay_rule(target, targets)
    status, reasons, ev = read_certificate(result.run_dir, certify_floor(target, targets), cert=None, rule=rule,
                                           replay=replay, encoding=registry_encoding(target, targets))
    # Any finding of the leg's runs blocks success too, whenever it was made, and so does any
    # triage error: its runs, this one included, are triaged now, and `eligibility` reads the
    # state at the cutoff, as the status reader does (Sol r2 B2, r3 B3 and M1). The certificate
    # records the evidence, the cutoff, what eligibility found then and the rules it was judged by.
    tri = run_triage(targets, only=[target], minimize=False, ready=[os.path.basename(result.run_dir)])
    blocked = eligibility(fdir, target, pin, ended)
    if ev is not None:
        write_private(os.path.join(result.run_dir, CERTIFICATE),
                      json.dumps({"evidence": ev, "cutoff": ended, "eligibility": blocked,
                                  "plateau": rule.record(), "replay": replay.record()}, sort_keys=True) + "\n")
    if blocked:
        status, reasons = "finding", blocked
    if t.route == "public":
        detail = (f" cov={ev['baseline'][0]}->{ev['final'][0]} ft={ev['baseline'][1]}->{ev['final'][1]}"
                  if ev and ev["baseline"] and ev["final"] else "")
        w = (ev or {}).get("window")
        facts = {**(ev or {}), "wall_seconds": w.get("wall_seconds") if isinstance(w, dict) else None}
        detail += "".join(f" {k}={facts[k]:g}" for k in ("new_lines", "cpu_seconds", "wall_seconds", "executions")
                          if finite(facts.get(k)))
        print(f"node-fuzz certify leg={a.leg} status={status}{detail} "
              f"run={os.path.basename(result.run_dir)} ({'; '.join(reasons) or 'all checks hold'})")
    else:
        print(f"node-fuzz certify: a private-route leg: status={status}")
    if status == "finding" or tri.stats["errors"]:
        print(tri.summary())
    return 0 if status == "certified" else 1


# ---------------------------------------------------------------------------------------------
# Export


def export_public(targets, dest):
    """Copies the regression vectors of public-route targets to `dest/<target>/{open,fixed}/`
    with a provenance file, for the manager to commit. Private-route vectors never leave."""
    fdir = fuzz_dir()
    count = 0
    for t in targets:
        if t.private:
            continue
        rows = []
        for st in ("open", "fixed"):
            src = os.path.join(fdir, t.name, "regress", st)
            if not os.path.isdir(src):
                continue
            out = os.path.join(dest, t.name, st)
            os.makedirs(out, exist_ok=True)
            for name in sorted(os.listdir(src)):
                if VECTOR_NAME.fullmatch(name):
                    shutil.copyfile(os.path.join(src, name), os.path.join(out, name))
                    meta = read_json(os.path.join(fdir, t.name, "regress", "meta", name + ".json"), {})
                    p = meta.get("provenance", {})
                    rows.append(f"| {st} | `{name}` | {meta.get('kind', '-')} | "
                                f"{meta.get('pin', '-')} | {meta.get('minimized', '-')} | "
                                f"{p.get('rust_dashcore', '-')} | {p.get('oracle_commit', '-')} |")
                    count += 1
        if rows:
            with open(os.path.join(dest, t.name, "PROVENANCE.md"), "w") as f:
                f.write(f"# {t.name} regression vectors\n\nFound by the node-fuzz farm (O3) and "
                        "minimised with libFuzzer through `node-fuzz/run.sh`; exported by "
                        "`fuzzctl export-public`.\n\n| State | Vector | Kind | Pin | Minimised "
                        "| rust-dashcore | Oracle |\n|---|---|---|---|---|---|---|\n"
                        + "\n".join(rows) + "\n")
    print(f"node-fuzz export-public vectors={count}")


# ---------------------------------------------------------------------------------------------


# Selftest cases timed by the wall clock, which can fail under agentbox load and pass alone. A
# failure of one of them reruns it alone, up to 3 tries (BL-134, as BL-100 does for the desktop
# gate); any other failure, or one that fails every try, fails the selftest.
LOAD_FLAKY = frozenset({
    "fuzzctl_test.Suspecting.test_a_suspected_leg_stops_for_the_night_and_its_slice_is_cut_within_a_poll",
    # The wall-clock mutant's segment must overshoot its CPU allotment within its wall seconds,
    # which a starved machine may not give it (BL-820).
    "fuzzctl_test.RunShCpuSegments.test_the_wall_clock_stop_fails_the_scenario",
})


def selftest(module="fuzzctl_test", flaky=LOAD_FLAKY):
    """Runs `module`'s tests. The last line is the verdict and names every failed case, how each
    allow-listed one fared alone, and every skipped case: a gate that keeps only that line still
    says what failed (BL-134). A pass that needed a rerun says `ok-after-rerun`, never plain `ok`."""
    import unittest

    if HERE not in sys.path:
        sys.path.insert(0, HERE)

    def run(name):
        return unittest.TextTestRunner(verbosity=1).run(unittest.defaultTestLoader.loadTestsFromName(name))

    def passes_alone(name):  # the one case ran and passed: a skip or nothing run is no pass
        r = run(name)
        return r.testsRun == 1 and not r.skipped and r.wasSuccessful()

    result = run(module)
    failed = sorted({t.id() for t, _ in result.failures + result.errors})
    notes, still = [], []
    for name in failed:
        tries = range(1, 4) if name in flaky else ()
        n = next((n for n in tries if passes_alone(name)), None)
        if n:
            notes.append(f"; failed {name}, passed alone on try {n}")
        else:
            still.append(name)
            notes.append(f"; failed {name}" + (", and all 3 tries alone" if tries else ""))
    ok = not still and not result.unexpectedSuccesses
    verdict = ("ok-after-rerun" if failed else "ok") if ok else "FAIL"
    print(f"fuzzctl selftest: {verdict} ({result.testsRun} tests, {len(result.skipped)} skipped"
          + "".join(notes)
          + "".join(f"; unexpected success {t.id()}" for t in result.unexpectedSuccesses)
          + "".join(f"; skipped {t.id()}: {why}" for t, why in result.skipped) + ")")
    return 0 if ok else 1


def main(argv=None):
    """The CLI. An unexpected error prints only its type, never its message, which may name a
    path in the private tree."""
    try:
        return cli(argv)
    except Exception as e:  # noqa: BLE001
        print(f"fuzzctl: internal error ({type(e).__name__})", file=sys.stderr)
        return 3


def cli(argv=None):
    p = argparse.ArgumentParser(prog="fuzzctl", description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check", help="the registry agrees with the code")
    sub.add_parser("targets", help="list the registry")
    s = sub.add_parser("smoke", help="the per-PR smoke")
    s.add_argument("--parallel", type=int, default=4)
    s.add_argument("--no-selftest", action="store_true")
    s.add_argument("--no-triage", action="store_true")
    s.add_argument("--only", nargs="+")
    t = sub.add_parser("triage", help="cluster, minimise and keep regression vectors")
    t.add_argument("--only", nargs="+")
    t.add_argument("--recheck", action="store_true", help="replay open vectors; move fixed ones")
    t.add_argument("--no-minimize", action="store_true")
    t.add_argument("--ack-sink-failures", action="store_true",
                   help="close the open sink-failure findings once their cause is fixed")
    cp = sub.add_parser("close-phantom", help="close one cluster of timeouts written after an interrupt, "
                                              "by hand (BL-173 D18)")
    cp.add_argument("cluster", help="the cluster id; every hit must carry a suspected_interrupt label")
    cp.add_argument("--reason", required=True, help="why it is not a finding, recorded in the audit log")
    cp.add_argument("--by", required=True, help="who is closing it, recorded in the audit log")
    sub.add_parser("status-line", help="the STATUS-ready summary")
    e = sub.add_parser("export-public", help="copy public-route regression vectors out")
    e.add_argument("dest")
    f = sub.add_parser("farm", help="the nightly farm")
    f.add_argument("action", choices=("launch", "work", "status", "stop"))
    f.add_argument("--night", default=None)
    f.add_argument("--hours", type=float, default=8.0)
    f.add_argument("--cores", type=int, default=8, help="CPUQuota in cores (the box runs at load 50-90)")
    f.add_argument("--jobs", type=int, default=4, help="libFuzzer workers per slice")
    f.add_argument("--slice-minutes", type=float, default=30.0)
    f.add_argument("--min-slice-minutes", type=float, default=5.0)
    f.add_argument("--max-slice-minutes", type=float, default=360.0,
                   help="the longest slice a leg's replay may ask for; at it the leg is replay-bound (BL-841)")
    f.add_argument("--replay-target", type=float, default=0.20,
                   help="the replay share a leg's slice length aims at, from its last counted slice (BL-841)")
    f.add_argument("--cmin", action="store_true",
                   help="launch/work: minimise a leg's corpus before its first slice when due (BL-841; "
                        "off by default, the manager's opt-in)")
    f.add_argument("--cmin-growth", type=float, default=0.25,
                   help="with --cmin: a leg's corpus is minimised before its first slice once it has grown "
                        "this much since its last minimisation (BL-841)")
    f.add_argument("--cmin-minutes", type=float, default=30.0,
                   help="with --cmin: the least wall time a minimisation is given; one that the time to the "
                        "deadline or a window can't hold waits (C-841-F8)")
    f.add_argument("--replay-mode", choices=("jobs", SHARD), default="jobs",
                   help="shard: a leg's slices run --shard, each worker loading 1/J of the corpus (BL-841b; "
                        "a plateau_ft=false target's still run in jobs mode); jobs: one -jobs parent")
    f.add_argument("--poll", type=float, default=30.0)
    f.add_argument("--windows", default=os.environ.get("NODE_FUZZ_BENCH_WINDOWS", DEFAULT_WINDOWS))
    f.add_argument("--on-calendar", help="launch: a systemd timer instead (the nightly: '*-*-* 01:07')")
    f.add_argument("--print-only", action="store_true", help="launch: print the command")
    f.add_argument("--plateau-minutes", type=float, default=60.0,
                   help="a leg that gains no cov and <= 0.1%% ft over this much of its own fuzzing "
                        "time is stopped for the night as a suspected plateau (a heuristic; only "
                        f"certify certifies; at least {MIN_PLATEAU_MINUTES})")
    f.add_argument("--only", action="extend", nargs="+", metavar="TARGET",
                   help="launch/work: farm only these targets' legs (repeatable)")
    c = sub.add_parser("certify", help="certify a leg's plateau (DEC-186)")
    c.add_argument("leg", metavar="TARGET@PIN")
    c.add_argument("--seconds", type=int, default=CERTIFY_SECONDS,
                   help=f"the window in CPU seconds after INITED; only {CERTIFY_SECONDS} or more certifies")
    c.add_argument("--wall-cap-hours", type=float, default=CERTIFY_WALL_HOURS,
                   help="the wall-clock cap on the window; reaching it gives `starved`")
    c.add_argument("--launch", action="store_true", help="run in a systemd unit at the farm's priority")
    c.add_argument("--print-only", action="store_true", help="--launch: print the command")
    sub.add_parser("selftest", help="run this tool's tests")
    u = sub.add_parser("under-test", help="the rustc crate names run.sh's under-test build instruments (BL-812)")
    u.add_argument("target")
    cr = sub.add_parser("cmin-ready", help="run.sh --minimize-corpus: the target has no open finding or job (BL-841)")
    cr.add_argument("target")
    rs = sub.add_parser("replay-share", help="a night's replay share per leg and slice (BL-841; counts and CPU only)")
    rs.add_argument("night")
    rs.add_argument("--leg", metavar="TARGET@PIN")
    rs.add_argument("--target", type=float, default=0.25, help="the share every counted slice must stay under")
    rt = sub.add_parser("replay-trend", help="a leg's replay cost per night and the next leg's projection "
                                             "(BL-841b; counts and CPU only)")
    rt.add_argument("leg", metavar="TARGET@PIN")
    rt.add_argument("--target", type=float, default=0.25, help="the acceptance share the margin is measured to")
    rt.add_argument("--replay-target", type=float, default=0.20, help="the farm's --replay-target")
    rt.add_argument("--slice-minutes", type=float, default=30.0, help="the farm's --slice-minutes")
    rt.add_argument("--max-slice-minutes", type=float, default=360.0, help="the farm's --max-slice-minutes")
    h = sub.add_parser("harness-smoke", help="Core-judged random op sequences (HX1 X4)")
    h.add_argument("--bin", required=True, help="a sb4-core-scenarios build.sh output directory")
    h.add_argument("--sequences", type=int, default=1000)
    h.add_argument("--seed", type=int, default=1)
    h.add_argument("--out", default="/work/scratch/sb4-core-scenarios/random")
    h.add_argument("--jobs", type=int, default=8)
    h.add_argument("--random-delivery", action="store_true", help="a random callback schedule per sequence")
    a = p.parse_args(argv)

    if a.cmd == "selftest":
        return selftest()
    if a.cmd == "harness-smoke":
        out = os.path.join(a.out, f"seed-{a.seed}" + ("-random-delivery" if a.random_delivery else ""))
        return 0 if harness_smoke(a.bin, a.sequences, a.seed, out, a.jobs, a.random_delivery) else 1
    targets = load_registry()
    if a.cmd == "under-test":
        t = next((t for t in targets if t.name == a.target), None)
        if t is None or not t.under_test:
            print(f"fuzzctl: {a.target} has no under_test in targets.toml", file=sys.stderr)
            return 4
        crates = path_crates()
        problems = list(under_test_problems(t, crates))
        for pr in problems:
            print(f"fuzzctl: {pr}", file=sys.stderr)
        if problems:
            return 5
        print("\n".join(sorted({crates[p] for p in t.under_test})))
        return 0
    if a.cmd == "cmin-ready":
        return cmin_ready(fuzz_dir(), a.target)
    if a.cmd == "replay-share":
        return replay_share_report(a, targets)
    if a.cmd == "replay-trend":
        return replay_trend_report(a)
    if a.cmd == "check":
        problems = check_registry()
        for pr in problems:
            print(f"fuzzctl check: {pr}")
        print(f"fuzzctl check: {'ok' if not problems else 'FAIL'} ({len(targets)} targets, "
              f"{len(legs(targets, 'smoke'))} smoke legs, {len(legs(targets, 'nightly'))} farm legs)")
        return 0 if not problems else 1
    if a.cmd == "targets":
        for t in targets:
            print(f"{t.name} owner={t.owner} kind={t.kind} pins={','.join(t.pins) or '-'} route={t.route} "
                  f"smoke={t.smoke:g}s nightly={t.nightly:g}")
        return 0
    if a.cmd == "smoke":
        problems = check_registry()
        if problems:
            print(f"fuzzctl check: {problems[0]}")
            print("node-fuzz smoke status=fail (the registry is inconsistent)")
            return 1
        return 0 if smoke(targets, a.parallel, not a.no_selftest, not a.no_triage, a.only) else 1
    if a.cmd == "close-phantom":
        return 0 if close_phantom(targets, a.cluster, a.reason, a.by) else 1
    triage_errors = 0
    if a.cmd == "triage":
        tri = run_triage(targets, only=a.only, recheck=a.recheck, minimize=not a.no_minimize,
                         ack_sink_failures=a.ack_sink_failures)
        print(tri.summary())
        triage_errors = tri.stats["errors"]
    if a.cmd in ("triage", "status-line"):
        lines, errors = status_lines(fuzz_dir(), targets)
        if a.cmd == "status-line":
            lines += [x for x in (farm_plateau_line(fuzz_dir(), targets), certify_line(fuzz_dir(), targets)) if x]
        print("\n".join(lines))
        return 1 if triage_errors or errors else 0
    if a.cmd == "export-public":
        export_public(targets, a.dest)
        return 0
    if a.cmd == "certify":
        if a.leg == "-":  # `--launch` of a leg that is not public
            a.leg = os.environ.get("NODE_FUZZ_CERTIFY_LEG", "")
        if a.seconds < 60:
            die("--seconds must be at least 60")
        if not (finite(a.wall_cap_hours) and wall_cap(a) >= a.seconds):
            die("--wall-cap-hours must cover at least --seconds")
        fuzz_dir()
        return certify(a, targets)
    if a.action in ("launch", "work") and not 0 < a.min_slice_minutes <= a.slice_minutes:
        die("--min-slice-minutes must be positive and at most --slice-minutes")
    if a.action in ("launch", "work") and not a.slice_minutes <= a.max_slice_minutes:
        die("--slice-minutes must be at most --max-slice-minutes")
    if a.action in ("launch", "work") and not 0 < a.replay_target < 1:
        die("--replay-target must be above 0 and below 1")
    if a.action in ("launch", "work") and not 0 < a.cmin_growth:
        die("--cmin-growth must be above 0")
    if a.action in ("launch", "work") and not 0 < a.cmin_minutes:
        die("--cmin-minutes must be above 0")
    if a.action in ("launch", "work") and a.replay_mode == SHARD and not 2 <= a.jobs <= 999:
        die("--replay-mode shard needs --jobs 2 to 999")
    if a.action in ("launch", "work") and not a.plateau_minutes >= MIN_PLATEAU_MINUTES:
        die(f"--plateau-minutes must be at least {MIN_PLATEAU_MINUTES}")
    farmed = {t.name for t, _ in legs(targets, "nightly")}
    if a.action in ("launch", "work") and a.only and not set(a.only) <= farmed:
        die("--only names a target with no farm legs")
    if a.night is not None and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*(\.[A-Za-z0-9_-]+)*", a.night):
        die("--night must be a plain name")
    if a.action == "status":
        return 0 if farm_status(a, targets) else 1
    if a.action == "stop":
        return 0 if farm_stop(a) else 1
    # A timer's runs, and a worker started without a night, take the date they start ("auto").
    a.night = a.night or ("auto" if a.on_calendar or a.action == "work" else today())
    if a.action == "work":
        return farm_work(a, targets)
    farm_launch(a)
    return 0


if __name__ == "__main__":
    sys.exit(main())
