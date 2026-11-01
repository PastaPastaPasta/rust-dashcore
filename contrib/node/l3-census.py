#!/usr/bin/env python3
"""L3's census scanner (CH3a a10, plan §6.1): the reject rows of Core's block validation.

It reads the required functions of a Dash Core tag straight from git (no checkout) and lists
each row of their bodies:
  R  reject: [<x>.]state.Invalid(<Block|Tx>ValidationResult::E, <reason>, ...) with a literal
     reason or a strprintf template;
  W  wrapper: the reason is <s>.GetRejectReason(), with the callee of the governing
     `if (!Callee(...))`;
  F  fatal: state.Error(...) or AbortNode(...).
A row's key is <file>:<function>#<ordinal>:<template>, the ordinal counting R rows in source
order (W and F rows are keyed by their callee or line instead). Any other Invalid/Error shape
fails the scan.

This a10-4 scanner makes a partial claim: R, W and F rows are scanned; the propagation (E) and
E-evo edges are the fixture's annotated rows, listed as not scanned.

  l3-census.py scan  --tree <dash> --tag <tag>          print the scan as JSON
  l3-census.py check --tree <dash> --fixture <json>     the fixture's scanned fields == the scan
        [--partial <report.json> ...]                   and every row's status over L3 run reports
  l3-census.py --selftest                               the census self-tests (plan §9), no tree

`--partial` (plan §6.5) prints each fixture row with its status and exits 0 iff the scan matches
the fixture and every report passes: its dashd is the fixture's tag and commit, every manifest
row ran, every leg is done with no gated E-evo counter entered, its E-evo keys are the fixture's,
every key it binds is a fixture row and no exempt row is bound. Each leg's E-evo keys also have
the observation the EVO_OBSERVATION table names (counted keys are ints, derived keys start with
`derived: `, the others read `not observed`) and `w.stx` is above 0 (a dead hook reads 0); the
report's `set` is known and its legs are exactly that set's (tier1: par1 and par4). A key is
covered by a trial that binds it in a done leg; an ambiguous binding covers nothing. Each covered
C row prints its covering trials with their classes, and a summary counts them by class.
Uncovered C rows are listed, not failed (a10-5's `--require-full` fails them).

Any `DoS(` call in a scanned function fails the scan (plan §6.1), as does any unknown Invalid
shape. The self-test holds a witness for each failure (plan §9).
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys

SCANNER_VERSION = 1

# file -> the functions whose bodies are scanned (plan §6.1); a missing one fails.
REQUIRED = {
    "src/validation.cpp": [
        "CheckBlockHeader",
        "CheckMerkleRoot",
        "CheckBlock",
        "ContextualCheckBlockHeader",
        "ContextualCheckBlock",
        "ContextualCheckTransaction",
        "AcceptBlockHeader",
        "AcceptBlock",
        "ProcessNewBlock",
        "TestBlockValidity",
        "ConnectBlock",
        "CheckInputScripts",
    ],
    "src/consensus/tx_check.cpp": ["CheckTransaction"],
    "src/consensus/tx_verify.cpp": ["Consensus::CheckTxInputs"],
    "src/evo/specialtxman.cpp": ["CheckSpecialTxInner", "ProcessSpecialTxsInBlock"],
}

# The `%s` of `ScriptErrorString(...)` in a reason template: the closed set of strings
# ScriptErrorString returns at the same tag (plan §6.1).
SCRIPT_ERRORS = "src/script/script_error.cpp"
SCRIPT_ERROR_RETURN = re.compile(r'\breturn\s+((?:"(?:[^"\\]|\\.)*"\s*)+);')
STRING_PIECE = re.compile(r'"((?:[^"\\]|\\.)*)"')

# What a leg's report says of each E-evo key: a count, a derivation (`derived: ...`) or
# `not observed`; and the legs each report set must have (plan §6.5).
EVO_OBSERVATION = {
    "w.stx": "counted",
    "w.cbtx": "counted",
    "w.quorum": "not observed",
    "w.mnhf": "not observed",
    "g.stx": "counted",
    "g.coll": "counted",
    "g.cbtx": "counted",
    "g.quorum": "derived",
    "g.dmn": "derived",
    "g.mnhf": "derived",
}
SET_LEGS = {"tier1": ["par1", "par4"]}

# The fields of a fixture row the scan reproduces.
SCANNED = ["key", "kind", "file", "function", "ordinal", "line", "template", "result", "callee", "sha"]

# The reason-template conversions the binder instantiates (node-l1 `l3::inventory::match_from`);
# any other fails the scan.
CONVERSIONS = ("%s", "%08x")
CONVERSION = re.compile(r"%[-+ #0-9.]*[a-zA-Z%]")


class ScanError(Exception):
    pass


def git(tree, *args):
    out = subprocess.run(["git", "-C", tree, *args], capture_output=True, text=True)
    if out.returncode != 0:
        raise ScanError(f"git {' '.join(args)}: {out.stderr.strip() or out.returncode}")
    return out.stdout


def blank(text, strings=True):
    """The text with comments and (with `strings`) string/char literals' contents replaced by
    spaces, newlines kept, so offsets and line numbers stay; brace and paren matching then sees
    code only."""
    out = list(text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            while i < n and text[i] != "\n":
                out[i] = " "
                i += 1
        elif text.startswith("/*", i):
            j = text.index("*/", i + 2) + 2
            for k in range(i, j):
                if text[k] != "\n":
                    out[k] = " "
            i = j
        elif c in "\"'":
            j = i + 1
            while text[j] != c:
                j += 2 if text[j] == "\\" else 1
            if strings:
                for k in range(i + 1, j):
                    out[k] = " "
            i = j + 1
        else:
            i += 1
    return "".join(out)


def match(code, i, open_c, close_c):
    """The index just past the bracket matching code[i] == open_c."""
    depth = 0
    for j in range(i, len(code)):
        if code[j] == open_c:
            depth += 1
        elif code[j] == close_c:
            depth -= 1
            if depth == 0:
                return j + 1
    raise ScanError(f"unbalanced {open_c} at offset {i}")


def line_of(text, i):
    return text.count("\n", 0, i) + 1


def find_function(code, name):
    """(start, end) of the body of the one definition of `name`, a qualified name exactly and a
    bare one with any class qualifier (a declaration ends in `;`, not a body)."""
    if "::" in name:
        head = re.escape(name)
    else:
        head = r"(?:\w+::)?" + re.escape(name)
    pattern = re.compile(r"^[A-Za-z][^\n;{}()]*?(?<![\w:])" + head + r"\(", re.M)
    found = []
    for m in pattern.finditer(code):
        close = match(code, m.end() - 1, "(", ")")
        rest = code[close:]
        brace, semi = rest.find("{"), rest.find(";")
        if brace != -1 and (semi == -1 or brace < semi):
            body = close + brace
            found.append((body, match(code, body, "{", "}")))
    if len(found) != 1:
        raise ScanError(f"{name}: {len(found)} definitions")
    return found[0]


def split_args(code, start, end):
    """The top-level comma-separated arguments of code[start:end], as (begin, end) spans."""
    spans, depth, begin = [], 0, start
    for j in range(start, end):
        c = code[j]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == "," and depth == 0:
            spans.append((begin, j))
            begin = j + 1
    spans.append((begin, end))
    return spans


LITERAL = re.compile(r'^"((?:[^"\\]|\\.)*)"$', re.S)
STRPRINTF = re.compile(r'^strprintf\(\s*"((?:[^"\\]|\\.)*)"', re.S)
REJECT = re.compile(r"^(\w+)\.GetRejectReason\(\)$")
RESULT = re.compile(r"^(BlockValidationResult|TxValidationResult)::(BLOCK|TX)_(\w+)$")
IF = re.compile(r"\bif\s*\(")
NEGATED_CALL = re.compile(r"!\s*((?:[\w]+\s*(?:\.|->)\s*)?[\w:]+)\s*\(")
CALL = re.compile(r"\b(?:(\w+)\s*\.\s*)?(Invalid|Error)\s*\(|\bAbortNode\s*\(|\bDoS\s*\(")


def governing_callee(code, start, at):
    """The negated call in the condition of the last `if` before `at` whose body holds `at`."""
    for m in reversed(list(IF.finditer(code, start, at))):
        cond_end = match(code, m.end() - 1, "(", ")")
        rest = code[cond_end:at]
        body_open = cond_end + len(rest) - len(rest.lstrip())
        if code[body_open] == "{" and match(code, body_open, "{", "}") < at:
            continue
        calls = NEGATED_CALL.findall(code, m.end(), cond_end - 1)
        if calls:
            return re.sub(r"\s+", "", calls[0])
    return None


def scan_function(path, name, text, code, bare):
    start, end = find_function(code, name)
    file = path.rsplit("/", 1)[-1]
    rows, ordinal = [], 0
    for m in CALL.finditer(code, start, end):
        open_paren = m.end() - 1
        close = match(code, open_paren, "(", ")")
        stmt = " ".join(bare[m.start() : close].split())
        line = line_of(text, m.start())
        sha = hashlib.sha256(stmt.encode()).hexdigest()[:16]
        if m.group(0).startswith("DoS"):
            raise ScanError(f"{file}:{line}: a DoS( call: {stmt}")
        if m.group(0).startswith("AbortNode") or m.group(2) == "Error":
            rows.append(
                {"kind": "F", "file": file, "function": name, "line": line,
                 "key": f"{file}:{name}@F{line}", "sha": sha}
            )
            continue
        args = [" ".join(bare[a:b].split()) for a, b in split_args(code, open_paren + 1, close - 1)]
        if len(args) < 2:
            raise ScanError(f"{file}:{line}: an Invalid( with {len(args)} arguments: {stmt}")
        result = RESULT.match(args[0])
        if not result:
            raise ScanError(f"{file}:{line}: an unknown result {args[0]!r}")
        cls = ("B_" if result.group(2) == "BLOCK" else "T_") + result.group(3)
        reason = args[1]
        if lit := LITERAL.match(reason):
            template = lit.group(1)
        elif fmt := STRPRINTF.match(reason):
            template = fmt.group(1)
        elif REJECT.match(reason):
            callee = governing_callee(code, start, m.start())
            if callee is None:
                raise ScanError(f"{file}:{line}: a GetRejectReason with no governing call")
            rows.append(
                {"kind": "W", "file": file, "function": name, "line": line, "result": cls,
                 "callee": callee, "key": f"{file}:{name}@W:{callee}", "sha": sha}
            )
            continue
        else:
            raise ScanError(f"{file}:{line}: an unknown reason form {reason!r}")
        for conversion in CONVERSION.findall(template):
            if conversion not in CONVERSIONS:
                raise ScanError(f"{file}:{line}: an unhandled conversion {conversion} in {template!r}")
        ordinal += 1
        rows.append(
            {"kind": "R", "file": file, "function": name, "ordinal": ordinal, "line": line,
             "template": template, "result": cls,
             "key": f"{file}:{name}#{ordinal}:{template}", "sha": sha}
        )
    return rows


def scan_sources(sources, required=REQUIRED):
    """The rows and the ScriptErrorString set of `sources` (path -> text)."""
    rows = []
    for path, names in required.items():
        text = sources[path]
        code = blank(text)
        bare = blank(text, strings=False)
        for name in names:
            rows.extend(scan_function(path, name, text, code, bare))
    keys = [r["key"] for r in rows]
    if len(set(keys)) != len(keys):
        raise ScanError("duplicate keys: " + ", ".join(sorted({k for k in keys if keys.count(k) > 1})))
    errors = sources[SCRIPT_ERRORS]
    script_errors = ["".join(STRING_PIECE.findall(r)) for r in SCRIPT_ERROR_RETURN.findall(errors)]
    if not script_errors or len(script_errors) != len(re.findall(r"\breturn\b", blank(errors))):
        raise ScanError(f"{SCRIPT_ERRORS}: a ScriptErrorString return that is not a string literal")
    return rows, script_errors


def scan(tree, tag):
    paths = [*REQUIRED, SCRIPT_ERRORS]
    sources = {path: git(tree, "show", f"{tag}:{path}") for path in paths}
    blobs = {path: git(tree, "rev-parse", f"{tag}:{path}").strip() for path in paths}
    rows, script_errors = scan_sources(sources)
    commit = git(tree, "rev-parse", "--verify", "--quiet", f"{tag}^{{commit}}").strip()
    return {
        "provenance": {"tag": tag, "commit": commit, "blobs": blobs, "scanner_version": SCANNER_VERSION},
        "script_errors": script_errors,
        "rows": rows,
    }


def scanned(row):
    return {k: row[k] for k in SCANNED if k in row}


def check(fixture, got):
    """The differences between the fixture and the scan `got` of its tag; the number of scanned
    rows; the annotated rows' keys."""
    prov = fixture["provenance"]
    problems = []
    for field in ("commit", "blobs", "scanner_version"):
        if got["provenance"][field] != prov[field]:
            problems.append(f"provenance {field}: fixture {prov[field]!r}, scan {got['provenance'][field]!r}")
    if got["script_errors"] != fixture["script_errors"]:
        problems.append("script_errors: the fixture's ScriptErrorString set is not the source's")
    want = {r["key"]: scanned(r) for r in fixture["rows"] if r.get("scanned", True)}
    have = {r["key"]: scanned(r) for r in got["rows"]}
    for key in sorted(want.keys() - have.keys()):
        problems.append(f"stale: {key} is in the fixture, not in the source")
    for key in sorted(have.keys() - want.keys()):
        problems.append(f"new: {key} is in the source, not in the fixture")
    for key in sorted(want.keys() & have.keys()):
        if want[key] != have[key]:
            problems.append(f"changed: {key}: fixture {want[key]}, scan {have[key]}")
    unscanned = [r["key"] for r in fixture["rows"] if not r.get("scanned", True)]
    return problems, len(have), unscanned


