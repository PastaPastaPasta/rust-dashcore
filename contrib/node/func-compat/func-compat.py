#!/usr/bin/env python3
"""Dash Core's functional tests against rdashd, per pinned release (O11; A1 §4.8).

The pin is v24.0 only (DEC-82, DEC-359): rdashd has no v23.1 profile. The frozen local
branch node/func-compat-v23.1 is not a pin and is never run.

Each pin has a local Core branch (`node/func-compat-<pin>`, never pushed) carrying the
func-compat patch set: per-node-index binary selection (`RDASHD_NODES` / `--rust-nodes`,
test_framework/rust_nodes.py), the passlist `test/functional/rust_passlist.txt` and the
expected failures `test/functional/rust_expected_failures.txt`.

  func-compat.py build --pin v24.0 [--wallet]         # export the tree, write config.ini
  func-compat.py run --pin v24.0 --phase M2 [-j 4]    # test_runner.py; writes the matrix
  func-compat.py ratchet --matrix <matrix.json>       # refuse a regression (exit 1)
  func-compat.py ratchet --matrix <m> --update        # grow the passlist in --core's worktree
  func-compat.py adaptations --pin v24.0              # fail if rdashd now takes an adapted option/RPC
  func-compat.py mixed-a --pin v24.0 [--update]      # mixed mode A over every DTF test (O12a):
                                                      # diff against mixed-a.json (exit 1), or rewrite it;
                                                      # --observer: add an rdashd observer; mixed-a-obs.json
  func-compat.py selftest                             # the ratchet and parsers on fixed inputs

The tree is `git archive` of the branch's test/ and share/rpcauth/ under --work, with a
config.ini whose BUILDDIR holds links to the pin's release dashd and dash-cli (from
contrib/setup-dashd.py's cache): those build the chain cache and run every node the mask
leaves to Core. Nothing in --core is written except by `ratchet --update`.

Matrix statuses: pass, fail, expected-fail (listed in rust_expected_failures.txt), skip.
The ratchet (`judge`) refuses:
- a passlisted test that did not pass (a regression);
- an expected failure that passed (move it with --update: the list only shrinks);
- a test of the run's phases that failed and has no expected-failures entry (a skip is the
  test's own verdict on the host, as with dashd, and is refused only on the passlist);
- an expected-failures entry whose owner is not a BACKLOG row or a PLAN task;
- a test on both lists, or a listed test the pin does not have;
- a test the branch's history ever passlisted that the passlist no longer has;
- a passlisted test that passed without starting rdashd: the patched framework appends a test's
  runner name to $RDASHD_STARTS at every rdashd start, so an empty or narrow mask, or a test's
  own binary=, cannot pass the ratchet on dashd alone (BL-356).
`ratchet --update` writes only when the sole refusals are expected failures that passed, for a
run with the phase's own mask at the branch's tip, into a clean worktree with the branch checked
out; otherwise it is a refusal too.
"""

import argparse
import fnmatch
import io
import json
import os
import random
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
RDC = HERE.parents[2]
PASSLIST = "test/functional/rust_passlist.txt"
XFAIL = "test/functional/rust_expected_failures.txt"
STATUSES = ("pass", "fail", "expected-fail", "skip")


@dataclass(frozen=True)
class Pin:
    branch: str
    tag: str  # the release tag the branch starts from
    release: str  # contrib/setup-dashd.py's DASHVERSION


PINS = {
    "v24.0": Pin("node/func-compat-v24.0-rc4", "v24.0.0-rc.4", "24.0.0-rc.4"),
}


def load_phases(path=HERE / "phases.toml"):
    with open(path, "rb") as f:
        return tomllib.load(f)  # dicts keep the file's order: the phase order


def phases_upto(phases, upto):
    """Every phase up to and including `upto`, in order."""
    names = list(phases)
    if upto not in names:
        raise SystemExit(f"unknown phase {upto}; phases: {', '.join(names)}")
    return [phases[name] for name in names[: names.index(upto) + 1]]


def phase_patterns(phases, upto):
    """The test patterns of every phase up to and including `upto`."""
    return [p for phase in phases_upto(phases, upto) for p in phase["tests"]]


def phase_blocked(phases, upto):
    """The blocked tests of every phase up to and including `upto`: {stem: what it waits for}."""
    return {t: why for phase in phases_upto(phases, upto)
            for t, why in phase.get("blocked", {}).items()}


def phase_tests(phases, upto, scripts):
    """The scripts (runner names) of every phase up to and including `upto`, but the blocked."""
    patterns = phase_patterns(phases, upto)
    held = phase_blocked(phases, upto)
    return [s for s in scripts
            if stem(s) not in held and any(fnmatch.fnmatchcase(stem(s), p) for p in patterns)]


def unmatched(phases, upto, scripts):
    """The phase patterns no script of the pin matches (a renamed or removed test)."""
    stems = {stem(s) for s in scripts}
    return [p for p in phase_patterns(phases, upto) if not fnmatch.filter(stems, p)]


def selection(names):
    """`names` without those test_runner.py already runs by prefix: it runs every script that
    starts with a name given (`feature_llmq_signing.py` also runs `... --spork21`)."""
    return [n for n in names if not any(o != n and n.startswith(o) for o in names)]


def stem(script):
    """`rpc_bind.py --ipv4` -> `rpc_bind`."""
    return script.split()[0].removesuffix(".py")


# ---- the Core lists -------------------------------------------------------------------------

def parse_passlist(text):
    """One runner name per line; `#` starts a comment."""
    out = []
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            out.append(" ".join(line.split()))
    return out


def parse_xfail(text):
    """`<runner name> | <reason> | <owner>` per line -> {name: (reason, owner)}."""
    out = {}
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = [p.strip() for p in line.split("|")]
        if len(parts) != 3 or not all(parts):
            raise ValueError(f"expected failures line {n}: want 'test | reason | owner': {line!r}")
        name = " ".join(parts[0].split())
        if name in out:
            raise ValueError(f"expected failures line {n}: {name} is listed twice")
        out[name] = (parts[1], parts[2])
    return out


def known_owners(program):
    """The BACKLOG row IDs and PLAN task IDs: the first column of their tables, not the headers,
    and only IDs with a digit (a task, not a family prefix or a key/value table's key)."""
    ids = set()
    for doc in ("BACKLOG.md", "PLAN.md"):
        lines = (Path(program) / doc).read_text(encoding="utf8").splitlines()
        for line, after in zip(lines, lines[1:] + [""]):
            m = re.match(r"\|\s*\**([A-Za-z][A-Za-z0-9-]*)\**\s*\|", line)
            if m and re.search(r"\d", m.group(1)) and not re.match(r"\|\s*:?-{3,}", after):
                ids.add(m.group(1))
    return ids


def owner_ids(owner):
    """The IDs an owner field names: comma- or space-separated."""
    return owner.replace(",", " ").split()


# ---- results ---------------------------------------------------------------------------------

