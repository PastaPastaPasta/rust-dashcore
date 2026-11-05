#!/usr/bin/env python3
"""The reject-string census of EV1 (designs/EV1-specialtx.md §3.13).

Reads Dash Core's sources at the pinned tag, straight from git (no checkout needed), and lists
every `Invalid(` call in the scoped functions, whatever its arguments, keyed by `file:line`:

- EV1's functions (scope `ev1`): the special-tx checks of §3.1;
- the neighbouring block-path and state-transition functions in the same files (scope `other`),
  which belong to EV2b, CH7a and EV3 and are listed so that a moved line is noticed.

A call whose reason is a string literal is a `literal` row. A call whose reason is anything else
must have an entry in the classification table (`ev1-census-classified.toml`), with its class and
every reason it can forward; it becomes one `forward` row per reason. An unclassified call, a
stale table entry, or a class that disagrees with the call fails the script. The table also marks
census rows `unreachable`, with the Core reason why; the mark goes into the row's note.

The output, `dash-dml/tests/data/ev1_census.tsv`, is what `specialtx::reasons::census_matches_pin`
checks the reason constants against.

Usage:
  ev1-census.py --core <Dash Core clone>            # rewrite the census
  ev1-census.py --core <Dash Core clone> --check    # fail if the census is stale

The tag must resolve to the recorded commit (`EXPECTED_COMMIT`); moving the pin is a deliberate
edit of this file. The script only reads the clone (`git show`).

`--pairs` validates the D19 pair table (`ev1-pairs.toml`, EV1 §6.2) against the checked-in census
instead, with no clone: every step is labelled with its table's key and names census reasons, and a loop step is a
driven step of its table; an
exclusion names steps or pairs of the table and carries its reason; an `elsewhere` test, and a table's
`driver`, exists; an
equal-string pair shares its reasons and an infeasible or world one does not; the file keeps the shape the
L1 test's line parser reads (D29). It prints each table's pair counts next to census coverage.
It does not check completeness: that each table lists every step of its Core function is by
review against the cited lines. The census itself is closed (every `Invalid(` site of the scoped
functions is a row), and the report shows how many EV1 reasons the driven steps reach.

  ev1-census.py --pairs                             # validate ev1-pairs.toml
"""

import argparse
import collections
import os
import re
import subprocess
import sys
import tomllib

TAG = "v24.0.0-rc.4"
# The commit the (annotated) tag peels to, as `git rev-parse <tag>^{commit}` prints it.
EXPECTED_COMMIT = "0e7382df80d0bc6811068d076b232a5a26d34a3c"

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "..", "dash-dml", "tests", "data", "ev1_census.tsv"))
TABLE = os.path.join(HERE, "ev1-census-classified.toml")
PAIRS = os.path.join(HERE, "ev1-pairs.toml")
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))

# (file, first line, last line, scope, what): the function ranges at the pin.
SCOPES = [
    ("src/evo/specialtxman.cpp", 33, 137, "other", "AddNetInfoEntries, SetStateVersion, ApplyProUpServTx (EV2b)"),
    ("src/evo/specialtxman.cpp", 139, 216, "other", "CheckCbTxBestChainlock (CH7a)"),
    ("src/evo/specialtxman.cpp", 217, 274, "ev1", "CheckSpecialTxInner"),
    ("src/evo/specialtxman.cpp", 283, 855, "other", "BuildNewListFromBlock and the list builder (EV2b)"),
    ("src/evo/specialtxman.cpp", 857, 1059, "other", "ProcessSpecialTxsInBlock (CH7a)"),
    ("src/evo/specialtxman.cpp", 1096, 1126, "other", "CheckCreditPoolDiffForBlock (EV3)"),
    ("src/evo/specialtxman.cpp", 1128, 1909, "ev1", "the ProTx checks and the shared-collateral covenant"),
    ("src/evo/providertx.cpp", 1, None, "ev1", "IsTriviallyValid, the payout/share lists, the network fields"),
    ("src/evo/providertx.h", 560, 567, "ev1", "CheckInputsHash"),
    ("src/evo/cbtx.cpp", 24, 55, "ev1", "CheckCbTx"),
    ("src/evo/cbtx.cpp", 60, 155, "other", "CheckCbTxMerkleRoots and friends (CH7a)"),
    ("src/evo/mnhftx.cpp", 94, 157, "ev1", "CheckMNHFTx"),
    ("src/evo/mnhftx.cpp", 190, 260, "other", "the MNHF block processing (EV3)"),
    ("src/evo/assetlocktx.cpp", 29, 232, "ev1", "CheckAssetLockTx, CheckAssetUnlockTx"),
    ("src/evo/assetlocktx.cpp", 256, 270, "other", "the asset-unlock block check (EV3)"),
    ("src/llmq/commitment.cpp", 240, 310, "ev1", "CheckLLMQCommitment"),
]