def exempt(row):
    return row.get("decision", "").startswith("X-")


def evo_keys(fixture):
    """Every row's E-evo key, as node-l1's `Inventory::evo_keys`."""
    return {r["evo_key"] for r in fixture["rows"] if "evo_key" in r}


def observed_as(want, state):
    """Whether an E-evo key's reported `state` is the `want` kind of EVO_OBSERVATION."""
    if want == "counted":
        return isinstance(state, int) and not isinstance(state, bool)
    if want == "derived":
        return isinstance(state, str) and state.startswith("derived: ")
    return state == "not observed"


def partial(fixture, reports):
    """The problems of `reports` (name -> parsed report) against the fixture, and each key's
    covering trials as (trial id, class) pairs."""
    prov = fixture["provenance"]
    rows = {r["key"]: r for r in fixture["rows"]}
    problems, covered = [], {}
    for name, report in reports.items():
        dashd = report.get("dashd", {})
        if (dashd.get("tag"), dashd.get("commit")) != (prov["tag"], prov["commit"]):
            problems.append(
                f"{name}: dashd {dashd.get('tag')} {dashd.get('commit')}, fixture {prov['tag']} {prov['commit']}"
            )
        if not report.get("manifest"):
            problems.append(f"{name}: no manifest rows")
        for m in report.get("manifest", []):
            if m.get("status") != "ran":
                problems.append(f"{name}: manifest row {m.get('trial_id')} {m.get('status')}")
        if not report.get("legs"):
            problems.append(f"{name}: no legs")
        required = SET_LEGS.get(report.get("set"))
        if required is None:
            problems.append(f"{name}: unknown set {report.get('set')!r}")
        else:
            names = sorted(str(leg.get("name")) for leg in report.get("legs", []))
            if names != sorted(required):
                problems.append(f"{name}: legs {names}, but set {report['set']} has exactly {sorted(required)}")
        for leg in report.get("legs", []):
            where = f"{name} {leg.get('name')}"
            done = leg.get("status") == "done"
            if not done:
                problems.append(f"{where}: {leg.get('status')} ({leg.get('error')})")
            keys = leg.get("evo_keys", {})
            if set(keys) != evo_keys(fixture):
                added, removed = sorted(set(keys) - evo_keys(fixture)), sorted(evo_keys(fixture) - set(keys))
                problems.append(f"{where}: E-evo keys differ from the fixture's: added {added}, removed {removed}")
            for key, state in sorted(keys.items()):
                want = EVO_OBSERVATION.get(key)
                if want is None:
                    problems.append(f"{where}: E-evo key {key} has no expected observation")
                elif not observed_as(want, state):
                    problems.append(f"{where}: E-evo key {key} = {state!r}, expected {want}")
                elif key.startswith("g.") and want == "counted" and state != 0:
                    problems.append(f"{where}: the gated E-evo edge {key} was entered ({state})")
                elif key == "w.stx" and state <= 0:
                    problems.append(f"{where}: w.stx = {state}: the E-evo hook is not live")
            for trial in leg.get("trials", []):
                occurrence = trial.get("occurrence")
                if not isinstance(occurrence, dict) or "bound" not in occurrence:
                    continue
                key = occurrence["bound"]
                if key not in rows:
                    problems.append(f"{where}: {trial['trial_id']} binds {key}, not a fixture row")
                elif exempt(rows[key]):
                    problems.append(
                        f"{where}: {trial['trial_id']} binds the exempt row {key} ({rows[key]['decision']})"
                    )
                elif done:
                    covered.setdefault(key, []).append((trial["trial_id"], trial.get("class", "unknown")))
    return problems, covered