RESULT = re.compile(r"^(?P<name>\S+\.py(?: [^|]*?)?)\s*\|\s*\S+\s+(?P<status>Passed|Failed|Skipped)\s*\|")


def parse_runner_output(text):
    """test_runner.py's summary table (ANSI off) -> {runner name: Passed|Failed|Skipped}."""
    out = {}
    for line in text.splitlines():
        m = RESULT.match(line.strip())
        if m:
            name = " ".join(m.group("name").split())
            if out.setdefault(name, m.group("status")) != m.group("status"):
                raise ValueError(f"{name} ran twice with different results")
    return out


def classify(results, xfail):
    status = {}
    for name, result in results.items():
        if result == "Passed":
            status[name] = "pass"
        elif result == "Skipped":
            status[name] = "skip"
        else:
            status[name] = "expected-fail" if name in xfail else "fail"
    return status


def judge(matrix, passlist, xfail, scripts, owners, ever_passlisted=()):
    """The ratchet: a list of refusals (empty = accepted)."""
    errors = []
    status, targets = matrix["status"], set(matrix["targets"])
    for name in sorted(set(ever_passlisted) - set(passlist)):
        errors.append(f"{name}: REGRESSION, removed from the passlist")
    for name in sorted(set(passlist) & set(xfail)):
        errors.append(f"{name}: on both the passlist and the expected failures")
    for name in sorted((set(passlist) | set(xfail)) - set(scripts)):
        errors.append(f"{name}: listed, but the pin has no such test")
    for name, (_, owner) in sorted(xfail.items()):
        ids = owner_ids(owner)
        if not ids or any(i not in owners for i in ids):
            errors.append(f"{name}: owner {owner!r} names no BACKLOG row or PLAN task")
    starts = matrix.get("rdashd_starts", {})
    for name in passlist:
        if name not in status:
            if matrix["complete"]:
                errors.append(f"{name}: REGRESSION, passlisted but not run")
        elif status[name] != "pass":
            errors.append(f"{name}: REGRESSION, passlisted but {status[name]}")
        elif not starts.get(name):
            errors.append(f"{name}: passlisted, but passed without starting rdashd")
    for name, st in sorted(status.items()):
        if name in xfail and st == "pass" and starts.get(name):
            errors.append(f"{name}: {UNEXPECTED_PASS}")
        elif name in targets and name not in passlist and name not in xfail and st == "fail":
            errors.append(f"{name}: failed with no expected-failures entry")
    return errors


UNEXPECTED_PASS = "expected to fail but passed; move it to the passlist (--update)"


def update(passlist, xfail, matrix):
    """The lists after a run: every passing target and expected failure that started rdashd joins
    the passlist."""
    starts = matrix.get("rdashd_starts", {})
    passed = [n for n, st in matrix["status"].items()
              if st == "pass" and starts.get(n) and n not in passlist
              and (n in matrix["targets"] or n in xfail)]
    return passlist + sorted(passed), {n: v for n, v in xfail.items() if n not in passed}


def header(path):
    """The comment lines a list file starts with, which a rewrite keeps."""
    text = path.read_text(encoding="utf8") if path.exists() else ""
    lines = text.splitlines(keepends=True)
    return "".join(lines[: next((i for i, l in enumerate(lines) if not l.startswith("#")), len(lines))])


def render_passlist(names):
    return "".join(f"{n}\n" for n in names)


def render_xfail(xfail):
    return "".join(f"{n} | {reason} | {owner}\n" for n, (reason, owner) in xfail.items())


def render_matrix(matrix):
    status = matrix["status"]
    counts = {s: sum(1 for v in status.values() if v == s) for s in STATUSES}
    lines = [
        f"# func-compat matrix: {matrix['pin']}, phase {matrix['phase']}",
        "",
        f"Core `{matrix['branch']}` at `{matrix['core_commit']}`; rdashd `{matrix['rdashd']}`;"
        f" rust nodes `{matrix['rust_nodes']}`; {'complete' if matrix['complete'] else 'partial'} run.",
        "",
        "| " + " | ".join(STATUSES) + " |",
        "|" + "---|" * len(STATUSES),
        "| " + " | ".join(str(counts[s]) for s in STATUSES) + " |",
        "",
        "| test | status | rdashd starts | owner |",
        "|---|---|---|---|",
    ]
    for name in sorted(status):
        owner = matrix["owners"].get(name, "")
        lines.append(f"| `{name}` | {status[name]} | {matrix['rdashd_starts'].get(name, 0)} | {owner} |")
    return "\n".join(lines) + "\n"


# ---- the tree --------------------------------------------------------------------------------

def git(core, *args):
    return subprocess.run(["git", "-C", str(core), *args], check=True, capture_output=True,
                          text=True).stdout


def release_bin(pin):
    cache = Path(os.environ.get("RUST_DASHCORE_TEST_CACHE", Path.home() / ".rust-dashcore-test"))
    bindir = cache / f"dashcore-{pin.release}" / "bin"
    if not (bindir / "dashd").exists():
        subprocess.run([sys.executable, str(RDC / "contrib/setup-dashd.py")], check=True,
                       env={**os.environ, "DASHVERSION": pin.release}, stdout=subprocess.DEVNULL)
    return bindir


def tree_dir(work, pin_name, wallet, branch=None):
    """The export of the pin's branch, or of another `branch` (mixed-a's) beside it, so that runs
    of the two never replace each other's tree."""
    name = f"{pin_name}-{'wallet' if wallet else 'nowallet'}"
    if branch and branch != PINS[pin_name].branch:
        name += "-" + branch.replace("/", "_")
    return Path(work) / name


def build(core, work, pin_name, wallet, branch=None):
    """Export the pin's test tree (or `branch`'s) under --work and write its config.ini; returns
    the tree."""
    pin = PINS[pin_name]
    branch = branch or pin.branch
    commit = git(core, "rev-parse", f"{branch}^{{commit}}").strip()
    tree = tree_dir(work, pin_name, wallet, branch)
    stamp = tree / "tree.json"
    if not (stamp.exists() and json.loads(stamp.read_text())["commit"] == commit):
        shutil.rmtree(tree, ignore_errors=True)
        (tree / "src").mkdir(parents=True)
        archive = subprocess.run(["git", "-C", str(core), "archive", commit, "test", "share/rpcauth"],
                                 check=True, capture_output=True).stdout
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(tree / "src", filter="data")
    bindir = release_bin(pin)
    builddir = tree / "build"
    (builddir / "src").mkdir(parents=True, exist_ok=True)
    for name in ("dashd", "dash-cli", "dash-util", "dash-wallet"):
        link = builddir / "src" / name
        link.unlink(missing_ok=True)
        link.symlink_to(bindir / name)
    src = tree / "src"
    config = (src / "test/config.ini.in").read_text(encoding="utf8")
    subst = {
        "@PACKAGE_NAME@": "Dash Core", "@PACKAGE_BUGREPORT@": "https://github.com/dashpay/dash/issues",
        "@abs_top_srcdir@": str(src), "@abs_top_builddir@": str(builddir), "@EXEEXT@": "",
        "@ENABLE_WALLET_TRUE@": "" if wallet else "#", "@USE_SQLITE_TRUE@": "" if wallet else "#",
        "@BUILD_BITCOIN_CLI_TRUE@": "", "@BUILD_BITCOIN_UTIL_TRUE@": "",
        "@BUILD_BITCOIN_WALLET_TRUE@": "", "@BUILD_BITCOIND_TRUE@": "",
    }
    for key, value in subst.items():
        config = config.replace(key, value)
    config = re.sub(r"^@[A-Z_]+_TRUE@", "#", config, flags=re.M)  # BDB, fuzz, ZMQ, signer, USDT
    (src / "test/config.ini").write_text(config, encoding="utf8")
    stamp.write_text(json.dumps({"pin": pin_name, "branch": branch, "commit": commit,
                                 "release": pin.release, "wallet": wallet}))
    return tree


