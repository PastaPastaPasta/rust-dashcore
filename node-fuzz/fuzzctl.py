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
import json
import math
import os
import re
import secrets
import shutil
import signal
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
REGISTRY_KEYS = {"name", "owner", "kind", "pins", "route", "smoke", "nightly", "public_seeds", "honggfuzz",
                 "min_certify_execs", "plateau_cov_slack", "plateau_ft", "replay_cov_slack",
                 "replay_ft_slack", "smoke_corpus_max", "smoke_corpus_max_bytes"}
# The workspace fuzz/ crate's directories of upstream's honggfuzz targets (outside the registry).
UPSTREAM_FUZZ_DIRS = ("dash", "hashes")
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
        sample = {k: t.get(k, default) for k, default in (("smoke_corpus_max", SMOKE_CORPUS_MAX),
                                                           ("smoke_corpus_max_bytes", SMOKE_CORPUS_MAX_BYTES))}
        if not all(is_count(x) for x in sample.values()):
            die(f"targets.toml: {name}: smoke_corpus_max and smoke_corpus_max_bytes must be whole numbers")
        targets.append(
            Target(name, kind, pins, route, smoke, nightly, tuple(t.get("public_seeds", [])), owner, twin, floor,
                   slack, ft, replay.cov_slack, replay.ft_slack, **sample)
        )
    return targets


def registered_names(source, const):
    """The `name: "…"` entries of the Rust slice constant `const` in `source`."""
    m = re.search(rf"pub const {const}: &\[\w+\] = &\[(.*?)\];", source, re.S)
    return re.findall(r'name: "([a-z0-9_]+)"', m.group(1)) if m else None


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


def check_registry(root=ROOT):
    """Problems that make the registry disagree with the code (empty when it agrees)."""
    here = os.path.join(root, "node-fuzz")
    targets = {t.name: t for t in load_registry(os.path.join(here, "targets.toml"))}
    with open(os.path.join(here, "Cargo.toml"), "rb") as f:
        bins = {b["name"]: b["path"] for b in tomllib.load(f).get("bin", [])}
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
    """(target, pin) for every target whose `budget` attribute is positive."""
    return [(t, p) for t in targets if getattr(t, budget) > 0 for p in t.pins]


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
    except (OSError, ValueError):
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


# The run.sh modes that fuzz, so their findings are new findings.
FUZZING_MODES = ("run", "merge", "certify")


def is_job_run(run_dir):
    """A run.sh job (a minimisation or replay), not a fuzzing run: its findings are expected
    (a minimisation aborts on purpose), so triage never treats them as new findings."""
    try:
        mode = open(os.path.join(run_dir, "mode")).read().strip()
    except OSError:
        mode = None
    return (mode not in (None, *FUZZING_MODES) or os.path.isdir(os.path.join(run_dir, "input"))
            or os.path.exists(os.path.join(run_dir, "abort-field")))


class Triage:
    """The clusters of every target, kept in `fuzz/<target>/triage.json` (private).

    Regression vectors are `fuzz/<target>/regress/{open,fixed}/<cluster id>-<generation>`: each
    file belongs to one cluster, so clusters that share an input never share (or delete, or
    promote) each other's file. Provenance is `regress/meta/<name>.json`, outside the directories
    libFuzzer reads."""

    # A run without a verdict (killed during the build or the summary) is triaged once it is
    # this old.
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
            verdict = read_json(os.path.join(run_dir, "verdict.json"))
            if verdict is None:
                with contextlib.suppress(OSError):
                    if run_id in self.ready or time.time() - os.stat(run_dir).st_mtime > self.stale_s:
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
        cluster's open regression vector."""
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