def class_summary(fixture, covered):
    """The covering trials of the C rows, counted by class."""
    counts = {}
    for row in fixture["rows"]:
        if row.get("decision") == "C":
            for _, cls in covered.get(row["key"], []):
                counts[cls] = counts.get(cls, 0) + 1
    return ", ".join(f"{cls} {n}" for cls, n in sorted(counts.items())) or "none"


def status_lines(fixture, covered):
    """One line per fixture row: its key, kind, decision, scan status and coverage (each
    covering trial with its class)."""
    lines = []
    for row in fixture["rows"]:
        decision = row.get("decision") or row.get("class") or "-"
        scanned_ = "scanned" if row.get("scanned", True) else "annotated"
        if row["key"] in covered:
            state = "covered by " + ", ".join(f"{t} ({cls})" for t, cls in sorted(covered[row["key"]]))
        elif exempt(row):
            state = "exempt"
        elif decision == "C":
            planned = ", ".join(row.get("covered_by", [])) or "-"
            state = f"UNCOVERED (planned: {planned})"
        else:
            state = "-"
        lines.append(f"{row['kind']:5} {decision:5} {scanned_:9} {row['key']}: {state}")
    return lines


SELFTEST_SOURCE = """
bool Twin(const CBlock& block, BlockValidationState& state, const CBlockIndex* prev)
{
    if (prev == nullptr) return state.Invalid(BlockValidationResult::BLOCK_INVALID_PREV, "bad-prevblk");
    if (prev->nStatus & BLOCK_FAILED_MASK) {
        return state.Invalid(BlockValidationResult::BLOCK_INVALID_PREV, "bad-prevblk");
    }
    if (block.nVersion < 2)
        return state.Invalid(BlockValidationResult::BLOCK_INVALID_HEADER, strprintf("bad-version(0x%08x)", block.nVersion));
    if (block.vtx.empty()) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-cb-missing");
    return true;
}
"""
SELFTEST_ERRORS = """
std::string ScriptErrorString(const ScriptError serror)
{
    switch (serror) {
        case SCRIPT_ERR_OK:
            return "No error";
    }
    return "unknown error";
}
"""
SELFTEST_REQUIRED = {"src/validation.cpp": ["Twin"]}
SELFTEST_EVO = list(EVO_OBSERVATION)
# A passing leg's E-evo keys: counted ones live (w) or gated shut (g), derived and unobserved ones as named.
SELFTEST_EVO_STATE = {
    k: {"counted": 0, "derived": "derived: because", "not observed": "not observed"}[v]
    for k, v in EVO_OBSERVATION.items()
}
SELFTEST_EVO_STATE.update({"w.stx": 3, "w.cbtx": 1})