def scripts_of(tree):
    """test_runner.py's ALL_SCRIPTS for the tree."""
    code = "import sys, json; sys.path.insert(0, sys.argv[1]); import test_runner; print(json.dumps(test_runner.ALL_SCRIPTS))"
    out = subprocess.run([sys.executable, "-c", code, str(tree / "src/test/functional")],
                         check=True, capture_output=True, text=True).stdout
    return [" ".join(s.split()) for s in json.loads(out)]


def pylib(work):
    """A directory with the framework's one third-party module (dash_hash, X11)."""
    lib = Path(work) / "pylib"
    probe = [sys.executable, "-c", "import dash_hash"]
    if subprocess.run(probe, env={**os.environ, "PYTHONPATH": str(lib)}, capture_output=True).returncode:
        subprocess.run([sys.executable, "-m", "pip", "install", "--quiet", "--target", str(lib),
                        "dash_hash==1.5.1"], check=True)
    return lib


def run(args):
    phases = load_phases()
    phase = phases.get(args.phase) or sys.exit(f"unknown phase {args.phase}")
    mask = args.rust_nodes if args.rust_nodes is not None else phase.get("rust_nodes")
    if mask is None:
        sys.exit(f"phase {args.phase} has no rust_nodes mask (mode A: `mixed-a`; mode B: M7)")
    tree = build(args.core, args.work, args.pin, phase["wallet"])
    meta = json.loads((tree / "tree.json").read_text())
    scripts = scripts_of(tree)
    targets = phase_tests(phases, args.phase, scripts)
    for pattern in unmatched(phases, args.phase, scripts):
        print(f"phases: {pattern} matches no test at {args.pin}", file=sys.stderr)
    passlist, xfail = read_lists(args.core, PINS[args.pin].branch)
    selected = args.tests or sorted(set(targets) | set(passlist) | set(xfail))
    rdashd, rdash_cli, stale = check_adaptations(tree, args.rdashd, args.rdash_cli, args.tmpdirprefix)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    # Unique to this run: a concurrent run with the same --out cannot lend it its starts.
    fd, starts_log = tempfile.mkstemp(prefix=f"rdashd-starts-{args.pin}-{args.phase}-", suffix=".log", dir=out)
    os.close(fd)
    starts_log = Path(starts_log)
    env = {**os.environ, "RDASHD": str(rdashd), "RDASH_CLI": str(rdash_cli), "RDASHD_NODES": mask,
           "RDASHD_STARTS": str(starts_log), "PYTHONPATH": str(pylib(args.work)), "RDASHD_OBSERVER": "0"}
    log = out / f"runner-{args.pin}-{args.phase}.log"
    cmd = [sys.executable, str(tree / "src/test/functional/test_runner.py"), f"--jobs={args.jobs}",
           f"--tmpdirprefix={args.tmpdirprefix}", "--combinedlogslen=200", *selection(selected)]
    with open(log, "w") as f:
        subprocess.run(cmd, env=env, stdout=f, stderr=subprocess.STDOUT)
    starts = starts_log.read_text(encoding="utf8").splitlines()
    starts_log.unlink()
    results = parse_runner_output(log.read_text(encoding="utf8", errors="replace"))
    missing = [s for s in selected if s not in results]
    if missing:
        sys.exit(f"test_runner.py reported no result for {missing}; see {log}")
    status = classify(results, xfail)
    matrix = {
        "pin": args.pin, "phase": args.phase, "branch": meta["branch"], "core_commit": meta["commit"],
        "rdashd": str(rdashd), "rust_nodes": mask, "complete": not args.tests,
        "targets": [t for t in targets if t in status], "status": status,
        "owners": {n: xfail[n][1] for n in status if n in xfail},
        "rdashd_starts": count_starts(starts, status),
    }
    path = out / f"matrix-{args.pin}.json"
    path.write_text(json.dumps(matrix, indent=1, sort_keys=True) + "\n")
    (out / f"matrix-{args.pin}.md").write_text(render_matrix(matrix))
    print(render_matrix(matrix))
    print(f"matrix: {path}")
    return ratchet_matrix(args, matrix, scripts, passlist, xfail) or (1 if stale else 0)


def count_starts(lines, names):
    """{runner name: rdashd starts} from the $RDASHD_STARTS file's lines, for each of `names`."""
    counts = Counter(lines)
    return {n: counts[n] for n in names}


def read_lists(core, branch):
    """The branch's passlist and expected failures; a missing file is an error, not an empty list."""
    return parse_passlist(git(core, "show", f"{branch}:{PASSLIST}")), parse_xfail(git(core, "show", f"{branch}:{XFAIL}"))


def ever_passlisted(core, pin):
    """Every test a commit of the pin's branch added to the passlist."""
    log = git(core, "log", "-p", "--cc", "--no-color", "--no-ext-diff", "--format=",
              f"{pin.tag}..{pin.branch}", "--", PASSLIST)
    return {n for line in log.splitlines() if line.startswith("+") and not line.startswith("+++")
            for n in parse_passlist(line[1:])}


def check_update(worktree, pin, matrix, errors):
    """Why `--update` may not write into `worktree` (None: it may)."""
    if matrix["rust_nodes"] != load_phases()[matrix["phase"]].get("rust_nodes"):
        return f"the run's mask {matrix['rust_nodes']!r} is not the phase's"
    if any(not e.endswith(UNEXPECTED_PASS) for e in errors):
        return "the run has refusals besides expected failures that passed"
    if git(worktree, "rev-parse", pin.branch).strip() != matrix["core_commit"]:
        return f"{pin.branch} moved since the run (matrix at {matrix['core_commit'][:12]}); run again"
    head = subprocess.run(["git", "-C", str(worktree), "symbolic-ref", "-q", "HEAD"], capture_output=True, text=True)
    if head.stdout.strip() != f"refs/heads/{pin.branch}":
        return f"{worktree} does not have {pin.branch} checked out"
    if git(worktree, "status", "--porcelain", "--", PASSLIST, XFAIL).strip():
        return f"{worktree} has uncommitted list edits"
    return None