# The census totals at the pin. The design (§3.13) records rc.3's 213 sites and 144 TX_BAD_SPECIAL;
# rc.4 drops the two `bad-protx-netinfo-bad` entry loops of the ProReg and ProUpServ trivial checks
# (CE4-03; providertx.cpp rc.3:322-326, 433-437), and the reason stays through `CheckNetInfo`.
EXPECTED_EV1 = {"sites": 211, "TX_BAD_SPECIAL": 142, "TX_CONSENSUS": 69, "distinct": 146}

INVALID = re.compile(r"\bInvalid\(")
ARGS = re.compile(r'\s*(?:TxValidationResult|BlockValidationResult)::(\w+)\s*,\s*("(?:[^"\\]|\\.)*")?')


def git(core, *args):
    return subprocess.run(["git", "-C", core, *args], check=True, capture_output=True, text=True).stdout


def call_args(src, start):
    """The text between `Invalid(` (ending at `start`) and its closing parenthesis."""
    depth, j = 1, start
    while depth:
        c = src[j]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == '"':
            j += 1
            while src[j] != '"':
                j += 2 if src[j] == "\\" else 1
        j += 1
    return src[start : j - 1]


def calls(core):
    """Every `Invalid(` call in a scoped range: (site, scope, class or None, literal or None, args)."""
    sources = {}
    out = []
    for path, first, last, scope, _ in SCOPES:
        if path not in sources:
            sources[path] = git(core, "show", f"{TAG}:{path}")
        src = sources[path]
        for m in INVALID.finditer(src):
            line = src.count("\n", 0, m.start()) + 1
            if line < first or (last is not None and line > last):
                continue
            args = call_args(src, m.end())
            parsed = ARGS.match(args)
            cls = parsed.group(1) if parsed else None
            literal = parsed.group(2)[1:-1] if parsed and parsed.group(2) else None
            out.append((f"{path}:{line}", scope, cls, literal, " ".join(args.split())))
    return out


def build(core):
    with open(TABLE, "rb") as f:
        table = tomllib.load(f)
    classified = {e["site"]: e for e in table.get("nonliteral", [])}
    unreachable = {e["site"]: e for e in table.get("unreachable", [])}
    errors = []
    rows = []
    seen_nonliteral = set()
    for site, scope, cls, literal, args in calls(core):
        if literal is not None:
            note = ""
            if site in unreachable:
                u = unreachable.pop(site)
                if u["reason"] != literal:
                    errors.append(f"{site}: unreachable mark names {u['reason']!r}, Core has {literal!r}")
                note = f"unreachable: {u['why']}"
            rows.append((site, scope, cls, literal, "literal", note))
            continue
        entry = classified.get(site)
        if entry is None:
            errors.append(f"{site}: unclassified non-literal reject ({args})")
            continue
        seen_nonliteral.add(site)
        if entry["class"] != cls:
            errors.append(f"{site}: classified as {entry['class']}, Core passes {cls}")
        if entry["expression"] not in args:
            errors.append(f"{site}: Core's argument is {args!r}, the table names {entry['expression']!r}")
        for reason in entry["reasons"]:
            rows.append((site, scope, cls, reason, "forward", f"forwards {entry['expression']}"))
    for site in classified.keys() - seen_nonliteral:
        errors.append(f"{site}: stale classification entry (no non-literal reject there)")
    for site in unreachable:
        errors.append(f"{site}: stale unreachable mark (no literal reject there)")
    ev1 = [r for r in rows if r[1] == "ev1" and r[4] == "literal"]
    totals = collections.Counter(r[2] for r in ev1)
    got = {"sites": len(ev1), "distinct": len({r[3] for r in ev1}), **totals}
    for key, want in EXPECTED_EV1.items():
        if got.get(key) != want:
            errors.append(f"EV1 {key}: {got.get(key)} at the pin, the design records {want}")
    return rows, got, errors


