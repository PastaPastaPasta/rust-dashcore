#!/usr/bin/env python3
"""Regenerate or check SB4's Core-judged vectors (designs/SB4-dash-chain.md §15).

Runs sb4_scenarios (built by build.sh) on every scenario in scenarios/ and on the
real-header jobs below, and writes one gzipped JSON file per case,
dash-chain/tests/data/core-scenarios/<case>.json.gz, each with its provenance
(compressed deterministically: level 9, mtime 0, no name). Before any job
it checks the build against its MANIFEST: the binary's and harness.cpp's
SHA-256, the binary's --commit against the v24.0 pin, and every linked archive
still in the build tree. Every run it writes or accepts must pass admission
(DEC-172/175, R4-1): the plain run and its replays under the `up`, `down` and
at least one seeded heap give one vector, so an unstable case fails instead of
being written in one heap's layout. --check regenerates and fails if any file
differs from the committed one, compressed or decompressed. Bench files must
match exactly except instructions_u, which must be within BENCH_TOLERANCE.
Each sequence in address-sensitive/ must instead be refused by admission first
at the step SENSITIVE_CUTS names, so that a random sequence like it would be cut
before that step, and each in reproducible/ must give one vector per seed in
REPRODUCE_PROCESSES processes (R4-3). With --check it also runs every scenario
that turns SPORK_19 on under --schedules pseudo-random delivery schedules, each
through admission, and fails unless each reaches the same state at the
enforcement fixed point (DEC-147, DEC-167).

--random N --seed S (HX1 X4) instead judges N generated operation sequences (see
random_scenario) and writes each as {scenario, provenance, vector} under
--random-out, not the committed vectors; with --check it compares with the files
there, so a seed rerun must reproduce sequences and outputs byte for byte.
--random-delivery runs each under a random:<seed> callback-delivery schedule
derived from its own seed, instead of boundary-flush. A sequence admission
refuses is cut before the first step at which any two runs differ. A harness
failure whose
terminal diagnostic (the last stderr line) and failure kind match a recorded
finding (findings/*.json, or a private one from $SB4_PRIVATE_FINDINGS) counts as
"known", any other as "fail"; the run passes with no fail and no differ.

usage: regen.py --bin <dir with sb4_scenarios and MANIFEST> [--check] [--only NAME ...]
                [--smoke] [--headers DIR] [--no-headers] [--schedules K]
       regen.py --bin <dir> --random N --seed S [--random-length L] [--random-out DIR]
                [--random-delivery] [--perturb-replays K>=1] [--check] [--jobs J]
"""

import argparse
import concurrent.futures
import hashlib
import glob
import gzip
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile

POLICY = "boundary-flush"

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
OUT = os.path.join(REPO, "dash-chain", "tests", "data", "core-scenarios")
CORE_TAG = "v24.0.0-rc.5"
# The smoke set D22 / Sol r1 m4 requires before the full set (and the gate runs).
SMOKE = ["smoke-pnbh", "a29-vii-fresh-first-lock", "a29-i-off-on-lower-height", "smoke-reload", "smoke-diskindex"]
# Instruction counts move by ~0.04% on agentbox (memory: agentbox-perf-counters).
BENCH_TOLERANCE = 0.005

# Real-header jobs: (name, network, file, op, args). The header files are the CS2
# dumps (/work/scratch/cs2/headers), recorded by SHA-256, not checked in.
HEADER_JOBS = [
    ("locator-mainnet", "main", "mainnet.bin", "locator", [2_551_766, 1, 64]),
    ("pnbh-mainnet-dgw", "main", "mainnet.bin", "pnbh", [2_000_000, 2_000]),
    ("pnbh-mainnet-kgw", "main", "mainnet.bin", "pnbh", [20_000, 2_000]),
    ("pnbh-testnet", "test", "testnet.bin", "pnbh", [1_000_000, 2_000]),
    ("bench-pnbh-mainnet-dgw", "main", "mainnet.bin", "bench-pnbh", [2_000_000, 2_000]),
    ("bench-pnbh-mainnet-kgw", "main", "mainnet.bin", "bench-pnbh", [20_000, 2_000]),
]


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def pinned_commit():
    with open(os.path.join(REPO, "dash-oracle-sys", "pins.txt")) as f:
        for line in f:
            cols = line.split()
            if cols and cols[0] == "v24.0":
                return cols[1]
    sys.exit("pins.txt has no v24.0 pin")