def ratchet_matrix(args, matrix, scripts, passlist, xfail):
    pin = PINS[matrix["pin"]]
    owners, history = known_owners(args.program), ever_passlisted(args.core, pin)
    errors = judge(matrix, passlist, xfail, scripts, owners, history)
    if args.update:
        worktree = Path(args.update)
        refusal = check_update(worktree, pin, matrix, errors)
        if refusal:
            errors.append(f"not updating: {refusal}")
        else:
            passlist, xfail = update(passlist, xfail, matrix)
            for path, body in ((worktree / PASSLIST, render_passlist(passlist)), (worktree / XFAIL, render_xfail(xfail))):
                path.write_text(header(path) + body, encoding="utf8")
            print(f"updated {worktree / PASSLIST} and {worktree / XFAIL}; commit them on {pin.branch}")
            errors = judge(matrix, passlist, xfail, scripts, owners, history)
    for e in errors:
        print(f"ratchet: {e}", file=sys.stderr)
    print(f"ratchet: {'REFUSED' if errors else 'ok'} ({len(passlist)} passlisted, {len(xfail)} expected failures)")
    return 1 if errors else 0


def ratchet(args):
    matrix = json.loads(Path(args.matrix).read_text())
    tree = tree_dir(args.work, matrix["pin"], load_phases()[matrix["phase"]]["wallet"])
    return ratchet_matrix(args, matrix, scripts_of(tree), *read_lists(args.core, PINS[matrix["pin"]].branch))


# ---- the adaptation tripwire ------------------------------------------------------------------

REFUSAL = " is not supported by rdashd yet ("


def adaptations_of(tree):
    """rust_nodes.py's ADAPTATIONS, RPC_ADAPTATIONS and MESSAGE_ADAPTATIONS in the tree."""
    code = ("import sys, json; sys.path.insert(0, sys.argv[1]); from test_framework import rust_nodes as r; "
            "print(json.dumps([r.ADAPTATIONS, r.RPC_ADAPTATIONS, r.MESSAGE_ADAPTATIONS]))")
    out = subprocess.run([sys.executable, "-c", code, str(tree / "src/test/functional")], check=True,
                         capture_output=True, text=True, env={**os.environ, "PYTHONPATH": ""}).stdout
    return json.loads(out)


def free_port():
    """A free port below the ephemeral range (32768-60999 collides with client sockets)."""
    for _ in range(100):
        port = random.randrange(20000, 32000)
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
                return port
            except OSError:
                continue
    raise SystemExit("no free port below 32000")


def lock_error(rdashd, datadir):
    """The init error of a second rdashd on the running node's `datadir` (probe "datadir-lock")."""
    r = subprocess.run([str(rdashd), f"-datadir={datadir}", "-regtest", "-disablewallet", "-noserver", "-listen=0"],
                       capture_output=True, text=True, timeout=60)
    return r.stderr


PROBES = {"datadir-lock": lock_error}


