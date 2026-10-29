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
"""

import argparse
import collections
import os
import re
import subprocess
import sys
import tomllib

TAG = "v24.0.0-rc.3"
# The commit the (annotated) tag peels to, as `git rev-parse <tag>^{commit}` prints it.
EXPECTED_COMMIT = "0364ed626ec2eaa518bf1ae4a0566ce638c7fe18"

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "..", "dash-dml", "tests", "data", "ev1_census.tsv"))
TABLE = os.path.join(HERE, "ev1-census-classified.toml")

# (file, first line, last line, scope, what): the function ranges at the pin.
SCOPES = [
    ("src/evo/specialtxman.cpp", 33, 137, "other", "AddNetInfoEntries, SetStateVersion, ApplyProUpServTx (EV2b)"),
    ("src/evo/specialtxman.cpp", 139, 216, "other", "CheckCbTxBestChainlock (CH7a)"),
    ("src/evo/specialtxman.cpp", 217, 274, "ev1", "CheckSpecialTxInner"),
    ("src/evo/specialtxman.cpp", 309, 794, "other", "BuildNewListFromBlock (EV2b)"),
    ("src/evo/specialtxman.cpp", 797, 999, "other", "ProcessSpecialTxsInBlock (CH7a)"),
    ("src/evo/specialtxman.cpp", 1036, 1066, "other", "CheckCreditPoolDiffForBlock (EV3)"),
    ("src/evo/specialtxman.cpp", 1068, 1849, "ev1", "the ProTx checks and the shared-collateral covenant"),
    ("src/evo/providertx.cpp", 1, None, "ev1", "IsTriviallyValid, the payout/share lists, the network fields"),
    ("src/evo/providertx.h", 560, 567, "ev1", "CProDisTx::IsTriviallyValid"),
    ("src/evo/cbtx.cpp", 24, 55, "ev1", "CheckCbTx"),
    ("src/evo/cbtx.cpp", 60, 155, "other", "CheckCbTxMerkleRoots and friends (CH7a)"),
    ("src/evo/mnhftx.cpp", 94, 157, "ev1", "CheckMNHFTx"),
    ("src/evo/mnhftx.cpp", 190, 260, "other", "the MNHF block processing (EV3)"),
    ("src/evo/assetlocktx.cpp", 29, 232, "ev1", "CheckAssetLockTx, CheckAssetUnlockTx"),
    ("src/evo/assetlocktx.cpp", 256, 270, "other", "the asset-unlock block check (EV3)"),
    ("src/llmq/commitment.cpp", 240, 310, "ev1", "CheckLLMQCommitment"),
]

# The census totals the design records for the pin (§3.13).
EXPECTED_EV1 = {"sites": 213, "TX_BAD_SPECIAL": 144, "TX_CONSENSUS": 69, "distinct": 146}

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


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--core", required=True, help="a Dash Core clone that has the tag")
    p.add_argument("--check", action="store_true", help="compare with the checked-in file instead of writing it")
    a = p.parse_args()
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