def verify_build(bin_dir, build_root):
    """R1-m1: MANIFEST is checked, not trusted (check-seam.sh's step 0 checks the same)."""
    rows, archives = manifest(bin_dir)
    missing = {"binary", "source", "commit"} - rows.keys()
    if missing or not archives:
        sys.exit(f"MANIFEST lacks {', '.join(sorted(missing)) or 'archive lines'}")
    binary = os.path.join(bin_dir, "sb4_scenarios")
    errors = []
    if sha256_file(binary) != rows["binary"]:
        errors.append("sb4_scenarios does not match MANIFEST's binary hash")
    if rows["source"].split()[0] != sha256_file(os.path.join(HERE, "harness.cpp")):
        errors.append("harness.cpp changed since this build (rebuild with build.sh)")
    commit = subprocess.run([binary, "--commit"], capture_output=True, text=True, check=True).stdout.strip()
    pin = pinned_commit()
    if not (commit == rows["commit"] == pin):
        errors.append(f"--commit {commit}, MANIFEST {rows['commit']}, pin {pin} differ")
    tree = os.path.join(build_root, rows["commit"])
    checked = 0
    if os.path.isdir(tree):
        for path, digest in archives.items():
            full = os.path.join(bin_dir if path in rows.get("object", []) else tree, path)
            if not os.path.exists(full):
                errors.append(f"linked input {path} is missing from {tree}")
            elif sha256_file(full) != digest:
                errors.append(f"linked input {path} does not match MANIFEST")
            else:
                checked += 1
    if errors:
        sys.exit("build check failed:\n  " + "\n  ".join(errors))
    print(f"build ok: binary {rows['binary'][:16]}, commit {commit[:12]} = v24.0 pin, harness source, "
          + (f"{checked} linked inputs" if checked else f"linked inputs not checked ({tree} is absent)"))


def manifest(bin_dir):
    rows = {}
    archives = {}
    with open(os.path.join(bin_dir, "MANIFEST")) as f:
        for line in f:
            key, _, rest = line.rstrip("\n").partition(" ")
            if key in ("wrap", "interpose"):
                rows.setdefault(key, []).append(rest)
            elif key in ("archive", "object"):  # every linked input, checked alike
                digest, path = rest.split(" ", 1)
                archives[path] = digest
                if key == "object":  # build.sh derives these under the out-dir
                    rows.setdefault("object", []).append(path)
            else:
                rows[key] = rest
    return rows, archives


def provenance(bin_dir, command, inputs):
    rows, archives = manifest(bin_dir)
    return {
        "core_tag": CORE_TAG,
        "core_commit": rows["commit"],
        "oracle_pin": rows["pin"],
        "configure": rows["configure"],
        "wrapped": rows["wrap"],
        "interposed": rows.get("interpose", []),
        "compiler": rows["compiler"],
        "harness_source_sha256": rows["source"].split()[0],
        "binary_sha256": rows["binary"],
        "archives_sha256": archives,  # and the interposed objects build.sh derives
        "command": command,
        "inputs_sha256": inputs,
    }


class HarnessFailure(RuntimeError):
    """sb4_scenarios exited non-zero: `kind` is how (FAILURE_KINDS) and
    `diagnostic` its terminal diagnostic, the last non-empty stderr line (the
    harness's error, or the assertion an abort follows), so text printed
    before it, a warning say, never decides what the failure is."""

    def __init__(self, argv, returncode, stderr):
        super().__init__(f"sb4_scenarios {' '.join(argv)} failed:\n{stderr[-4000:]}")
        self.kind = failure_kind(returncode)
        lines = [l for l in stderr.splitlines() if l.strip()]
        self.diagnostic = lines[-1] if lines else ""


# How sb4_scenarios can fail: "error" (exit 1, the harness's own error line),
# "abort" (SIGABRT, e.g. a Core assertion), "signal" (any other signal).
FAILURE_KINDS = ("error", "abort", "signal")


def failure_kind(returncode):
    if returncode == -6:
        return "abort"
    if returncode < 0:
        return "signal"
    return "error" if returncode == 1 else f"exit {returncode}"


def run(bin_dir, argv, tmp, perturb=None):
    """One sb4_scenarios run; `perturb` names a heap-perturbation seed (DEC-172)."""
    env = dict(os.environ, TMPDIR=tmp, LC_ALL="C", TZ="UTC")
    pre = ["--perturb-heap", str(perturb)] if perturb is not None else []
    proc = subprocess.run([os.path.join(bin_dir, "sb4_scenarios")] + pre + argv, env=env, capture_output=True, text=True)
    if proc.returncode != 0:
        raise HarnessFailure(argv, proc.returncode, proc.stderr)
    return json.loads(proc.stdout)


def scenario_inputs(name, headers_dir):
    """The scenario file and, for a preload, the prefix of the header dump it reads."""
    path = os.path.join(HERE, "scenarios", name + ".json")
    rel = os.path.relpath(path, REPO)
    inputs = {rel: sha256_file(path)}
    with open(path) as f:
        pre = json.load(f).get("preload")
    if pre:
        with open(os.path.join(headers_dir, pre["file"]), "rb") as f:
            inputs[f"{pre['file']}[:{pre['count']} headers]"] = hashlib.sha256(f.read(80 * pre["count"])).hexdigest()
    return path, rel, inputs, pre is not None


def scenario_job(bin_dir, name, headers_dir, tmp, delivery=POLICY):
    """A committed scenario under `delivery`, through admission: (doc, admission's difference)."""
    path, rel, inputs, preload = scenario_inputs(name, headers_dir)
    extra = ["--headers", headers_dir] if preload else []
    vector, diff = admission(bin_dir, ["run", path, "--delivery", delivery] + extra, tmp, ("scenario", name, delivery))
    if vector.pop("delivery_policy") != delivery:
        raise RuntimeError(f"{name}: the harness ran another delivery policy")
    command = f"sb4_scenarios run {rel} --delivery {delivery}" + (" --headers <CS2 dumps>" if preload else "")
    prov = provenance(bin_dir, command, inputs)
    prov["delivery_policy"] = delivery
    return {"provenance": prov, "vector": vector}, diff