def render(rows, got):
    lines = [
        f"# EV1 reject-string census (EV1 §3.13): Dash Core {TAG} ({EXPECTED_COMMIT}).",
        "# Generated by contrib/node/ev1-census.py from ev1-census-classified.toml; do not edit.",
        f"# EV1 literal sites: {got['sites']} ({got['TX_BAD_SPECIAL']} TX_BAD_SPECIAL, "
        f"{got['TX_CONSENSUS']} TX_CONSENSUS), {got['distinct']} distinct reasons.",
        "# site\tscope\tclass\treason\tform\tnote",
    ]
    lines += ["\t".join(r) for r in rows]
    return "\n".join(lines) + "\n"


# The restricted shape of ev1-pairs.toml (D29): every line is blank, a comment, an entry header, a
# `key = "string"`, a `key = [` with its strings one per line, or the closing `]`.
SHAPE = [
    re.compile(r"\s*(#.*)?"),
    re.compile(r"\[\[[a-z-]+\]\]"),
    re.compile(r'[a-z]+ = "[^"\\]*"'),
    re.compile(r"[a-z]+ = \["),
    re.compile(r'  "[^"\\]*",'),
    re.compile(r"\]"),
]
KINDS = {"table", "unreachable", "elsewhere", "infeasible", "world", "equal-string"}


def census_reasons(path=OUT):
    """Every reason of the checked-in census, with whether an EV1 literal site gives it."""
    reasons = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.strip() and not line.startswith("#"):
                _, scope, _, reason, form, _ = line.rstrip("\n").split("\t")
                reasons[reason] = reasons.get(reason, False) or (scope == "ev1" and form == "literal")
    return reasons


def has_test(root, ref):
    """Whether `file::fn` names a function in that file."""
    path, _, name = ref.partition("::")
    try:
        with open(os.path.join(root, path), encoding="utf-8") as f:
            return re.search(rf"\bfn {re.escape(name)}\(", f.read()) is not None
    except OSError:
        return False