def tripwire(rdashd, rdash_cli, options, methods, scratch, messages=None):
    """What rdashd now accepts or serves of the adaptations: each such line means "drop it".

    `options` is {name: (owner, the value the framework sets)}, `methods` {name: owner},
    `messages` {probe: (owner, framework text, rdashd text)}: a message whose probe's error no
    longer contains the rdashd text means "update or drop it"."""
    stale = []
    base = Path(tempfile.mkdtemp(prefix="tripwire-", dir=scratch))
    try:
        for n, (option, (owner, value)) in enumerate(sorted(options.items())):
            datadir = base / f"opt{n}"
            datadir.mkdir()
            args = [str(rdashd), f"-datadir={datadir}", "-regtest", "-disablewallet", "-listen=0",
                    f"-rpcport={free_port()}", f"-{option}={value}"]
            proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            try:
                _, err = proc.communicate(timeout=60)
            except subprocess.TimeoutExpired:  # it started: the option was accepted
                proc.kill()
                proc.communicate()
                err = ""
            if f"-{option}{REFUSAL}" not in err:
                stale.append(f"rdashd accepts -{option}={value} now ({owner}): drop it from ADAPTATIONS")
        datadir = base / "rpc"
        datadir.mkdir()
        port = free_port()
        # -listen=0: no p2p port, which another node on the box may hold (the regtest default).
        node = subprocess.Popen([str(rdashd), f"-datadir={datadir}", "-regtest", "-disablewallet", "-listen=0",
                                 f"-rpcport={port}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            cli = [str(rdash_cli), f"-datadir={datadir}", "-regtest", f"-rpcport={port}", "-rpcwait",
                   "-rpcwaittimeout=60"]
            for method, owner in sorted(methods.items()):
                r = subprocess.run(cli + [method], capture_output=True, text=True)
                if node.poll() is not None:
                    raise SystemExit(f"tripwire: rdashd exited ({node.returncode}) before answering {method}")
                if "error code: -32601" not in r.stderr:
                    stale.append(f"rdashd serves {method} now ({owner}): drop it from RPC_ADAPTATIONS")
            for probe, (owner, _, text) in sorted((messages or {}).items()):
                if probe not in PROBES:
                    stale.append(f"no probe for MESSAGE_ADAPTATIONS {probe!r} ({owner})")
                elif text not in PROBES[probe](rdashd, datadir):
                    stale.append(f"rdashd's {probe} error no longer contains {text!r} ({owner}): "
                                 "update MESSAGE_ADAPTATIONS")
            subprocess.run(cli + ["stop"], capture_output=True)
            node.wait(timeout=60)
        finally:
            if node.poll() is None:
                node.kill()
                node.wait()
    finally:
        shutil.rmtree(base, ignore_errors=True)
    return stale


def check_adaptations(tree, rdashd, rdash_cli, scratch):
    """Run the tripwire; returns the resolved (rdashd, rdash-cli) and the stale lines."""
    rdashd, rdash_cli, scratch = Path(rdashd).resolve(), Path(rdash_cli).resolve(), Path(scratch)
    scratch.mkdir(mode=0o700, parents=True, exist_ok=True)
    options, methods, messages = adaptations_of(tree)
    stale = tripwire(rdashd, rdash_cli, options, methods, scratch, messages)
    for line in stale:
        print(f"adaptations: {line}", file=sys.stderr)
    print(f"adaptations: {'STALE' if stale else 'ok'} ({len(options)} options, {len(methods)} methods,"
          f" {len(messages)} messages)")
    return rdashd, rdash_cli, stale


# ---- mixed mode A (O12a) --------------------------------------------------------------------
#
# Every DashTestFramework test runs with node 0 and the masternodes on dashd and every other node
# on rdashd (A1 §4.8; rust_nodes.py's role mask "non-mn"). Its result is one passlist for the
# whole mode, every test with its status and every failure with its class, committed as
# mixed-a.json. `mixed-a` regenerates it and diffs it against the committed file (exit 1 on any
# difference); `--update` rewrites the file instead.

# The passlist per variant: literal mode A, and the observer variant (DEC-451), the same run plus
# one rdashd node connected to node 0 after DashTestFramework's setup (RDASHD_OBSERVER=1).
MIXED_A, MIXED_A_OBS = "mixed-a", "mixed-a-obs"
MIXED_A_MASK = "non-mn"
# The Core branch with mode A's hooks (the role mask, the observer, RPC_FAILURE); `mixed-a` and the
# nightly export it unless --branch says otherwise, and refuse a tree without MIXED_A_HOOKS.
MIXED_A_CORE_BRANCH = "node/o12a-mixed-a"
MIXED_A_HOOKS = {
    "test/functional/test_framework/rust_nodes.py": ("NON_MN", "RDASHD_OBSERVER", "RPC_FAILURE"),
    "test/functional/test_framework/test_framework.py": (
        "rust_nodes.masternodes()", "add_rust_observer", "rejoin_rust_observer", "rpc_failure",
        "not linking the rdashd observer", "the rdashd observer exited with status"),
}
# A node's p2p, rpc and tor ports are p2p_port(n), +10000 and +20000 (util.py), and stay below
# 32768 from here; the default 11000 would put its tor ports in the ephemeral range.
MIXED_A_PORT_MIN = 2700
FAILURE_CLASSES = ("unimplemented-rpc", "unimplemented-option", "divergence", "harness", "timeout", "allowed-diff")
# DEC-483: rdashd's readiness refusal (dashd-chainstate connect/guard.rs refusal_text) for a block
# it cannot validate yet; it aborts the node with exit 1, as Core's AbortNode does. A failed run
# whose rdashd node logged it is the declared difference, whatever else failed after it.
D18_REFUSAL = re.compile(r"rdashd cannot validate block \S+ at height \d+ on \S+: .+ is not built \(ALLOWED-DIFFS D18\)")
ALLOWED_D18 = {"status": "fail", "class": "allowed-diff", "detail": "D18"}
# test_runner.py sections: a failed test's datadir, and a node that exited non-zero at stop.
DATADIR = re.compile(r"Not cleaning up dir (.+?)(?: due to perf data)?\s*$", re.M)
STOP_EXIT = re.compile(r"Node returned non-zero exit code \(-?\d+\) when stopping")
DTF = re.compile(r"^class \w+ *\((?:\w+, *)*DashTestFramework\)", re.M)


def missing_hooks(tree):
    """MIXED_A_HOOKS the exported `tree` lacks, as `file: token`."""
    missing = []
    for path, tokens in MIXED_A_HOOKS.items():
        source = tree / "src" / path
        text = source.read_text(encoding="utf8") if source.exists() else ""
        missing.extend(f"{path}: {token}" for token in tokens if token not in text)
    return missing


def dtf_scripts(tree, scripts):
    """The runner names of the tree's DashTestFramework tests."""
    functional = tree / "src/test/functional"
    dtf = {p.stem for p in functional.glob("*.py") if DTF.search(p.read_text(encoding="utf8"))}
    return [s for s in scripts if stem(s) in dtf]


FAILED = re.compile(r"^\d+/\d+ - (?P<name>\S+\.py(?: .*?)?) failed, Duration: \d+ s$")
SUMMARY = re.compile(r"^TEST\s*\|\s*STATUS\s*\|")


def failure_sections(text):
    """{runner name: its part of test_runner.py's output}: the failed test's stdout and stderr,
    up to the next failure or the summary table."""
    sections, name = {}, None
    for line in text.splitlines():
        m = FAILED.match(line.strip())
        if m:
            name = " ".join(m.group("name").split())
            sections[name] = []
        elif SUMMARY.match(line.strip()):
            name = None
        elif name:
            sections[name].append(line)
    return {n: "\n".join(lines) for n, lines in sections.items()}


FRAME = re.compile(r'^\s*File "(?P<file>[^"]+)", line (?P<line>\d+), in \S+\n(?P<code>.*)$', re.M)
# The messages main() logs just before the traceback of the exception that failed the test
# (test_framework.py). A traceback after them (stop_nodes() on the way out) is a consequence.
CAUSE = re.compile(r"\(ERROR\): (?:JSONRPC error|Assertion failed|Key error|Called Process failed|"
                   r"Unexpected exception caught during testing)")
LOG_LINE = re.compile(r"^\S*\d{4}-\d\d-\d\dT\S+ ", re.M)
OPTION_REFUSED = re.compile(r"(-[a-z0-9]+) is not supported by rdashd yet \(")
METHOD_NOT_FOUND = "Method not found (-32601)"
# `.getnetworkinfo(`, or TestNode's wrappers' `self.__getattr__('createwallet')(`.
RPC_CALL = re.compile(r"\.(?:__getattr__\(['\"])?(?P<method>[a-z][a-z0-9_]*)(?:['\"]\))?\(")
RPC_PROXY = ("coverage.py", "authproxy.py")
# A test line that asserts a peer count: the observer is one more peer of node 0's, so such an
# assertion fails because of the variant, not rdashd (DEC-451).
PEER_COUNT = re.compile(r"getconnectioncount\(|len\([^)]*getpeerinfo\(\)|\[['\"]connections")
# A test's RPC to a node that is not running: in the observer variant, the observer a test stopped
# without knowing it (the framework restarts it with node 0; this is the backstop).
NO_RPC = "Error: no RPC connection"
# Errors of an RPC's connection, not of what the node answered: these, and any http.client exception.
TRANSPORT = {
    "ConnectionError", "ConnectionRefusedError", "ConnectionResetError", "ConnectionAbortedError",
    "BrokenPipeError", "RemoteDisconnected", "HTTPException", "IncompleteRead", "BadStatusLine",
    "CannotSendRequest", "ResponseNotReady", "ImproperConnectionState",
}
# Exceptions of the process and transport around a node, not of what a node answered: the harness's.
# Any other exception in a test-script frame (an assertion, an RPC error, a KeyError, TypeError,
# ValueError or IndexError on an RPC result) is the test meeting an answer it did not expect.
INFRASTRUCTURE = TRANSPORT | {"FailedToStartError", "CalledProcessError", "OSError", "FileNotFoundError",
                              "PermissionError"}
# The Core harness's line (rust_nodes.RPC_FAILURE) when the RPC that failed the test was to an
# rdashd node whose process had exited by then (DEC-457): a transport error is then rdashd's.
RDASHD_GONE = re.compile(r"rdashd RPC failure: node \d+ exited -?\d+")
# wait_until() raises "AssertionError: Predicate ... not true after N seconds", and the predicate's
# source is multi-line, so the exception line itself shows only its start.
TIMEOUTS = ("wait_until() failed", "AssertionError: Predicate '", "timed out", "Timeout")
# What may mean that an rdashd node crashed: in a failed test's output (exit codes, a node that
# went away under an RPC: any TRANSPORT error, or a refused connection) or in its nodes' stderr. Such tests are listed in the run's untracked
# report (`possible_crashes`) and their datadirs kept for triage: a crash may be security-relevant,
# so the committed passlist names a class and a test line and nothing more.
CRASH = re.compile(r"panicked at|exit code \((?:-\d+|101)\)|non-zero exit code|exited with status (?:-\d+|101)|"
                   r"Segmentation fault|SIGABRT|the rdashd observer exited|ConnectionRefusedError|"
                   # TestNode.stop_node logs CannotSendRequest after an RPC that timed out: shutdown noise.
                   r"http\.client\.(?!CannotSendRequest)\w+|" + "|".join(sorted(TRANSPORT - {"CannotSendRequest"})))


def cause_traceback(text):
    """The frames and the exception line of the traceback that failed the test: the one after
    main()'s last CAUSE message, or the last one in `text` if there is none. None if absent."""
    causes = list(CAUSE.finditer(text))
    header = "Traceback (most recent call last):"
    start = text.find(header, causes[-1].end()) if causes else text.rfind(header)
    if start < 0:
        return None
    body = text[start:]
    following = LOG_LINE.search(body)  # the traceback ends at the next log line
    body = body[:following.start()] if following else body
    frames = list(FRAME.finditer(body))
    if not frames:
        return None
    tail = body[frames[-1].end():]
    exc = next((l.strip() for l in tail.splitlines() if l.strip() and not l.startswith(" ")), "")
    return frames, exc


def test_frame(frames):
    """The innermost frame in a test script (not the framework), or None."""
    return next((f for f in reversed(frames) if Path(f.group("file")).parent.name == "functional"), None)


def test_location(frames):
    """`script.py:line` of test_frame: stable for a given tree, unlike the values an assertion
    prints."""
    f = test_frame(frames)
    return f"{Path(f.group('file')).name}:{f.group('line')}" if f else "no test frame"


def rpc_method(frames):
    """The RPC named by the frame that calls into the RPC proxy (coverage.py, authproxy.py)."""
    for caller, callee in zip(frames, frames[1:]):
        if Path(callee.group("file")).name in RPC_PROXY:
            calls = RPC_CALL.findall(caller.group("code"))
            return calls[-1] if calls else None
    return None


def classify_failure(text, observer=False):
    """(class, detail) for a failed test's output (FAILURE_CLASSES). The detail is a name (an
    option, an RPC) or a test line, never an exception or its message. A failure with no test
    frame (setup in the framework) or of INFRASTRUCTURE is the harness's, but for a TRANSPORT
    error on an RPC to an rdashd node that had exited (RDASHD_GONE); any other exception raised
    through a test line is a divergence. With `observer`, a failed assertion whose test
    line asserts a peer count (a wait on one included), or an RPC to a node that is not running,
    is the harness's."""
    options = sorted(set(OPTION_REFUSED.findall(text)))
    if options:
        return "unimplemented-option", " ".join(options)
    tb = cause_traceback(text)
    if tb is None:
        return "harness", "no traceback"
    frames, exc = tb
    line = test_frame(frames)
    where = test_location(frames)
    if METHOD_NOT_FOUND in exc:
        return "unimplemented-rpc", rpc_method(frames) or where
    qualified = exc.split(":", 1)[0]
    kind = qualified.rsplit(".", 1)[-1]  # test_framework.authproxy.JSONRPCException
    transport = kind in TRANSPORT or qualified.startswith("http.client.")
    if observer and kind == "AssertionError":
        if NO_RPC in exc or (line and PEER_COUNT.search(line.group("code"))):
            return "harness", where
    if any(t in exc for t in TIMEOUTS):
        return "timeout", where
    if transport and RDASHD_GONE.search(text):
        return "divergence", where
    if line is None or transport or kind in INFRASTRUCTURE:
        return "harness", where
    return "divergence", where


NO_RDASHD = {"status": "skip", "detail": "no rdashd node"}


def mixed_a_entries(results, sections, starts, observer=False, d18=()):
    """The passlist's entries for test_runner.py's `results`: {name: {"status", and for a failure
    "class" and "detail", for a skip "detail"}}. `starts` is count_starts' {name: rdashd starts}.
    A test that started no rdashd node is a skip whatever its result: mode A left it all dashd (it
    has no node besides node 0 and the masternodes), so it says nothing about rdashd. A failed
    test in `d18` (an rdashd node logged D18_REFUSAL) is ALLOWED_D18 (DEC-483)."""
    out = {}
    for name, result in sorted(results.items()):
        if result == "Skipped":
            out[name] = {"status": "skip", "detail": "skipped by the test"}
        elif not starts[name]:
            out[name] = dict(NO_RDASHD)
        elif result == "Passed":
            out[name] = {"status": "pass"}
        elif name in d18:
            out[name] = dict(ALLOWED_D18)
        else:
            cls, detail = classify_failure(sections.get(name, ""), observer)
            out[name] = {"status": "fail", "class": cls, "detail": detail}
    return out


def render_entry(entry):
    """`fail timeout x.py:3`, `pass`, or `(absent)`."""
    if entry is None:
        return "(absent)"
    return " ".join(str(entry[k]) for k in ("status", "class", "detail") if k in entry)


def mixed_a_diff(old, new):
    """Human lines for every test whose entry differs between two passlists' `tests`."""
    lines = []
    for name in sorted(set(old) | set(new)):
        a, b = old.get(name), new.get(name)
        if a != b:
            lines.append(f"{name}: {render_entry(a)} -> {render_entry(b)}")
    return lines


def mixed_a_totals(tests):
    """Counts of passes, skips and each failure class (a failure counts under its class)."""
    counts = Counter(t.get("class", t["status"]) for t in tests.values())
    return {k: counts[k] for k in ("pass", "skip", *FAILURE_CLASSES)}


def failed_datadirs(sections):
    """{runner name: the datadir test_runner.py left} for the failed tests' `sections`."""
    dirs = {}
    for name, text in sections.items():
        m = DATADIR.search(text)
        if m:
            dirs[name] = Path(m.group(1))
    return dirs


def logged_d18(datadirs):
    """The names of `datadirs` ({name: datadir}) with an rdashd node whose debug.log has D18_REFUSAL."""
    def refused(log):
        return D18_REFUSAL.search(log.read_text(encoding="utf8", errors="replace"))
    return {name for name, d in datadirs.items() if any(refused(f) for f in d.glob("node*/regtest/debug.log"))}


# What a D18 abort (exit 1) explains in a test's output: the transport errors of RPCs to the
# aborted node, RDASHD_GONE, and an exit 1 at stop. Anything here is not explained, and is still
# flagged and kept (DEC-483 excuses the abort, not a crash).
CRASH_BEYOND_D18 = re.compile(r"panicked at|exit code \((?:-\d+|101)\)|non-zero exit code \((?!1\))-?\d+\)|"
                              r"exited with status (?:-\d+|101)|rdashd RPC failure: node \d+ exited (?!1\b)-?\d+|"
                              r"Segmentation fault|SIGABRT")


def keep_crashed(tmpdir, evidence, names, keep=(), named=None):
    """Move each test datadir test_runner.py left (a failed test's) whose nodes' stderr matches
    CRASH, or that is a datadir of a test in `keep` (runner names: an RPC to an rdashd that exited, a node that exited non-zero at
    stop, a crash beyond a D18 abort), into a fresh directory under `evidence` (mode 700), for
    private triage; returns the runner names (of `names`) of the tests moved. `named` ({datadir:
    runner name}, from failed_datadirs) names a datadir exactly; any other is named by its script
    stem (`<script stem>_<portseed>`)."""
    named = {Path(d).resolve(): n for d, n in (named or {}).items()}
    kept = []
    for testdir in sorted(Path(tmpdir).glob("test_runner_*/*/")):
        here = testdir.resolve()
        stderr = (f.read_text(encoding="utf8", errors="replace") for f in testdir.glob("node*/stderr/*"))
        if named.get(here) in keep or any(CRASH.search(text) for text in stderr):
            evidence.mkdir(parents=True, exist_ok=True)
            os.chmod(evidence, 0o700)
            shutil.move(testdir, tempfile.mkdtemp(prefix=f"{testdir.name}-", dir=evidence))
            script = testdir.name.rsplit("_", 1)[0]
            kept.extend([named[here]] if here in named else [n for n in names if stem(n) == script])
    return sorted(set(kept))


def run_runner(tree, env, tmpdirprefix, jobs, names, log, evidence):
    """test_runner.py over `names`, their datadirs in a fresh directory under `tmpdirprefix` that
    is removed afterwards (but for keep_crashed's); returns (results, failure sections, the kept
    datadirs' names, the names whose rdashd logged D18_REFUSAL)."""
    Path(tmpdirprefix).mkdir(mode=0o700, parents=True, exist_ok=True)
    tmpdir = tempfile.mkdtemp(prefix="mixed-a-", dir=tmpdirprefix)
    cmd = [sys.executable, str(tree / "src/test/functional/test_runner.py"), f"--jobs={jobs}",
           f"--tmpdirprefix={tmpdir}", *selection(names)]
    try:
        with open(log, "w") as f:
            subprocess.run(cmd, env=env, stdout=f, stderr=subprocess.STDOUT)
        text = log.read_text(encoding="utf8", errors="replace")
        sections = failure_sections(text)
        dirs = failed_datadirs(sections)
        d18 = logged_d18(dirs)
        keep = {n for n in dirs
                if (CRASH_BEYOND_D18.search(sections[n]) if n in d18
                    else RDASHD_GONE.search(sections[n]) or STOP_EXIT.search(sections[n]))}
        kept = keep_crashed(tmpdir, evidence, names, keep, {d: n for n, d in dirs.items()})
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)
    results = parse_runner_output(text)
    missing = [n for n in names if n not in results]
    if missing:
        sys.exit(f"test_runner.py reported no result for {missing}; see {log}")
    return results, sections, kept, d18