def uses_chainlocks(name):
    with open(os.path.join(HERE, "scenarios", name + ".json")) as f:
        return any(o["op"] == "spork19" for o in json.load(f)["ops"])


def semantic(vector):
    """DEC-167's comparison: each step's op, result and view (the state at the
    enforcement fixed point), the trace without its transient spans, and each
    lock's first EnforceBlock boundary; where passes ended, the later passes and
    their number are free."""
    out = []
    for step in vector["steps"]:
        view = dict(step["view"])
        trace = view.pop("trace")
        transient = {i for a, b in view.pop("trace_transient_spans") for i in range(a, b + 1)}
        firsts = [{k: v for k, v in b.items() if k != "pass_end"}
                  for b in view.pop("enforce_block_boundaries", []) if not b["repeat"]]
        exact = [e for i, e in enumerate(trace) if i not in transient]
        out.append([step["op"], step.get("result"), view, exact, firsts])
    return out


def passes(vector):
    return [len(s["view"].get("enforce_block_boundaries", [])) for s in vector["steps"]]


def header_job(bin_dir, job, headers_dir, tmp):
    """A real-header job, through admission: (doc, admission's difference). A
    bench's instruction count moves with the heap, so admission ignores it (the
    committed count is within BENCH_TOLERANCE instead)."""
    name, net, fname, op, args = job
    path = os.path.join(headers_dir, fname)
    vector, diff = admission(bin_dir, ["headers", net, path, op] + [str(a) for a in args], tmp, ("headers", name),
                             ignore=("instructions_u",))
    prov = provenance(bin_dir, f"sb4_scenarios headers {net} <{fname}> {op} {' '.join(map(str, args))}",
                      {fname: sha256_file(path)})
    return {"provenance": prov, "vector": vector}, diff


def dump(doc):
    return json.dumps(doc, indent=1) + "\n"


def compress(text):
    """The committed form: gzip at level 9 with mtime 0 and no file name, so the
    same text gives the same bytes."""
    return gzip.compress(text.encode(), compresslevel=9, mtime=0)


def vector_path(name):
    return os.path.join(OUT, name + ".json.gz")


def compare(name, new, old_path, packed=True):
    """Why `new` (JSON text) differs from the file at old_path, or None: a
    committed vector (gzipped, `packed`) or a random run's plain JSON output."""
    if not os.path.exists(old_path):
        return "not committed"
    with open(old_path, "rb") as f:
        stored = f.read()
    old = (gzip.decompress(stored) if packed else stored).decode()
    if name.startswith("bench-"):
        # Everything but the count exactly; the count within the tolerance.
        a, b = json.loads(old), json.loads(new)
        na, nb = a["vector"].pop("instructions_u"), b["vector"].pop("instructions_u")
        if a != b:
            return "differs from the committed vector (other than instructions_u)"
        return None if abs(na - nb) <= BENCH_TOLERANCE * na else f"{nb} instructions vs committed {na}"
    if old != new:
        return "differs from the committed vector"
    if packed and stored != compress(new):
        return "same JSON, but its gzip bytes differ (zlib build?)"
    return None


# --- X4: random operation sequences (designs/CH1-node.md §11, HX1 X4) -------------
# A sequence is an ordinary scenario file built from a seeded generator, so each one
# is judged by Core exactly as a committed scenario is, transcript (X1) and dumps
# (X3) included. Sequence i of seed S uses random.Random(f"{S}:{i}") (string seeds
# hash deterministically, unlike hash()), so any sequence can be rerun on its own.

RANDOM_ARGS = ["-dip3params=2000:2000", "-testactivationheight=v20@2000", "-testactivationheight=mn_rr@2000"]
RANDOM_MAX_HEIGHT = 400  # coinbase-only blocks connect below DIP3 (2,000); keep runs short