def check_pairs(text, census, root=ROOT):
    """Validate the pair table's text: (errors, per-table report lines)."""
    errors = []
    for n, line in enumerate(text.splitlines(), 1):
        if not any(r.fullmatch(line) for r in SHAPE):
            errors.append(f"line {n}: outside the restricted shape (D29): {line!r}")
    try:
        doc = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        return errors + [f"not TOML: {e}"], []
    for kind in doc.keys() - KINDS:
        errors.append(f"unknown entry kind [[{kind}]]")
    steps, table_of, tables = {}, {}, {}
    for t in doc.get("table", []):
        key = t.get("key", "")
        if not key or not t.get("section") or not t.get("function") or not t.get("steps"):
            errors.append(f"table {key!r}: needs key, section, function and steps")
            continue
        if key in tables:
            errors.append(f"table {key}: declared twice")
        tables[key] = t
        if "driver" in t and not has_test(root, t["driver"]):
            errors.append(f"table {key}: driver {t['driver']} not found")
        for entry in t["steps"]:
            if not entry.split():
                errors.append(f"table {key}: an empty step")
                continue
            step, *reasons = entry.split()
            if step.partition(":")[0] != key:
                errors.append(f"table {key}: step {step} is not labelled {key}:")
            if step in steps:
                errors.append(f"table {key}: step {step} declared twice")
            if not reasons:
                errors.append(f"table {key}: step {step} names no reason")
            for r in reasons:
                if r not in census:
                    errors.append(f"table {key}: step {step}: {r} is not a census reason")
            steps[step] = frozenset(reasons)
            table_of[step] = key
        for step in t.get("loops", []):
            if table_of.get(step) != key:
                errors.append(f"table {key}: loop step {step} is not a step of the table")
    excluded = {}
    for kind in ("unreachable", "elsewhere"):
        for e in doc.get(kind, []):
            if not e.get("why", "").strip():
                errors.append(f"[[{kind}]] {e.get('steps')}: no reason")
            if not e.get("steps"):
                errors.append(f"[[{kind}]]: names no step")
            for step in e.get("steps", []):
                if step not in steps:
                    errors.append(f"[[{kind}]]: {step} is not a step of any table")
                elif step in excluded:
                    errors.append(f"[[{kind}]]: {step} is already {excluded[step]}")
                else:
                    excluded[step] = kind
            if kind == "elsewhere":
                if not e.get("tests"):
                    errors.append(f"[[elsewhere]] {e.get('steps')}: names no test")
                for ref in e.get("tests", []):
                    if not has_test(root, ref):
                        errors.append(f"[[elsewhere]]: test {ref} not found")
    for t in tables.values():
        for step in t.get("loops", []):
            if step in excluded:
                errors.append(f"table {t['key']}: loop step {step} is {excluded[step]}")
    listed = {}
    for kind in ("infeasible", "world", "equal-string"):
        for e in doc.get(kind, []):
            if not e.get("why", "").strip():
                errors.append(f"[[{kind}]] {e.get('pairs')}: no reason")
            if not e.get("pairs"):
                errors.append(f"[[{kind}]]: names no pair")
            for pair in e.get("pairs", []):
                ab = pair.split()
                if len(ab) != 2 or ab[0] == ab[1]:
                    errors.append(f"[[{kind}]]: {pair!r} is not two steps")
                    continue
                missing = [s for s in ab if s not in steps]
                if missing:
                    errors.append(f"[[{kind}]] {pair}: {', '.join(missing)} is not a step of any table")
                    continue
                if table_of[ab[0]] != table_of[ab[1]]:
                    errors.append(f"[[{kind}]] {pair}: the steps are in different tables")
                for s in ab:
                    if s in excluded:
                        errors.append(f"[[{kind}]] {pair}: {s} is {excluded[s]}, so no pair of it runs")
                key = frozenset(ab)
                if key in listed:
                    errors.append(f"[[{kind}]] {pair}: already {listed[key]}")
                listed[key] = kind
                same = steps[ab[0]] == steps[ab[1]]
                if kind == "equal-string" and not same:
                    errors.append(f"[[equal-string]] {pair}: the reasons differ")
                if kind in ("infeasible", "world") and same:
                    errors.append(f"[[{kind}]] {pair}: the reasons are equal (no pair to reach)")
    report = []
    covered = set()
    for key, t in tables.items():
        names = [s for s, k in table_of.items() if k == key]
        driven = [s for s in names if s not in excluded]
        covered |= {r for s in driven for r in steps[s]}
        pairs = [frozenset((a, b)) for i, a in enumerate(driven) for b in driven[i + 1 :]]
        equal = sum(steps[a] == steps[b] for a, b in map(tuple, pairs))
        infeasible = sum(listed.get(p) == "infeasible" for p in pairs)
        world = sum(listed.get(p) == "world" for p in pairs)
        report.append(
            f"{key:4} §{t['section']:5} {len(names):2} steps, {len(driven):2} driven; pairs: "
            f"{len(pairs) - equal - infeasible - world:3} to reach, {infeasible:2} infeasible, "
            f"{world} world, {equal:2} equal-string"
        )
    ev1 = {r for r, own in census.items() if own}
    report.append(f"census: the driven steps reach {len(covered & ev1)} of the {len(ev1)} EV1 reasons")
    return errors, report


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--core", help="a Dash Core clone that has the tag")
    p.add_argument("--check", action="store_true", help="compare with the checked-in file instead of writing it")
    p.add_argument("--pairs", action="store_true", help="validate ev1-pairs.toml against the checked-in census")
    a = p.parse_args()
    if a.pairs:
        with open(PAIRS, encoding="utf-8") as f:
            errors, report = check_pairs(f.read(), census_reasons())
        if errors:
            sys.exit("ev1-census --pairs:\n  " + "\n  ".join(errors))
        print("\n".join(report))
        return
    if not a.core:
        p.error("--core is required unless --pairs")
    commit = git(a.core, "rev-parse", f"{TAG}^{{commit}}").strip()
    if commit != EXPECTED_COMMIT:
        sys.exit(f"ev1-census: {TAG} is {commit} in {a.core}, expected {EXPECTED_COMMIT}")
    rows, got, errors = build(a.core)
    if errors:
        sys.exit("ev1-census:\n  " + "\n  ".join(errors))
    text = render(rows, got)
    if a.check:
        with open(OUT, encoding="utf-8") as f:
            if f.read() != text:
                sys.exit(f"ev1-census: {os.path.relpath(OUT)} is stale; rerun without --check")
        print(f"ev1-census: {len(rows)} rows match {TAG}")
        return
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"ev1-census: wrote {len(rows)} rows to {os.path.relpath(OUT)}")


if __name__ == "__main__":
    main()