def merge_rerun(rerun, started, committed=None):
    """The rerun entries that may replace the first run's, and the names whose rerun may not. A
    rerun that started no rdashd node says nothing about rdashd, so the test keeps its first
    entry, unless the rerun gives back a `committed` "no rdashd node" (DEC-457)."""
    committed = committed or {}
    held = sorted(n for n in rerun if n not in started and not rerun[n] == committed.get(n) == NO_RDASHD)
    return {n: e for n, e in rerun.items() if n not in held}, held


def confirm(committed, first, rerun):
    """BL-392: the entries after a confirming rerun of the tests whose first entry differs from the
    committed one, and {name: outcome}. A rerun that gives the committed entry back means the
    change was a flake ("flaky"), and the committed entry stays. Otherwise the rerun's entry
    stands: "confirmed" if it repeats the first run's, "unstable" if it differs from both."""
    entries, outcome = dict(first), {}
    for name, entry in sorted(rerun.items()):
        entries[name] = entry
        if entry == committed[name]:
            outcome[name] = "flaky"
        else:
            outcome[name] = "confirmed" if entry == first[name] else "unstable"
    return entries, outcome


def publish(path, text):
    """Write `path` atomically: a reader sees the old file or the whole new one."""
    partial = path.with_name(path.name + ".partial")
    partial.write_text(text)
    os.replace(partial, path)