def random_scenario(seed, i, length):
    """A random op sequence: a tree of branches built, announced and delivered in
    random orders (mostly with known parents, so Core gets past its first checks),
    invalid blocks, the RPCs, locks with the spork flipping, timers, reloads. The
    generator only guesses what Core knows; Core's verdicts are the output."""
    rng = random.Random(f"{seed}:{i}")
    parent = {"G": None}  # every built label -> its parent
    height = {"G": 0}
    tips = {}  # branch -> its highest label
    headers = {"G"}  # labels whose header Core should have
    data = {"G"}  # labels whose data Core was given
    branches = iter("ABCDEFHIJKLMNOPQSTUVWXYZ")  # not G or R (genesis, preloaded)
    ops = []

    def chain(label):  # label's ancestors, oldest first, back to the first one with data
        out = []
        while label not in data:
            out.append(label)
            label = parent[label]
        return out[::-1]

    def likely_tip():  # the highest label whose whole chain has data
        full = [l for l in data if all(a in data for a in chain(l))]
        return max(full, key=lambda l: (height[l], l))

    def announce(run):
        ops.append({"op": "pnbh", "headers": run})
        headers.update(run)

    def build(branch, base, count, submit, invalid=None):
        op = {"op": "build", "branch": branch, "parent": base, "count": count, "submit": submit}
        if invalid:
            op["invalid"] = invalid
        ops.append(op)
        labels = [f"{branch}{h}" for h in range(height[base] + 1, height[base] + count + 1)]
        prev = base
        for label in labels:
            parent[label], height[label] = prev, height[prev] + 1
            prev = label
        tips[branch] = prev
        if submit != "none":
            headers.update(labels)
        if submit == "connect":
            data.update(labels)
        return labels

    build(next(branches), "G", rng.randint(5, 30), "connect")
    while len(ops) < length:
        kind = rng.choices(
            ["extend", "fork", "deliver", "scatter", "pnbh", "pnb", "invalidate", "reconsider", "precious",
             "propose", "cl", "spork19", "timer", "enforce", "mocktime", "reload"],
            [14, 10, 12, 2, 6, 6, 4, 4, 4, 4, 8, 3, 4, 2, 2, 1])[0]
        if kind in ("extend", "fork"):
            if kind == "extend" and tips:
                branch = rng.choice(sorted(tips))
                base = tips[branch]
            else:
                branch = next(branches, None)
                if branch is None:
                    continue
                base = rng.choice(sorted(headers, key=lambda l: (height[l], l)))
            count = rng.randint(1, 12)
            if height[base] + count > RANDOM_MAX_HEIGHT:
                continue
            submit = rng.choice(["connect", "connect", "headers", "none"])
            invalid = rng.choice([None] * 6 + ["cb-amount", "cb-height"]) if count <= 3 else None
            build(branch, base, count, submit, invalid)
        elif kind == "deliver":
            # Data for a branch's missing blocks, in order or shuffled (out-of-order
            # data fills m_blocks_unlinked); their headers are first sent when unknown.
            missing = sorted(l for l in parent if l not in data)
            if not missing:
                continue
            run = chain(rng.choice(missing))
            if any(l not in headers for l in run):
                announce(run)
            if rng.random() < 0.4:
                rng.shuffle(run)
            ops.append({"op": "connect", "blocks": run})
            data.update(run)
        elif kind == "scatter":  # blocks whose parents Core may not know
            pool = sorted(l for l in parent if l != "G")
            picks = rng.sample(pool, min(len(pool), rng.randint(1, 4)))
            ops.append({"op": "connect", "blocks": picks})
        elif kind == "pnbh":
            run = chain(rng.choice(sorted(parent)))[-rng.randint(1, 8):]
            if run:
                announce(run)
        elif kind == "pnb":
            label = rng.choice(sorted(l for l in parent if l != "G"))
            force = rng.random() < 0.5
            ops.append({"op": "pnb", "block": label, "force": force})
            if parent[label] in headers:
                headers.add(label)
                if force:
                    data.add(label)
        elif kind in ("invalidate", "reconsider", "precious"):
            ops.append({"op": kind, "block": rng.choice(sorted(headers - {"G"}) or ["G"])})
        elif kind == "propose":
            # A fresh child of the likely tip (TestBlockValidity needs the tip as parent).
            base = likely_tip() if rng.random() < 0.8 else rng.choice(sorted(headers))
            branch = next((b for b, l in sorted(tips.items()) if l == base), None) or next(branches, None)
            if branch is None or height[base] + 1 > RANDOM_MAX_HEIGHT:
                continue
            invalid = rng.choice([None, None, "cb-amount", "cb-height"])
            [label] = build(branch, base, 1, "none", invalid)
            ops.append({"op": "propose", "block": label})
        elif kind == "cl":
            known = sorted(headers - {"G"})
            if known and rng.random() < 0.8:
                ops.append({"op": "cl", "block": rng.choice(known)})
            else:
                ops.append({"op": "cl", "unknown": f"u{len(ops)}", "height": rng.randint(1, RANDOM_MAX_HEIGHT)})
        elif kind == "spork19":
            ops.append({"op": "spork19", "value": rng.choice(["on", "on", "off"])})
        elif kind in ("timer", "enforce"):
            ops.append({"op": kind})
        elif kind == "mocktime":
            ops.append({"op": "mocktime", "advance": rng.choice([1, 60, 3600])})
        elif kind == "reload":
            ops.append({"op": "reload"})
    return {
        "name": f"random-{seed}-{i}",
        "description": f"regen.py --random sequence {i} of seed {seed}",
        "rows": [],
        "network": "regtest",
        "args": RANDOM_ARGS,
        "ops": ops,
    }