def selftest_fixture():
    """A fixture over the synthetic source: its rows C but `bad-cb-missing` exempt, plus the
    ten E-evo edges."""
    rows, script_errors = scan_sources(
        {"src/validation.cpp": SELFTEST_SOURCE, SCRIPT_ERRORS: SELFTEST_ERRORS}, SELFTEST_REQUIRED
    )
    for row in rows:
        row["decision"] = "X-reg" if row["template"] == "bad-cb-missing" else "C"
    rows += [{"key": f"Twin@E-evo:{k}", "kind": "E-evo", "evo_key": k, "scanned": False} for k in SELFTEST_EVO]
    prov = {"tag": "v0", "commit": "c" * 40, "blobs": {}, "scanner_version": SCANNER_VERSION}
    return {"provenance": prov, "script_errors": script_errors, "rows": rows}


def selftest_report(bindings, status="done", evo=None, commit="c" * 40, ran="ran", legs=("par1", "par4"),
                    set_="tier1", cls="runtime"):
    """A report whose legs `legs` all carry `evo`; the first leg's trials bind `bindings`
    (trial id -> occurrence)."""
    trials = [{"trial_id": t, "class": cls, "occurrence": o} for t, o in bindings.items()]
    return {
        "set": set_,
        "dashd": {"tag": "v0", "commit": commit},
        "manifest": [{"trial_id": t, "status": ran} for t in bindings],
        "legs": [
            {
                "name": name,
                "status": status,
                "evo_keys": dict(SELFTEST_EVO_STATE) if evo is None else evo,
                "trials": trials if i == 0 else [],
            }
            for i, name in enumerate(legs)
        ],
    }