def mixed_a(args):
    if args.update and args.tests:
        sys.exit("mixed-a: --update needs a complete run (no test names)")
    tree = build(args.core, args.work, args.pin, True, args.branch)
    meta = json.loads((tree / "tree.json").read_text())
    # A tree without mode A's hooks runs every node on dashd, so every test would come out a
    # skip: that is a broken run (exit 2, no result), not a difference.
    missing = missing_hooks(tree)
    if meta["branch"] != args.branch or missing:
        print(f"mixed-a: the exported tree ({meta['branch']} {meta['commit']}) is not mode A's"
              f" ({args.branch}); missing: {', '.join(missing) or 'none'}", file=sys.stderr)
        return 2
    dtf = dtf_scripts(tree, scripts_of(tree))
    names = args.tests or dtf
    unknown = [n for n in names if n not in dtf]
    if unknown:
        sys.exit(f"not DashTestFramework tests at {args.pin}: {unknown}")
    kind = MIXED_A_OBS if args.observer else MIXED_A
    passlist = HERE / f"{kind}.json"
    # Read before the run: a bad committed file is a harness error (exit 1, no result written),
    # never a difference.
    committed = json.loads(passlist.read_text())["tests"] if passlist.exists() else {}
    rdashd, rdash_cli, stale = check_adaptations(tree, args.rdashd, args.rdash_cli, args.tmpdirprefix)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    fd, starts_log = tempfile.mkstemp(prefix=f"rdashd-starts-{args.pin}-{kind}-", suffix=".log", dir=out)
    os.close(fd)
    starts_log = Path(starts_log)
    env = {**os.environ, "RDASHD": str(rdashd), "RDASH_CLI": str(rdash_cli), "RDASHD_NODES": MIXED_A_MASK,
           "RDASHD_STARTS": str(starts_log), "PYTHONPATH": str(pylib(args.work)),
           "TEST_RUNNER_PORT_MIN": str(MIXED_A_PORT_MIN), "RDASHD_OBSERVER": "1" if args.observer else "0"}
    crashes, dashd_only = set(), set()

    def one_pass(selected, jobs, log):
        starts_log.write_text("")
        results, sections, kept, d18 = run_runner(tree, env, args.tmpdirprefix, jobs, selected, out / log,
                                                  out / "possible-crashes")
        starts = count_starts(starts_log.read_text(encoding="utf8").splitlines(), results)
        # A D18 abort (DEC-483) is the declared difference, not a possible crash; what it does
        # not explain (CRASH_BEYOND_D18) is kept by run_runner, and so flagged through `kept`.
        crashes.update(n for n, text in sections.items() if n not in d18 and CRASH.search(text))
        crashes.update(kept)
        dashd_only.update(n for n, r in results.items() if r == "Failed" and not starts[n])
        entries = mixed_a_entries(results, sections, starts, args.observer, d18)
        # test_runner.py also runs the names `selected` prefixes: only `selected` are returned.
        return {n: e for n, e in entries.items() if n in selected}, {n for n in selected if starts.get(n)}

    held = set()  # names whose latest rerun started no rdashd node and was not taken

    def rerun(selected, log, committed=None):
        """`selected` run again alone; the entries that may replace the first run's (merge_rerun)."""
        entries, started = one_pass(selected, 1, log)
        entries, kept = merge_rerun(entries, started, committed)
        held.difference_update(entries)
        held.update(kept)
        return entries

    tests, _ = one_pass(names, args.jobs, f"runner-{args.pin}-{kind}.log")
    # A timeout may be the box's load: run each such test again, alone, before classifying it.
    again = [n for n, t in tests.items() if t.get("class") == "timeout"]
    if again:
        print(f"{kind}: rerunning {len(again)} timed-out test(s) alone: {', '.join(again)}", flush=True)
        tests.update(rerun(again, f"runner-{args.pin}-{kind}-rerun.log"))
    # BL-392: a change against the committed passlist stands only if a second run repeats it.
    # Run alone, like a timeout's rerun (D5).
    changed = sorted(n for n, e in tests.items() if n in committed and e != committed[n])
    outcome = {}
    if changed:
        print(f"{kind}: confirming {len(changed)} changed entr(y/ies): {', '.join(changed)}", flush=True)
        tests, outcome = confirm(committed, tests, rerun(changed, f"runner-{args.pin}-{kind}-confirm.log", committed))
        # merge_rerun held these back: the first run's change stands, unconfirmed.
        outcome.update({n: "unconfirmed" for n in changed if n not in outcome})
    starts_log.unlink()
    head = {"pin": args.pin, "mask": MIXED_A_MASK}
    if args.observer:
        head["observer"] = True
    body = json.dumps({**head, "tests": tests}, indent=1, sort_keys=True) + "\n"
    run_meta = {"core_commit": meta["commit"], "branch": meta["branch"], "rdashd": str(rdashd),
                "complete": not args.tests, "reruns": again, "confirm": outcome,
                "rerun_without_rdashd": sorted(held), "possible_crashes": sorted(crashes),
                "dashd_only_failures": sorted(dashd_only)}
    (out / f"{kind}-{args.pin}.run.json").write_text(json.dumps(run_meta, indent=1) + "\n")
    if crashes:
        print(f"{kind}: possible rdashd crash in {', '.join(sorted(crashes))}: see the runner logs and"
              f" {out / 'possible-crashes'} (do not describe it in tracked text)")
    for name, result in outcome.items():
        print(f"{kind}: {name}: change {result}")
    for name in sorted(held):
        print(f"{kind}: {name}: rerun started no rdashd node; first entry kept")
    print(f"{kind} totals: " + ", ".join(f"{k} {v}" for k, v in mixed_a_totals(tests).items()))
    if args.tests:  # a partial run is compared on what it ran
        committed = {n: e for n, e in committed.items() if n in tests}
    diff = mixed_a_diff(committed, tests)
    for line in diff:
        print(f"{kind}: {line}")
    if args.update:
        publish(passlist, body)
        print(f"{kind}: wrote {passlist}")
    else:
        print(f"{kind}: {'DIFFERS from' if diff else 'same as'} {passlist.name}")
    # The result is published last: its presence says the run completed, so the nightly can tell
    # a difference (exit 1, result) from a broken run (no result).
    publish(out / f"{kind}-{args.pin}.json", body)
    return 1 if stale or (diff and not args.update) else 0