# DEC-172/175: what Core decides by heap address (D9: m_failed_blocks' pointer
# order, work and sequence-id ties, loaded or live) is found by replaying, not
# predicted. Every run regen.py writes or accepts goes through admission: the
# plain run, then replays under perturbed heaps (perturbations: `up` and `down`,
# which swap any two block index entries, and at least one seeded one), and any
# difference in outcome, state or trace between any two of them is reported at
# the first step that differs: a generated sequence is cut before it, a
# committed scenario, header job or delivery schedule fails (Sol HX1 r4 R4-1).
# The harness's possibly_address_sensitive is a diagnostic label only: a
# predicate missed three times (R1-3, R2-1, R3-1).
# Seeded replays per run, at least one (R4-2), each seed derived from the run's
# own identity (a sequence's seed, index and length; a scenario's name and
# delivery policy; a header job's name), so a cut reproduces from the sequence alone: `up` and
# `down` pick the extremes of a tie among three or more entries, a seeded heap
# may pick a middle one.
PERTURB_REPLAYS = 1
# Committed address-sensitive sequences, each with the step a random sequence
# like it must be cut before: --check requires admission to refuse it first at
# exactly that step.
SENSITIVE_DIR = os.path.join(HERE, "address-sensitive")
SENSITIVE_CUTS = {
    "hx1-r2-walk-new-parent-batch": 5,  # R2-1: a failed walk through an in-batch parent
    "hx1-r3-precious-reset-tie": 7,  # R3-1: a live tie on precious's reset sequence id
}
# R4-3: committed sequences whose seeded replays must reproduce across
# processes. Each maps numeric --perturb-heap seeds to the step at which that
# seed's vector first differs from `up`'s and `down`'s (None: never), which
# --check requires of REPRODUCE_PROCESSES independent runs, all one vector.
REPRODUCIBLE_DIR = os.path.join(HERE, "reproducible")
REPRODUCIBLE = {
    # Sol's three-way reload tie: mark-conflicting on a middle B3 moves m_best_header
    # to B2 at step 5; Sol's three seeds, which each gave both outcomes before.
    "hx1-r4-three-way-mark-middle": {1006220522186421180: 5, 1066088033188675652: None, 314087182294972517: None},
}
REPRODUCE_PROCESSES = 4


def perturb_seed(*parts):
    return int(hashlib.sha256(":".join(map(str, parts)).encode()).hexdigest()[:15], 16)


def perturbations(replays, *parts):
    """The --perturb-heap modes a run is replayed under: `up`, `down`, and
    `replays` seeds derived from `parts`; `replays` must be at least 1 (R4-2)."""
    if not isinstance(replays, int) or replays < 1:
        raise ValueError(f"at least one seeded replay is required, not {replays!r} (R4-2)")
    return ["up", "down"] + [perturb_seed(*parts, k) for k in range(replays)]


STEP_CHANNELS = ("transcript", "accept_checks")  # lists of entries, each with its step


def first_difference(a, b):
    """The earliest step (index into `steps`; 0 is setup) at which two runs'
    vectors differ, or None: the minimum over every channel (R3-2), namely the
    steps' results and views by index, the transcript and acceptance verdicts by
    their first differing entry's step (an entry only one run has counts at its
    own step), and any other field at the last step. A vector without steps (a
    header job) differs at 0."""
    if "steps" not in a or "steps" not in b:
        return None if a == b else 0
    sa, sb = a["steps"], b["steps"]
    last = min(len(sa), len(sb)) - 1
    found = []
    for k in range(max(len(sa), len(sb))):
        if k >= len(sa) or k >= len(sb) or sa[k] != sb[k]:
            found.append(k)
            break
    for key in STEP_CHANNELS:
        xs, ys = a.get(key) or [], b.get(key) or []
        for k in range(max(len(xs), len(ys))):
            x, y = (xs[k] if k < len(xs) else None), (ys[k] if k < len(ys) else None)
            if x != y:
                found.append(min(e.get("step", last) for e in (x, y) if e is not None))
                break
    if any(a.get(key) != b.get(key) for key in (set(a) | set(b)) - {"steps", *STEP_CHANNELS}):
        found.append(last)
    return min(found, default=None)


def perturbed_difference(bin_dir, argv, tmp, base, modes, ignore=()):
    """Replays `argv` under each perturbation mode: (mode, HarnessFailure) of
    the first replay that fails; else (mode, step) for the first step at which
    any two of the runs (`base`, the plain run, and the replays; the `ignore`d
    fields aside) differ, the mode naming a replay of that pair; or None when
    all match. The replays reproduce, and `up` and `down` disagree wherever two
    entries' order decides, so the step depends on the plain run's layout only
    where it decided some other way than every replay."""
    def strip(v):
        return {k: x for k, x in v.items() if k not in ignore and k != "delivery_policy"}

    runs = [(None, strip(base))]
    for m in modes:
        try:
            runs.append((m, strip(run(bin_dir, argv, tmp, perturb=m))))
        except HarnessFailure as e:
            return m, e
    found = None
    for i, (_, x) in enumerate(runs):
        for m, y in runs[i + 1:]:
            k = first_difference(x, y)
            if k is not None and (found is None or k < found[1]):
                found = (m, k)
    return found