def selftest_scan(source):
    """The scan of the synthetic function `source` as a `check` input (the fixture's provenance)."""
    rows, script_errors = scan_sources(
        {"src/validation.cpp": source, SCRIPT_ERRORS: SELFTEST_ERRORS}, SELFTEST_REQUIRED
    )
    return {"provenance": selftest_fixture()["provenance"], "script_errors": script_errors, "rows": rows}


def selftest_error(source):
    """The ScanError message of the scan of `source`, or None if it scans."""
    try:
        selftest_scan(source)
    except ScanError as e:
        return str(e)
    return None


def selftest_edit(old, new):
    """SELFTEST_SOURCE with `old` (which it must hold) replaced by `new`."""
    assert old in SELFTEST_SOURCE, old
    return SELFTEST_SOURCE.replace(old, new)


SELFTEST_TAIL = "    return true;\n}"
SELFTEST_CB_MISSING = (
    '    if (block.vtx.empty()) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-cb-missing");\n'
)
SELFTEST_WRAPPER = """    BlockValidationState s;
    if (!CheckFoo(block, s)) {
        return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, s.GetRejectReason());
    }
"""


def selftest():
    """The census self-tests (plan §9); each case asserts its failure and, where it has one,
    its passing baseline."""
    fixture = selftest_fixture()
    first, second = "validation.cpp:Twin#1:bad-prevblk", "validation.cpp:Twin#2:bad-prevblk"
    exempt_key = "validation.cpp:Twin#4:bad-cb-missing"
    failures = []

    def expect(name, ok):
        if not ok:
            failures.append(name)

    def with_tail(code):
        return selftest_edit(SELFTEST_TAIL, code + SELFTEST_TAIL)

    def fails_with(source, text):
        return text in (selftest_error(source) or "")

    def report_problems(report):
        return partial(fixture, {"r": report})[0]

    def evo_problems(over=None, drop=()):
        evo = {k: v for k, v in {**SELFTEST_EVO_STATE, **(over or {})}.items() if k not in drop}
        return report_problems(selftest_report({}, evo=evo))

    keys = [r["key"] for r in fixture["rows"] if r["kind"] == "R"]
    expect("twins scan as two keys", keys[:2] == [first, second])
    expect("a %08x template scans", "validation.cpp:Twin#3:bad-version(0x%08x)" in keys)

    # Baseline: a passing report.
    problems, covered = partial(fixture, {"r": selftest_report({"t1+cold": {"bound": first}})})
    expect("baseline passes", problems == [])
    # The unexecuted twin (M-9): binding the first occurrence leaves the second uncovered.
    expect("the bound twin is covered", covered.get(first) == [("t1+cold", "runtime")])
    expect("the unexecuted twin stays uncovered", second not in covered)
    lines = status_lines(fixture, covered)
    expect("the twin prints UNCOVERED", any(l.endswith(f"{second}: UNCOVERED (planned: -)") for l in lines))
    covered_line = f"{first}: covered by t1+cold (runtime)"
    expect("a covered row prints its trial's class", any(l.endswith(covered_line) for l in lines))
    expect("the class summary counts C rows", class_summary(fixture, covered) == "runtime 1")
    # An ambiguous binding gets no site credit.
    _, covered = partial(fixture, {"r": selftest_report({"t1+cold": {"ambiguous": [first, second]}})})
    expect("ambiguous covers nothing", covered == {})
    # A strprintf reason with an unhandled conversion fails the scan.
    bad = SELFTEST_SOURCE.replace("bad-version(0x%08x)", "bad-version(%d)")
    expect("an unhandled conversion fails", "unhandled conversion %d" in (selftest_error(bad) or ""))
    # An E-evo edge added or removed fails.
    for problems in (evo_problems({"g.new": 0}), evo_problems(drop=("w.stx",))):
        expect("an E-evo key added or removed fails", any("E-evo keys differ" in p for p in problems))
    # A fixture commit that differs from the tag's fails, in the scan and in a report.
    got = {"provenance": dict(fixture["provenance"], commit="d" * 40), "script_errors": fixture["script_errors"],
           "rows": [r for r in fixture["rows"] if r["kind"] != "E-evo"]}
    expect("check baseline passes", check(fixture, dict(got, provenance=fixture["provenance"]))[0] == [])
    expect("a fixture commit that is not the tag's fails", any("provenance commit" in p for p in check(fixture, got)[0]))
    problems = report_problems(selftest_report({}, commit="d" * 40))
    expect("a report from another dashd fails", any("dashd v0" in p for p in problems))
    # An exempt row bound fails; so does a key the fixture lacks.
    problems = report_problems(selftest_report({"t2+cold": {"bound": exempt_key}}))
    expect("a bound exempt row fails", any("the exempt row" in p for p in problems))
    problems = report_problems(selftest_report({"t2+cold": {"bound": "x.cpp:F#1:new"}}))
    expect("an unknown bound key fails", any("not a fixture row" in p for p in problems))
    # An entered gated edge, a failed leg (no coverage from it) and a row that did not run fail.
    expect("an entered gated edge fails", any("g.stx was entered" in p for p in evo_problems({"g.stx": 1})))
    problems, covered = partial(fixture, {"r": selftest_report({"t1+cold": {"bound": first}}, status="failed")})
    expect("a failed leg fails and covers nothing", problems != [] and covered == {})
    problems = report_problems(selftest_report({"t1+cold": {"bound": first}}, ran="not run"))
    expect("a manifest row not run fails", any("not run" in p for p in problems))

    # m-6: the report's key states and leg set are asserted, not trusted.
    for bad_state in ("derived: x", True):
        problems = evo_problems({"g.stx": bad_state})
        expect(f"a gated key given as {bad_state!r} fails", any("E-evo key g.stx" in p for p in problems))
    expect("a derived key that is not derived fails", any("E-evo key g.dmn" in p for p in evo_problems({"g.dmn": 0})))
    expect("a w.quorum of 0 fails", any("E-evo key w.quorum" in p for p in evo_problems({"w.quorum": 0})))
    expect("a counted key given as a string fails", any("E-evo key w.cbtx" in p for p in evo_problems({"w.cbtx": "1"})))
    expect("a dead w.stx fails", any("not live" in p for p in evo_problems({"w.stx": 0})))
    for legs in (("par1",), ("par4",), ("par1", "par4", "par9"), ("par1", "par1")):
        problems = report_problems(selftest_report({}, legs=legs))
        expect(f"the legs {legs} fail the tier1 set", any("has exactly" in p for p in problems))
    expect("an unknown set fails", any("unknown set" in p for p in report_problems(selftest_report({}, set_="tier9"))))

    # m-7: census witnesses. Each variant shares the passing baseline source.
    expect("the baseline source matches the fixture", check(fixture, selftest_scan(SELFTEST_SOURCE))[0] == [])
    wrapper = with_tail(SELFTEST_WRAPPER)
    rows = selftest_scan(wrapper)["rows"]
    expect("a wrapper row scans", any(r["key"] == "validation.cpp:Twin@W:CheckFoo" for r in rows))
    # A new occurrence is reported, not absorbed.
    new_row = '    if (block.vtx.size() > 9) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-new");\n'
    problems = check(fixture, selftest_scan(with_tail(new_row)))[0]
    new_key = "validation.cpp:Twin#5:bad-new"
    expect("a new occurrence is reported", problems == [f"new: {new_key} is in the source, not in the fixture"])
    # A required function that is gone, or defined twice, fails the scan.
    expect("a removed required function fails", fails_with(selftest_edit("Twin(", "Other("), "Twin: 0 definitions"))
    expect("a duplicated function fails", fails_with(SELFTEST_SOURCE + SELFTEST_SOURCE, "Twin: 2 definitions"))
    # Two wrapper rows with one callee collide.
    twice = with_tail(SELFTEST_WRAPPER * 2)
    expect("duplicate keys fail", fails_with(twice, "duplicate keys: validation.cpp:Twin@W:CheckFoo"))
    # Unknown Invalid shapes fail.
    for what, text, want in (
        ("a variable reason", 'state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, reason)', "unknown reason form"),
        ("a variable result", 'state.Invalid(result, "bad-x")', "unknown result"),
        ("a foreign result", 'state.Invalid(ValidationResult::CONSENSUS, "bad-x")', "unknown result"),
        ("one argument", "state.Invalid(BlockValidationResult::BLOCK_CONSENSUS)", "1 arguments"),
    ):
        expect(f"{what} fails", fails_with(with_tail(f"    return {text};\n"), want))
    # A GetRejectReason with no `if (!Call(...))` fails.
    ungoverned = '    return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, s.GetRejectReason());\n'
    expect("an ungoverned GetRejectReason fails", fails_with(with_tail(ungoverned), "no governing call"))
    # Fixture and source disagree: stale, new, changed (line, sha).
    problems = check(fixture, selftest_scan(selftest_edit(SELFTEST_CB_MISSING, "")))[0]
    expect("a stale row is reported", problems == [f"stale: {exempt_key} is in the fixture, not in the source"])
    problems = check(fixture, selftest_scan("\n" + SELFTEST_SOURCE))[0]
    expect("a moved row is reported changed", len(problems) == 4 and all(p.startswith("changed: ") for p in problems))
    extra_arg = selftest_edit('"bad-cb-missing")', '"bad-cb-missing", "debug")')
    problems = check(fixture, selftest_scan(extra_arg))[0]
    expect("a changed statement is reported", len(problems) == 1 and problems[0].startswith(f"changed: {exempt_key}"))
    # A DoS( call fails the scan, member or free.
    for call in ('state.DoS(10, false, REJECT_INVALID, "bad-dos")', 'DoS(10, "bad-dos")'):
        expect(f"{call.split('(')[0]} fails", fails_with(with_tail(f"    {call};\n"), "a DoS( call"))

    for name in failures:
        print(f"l3-census: selftest FAILED: {name}", file=sys.stderr)
    if not failures:
        print("l3-census: selftest passed")
    return 1 if failures else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command")
    s = sub.add_parser("scan")
    s.add_argument("--tree", required=True)
    s.add_argument("--tag", required=True)
    c = sub.add_parser("check")
    c.add_argument("--tree", required=True)
    c.add_argument("--fixture", required=True)
    c.add_argument("--partial", nargs="+", metavar="REPORT", help="L3 run reports to give each row's status from")
    parser.add_argument("--selftest", action="store_true", help="run the census self-tests")
    args = parser.parse_args()
    if args.selftest:
        return selftest()
    if args.command is None:
        parser.error("a command or --selftest")
    try:
        if args.command == "scan":
            json.dump(scan(args.tree, args.tag), sys.stdout, indent=1)
            print()
            return 0
        with open(args.fixture) as f:
            fixture = json.load(f)
        problems, n, unscanned = check(fixture, scan(args.tree, fixture["provenance"]["tag"]))
    except ScanError as e:
        print(f"l3-census: {e}", file=sys.stderr)
        return 1
    if problems:
        print(f"l3-census: {args.fixture} differs from the source:", *problems, sep="\n  ", file=sys.stderr)
        return 1
    print(f"l3-census: {n} scanned rows match; {len(unscanned)} annotated rows not scanned (partial claim)")
    if not args.partial:
        return 0
    reports = {}
    for path in args.partial:
        with open(path) as f:
            reports[path] = json.load(f)
    problems, covered = partial(fixture, reports)
    print(*status_lines(fixture, covered), sep="\n")
    c_rows = [r["key"] for r in fixture["rows"] if r.get("decision") == "C"]
    uncovered = [k for k in c_rows if k not in covered]
    print(
        f"l3-census: --partial: {len(c_rows) - len(uncovered)} of {len(c_rows)} C rows covered, "
        f"{len(uncovered)} uncovered"
    )
    print(f"l3-census: --partial: covering trials by class over C rows: {class_summary(fixture, covered)}")
    if problems:
        print("l3-census: --partial fails:", *problems, sep="\n  ", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