# ---- selftest --------------------------------------------------------------------------------

def selftest():
    import unittest
    sys.path.insert(0, str(HERE))
    suite = unittest.defaultTestLoader.loadTestsFromName("test_func_compat")
    return 0 if unittest.TextTestRunner(verbosity=1).run(suite).wasSuccessful() else 1


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    home = Path.home()
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--core", default=home / "workspace/dash", help="Dash Core repository with the pin branches")
    common.add_argument("--work", default=Path(os.environ.get("TMPDIR", "/work/tmp")) / "func-compat",
                        help="where trees are exported")
    lists = argparse.ArgumentParser(add_help=False)
    lists.add_argument("--program", default=home / "workspace/node-program", help="BACKLOG.md and PLAN.md")
    lists.add_argument("--update", metavar="WORKTREE", help="write the grown lists into this Core worktree")
    b = sub.add_parser("build", parents=[common])
    b.add_argument("--pin", choices=PINS, required=True)
    b.add_argument("--wallet", action="store_true")
    node = argparse.ArgumentParser(add_help=False)
    node.add_argument("--pin", choices=PINS, required=True)
    node.add_argument("--rdashd", default=RDC / "target/debug/rdashd")
    node.add_argument("--rdash-cli", default=RDC / "target/debug/rdash-cli")
    node.add_argument("--tmpdirprefix", default=Path(os.environ.get("TMPDIR", "/work/tmp")) / "func-compat-runs",
                      help="root of the test datadirs")
    r = sub.add_parser("run", parents=[common, node, lists])
    r.add_argument("--phase", required=True)
    r.add_argument("--rust-nodes", help="override the phase's RDASHD_NODES mask")
    r.add_argument("--jobs", "-j", type=int, default=4)
    r.add_argument("--out", default=Path(os.environ.get("TMPDIR", "/work/tmp")) / "func-compat-out")
    r.add_argument("tests", nargs="*", help="runner names; default: the phase targets and both lists")
    k = sub.add_parser("ratchet", parents=[common, lists])
    k.add_argument("--matrix", required=True)
    m = sub.add_parser("mixed-a", parents=[common, node])
    m.add_argument("--branch", default=MIXED_A_CORE_BRANCH, help="Core branch to export (default: %(default)s)")
    m.add_argument("--jobs", "-j", type=int, default=2)
    m.add_argument("--out", default=Path(os.environ.get("TMPDIR", "/work/tmp")) / "func-compat-out")
    m.add_argument("--update", action="store_true", help="rewrite the committed passlist instead of diffing")
    m.add_argument("--observer", action="store_true",
                   help=f"add an rdashd observer connected to node 0; passlist {MIXED_A_OBS}.json")
    m.add_argument("tests", nargs="*", help="runner names; default: every DashTestFramework test")
    a = sub.add_parser("adaptations", parents=[common, node])
    a.add_argument("--wallet", action="store_true")
    sub.add_parser("selftest")
    args = p.parse_args(argv)
    if args.cmd == "build":
        print(build(args.core, args.work, args.pin, args.wallet))
        return 0
    if args.cmd == "run":
        return run(args)
    if args.cmd == "ratchet":
        return ratchet(args)
    if args.cmd == "mixed-a":
        return mixed_a(args)
    if args.cmd == "adaptations":
        tree = build(args.core, args.work, args.pin, args.wallet)
        return 1 if check_adaptations(tree, args.rdashd, args.rdash_cli, args.tmpdirprefix)[2] else 0
    return selftest()


if __name__ == "__main__":
    sys.exit(main())