def admission(bin_dir, argv, tmp, parts, replays=PERTURB_REPLAYS, ignore=()):
    """The one admission rule (R4-1) for every run regen.py writes or accepts: a
    generated sequence, a committed scenario or header job (written or checked)
    and each delivery schedule. Runs `argv` plain (a HarnessFailure propagates),
    then under perturbations(replays, *parts): (the plain vector, None) when
    every run matches, else (it, perturbed_difference's (mode, step or
    HarnessFailure))."""
    modes = perturbations(replays, *parts)  # refuses replays < 1 before any run
    vector = run(bin_dir, argv, tmp)
    return vector, perturbed_difference(bin_dir, argv, tmp, vector, modes, ignore)


def instability(diff):
    """Why admission refused a committed run, or None."""
    if diff is None:
        return None
    mode, k = diff
    if isinstance(k, HarnessFailure):
        return f"fails under --perturb-heap {mode}: {k.diagnostic}"
    return f"differs under --perturb-heap {mode}" + (f" from step {k}" if k else "")


def known_finding(entry, name, where):
    """A recorded finding as (kind, signature, name, private dir or None). The
    signature must be non-empty text of the terminal diagnostic, and the kind one
    of FAILURE_KINDS; a finding matches only a failure of that kind whose
    terminal diagnostic contains it."""
    kind, signature = entry.get("kind"), entry.get("signature")
    if kind not in FAILURE_KINDS:
        raise ValueError(f"{name}: kind must be one of {', '.join(FAILURE_KINDS)}, not {kind!r}")
    if not isinstance(signature, str) or not signature.strip():
        raise ValueError(f"{name}: empty signature")
    return kind, signature, name, where


def known_findings():
    """The Core behaviours already found: findings/*.json, the minimized
    sequences, each with the kind and terminal diagnostic of its failure ("kind",
    "signature"), and the private ones (PLAN §7) listed in
    $SB4_PRIVATE_FINDINGS/signatures.json as [{name, kind, signature}]."""
    known = []
    for f in sorted(glob.glob(os.path.join(HERE, "findings", "*.json"))):
        with open(f) as fh:
            known.append(known_finding(json.load(fh), f"findings/{os.path.basename(f)}", None))
    private = os.environ.get("SB4_PRIVATE_FINDINGS")
    if private:
        with open(os.path.join(private, "signatures.json")) as f:
            known += [known_finding(e, f"{e.get('name')} (private)", private) for e in json.load(f)]
    return known


def match_finding(failure, known):
    """The recorded finding a HarnessFailure is, or None: same kind, and its
    signature in the terminal diagnostic."""
    return next(((n, d) for k, s, n, d in known if k == failure.kind and s in failure.diagnostic), None)


def random_job(bin_dir, seed, i, length, out_dir, tmp, delivery, known, replays=PERTURB_REPLAYS):
    """Sequence i of seed S, judged by Core: returns (name, outcome, file text),
    the file being {scenario, provenance, vector}. Every run goes through
    admission (DEC-175: `up`, `down` and `replays` seeded heaps, at least one,
    whatever the harness's possibly_address_sensitive label says); if any two runs differ
    from step N on, the sequence is cut to the ops before step N and judged
    again ("cut"). A harness failure is a finding, its
    stderr kept in <out_dir>/<name>.err: "known" when it carries the signature
    of a recorded one (`known`; a private one's stderr goes to its own
    directory), else "fail"; so is a replay that fails where the plain run did
    not, or one differing at setup."""

    def finding(e, note=""):
        found, err_dir = match_finding(e, known) or (None, None)
        with open(os.path.join(err_dir or out_dir, name + ".err"), "w") as f:
            f.write(note + str(e) + "\n")
        return name, "known" if found else "fail", found or text

    scenario = random_scenario(seed, i, length)
    name = scenario["name"]
    outcome = "ok"
    while True:
        path = os.path.join(tmp, name + ".json")
        text = json.dumps(scenario, indent=1) + "\n"
        with open(path, "w") as f:
            f.write(text)
        argv = ["run", path, "--delivery", delivery]
        try:
            vector, diff = admission(bin_dir, argv, tmp, (seed, i, len(scenario["ops"])), replays)
        except HarnessFailure as e:
            return finding(e)
        vector.pop("delivery_policy")
        if diff is None:
            break
        s, k = diff
        if isinstance(k, HarnessFailure):
            return finding(k, f"only under --perturb-heap {s} (the plain run passed):\n")
        if k == 0:
            with open(os.path.join(out_dir, name + ".err"), "w") as f:
                f.write(f"setup differs under --perturb-heap {s}\n")
            return name, "fail", text
        scenario["ops"] = scenario["ops"][: k - 1]
        outcome = "cut"
    prov = provenance(bin_dir, f"regen.py --random {i + 1} --seed {seed} (sequence {i}), delivery {delivery}",
                      {"scenario": hashlib.sha256(text.encode()).hexdigest()})
    prov["delivery_policy"] = delivery
    return name, outcome, dump({"scenario": scenario, "provenance": prov, "vector": vector})


def random_delivery(seed, i):
    return f"random:{int(hashlib.sha256(f'{seed}:{i}'.encode()).hexdigest()[:15], 16)}"