def status_lines(fdir, targets):
    """The STATUS-ready summary and the number of triage errors behind it. The lines: one
    count line, one task line per open public cluster, and one aggregate task line for every
    open private finding (PLAN §7.5: counts only). Unreadable state, runs that could not be
    triaged and missing vectors are errors: the count is then UNKNOWN, never a known zero."""
    open_pub, open_priv, fixed, errors = [], 0, 0, 0
    for t in targets:
        if t.kind == "selftest":
            continue
        try:
            state = load_triage(fdir, t.name)
        except ValueError:
            errors += 1
            continue
        errors += len(state["run_errors"])
        for cid, c in sorted(state["clusters"].items()):
            errors += vector_problem(fdir, t.name, cid, c) is not None
            if c["state"] == "fixed":
                fixed += 1
            elif t.private:
                open_priv += 1
            else:
                open_pub.append((t.name, cid, c))
    known = (f"{open_priv + len(open_pub)} (private {open_priv}, public {len(open_pub)}); "
             f"fixed {fixed}")
    lines = [f"SW-FUZZ open findings: UNKNOWN (triage errors: {errors}); known open {known}"
             if errors else f"SW-FUZZ open findings: {known}"]
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
    ledger keeps every slice, so totals still report all the time spent."""
    credit = {}
    for r in records:
        if r["status"] == "finding":
            credit[r["leg"]] = 0.0
        elif r["status"] in CLEAN:
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


def suspected(history, window_s, rule=Plateau()):
    """Whether a leg is suspected of a plateau: its `history` is (fuzzing seconds, cov growth, ft
    growth, highest ft) rows, each cumulative over the leg's processes, starting at 0 seconds.
    Over the last `window_s` seconds of that fuzzing time (a full window must have run) it gained
    no cov (at most the target's `rule.cov_slack`) and, unless its `rule` leaves ft out, at most
    PLATEAU_FT of the ft it had at the window's start, which is the last row at or before the
    window's nominal start. Returns {window_minutes, fuzz_hours, ft, ft_gain}, or None. A stop
    heuristic only: certification is `certify`'s (DEC-186)."""
    if not history or history[-1][0] < window_s:
        return None
    end_s, cov, ft_gain, ft = history[-1]
    _, cov0, ft_gain0, ft0 = history[bisect.bisect_right(history, end_s - window_s, key=lambda h: h[0]) - 1]
    if ft0 == 0 or cov - cov0 > rule.cov_slack or (rule.ft and ft_gain - ft_gain0 > PLATEAU_FT * ft0):
        return None
    return {"window_minutes": window_s / 60, "fuzz_hours": round(end_s / 3600, 4), "ft": ft,
            "ft_gain": ft_gain - ft_gain0}


HISTORY_KEYS = {"leg", "fuzz_s", "cov_gain", "ft_gain", "ft"}


def load_coverage(state_dir, legs_):
    """Each leg's coverage history (`suspected`) and its recorded suspicion, from the night's
    `coverage.jsonl`. Rows: `{leg, fuzz_s, cov_gain, ft_gain, ft}` (cumulative; a leg's history
    starts at 0 seconds), `{leg, suspected: {…}}` when the farm stopped the leg, and
    `{leg, reset: true}` after a finding, which starts the leg's history afresh (CS0 §9.3). A
    missing file is no history; a malformed row, a history without its 0-second start, or one
    that goes backwards is FarmStateError; a torn final row is ignored (`repair_ledger`)."""
    try:
        data = open(os.path.join(state_dir, COVERAGE), "rb").read()
    except FileNotFoundError:
        return {}, {}
    history, marks = {}, {}
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
            continue
        if set(row) == {"leg", "suspected"} and suspicion_problem(row["suspected"]) is None:
            marks[leg] = row["suspected"]
            continue
        if (set(row) != HISTORY_KEYS or not finite(row["fuzz_s"])
                or not all(is_count(row[k]) for k in ("cov_gain", "ft_gain", "ft"))):
            raise FarmStateError("the night's coverage history has an invalid row")
        rows = history.setdefault(leg, [])
        new = (row["fuzz_s"], row["cov_gain"], row["ft_gain"], row["ft"])
        if (not rows and new[0] != 0) or (rows and any(a < b for a, b in zip(new, rows[-1]))):
            raise FarmStateError("the night's coverage history has no start or goes backwards")
        rows.append(new)
    return history, marks


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
    return {
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


def replay_rule(target, targets):
    """`certify`'s replay comparisons' rule for `target` (`Replay`; the default for a target the
    registry no longer lists)."""
    t = listed(target, targets)
    return t.replay if t else Replay()


def judge(ev, floor=CERTIFY_EXECS, identity=None, rule=Plateau(), replay=Replay()):
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
    leg's lock and the binary's copy make the window's corpus and binary the replays'."""
    if ev is None:
        return "no-evidence", ["no readable certify verdict"]
    if sum(ev[k] for k in ("divergences", "crashes", "sink_failures")):
        return "finding", ["divergences, crashes or sink failures"]
    problems = window_problems(ev, identity or (ev["target"], ev["pin"], ev["run"]))
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


def read_certificate(run_dir, floor=CERTIFY_EXECS, cert=UNREAD, rule=Plateau(), replay=Replay()):
    """(status, reasons, evidence) of a certify run: derived from the run's own evidence, bound to
    the run's place in the tree, which its certificate's evidence, when it has a certificate, must
    equal (a certificate that is unreadable, malformed or disagrees with its run is invalid; Sol r2
    m1). `cert` is the run's `certificate_record`, when the caller has read it. A certificate's run
    is judged by the stricter of `rule` and `replay` (the target's now) and the rules it recorded,
    so changing the registry never certifies an earlier run. Whether the leg may be certified at
    all is `eligibility`'s (`certificates`)."""
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
    return (*judge(ev, floor, run_identity(run_dir), rule, replay), ev)


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
        status, reasons, ev = read_certificate(run, certify_floor(name, targets), cert, plateau_rule(name, targets),
                                               replay_rule(name, targets))
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


def valid_row(row, legs_):
    if not isinstance(row, dict) or row.get("event") not in ("start", "done"):
        return False
    if not isinstance(row.get("slice"), str) or not SLICE_ID.fullmatch(row["slice"]):
        return False
    if row.get("leg") not in legs_:
        return False
    if row["event"] == "start":
        return finite(row.get("time"))
    counts = ("executions", *FINDING_KEYS)
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
    outcome once it is accounted for. Every whole row must be a valid row for one of `legs_`,
    or FarmStateError; a torn final row (no newline) is ignored here and dropped by
    `repair_ledger`."""
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
            started[sid] = row["leg"]
        elif started.get(sid) != row["leg"] or sid in done:
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
        self.scan = LogScan()
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
        self.coverage, self.suspicions = load_coverage(self.dir, self.plan["legs"])
        only = set(args.only or ())
        self.selected = {leg for leg in self.plan["legs"] if not only or leg.split("@")[0] in only}

    def records(self):
        return load_records(self.dir, self.plan["legs"])

    def record(self, row, settles=None):
        with self.lock:
            append_row(self.dir, row)
            self.given.pop(settles, None)

    def done_row(self, sid, leg, status, verdict, run_dir, recovered, **extra):
        return {"event": "done", "slice": sid, "leg": leg, "status": status,
                "run": os.path.basename(run_dir), "rev": verdict.get("rust_dashcore", "-"),
                "recovered": recovered, **extra,
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
            dcov, dft, top, epoch = self.scan(run)
            now = self.clock()
            last = self.sampled.get(sid)
            if last is None and epoch is None:
                # Nothing is fuzzing time yet, and growth so far counts at the next sample.
                self.sampled[sid] = (now, (0, 0), None)
                return
            self.sampled[sid] = (now, (dcov, dft), epoch)
            seen = last[1] if last else (0, 0)
            fuzzed = max(0.0, now - last[0]) if last and epoch is not None and last[2] == epoch else 0
            history = self.coverage.setdefault(leg, [])
            prev = history[-1] if history else (0, 0, 0, 0)
            # A poll shorter than a row's 0.1 s still counts: the sum is kept unrounded (BL-134).
            fuzz_s = self.fuzz_s[leg] = self.fuzz_s.get(leg, prev[0]) + fuzzed
            row = (round(fuzz_s, 1), prev[1] + max(0, dcov - seen[0]),
                   prev[2] + max(0, dft - seen[1]), max(prev[3], top))
            if history and row == prev:
                return
            append_row(self.dir, dict(zip(("fuzz_s", "cov_gain", "ft_gain", "ft"), row), leg=leg), COVERAGE)
            history.append(row)

    def suspicion(self, leg):
        with self.cov_lock:
            rule = plateau_rule(leg.split("@")[0], self.targets)
            return suspected(self.coverage.get(leg, []), self.window_s, rule)

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
            self.record(self.done_row(start["slice"], start["leg"], status, verdict, run, True))
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
            tri = run_triage(self.targets, runner=self.tracked("triage"), stop=stop,
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
        with self.cov_lock:
            self.slice_ids[leg] = sid
        try:
            result = self.tracked(leg)(target, pin, ["--cpu-hours", f"{cpu_hours:.4f}", "--jobs",
                                                     str(self.a.jobs), "--run-tag", sid])
            self.sample(leg)  # up to the slice's end
        finally:
            with self.cov_lock:
                self.slice_ids.pop(leg, None)
                self.sampled.pop(sid, None)
                run = self.slice_runs.pop(sid, None)
                if run:
                    self.scan.forget(run)
        status, v = self.accounted(result.run_dir, result.verdict, result.interrupted)
        if labelled := len(suspected_interrupts(result.run_dir)) if result.run_dir else 0:
            self.log(f"node-fuzz farm: {labelled} timeouts written after an interrupt are findings labelled "
                     "suspected_interrupt (BL-173 D18)")
        if status == "finding":
            self.reset_coverage(leg)
        # Recorded before the slice is acknowledged: a kill after this keeps it accounted for.
        self.record(self.done_row(sid, leg, status, v, result.run_dir, False,
                                  wall_s=round(self.clock() - start)), settles=leg)
        private = not legs_named([leg], self.targets)
        self.log(f"node-fuzz farm: a private-route slice ended: {status}" if private else result.line)
        if status == "finding":
            self.triage(only={target})
        return status

    def work(self):
        """Runs slices until every leg of the launch (`--only`'s, or all) is suspected of a
        plateau or has spent its share (`complete`), the night's cap is spent (`cap`), the
        launch's hours are up (`deadline`), or the unit stops (`stopped`). Shares are the cap
        split among the launch's legs (`launch_shares`); nothing is redistributed, and no slice is
        given more than what is left of the cap after all the CPU time of the night's slices
        (credited or not) and the slices running, so no sequence of launches exceeds it (Sol r1
        M4); a leg that restarts its share after a finding gets what the cap still allows. A leg suspected of a
        plateau (`suspected`; checked on every poll, so its running slice is cut within a poll)
        stops for the night; that is a heuristic that saves CPU, and only `certify` certifies a plateau
        (DEC-186). Yields to bench windows and node-bench runs: no slice starts that would overlap
        a reserved window, and running slices are interrupted when one is about to start or
        node-bench is running. A leg retires for the rest of the launch after a finding (CS0
        §9.3: the leg has failed; the finding is triaged) or after two failures without one (a
        broken build, say)."""
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
        plan = launch_shares(self.plan["legs"], self.plan["cpu_hours"], self.selected)
        parallel = max(1, a.cores // a.jobs)
        min_slice = a.min_slice_minutes * 60
        running, retired, failures = {}, set(held), {}
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
                slice_s = min(a.slice_minutes * 60, left - self.margin)
                blocked = self.bench.blocked(slice_s + self.lead)
                if blocked:
                    reason, until = blocked
                    if until <= self.imminent:
                        self.interrupt_all(reason)
                    slice_s = min(slice_s, until - self.lead)
                hold = blocked[0] if blocked and slice_s < min_slice else None
                if hold and hold != waiting:
                    self.log(f"node-fuzz farm: waiting: {hold}")
                waiting = hold
                with self.lock:  # a slice's done row and its `given` entry, never both or neither
                    records = self.records()
                    pending = sum(self.given.values())
                spent = credit_by_leg(records)
                cap_left = self.plan["cpu_hours"] - total(records, "cpu_hours") - pending
                open_plan = {leg: b for leg, b in plan.items() if leg not in retired | suspects}
                while len(running) < parallel and slice_s >= min_slice and cap_left > DONE_EPS:
                    leg = next_leg(open_plan, spent, running)
                    if leg is None:
                        break
                    cpu = min(open_plan[leg] - spent.get(leg, 0), a.jobs * slice_s / 3600, cap_left)
                    cap_left -= cpu
                    self.given[leg] = cpu
                    running[leg] = pool.submit(self.slice, leg, cpu)
                if not running and waiting is None:
                    # Nothing left to schedule: the cap spent, every leg suspected or its share
                    # spent, every remaining leg retired, or too little time for a slice.
                    if next_leg(open_plan, spent, {}) is not None:
                        outcome = "cap" if cap_left <= DONE_EPS else "deadline"
                    elif next_leg({leg: plan[leg] for leg in retired if leg in plan}, spent, {}) is not None:
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
        records = self.records()
        line = (f"node-fuzz farm night={self.a.night} slices={len(records)} "
                f"cpu_hours={total(records, 'cpu_hours'):.2f} credited_cpu_hours="
                f"{sum(min(c, self.plan['legs'][leg]) for leg, c in credit_by_leg(records).items()):.2f}"
                f"/{self.plan['cpu_hours']:g} "
                f"executions={total(records, 'executions')} "
                + " ".join(f"{k}={total(records, k)}" for k in FINDING_KEYS)
                + f" slow_units={total(records, 'slow_units')}"
                + f" interrupted={interrupted_count(records)} retired_legs={retired}"
                + f" suspected_legs={suspected_}"
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
                                         "CARGO_HOME", "RUSTUP_HOME", "RUSTC_WRAPPER")
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
              "--poll", str(a.poll), "--windows", a.windows, "--plateau-minutes", str(a.plateau_minutes),
              *(["--only", *a.only] if a.only else [])]
    quota = f"CPUQuota={a.cores * 100}%"
    timer = [f"--on-calendar={a.on_calendar}", "--timer-property=AccuracySec=1min"] if a.on_calendar else []
    if not systemd_launch(unit, "node-fuzz farm", quota, worker, a.print_only, timer):
        return
    on = f", on {a.on_calendar}" if a.on_calendar else ""
    print(f"node-fuzz farm: launched {unit} ({quota}, {a.hours:g} h{on})")


def farm_status(a, targets):
    fdir = fuzz_dir()
    failed = False
    farm = os.path.join(fdir, ".farm")
    nights = sorted(os.listdir(farm)) if os.path.isdir(farm) else []
    print("node-fuzz farm units: " + (" ".join(farm_units()) or "none active"))
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
        print(f"node-fuzz farm night={night} unit={unit_state(night)} slices={len(records)} "
              f"cpu_hours={total(records, 'cpu_hours'):.2f} credited_cpu_hours={done:.2f}/{plan['cpu_hours']:g} "
              f"legs_complete={complete}/{len(plan['legs'])} legs_suspected={len(suspicions)}/{len(plan['legs'])} "
              + " ".join(f"{k}={total(records, k)}" for k in FINDING_KEYS)
              + f" slow_units={total(records, 'slow_units')} interrupted={interrupted_count(records)}")
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


def standing(history, marks, targets):
    """The legs the farm stopped on a suspected plateau whose history still shows it over the
    suspicion's own window, under its target's rule: a later launch with a longer window may have
    run the leg again."""
    return {leg: p for leg, p in marks.items()
            if suspected(history.get(leg, []), p["window_minutes"] * 60, plateau_rule(leg.split("@")[0], targets))}


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
                                           replay=replay)
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