def random_main(a, bin_dir, tmp):
    os.makedirs(a.random_out, exist_ok=True)
    if not a.check:  # a rerun's outputs must not sit beside an earlier run's
        for f in glob.glob(os.path.join(a.random_out, f"random-{a.seed}-*.*")):
            os.remove(f)
    known = known_findings()
    counts = {"ok": 0, "cut": 0, "known": 0, "fail": 0, "differ": 0}

    def one(i):
        jtmp = os.path.join(tmp, f"random-{i}")
        os.makedirs(jtmp)
        try:
            delivery = random_delivery(a.seed, i) if a.random_delivery else POLICY
            return random_job(bin_dir, a.seed, i, a.random_length, a.random_out, jtmp, delivery, known,
                              a.perturb_replays)
        finally:
            shutil.rmtree(jtmp, ignore_errors=True)  # a thousand datadirs would fill the tmpfs

    with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool:
        for name, outcome, text in pool.map(one, range(a.random)):
            dest = os.path.join(a.random_out, name + ".json")
            if outcome in ("fail", "known") and a.check and os.path.exists(dest):
                print(f"FAIL {name}: the harness failed (see {name}.err) where the earlier run did not")
                outcome = "differ"
            elif outcome == "fail":
                print(f"FAIL {name}: the harness failed (see {name}.err)")
            elif outcome == "known":
                print(f"KNOWN {name}: {text}")
            elif a.check:
                err = compare(name, text, dest, packed=False)
                if err:
                    outcome = "differ"
                    print(f"FAIL {name}: {err}")
            else:
                with open(dest, "w") as f:
                    f.write(text)
            counts[outcome] += 1
    print(f"random seed={a.seed} sequences={a.random} length={a.random_length} "
          f"delivery={'random' if a.random_delivery else POLICY} "
          + " ".join(f"{k}={v}" for k, v in counts.items()))
    return counts["fail"] + counts["differ"] == 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--smoke", action="store_true", help="only the smoke set")
    ap.add_argument("--only", nargs="*")
    ap.add_argument("--headers", default="/work/scratch/cs2/headers")
    ap.add_argument("--no-headers", action="store_true")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--schedules", type=int, default=8, help="random delivery schedules per CL scenario (--check)")
    ap.add_argument("--random", type=int, metavar="N", help="judge N generated operation sequences (X4)")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--random-length", type=int, default=30, help="ops per generated sequence")
    ap.add_argument("--random-out", default="/work/scratch/sb4-core-scenarios/random")
    ap.add_argument("--random-delivery", action="store_true", help="a random:<seed> schedule per sequence")
    ap.add_argument("--perturb-replays", type=int, default=PERTURB_REPLAYS,
                    help="seeded perturbed replays of every sequence, besides up and down, at least 1 (R4-2)")
    a = ap.parse_args()
    if a.perturb_replays < 1:
        ap.error("--perturb-replays: at least one seeded replay is required (R4-2)")
    bin_dir = os.path.abspath(a.bin)
    verify_build(bin_dir, os.environ.get("SB4_BUILD", "/work/scratch/sb4-core-scenarios/core-build"))
    # Per-job datadirs: TMPDIR when set (agentbox's /dev/shm is a shared quota), else /dev/shm.
    tmp_root = os.environ.get("TMPDIR") or ("/dev/shm" if os.path.isdir("/dev/shm") else None)
    if a.random is not None:
        with tempfile.TemporaryDirectory(prefix="sb4-random-", dir=tmp_root) as tmp:
            sys.exit(0 if random_main(a, bin_dir, tmp) else 1)

    names = sorted(f[:-5] for f in os.listdir(os.path.join(HERE, "scenarios")) if f.endswith(".json"))
    if a.smoke:
        names = SMOKE
    jobs = [("scenario", n) for n in names]
    if not a.smoke and not a.no_headers:
        jobs += [("headers", j) for j in HEADER_JOBS]
    if a.only:
        unknown = set(a.only) - {j[1] if j[0] == "scenario" else j[1][0] for j in jobs}
        if unknown:
            sys.exit(f"--only: no such case: {', '.join(sorted(unknown))}")
        jobs = [j for j in jobs if (j[1] if j[0] == "scenario" else j[1][0]) in a.only]

    os.makedirs(OUT, exist_ok=True)
    failures = []
    with tempfile.TemporaryDirectory(prefix="sb4-core-scenarios-", dir=tmp_root) as tmp:
        def work(job):
            kind, spec = job
            name = spec if kind == "scenario" else spec[0]
            jtmp = os.path.join(tmp, name)
            os.makedirs(jtmp)
            doc, diff = (scenario_job if kind == "scenario" else header_job)(bin_dir, spec, a.headers, jtmp)
            return name, doc, dump(doc), instability(diff)

        def sensitive(path):
            """A committed address-sensitive sequence (DEC-172/175 regression):
            admission must refuse it first at exactly its SENSITIVE_CUTS step,
            whatever the harness's label says."""
            name = os.path.basename(path)[:-5]
            if name not in SENSITIVE_CUTS:
                return name, "no SENSITIVE_CUTS entry"
            jtmp = os.path.join(tmp, "sensitive-" + name)
            os.makedirs(jtmp)
            _, diff = admission(bin_dir, ["run", path, "--delivery", POLICY], jtmp, ("address-sensitive", name, POLICY))
            if diff is None:
                return name, "admitted under every perturbation"
            if isinstance(diff[1], HarnessFailure):  # a broken replay is not a difference
                return name, f"failed under --perturb-heap {diff[0]}: {diff[1].diagnostic}"
            if diff[1] != SENSITIVE_CUTS[name]:  # an earlier, unrelated difference proves nothing
                return name, f"first differs at step {diff[1]}, not {SENSITIVE_CUTS[name]}"
            return name, None

        def reproducible(path):
            """R4-3's regression: under each REPRODUCIBLE seed, REPRODUCE_PROCESSES
            independent runs give one vector, which first differs from `up`'s and
            `down`'s at the recorded step."""
            name = os.path.basename(path)[:-5]
            if name not in REPRODUCIBLE:
                return name, "no REPRODUCIBLE entry"
            jtmp = os.path.join(tmp, "reproducible-" + name)
            os.makedirs(jtmp)
            argv = ["run", path, "--delivery", POLICY]
            # One jtmp for every run: each makes its own datadir under it.
            ends = [run(bin_dir, argv, jtmp, perturb=m) for m in ("up", "down")]
            for seed, want in REPRODUCIBLE[name].items():
                vectors = [run(bin_dir, argv, jtmp, perturb=seed) for _ in range(REPRODUCE_PROCESSES)]
                if any(v != vectors[0] for v in vectors):
                    return name, f"--perturb-heap {seed} gives different vectors in different processes"
                got = [first_difference(e, vectors[0]) for e in ends]
                if got != [want, want]:
                    return name, f"--perturb-heap {seed} first differs from up, down at {got}, not {want}"
            return name, None

        def schedule(job):
            name, seed = job
            jtmp = os.path.join(tmp, f"{name}-random-{seed}")
            os.makedirs(jtmp)
            doc, diff = scenario_job(bin_dir, name, a.headers, jtmp, f"random:{seed}")
            return name, seed, doc["vector"], instability(diff)

        flushed = {}
        n_exact = n_tolerance = 0
        with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool:
            for name, doc, text, unstable in pool.map(work, jobs):
                dest = vector_path(name)
                flushed[name] = doc["vector"]
                if a.check:
                    err = compare(name, text, dest) or unstable
                    print(f"{'FAIL' if err else 'ok  '} {name}" + (f": {err}" if err else ""))
                    if err:
                        failures.append(f"{name}: {err}")
                    elif name.startswith("bench-"):
                        n_tolerance += 1
                    else:
                        n_exact += 1
                elif unstable:  # R4-1: an unstable case is not written in one heap's layout
                    print(f"FAIL {name}: {unstable}; not written")
                    failures.append(f"{name}: {unstable}")
                else:
                    with open(dest, "wb") as f:
                        f.write(compress(text))
                    print(f"wrote {os.path.relpath(dest, REPO)}")
            if a.check:
                print(f"{n_exact} exact + {n_tolerance} tolerance-checked, each admitted under up, down and a seeded heap")
                for title, where, table, check in (("address-sensitive", SENSITIVE_DIR, SENSITIVE_CUTS, sensitive),
                                                   ("reproducible", REPRODUCIBLE_DIR, REPRODUCIBLE, reproducible)):
                    committed = sorted(glob.glob(os.path.join(where, "*.json")))
                    for name in sorted(set(table) - {os.path.basename(f)[:-5] for f in committed}):
                        failures.append(f"{title} {name}: missing from {where}")
                    for name, err in pool.map(check, committed):
                        print(f"{'FAIL' if err else 'ok  '} {title} {name}" + (f": {err}" if err else ""))
                        if err:
                            failures.append(f"{title} {name}: {err}")
            cl = [n for kind, n in jobs if kind == "scenario" and uses_chainlocks(n)] if a.check else []
            runs = {}
            for name, seed, vector, unstable in pool.map(schedule, [(n, s) for n in cl for s in range(1, a.schedules + 1)]):
                runs.setdefault(name, []).append((seed, vector))
                if unstable:
                    print(f"FAIL {name}: delivery random:{seed} {unstable}")
                    failures.append(f"{name}: delivery random:{seed} {unstable}")
            for name in cl:
                base = semantic(flushed[name])
                bad = [seed for seed, v in runs[name] if semantic(v) != base]
                spread = [passes(flushed[name])] + [passes(v) for _, v in runs[name]]
                per_step = [f"{min(c)}-{max(c)}" if min(c) != max(c) else str(c[0])
                            for c in zip(*spread) if max(c) > 0]
                print(f"{'FAIL' if bad else 'ok  '} {name}: {a.schedules} schedules, enforcement passes per step "
                      f"{' '.join(per_step) or 'none'}" + (f"; state differs for seeds {bad}" if bad else ""))
                if bad:
                    failures.append(f"{name}: schedules {bad} reach another state")
    if failures:
        sys.exit(f"{len(failures)} check(s) failed")


if __name__ == "__main__":
    main()
