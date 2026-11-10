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
        [--require-full <full> <warn> <av>              or the census union, every C row covered,
         [--expect-head <sha>]]                         at one rdc head
  l3-census.py --selftest                               the census self-tests (plan §9), no tree

O10 (the O10 plan, /work/scratch/l3/O10-PLAN.md rev2.1) extends the same scanner:
  l3-census.py tree      --tree <dash> --tag <tag>      the whole-tree census (§5.1)
  l3-census.py calltree  --tree <dash> --tag <tag>      the call tree and precedence model (§4.2)
  l3-census.py o10-check --tree <dash> --tag <tag>      O10's fixtures regenerate from the tag,
                                                        byte for byte, and the overlay
                                                        (o10-overlay-<tag>.json) stands over
                                                        a10's inventory, and the claim manifests
                                                        (claims/m3.json, m4, m5) regenerate
  l3-census.py --claim m3 [--partial] --head <sha> <report.json> ...
                                                        the M3 claim over L3 run reports (§5.3)

A Core tag move (OD-L3-37, OD-L3-38) maps lines by the diff, each mapped line's text checked:
  l3-census.py carry --tree <dash> --inventory <old> --tag <new> [--annotations <json>]
        [--report <json>]                               the inventory carried onto the new tag:
                                                        rows matched by (file, function, sha,
                                                        template), new rows annotated only from
                                                        the file, any unmatched row a failure
  l3-census.py remap-cites --tree <dash> --from <old> --to <new> [--write]
        [--forms all|long] [--keep tags|none] <path> ... every citation (v:N, st:N, ...,
                                                        validation.cpp:N, and a bare :N after
                                                        one) moved by content; one naming the
                                                        old tag kept (with --forms long, only
                                                        long forms move, and one naming either
                                                        tag is kept; with --keep none, nothing
                                                        is kept but skip-marked lines)

`--partial` (plan §6.5) prints each fixture row with its status and exits 0 iff the scan matches
the fixture and every report passes: its dashd is the fixture's tag and commit, every manifest
row ran, every leg is done with no gated E-evo counter entered, its E-evo keys are the fixture's,
every key it binds is a fixture row and no exempt row is bound. Each leg reports every E-evo key
in both EVO_MODES (a submit's count under the key, a proposal's under its `p.` mirror, OD-L3-43)
as an int, each gated key's 0, and `w.stx` above 0 (a dead hook reads 0); the report's `set` is known and its legs are exactly that set's (tier1: par1 and par4). A key is
covered by a trial that binds it in a done leg; an ambiguous binding covers nothing. Each covered
C row prints its covering trials with their classes, and a summary counts them by class.
Uncovered C rows are listed, not failed.

`--require-full` (a10-5, plan §6.3) applies the same checks to the census union, one report per
set: full (par1, par4), warn and av (the six AV fixtures). It also fails every uncovered C row, a
missing or repeated set, a devnet report given as an input (M-12: not a census leg), and a union
not run at one rdc head: a report without `rdc_head`, two heads, or a head other than
`--expect-head <sha>` (OD-L3-72 m5). It prints each
AV-on fixture's header-op parent-state counts (recorded, unreadable, n/a: an M-13 deviation,
OD-L3-25 (a)) and fails an AV-on leg without them.

`--claim <m>` (O10 plan §5.3) first checks that claims/<m>.json stands over the fixtures (its
input shas, every row an inventory row or tree string, every exemption with a DEC id, no deferred
row with a coverage field), then reads the reports: a10's census sets and O10's M3 sets (refused
until their slice defines its legs), never an O10-5, devnet or replay report. Every report
must carry the manifest's dashd pin and the claimed rdc head (`--head`), and be complete (a census
set's legs say they ran unfiltered). A row is exercised by a runtime or transferred binding in a
done leg; a site-only or unverified one exercises it with the class unverified, which counts and
is named in the verdict for L3 to escalate (§5.2). A deferred row reported covered, or a binding of
an unknown string, fails. It prints
every row's, case's and deferred row's status and the verdict: complete (exit 0),
incomplete-with-exemption with the DEC ids (exit 2), incomplete or failed (exit 1); with
`--partial` it exits 0 unless something failed.

Any `DoS(` call in a scanned function fails the scan (plan §6.1), as does any unknown Invalid
shape. The self-test holds a witness for each failure (plan §9).
"""

import argparse
import difflib
import hashlib
import json
import re
from pathlib import Path
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

# The E-evo keys V counts (node-l1's `evo::EVO_KEYS`), each in both modes since BL-564: a
# submit's count under the key, a proposal's under its `p.` mirror (OD-L3-43, OD-L3-56); and
# the legs each report set must have (plan §6.5).
EVO_KEYS = ("w.stx", "w.cbtx", "w.quorum", "w.mnhf", "g.stx", "g.coll", "g.cbtx", "g.quorum", "g.dmn", "g.mnhf")
EVO_MODES = {"Submit": "", "Proposal": "p."}
SET_LEGS = {
    "tier1": ["par1", "par4"],
    "full": ["par1", "par4"],
    "warn": ["warn"],
    "av": [f"av-{side}-{d}" for d in ("d1", "d2", "d3") for side in ("off", "on")],
}

# `--require-full`'s union (plan §6.3): one report per census set. The devnet leg is not a
# census leg (M-12): its report is refused as an input.
CENSUS_SETS = ("full", "warn", "av")

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


def scan_function(path, name, text, code, bare, spans=None):
    """The rows of `name`'s body; `spans`, if given, gets each R and W row's statement span
    (key -> (start, end) offsets)."""
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
            if spans is not None:
                spans[rows[-1]["key"]] = (m.start(), close)
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
        if spans is not None:
            spans[rows[-1]["key"]] = (m.start(), close)
    return rows


def scan_sources(sources, required=REQUIRED, spans=None):
    """The rows and the ScriptErrorString set of `sources` (path -> text); `spans`, if given,
    gets each R and W row's (path, start, end)."""
    rows = []
    for path, names in required.items():
        text = sources[path]
        code = blank(text)
        bare = blank(text, strings=False)
        for name in names:
            found = {} if spans is not None else None
            rows.extend(scan_function(path, name, text, code, bare, found))
            if spans is not None:
                spans.update({k: (path, a, b) for k, (a, b) in found.items()})
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


# --- O10 (O10 plan §5.1): the whole-tree census -----------------------------------------------
#
# Every `"bad-…"` string in Core's `src/` at the tag, minus src/test, src/qt and src/bench, comments
# included (txmempool.h:535 names `bad-protx-hash` in one), with each occurrence's file and line.
# A template's bucket: `evo` if it occurs under src/evo/ or src/llmq/ (so v:834's
# `bad-assetunlock-too-old-quorum` is evo, its other occurrence being al:129); else `atmp` if every
# occurrence is in a MemPoolAccept member; else `block`. An occurrence inside one of a10's R or W
# rows carries that row's key, and the a10 subset rule (§5.1) is that those keys, with the
# informational rows', are exactly the a10 scan's R rows. A bucket is not an obligation: the claim
# manifests are (§5.3).

TREE_VERSION = 1
TREE_EXCLUDED = ("src/test/", "src/qt/", "src/bench/")
BAD = re.compile(r'"(bad-[^"]*)"')
# A reject's literal (or strprintf template) reason anywhere in the tree: the informational rows
# take the ones not starting `bad-`.
TREE_INVALID = re.compile(r'\bInvalid\(\s*(\w+ValidationResult::\w+)\s*,\s*(?:strprintf\(\s*)?"((?:[^"\\]|\\.)*)"')
EVO_DIRS = ("src/evo/", "src/llmq/")
ATMP = "MemPoolAccept::"
DEFINITION = re.compile(r"^[A-Za-z][^\n;{}()]*?(?<![\w:])((?:\w+::)*~?\w+)\(", re.M)


def definitions(code):
    """(name, body start, body end) of every function definition in `code` (blanked)."""
    found = []
    for m in DEFINITION.finditer(code):
        try:
            close = match(code, m.end() - 1, "(", ")")
        except ScanError:
            continue
        rest = code[close:]
        brace, semi = rest.find("{"), rest.find(";")
        if brace != -1 and (semi == -1 or brace < semi) and not rest[:brace].strip().startswith(("=", ":")):
            body = close + brace
            found.append((m.group(1), body, match(code, body, "{", "}")))
    return found


def tree_scan_sources(sources, a10_sources, required=REQUIRED):
    """The tree census of `sources` (path -> text, the scanned files) with a10's rows from
    `a10_sources` (as `scan_sources`)."""
    spans = {}
    a10_rows, _ = scan_sources(a10_sources, required, spans)
    by_path = {}
    for key, (path, a, b) in spans.items():
        by_path.setdefault(path, []).append((a, b, key))
    occurrences, informational = {}, []
    for path in sorted(sources):
        text = sources[path]
        hits = list(BAD.finditer(text))
        if not hits and "Invalid(" not in text:
            continue
        file = path.removeprefix("src/")
        code = blank(text, strings=False)
        invalid = list(TREE_INVALID.finditer(code))
        atmp = [(a, b) for name, a, b in definitions(blank(text)) if name.startswith(ATMP)]

        def a10_key(offset):
            keys = [k for a, b, k in by_path.get(path, []) if a <= offset < b]
            if len(keys) > 1:
                raise ScanError(f"{file}: offset {offset} in {len(keys)} a10 rows")
            return keys[0] if keys else None

        for m in hits:
            occ = {"file": file, "line": line_of(text, m.start())}
            if code[m.start()] != '"':
                occ["comment"] = True
            if any(a <= m.start() < b for a, b in atmp):
                occ["atmp"] = True
            if key := a10_key(m.start()):
                occ["a10_key"] = key
            occurrences.setdefault(m.group(1), []).append(occ)
        for m in invalid:
            if m.group(2).startswith("bad-"):
                continue
            row = {"template": m.group(2), "file": file, "line": line_of(text, m.start()),
                   "result": m.group(1).split("::")[1]}
            if key := a10_key(m.start()):
                row["a10_key"] = key
            informational.append(row)
    templates = []
    for template, occ in sorted(occurrences.items()):
        if any(("src/" + o["file"]).startswith(EVO_DIRS) for o in occ):
            bucket = "evo"
        elif all(o.get("atmp") for o in occ):
            bucket = "atmp"
        else:
            bucket = "block"
        templates.append({"template": template, "bucket": bucket, "occurrences": occ})
    buckets = {b: sum(t["bucket"] == b for t in templates) for b in ("block", "evo", "atmp")}
    per_file = {}
    for t in templates:
        for o in t["occurrences"]:
            per_file[o["file"]] = per_file.get(o["file"], 0) + 1
    summary = {
        "templates": len(templates),
        "occurrences": sum(per_file.values()),
        "buckets": buckets,
        "per_file": dict(sorted(per_file.items(), key=lambda kv: (-kv[1], kv[0]))),
        "informational": len(informational),
    }
    return {"summary": summary, "templates": templates, "informational": informational}, a10_rows


def a10_subset_problems(census, a10_rows):
    """The a10 subset rule (§5.1): the a10 keys the tree's occurrences and informational rows
    carry are exactly the a10 scan's R rows, each by one `bad-` occurrence or one informational
    row; a W row carries no string of its own."""
    want = {r["key"] for r in a10_rows if r["kind"] == "R"}
    seen = {}
    for t in census["templates"]:
        for o in t["occurrences"]:
            if "a10_key" in o and not o.get("comment"):
                seen.setdefault(o["a10_key"], []).append(f"{o['file']}:{o['line']}")
    for row in census["informational"]:
        if "a10_key" in row:
            seen.setdefault(row["a10_key"], []).append(f"{row['file']}:{row['line']}")
    problems = [f"a10 row {k} has no tree occurrence" for k in sorted(want - seen.keys())]
    problems += [f"tree occurrence {seen[k]} binds {k}, not an a10 R row" for k in sorted(seen.keys() - want)]
    problems += [f"a10 row {k} has {len(v)} tree occurrences: {v}" for k, v in sorted(seen.items()) if len(v) > 1]
    return problems


def tree_files(tree, tag):
    """The scanned files at `tag`: those under src/ (minus TREE_EXCLUDED) holding a `"bad-` or an
    `Invalid(`."""
    out = git(tree, "grep", "-l", "-F", "-e", '"bad-', "-e", "Invalid(", tag, "--", "src")
    paths = [line.split(":", 1)[1] for line in out.splitlines() if line]
    return sorted(p for p in paths if not p.startswith(TREE_EXCLUDED))


def tree_scan(tree, tag):
    paths = tree_files(tree, tag)
    sources = {path: git(tree, "show", f"{tag}:{path}") for path in paths}
    a10_paths = [*REQUIRED, SCRIPT_ERRORS]
    a10_sources = {path: git(tree, "show", f"{tag}:{path}") for path in a10_paths}
    census, a10_rows = tree_scan_sources(sources, a10_sources)
    if problems := a10_subset_problems(census, a10_rows):
        raise ScanError("the a10 subset rule fails: " + "; ".join(problems))
    commit = git(tree, "rev-parse", "--verify", "--quiet", f"{tag}^{{commit}}").strip()
    prov = {
        "tag": tag,
        "commit": commit,
        "src_tree": git(tree, "rev-parse", f"{tag}:src").strip(),
        "excluded": list(TREE_EXCLUDED),
        "tree_version": TREE_VERSION,
        "note": "l3-census.py tree (O10 plan §5.1): every \"bad-...\" string of src/ at the tag; "
                "a bucket is lexical, not an obligation (the claim manifests are, §5.3)",
    }
    return {"provenance": prov, **census}


def dumps(value):
    """The canonical fixture text: a regenerated fixture equals the checked-in one byte for byte."""
    return json.dumps(value, indent=1, ensure_ascii=False) + "\n"


# --- O10 (§5.1-5.2): the overlay over a10's inventory --------------------------------------------
#
# `o10-overlay-<tag>.json` holds O10's decision for a10 keys it re-decides: a reviewed
# unreachability proof, a deferral to a later slice, a row pending its O10 slice, or a
# manager-approved exemption. Each names the a10 decision it read; an a10 C row is never re-decided.

# Row statuses (§5.2); `regime-excluded` is a reason, never a status.
STATUSES = ("exercised", "exercised-unverified", "proven-unreachable", "deferred", "unproven-exempt")
# The fields each overlay status needs. `pending` is the overlay's: a row an M3 slice of O10 owns
# (O10-11), whose claim is `exercised` or, failing that, an exemption with its DEC.
OVERLAY_FIELDS = {
    "proven-unreachable": ("modes", "proof"),
    "deferred": ("slice", "dep", "milestone", "reason"),
    "pending": ("slice", "milestone", "target", "reason"),
    "unproven-exempt": ("dec", "owner", "milestone", "reason", "core", "rdc", "parity"),
}
DEC_ID = re.compile(r"^DEC-\d+$")
MILESTONES = ("M3", "M4", "M5")


def overlay_problems(overlay, inventory):
    """Why `overlay` does not stand over a10's `inventory` (§5.1): its keys are a10's, each names
    its a10 decision, no a10 C row is re-decided and each status has its fields."""
    rows = {r["key"]: r for r in inventory["rows"]}
    problems, seen = [], set()
    if overlay["provenance"]["commit"] != inventory["provenance"]["commit"]:
        problems.append("the overlay's commit is not the inventory's")
    for row in overlay["rows"]:
        key = row["key"]
        if key in seen:
            problems.append(f"{key}: twice in the overlay")
        seen.add(key)
        a10 = rows.get(key)
        if a10 is None:
            problems.append(f"{key}: not an a10 key")
            continue
        decision = a10.get("decision", "-")
        if row.get("a10_decision") != decision:
            problems.append(f"{key}: the overlay read a10 decision {row.get('a10_decision')!r}, a10 has {decision!r}")
        if decision == "C":
            problems.append(f"{key}: weakens a10's C decision to {row.get('status')!r}")
        status = row.get("status")
        if status not in OVERLAY_FIELDS:
            problems.append(f"{key}: unknown status {status!r}")
            continue
        for field in OVERLAY_FIELDS[status]:
            if not row.get(field):
                problems.append(f"{key}: {status} without {field}")
        if row.get("milestone", "M3") not in MILESTONES:
            problems.append(f"{key}: unknown milestone {row['milestone']!r}")
        if status == "unproven-exempt" and not DEC_ID.match(str(row.get("dec", ""))):
            problems.append(f"{key}: an exemption without a DEC id ({row.get('dec')!r})")
        if status == "proven-unreachable" and not set(row.get("modes", [])) <= {"Submit", "Proposal", "Header"}:
            problems.append(f"{key}: unknown modes {row.get('modes')}")
    return problems


# --- O10 (§5.2-5.3): milestone claim manifests ---------------------------------------------------
#
# `claims/m3.json` lists every row and case M3 requires, with the status it must reach, and every
# row and string deferred past M3, with its slice, dependency and milestone and no coverage field.
# `m4.json` and `m5.json` are skeletons: the deferred rows of their milestone, filled by their
# slices. All three are generated from a10's inventory, the overlay and the tree census, and their
# provenance holds the sha256 of each, so a manifest over a changed input is stale.
# `claim <m> <report> ...` reads L3 run reports against one (§5.3 Inputs).

CLAIMS_VERSION = 1
CLAIMS_DIR = "claims"
# The M3 status each a10 decision requires; the overlay re-decides X rows, and the X rows it
# leaves are deferred by A10_DEFERRED. W and F rows are wrappers and fatal paths, not obligations.
A10_REQUIRE = {"C": "exercised", "X-unr": "proven-unreachable"}
# §5.3 Deferred rows, deps from §3.3: X-cl → O10-7a, `bad-cb-payee` → O10-9, the CbTx evo rows →
# O10-7a. A key entry wins over its decision's.
A10_DEFERRED = {
    "X-cl": {"slice": "O10-7a", "dep": ["MN0-3", "CH7b-3", "LK1a"], "milestone": "M4",
             "reason": "ChainLock and InstantSend rows (§5.3: X-cl → O10-7a; §3.3 CbTx/CL deps)"},
    "X-evo": {"slice": "O10-7a", "dep": ["MN0-3", "CH7b-2", "CH7b-3"], "milestone": "M4",
              "reason": "a decoded CbTx's merkle roots (§3.3 CbTx fields and QcTx)"},
    "validation.cpp:ConnectBlock#10:bad-cb-payee": {
        "slice": "O10-9", "dep": ["SP1", "MN0-3"], "milestone": "M5",
        "reason": "regime-excluded: the synced leg (D49, mnsync not synced on the census legs; §5.3, §3.3)"},
}
# The slice of each tree string outside a10's rows (§5.3 "every evo/T/P2P string → its slice"),
# by the §1.3/§3.3 areas, first match wins; a string no rule names fails the generation, so a new
# tag's strings get a decision. ATMP-only occurrences are the mempool path's (O10-8) whatever the
# string.
TREE_SLICES = [
    (re.compile(r"^bad-(assetlocktx|assetunlock|assetunlocktx|txns-assetunlock|cbtx-assetlocked)-?"),
     {"slice": "O10-7c", "dep": ["MN0-3", "CH7b-2", "EV3"], "milestone": "M5",
      "reason": "asset lock/unlock and credit pool (§3.3; §1.3 M4-M5, listed under M5 where it closes)"}),
    (re.compile(r"^bad-(cbtx|qc)-"),
     {"slice": "O10-7a", "dep": ["MN0-3", "CH7b-2", "CH7b-3"], "milestone": "M4",
      "reason": "CbTx fields and QcTx (§3.3)"}),
    (re.compile(r"^bad-(protx|prodis|proupshare|proupsharedreg|shared|dip3|tx-type)-"),
     {"slice": "O10-7b", "dep": ["MN0-3", "CH7b-2"], "milestone": "M4",
      "reason": "ProTx and the special-tx type checks (§3.3 ProTx)"}),
    (re.compile(r"^bad-mnhf-"),
     {"slice": "O10-7b", "dep": ["MN0-3", "CH7b-2"], "milestone": "M4", "reason": "MNHF (§3.3)"}),
]
# Planes no M3 report tests, each deferred to its slice and carried by every manifest up to its
# milestone, and stated in every verdict (Q-13: §1.3 M3 exit 3, §7 O10-9).
PLANES = {
    "peer-penalty": {"slice": "O10-9", "dep": ["P4", "P2b", "SP1", "GV1b"], "milestone": "M5",
                     "reason": "Q-13: the P2P penalty plane (scores by result class, §3.3); "
                               "every report states peer penalty untested/deferred until O10-9"},
}
TREE_ATMP_SLICE = {"slice": "O10-8", "dep": ["MP1a"], "milestone": "M5",
                   "reason": "the mempool path (§3.3; ATMP-only occurrences)"}
# The M3 cases (§5.3) beyond a10's rows, each with the slice whose report states it. A case is
# met only by an `o10_cases` entry "pass" in a complete union report; every pair and witness has
# its solo controls as a case of their own (M-1).
GEN_FAMILIES = ["hdr.version", "hdr.prev", "hdr.merkle", "hdr.time", "hdr.bits", "cb.scriptsig", "cb.value",
                "cb.outputs", "tx.version", "tx.locktime", "tx.sequence", "tx.prevout", "tx.value", "tx.script",
                "blk.structure"]
WITNESSES = ["W1", "W2", "W3", "W3b", "W4", "W5", "W6"]
# §4.5's pair classes, as O10-2's report names them (node-l1 l3/pairset.rs); a witness case is its
# placement and the reversed one §4.4 gives it.
PAIR_CLASSES = {
    1: "cross-call within one tx iteration",
    2: "same loop, different tx, both orders",
    3: "same tx, different callee rows",
    4: "input order within CheckTxInputs",
    5: "mode-divergent (Submit != Proposal)",
    6: "par-divergent, deduplicated against a10 group M",
    7: "AcceptBlockHeader prev-state x header defect",
    8: "header-only pairs in CCBH order",
    9: "pre-loop row x loop-body row, at index 1 and the last index",
    10: "loop-body row x post-loop row, at index 1 and the last index",
}


def o10_cases():
    cases = []
    for n, name in PAIR_CLASSES.items():
        cases.append({"id": f"o10.pair.class{n}", "slice": "O10-2", "par": ["par1", "par4"],
                      "require": "exercised", "class": name, "source": "§4.5"})
        cases.append({"id": f"o10.pair.class{n}.solos", "slice": "O10-2", "require": "exercised",
                      "source": "§4.5, M-1 solo controls"})
    for w in WITNESSES:
        cases.append({"id": f"o10.w.{w}", "slice": "O10-2", "par": ["par1", "par4"], "require": "exercised",
                      "source": "§4.4"})
        cases.append({"id": f"o10.w.{w}.solos", "slice": "O10-2", "require": "exercised",
                      "source": "§4.4, M-1 solo controls"})
    for twin in ("o10.time_mtp_edge", "o10.time_future_edge"):
        cases.append({"id": twin, "slice": "O10-3", "modes": ["Submit", "Proposal", "Header"],
                      "require": "exercised", "source": "§3.1 (accept twins)"})
    cases.append({"id": "o10.gen.minimums", "slice": "O10-3", "require": "exercised", "n_per_par": 2000,
                  "families": GEN_FAMILIES, "min_verified_per_family": 40, "min_distinct_bound": 25,
                  "source": "§3.4 (a run below a minimum is incomplete)"})
    cases.append({"id": "o10.base.controls", "slice": "O10-4", "require": "exercised",
                  "source": "§6.2 (two base controls at one parent and mocktime: distinct hashes, both pass)"})
    return cases


# The report sets `claim` reads (§5.3 Inputs): a10's census sets (SET_LEGS) and O10's M3 census
# and regime sets, whose leg rules their slices define (None: not defined yet, so refused); and
# the sets it refuses as union inputs (M-12 pattern). O10-3's o10_gen runs each -par over the
# nightly's four shards (§8.2, OD-L3-40).
O10_SETS = {"o10_pairs": ["par1", "par4"],
            "o10_gen": [f"{par}.s{k}" for par in ("par1", "par4") for k in range(4)],
            "o10_alt_smoke": None, "o10_regime": None}
# §3.4: generated trials are differential only; they never satisfy a claim row (§5.3).
GEN_TRIAL_PREFIX = "o10.gen."
REFUSED_SETS = {
    "o10_dip3": "an O10-5 leg (M4, its own pass rule)",
    "devnet": "a devnet leg (its own pass rule, M-12)",
    "replay": "a replay (§6.1: replay is never coverage)",
}
# The bound classes that exercise a row (§5.2); `unverified` and `site-only` exercise it with the
# class unverified; any other class (n/a, unscored) covers nothing.
EXERCISED_CLASSES = {"runtime": "exercised", "transferred": "exercised",
                     "unverified": "exercised-unverified", "site-only": "exercised-unverified"}


def sha256_text(text):
    return hashlib.sha256(text.encode()).hexdigest()


def deferral(entry, **extra):
    return {**extra, "status": "deferred", **{k: entry[k] for k in ("slice", "dep", "milestone", "reason")}}


def claims(inventory, overlay, tree, texts):
    """The claim manifests (milestone -> value) over a10's inventory, the overlay and the tree
    census; `texts` holds each input's fixture text for the provenance shas."""
    over = {r["key"]: r for r in overlay["rows"]}
    rows, deferred = [], []
    for row in inventory["rows"]:
        key, decision = row["key"], row.get("decision")
        if key in over:
            o = over[key]
            if o["status"] == "deferred":
                deferred.append(deferral(o, key=key, a10_decision=decision, source="o10 overlay"))
            elif o["status"] == "pending":
                rows.append({"key": key, "a10_decision": decision, "require": o["target"],
                             "or": "unproven-exempt(DEC)", "slice": o["slice"], "source": "o10 overlay"})
            elif o["status"] == "proven-unreachable":
                rows.append({"key": key, "a10_decision": decision, "require": "proven-unreachable",
                             "modes": o["modes"], "proof": f"o10 overlay: {key}"})
            else:
                rows.append({"key": key, "a10_decision": decision, "require": "unproven-exempt",
                             "dec": o["dec"], "owner": o["owner"], "source": "o10 overlay"})
        elif key in A10_DEFERRED or decision in A10_DEFERRED:
            deferred.append(deferral(A10_DEFERRED.get(key) or A10_DEFERRED[decision], key=key, a10_decision=decision))
        elif decision in A10_REQUIRE:
            entry = {"key": key, "a10_decision": decision, "require": A10_REQUIRE[decision]}
            if decision == "X-unr":
                entry["proof"] = f"a10 inventory: {key} ({row.get('note', '')})"
            rows.append(entry)
        elif decision in ("W", "F") or "decision" not in row:
            continue
        else:
            raise ScanError(f"{key}: a10 decision {decision!r} has no M3 claim rule")
    for t in tree["templates"]:
        outside = [o for o in t["occurrences"] if "a10_key" not in o]
        if not outside:
            continue
        sites = [f"{o['file']}:{o['line']}" for o in outside]
        if all(o.get("atmp") for o in outside):
            rule = TREE_ATMP_SLICE
        else:
            rule = next((r for pattern, r in TREE_SLICES if pattern.match(t["template"])), None)
        if rule is None:
            raise ScanError(f"the tree string {t['template']} ({', '.join(sites)}) has no slice (TREE_SLICES)")
        deferred.append(deferral(rule, template=t["template"], bucket=t["bucket"], sites=sites))
    deferred += [deferral(entry, plane=name) for name, entry in PLANES.items()]
    prov = {
        "tag": inventory["provenance"]["tag"],
        "commit": inventory["provenance"]["commit"],
        "claims_version": CLAIMS_VERSION,
        "inputs": {name: sha256_text(text) for name, text in sorted(texts.items())},
    }
    m3 = {
        "provenance": {**prov, "note": "l3-census.py o10-check (O10 plan §5.3): every row and case M3 requires, "
                                       "and every row and string deferred past M3 (never covered)"},
        "milestone": "M3",
        "inputs": {"union": sorted([*SET_LEGS, *(s for s in O10_SETS if s != "o10_regime")]),
                   "regime": ["o10_regime"], "refused": sorted(REFUSED_SETS)},
        "rows": rows,
        "cases": o10_cases(),
        "deferred": deferred,
    }
    out = {"m3": m3}
    for m in ("M4", "M5"):
        out[m.lower()] = {
            "provenance": {**prov, "note": f"a skeleton (O10 plan §5.3): M3's rows deferred to {m}, "
                                           "filled by their slices"},
            "milestone": m,
            "skeleton": True,
            "rows": [dict(d, status="unfilled" if d["milestone"] == m else "deferred") for d in deferred
                     if d["milestone"] == m or ("plane" in d and MILESTONES.index(d["milestone"]) > MILESTONES.index(m))],
        }
    return out


def deferred_label(entry):
    return f"deferred({entry['slice']}, {'+'.join(entry['dep']) or '-'}, {entry['milestone']})"


def deferred_name(entry):
    return entry.get("key") or entry.get("template") or f"plane {entry.get('plane')}"


def plane_notes(entries):
    """The verdict's statement of each untested plane in `entries` (Q-13)."""
    return "".join(f"; {d['plane']} untested, {deferred_label(d)}" for d in entries if "plane" in d)


def manifest_problems(manifest, inventory, tree, texts):
    """Why `manifest` is not a manifest over these inputs: stale shas, a row not in the tree or
    the inventory, an unknown status, an exemption without a DEC id, a deferred row with a
    coverage field, an unknown plane or one missing before its milestone."""
    problems = []
    keys = {r["key"] for r in inventory["rows"]}
    templates = {t["template"] for t in tree["templates"]}
    want = {name: sha256_text(text) for name, text in texts.items()}
    if manifest.get("provenance", {}).get("inputs") != want:
        problems.append("stale: the manifest's input shas are not the fixtures' (regenerate it)")
    skeleton = manifest.get("skeleton", False)
    required = [] if skeleton else manifest.get("rows", [])
    for row in required:
        key = row.get("key")
        if key not in keys:
            problems.append(f"stale: {key} is not an inventory row")
        if row.get("require") not in STATUSES:
            problems.append(f"{key}: unknown status {row.get('require')!r}")
        if row.get("require") == "unproven-exempt" and not DEC_ID.match(str(row.get("dec", ""))):
            problems.append(f"{key}: an exemption without a DEC id ({row.get('dec')!r})")
        if row.get("require") == "proven-unreachable" and not row.get("proof"):
            problems.append(f"{key}: proven-unreachable without a proof")
    seen = set()
    for d in manifest.get("rows", []) if skeleton else manifest.get("deferred", []):
        name = deferred_name(d)
        if "plane" in d and d["plane"] not in PLANES:
            problems.append(f"{name}: not a known plane ({', '.join(PLANES)})")
        if "key" in d and d["key"] not in keys:
            problems.append(f"stale: {name} is not an inventory row")
        if "template" in d and d["template"] not in templates:
            problems.append(f"stale: {name} is not a tree string")
        if any(f in d for f in ("covered", "coverage", "covered_by", "require")):
            problems.append(f"{name}: a deferred row with a coverage field")
        for f in ("slice", "dep", "milestone"):
            if f not in d:
                problems.append(f"{name}: deferred without {f}")
        if d.get("milestone") not in MILESTONES[1:]:
            problems.append(f"{name}: deferred to {d.get('milestone')!r}, not a later milestone")
        seen.add(name)
    for row in required:
        if row.get("key") in seen:
            problems.append(f"{row['key']}: both required and deferred")
    milestone = manifest.get("milestone")
    for plane, entry in PLANES.items():
        if milestone in MILESTONES and MILESTONES.index(milestone) <= MILESTONES.index(entry["milestone"]) \
                and f"plane {plane}" not in seen:
            problems.append(f"plane {plane}: missing, {deferred_label(entry)} until {entry['milestone']} ({entry['reason']})")
    return problems


def claim(manifest, inventory, reports, head=None, o10_sets=None):
    """The claim over `reports` (name -> report): (problems, status lines, verdict), the verdict
    one of complete, incomplete-with-exemption (DEC ids), incomplete or failed."""
    if manifest.get("skeleton"):
        lines = [f"{r['status']} {deferred_name(r)}: {deferred_label(r)}" for r in manifest["rows"]]
        return ([f"{manifest['milestone']}: a skeleton manifest, not yet filled by its slices"], lines,
                "failed" + plane_notes(manifest["rows"]))
    o10_sets = O10_SETS if o10_sets is None else o10_sets
    prov = manifest["provenance"]
    rows = {r["key"]: r for r in manifest["rows"]}
    deferred = {d["key"]: d for d in manifest.get("deferred", []) if "key" in d}
    deferred_strings = {d["template"] for d in manifest.get("deferred", []) if "template" in d}
    inventory_keys = {r["key"] for r in inventory["rows"]}
    problems, covered, cases = [], {}, {}
    heads = {}
    for name, report in reports.items():
        set_ = report.get("set")
        if is_devnet(report):
            problems.append(f"{name}: refused as a union input: {REFUSED_SETS['devnet']}")
            continue
        if set_ in REFUSED_SETS or "replay_case" in report:
            problems.append(f"{name}: refused as a union input: {REFUSED_SETS.get(set_, REFUSED_SETS['replay'])}")
            continue
        dashd = report.get("dashd", {})
        if (dashd.get("tag"), dashd.get("commit")) != (prov["tag"], prov["commit"]):
            problems.append(f"{name}: dashd {dashd.get('tag')} {dashd.get('commit')}, manifest {prov['tag']} {prov['commit']}")
        heads[name] = report.get("rdc_head")
        if not report.get("rdc_head"):
            problems.append(f"{name}: no rdc_head")
        elif head and report["rdc_head"] != head:
            problems.append(f"{name}: rdc_head {report['rdc_head']}, not the claimed head {head}")
        if set_ in SET_LEGS:
            problems += [f"incomplete: {p}" for p in partial(inventory, {name: report})[0]]
            # A filtered run (L3_ONLY) lists only its selected trials, every one `ran`.
            for leg in report.get("legs", []):
                if "only" not in leg or leg["only"] is not None:
                    problems.append(f"incomplete: {name} {leg.get('name')}: filtered to {leg.get('only', 'unknown')!r}")
        elif set_ in o10_sets:
            legs = o10_sets[set_]
            if legs is None:
                problems.append(f"{name}: set {set_}'s leg rules are its slice's, not defined yet")
                continue
            if report.get("complete") is not True:
                problems.append(f"incomplete: {name}: not complete")
            names = sorted(str(leg.get("name")) for leg in report.get("legs", []))
            if names != sorted(legs):
                problems.append(f"incomplete: {name}: legs {names}, set {set_} has exactly {sorted(legs)}")
            for leg in report.get("legs", []):
                if leg.get("status") != "done":
                    problems.append(f"incomplete: {name} {leg.get('name')}: {leg.get('status')}")
            if set_ == "o10_gen":
                # §3.4: a run below a minimum, or below the nightly's N, is incomplete.
                case = next((c for c in manifest.get("cases", []) if c["id"] == "o10.gen.minimums"), {})
                gen = report.get("generator") or {}
                if gen.get("minimums", {}).get("met") is not True:
                    problems.append(f"incomplete: {name}: the generator's minimums are not met "
                                    f"({gen.get('minimums', {}).get('short', 'no generator summary')})")
                if not isinstance(gen.get("n_per_par"), int) or gen["n_per_par"] < case.get("n_per_par", 0):
                    problems.append(f"incomplete: {name}: N = {gen.get('n_per_par')} per -par, "
                                    f"the minimums are at {case.get('n_per_par')}")
            for case, state in report.get("o10_cases", {}).items():
                if not any(c["id"] == case for c in manifest.get("cases", [])):
                    problems.append(f"{name}: unknown case {case}")
                elif state == "pass":
                    cases.setdefault(case, []).append(name)
                else:
                    problems.append(f"{name}: case {case} {state}")
        else:
            problems.append(f"{name}: unknown set {set_!r}")
            continue
        for leg in report.get("legs", []):
            for trial in leg.get("trials", []):
                occurrence = trial.get("occurrence")
                if not isinstance(occurrence, dict) or "bound" not in occurrence:
                    continue
                key, where = occurrence["bound"], f"{name} {leg.get('name')} {trial.get('trial_id')}"
                if not isinstance(key, str):
                    problems.append(f"{where}: binds {key!r}, not a row key")
                elif key in deferred:
                    problems.append(f"{where}: reports the deferred row {key} ({deferred_label(deferred[key])}) as covered")
                elif rows.get(key, {}).get("require") == "proven-unreachable":
                    modes = f" on {', '.join(rows[key]['modes'])}" if rows[key].get("modes") else ""
                    problems.append(f"{where}: binds {key}, a proven-unreachable row{modes}: "
                                    f"the proof ({rows[key]['proof']}) fails")
                elif rows.get(key, {}).get("require") == "unproven-exempt":
                    problems.append(f"{where}: binds {key}, an unproven-exempt row ({rows[key]['dec']}): "
                                    "evidence against the exemption's premise, refused (L3 decides the row)")
                elif key in rows:
                    state = EXERCISED_CLASSES.get(trial.get("class"))
                    generated = set_ == "o10_gen" and str(trial.get("trial_id", "")).startswith(GEN_TRIAL_PREFIX)
                    if state and leg.get("status") == "done" and not generated:
                        covered.setdefault(key, []).append((trial.get("trial_id"), trial.get("class"), state))
                elif key in inventory_keys:
                    problems.append(f"{where}: binds {key}, not a claim row")
                else:
                    template = key.rsplit(":", 1)[-1]
                    kind = "a deferred tree string" if template in deferred_strings else "an unknown string"
                    problems.append(f"{where}: binds {key}, {kind}")
    if len(set(heads.values())) > 1:
        problems.append(f"mixed heads: {sorted(set(h for h in heads.values() if h))}")
    lines, missing, exemptions, unverified = [], [], [], []
    for row in manifest["rows"]:
        key, want = row["key"], row["require"]
        if want == "proven-unreachable":
            state = f"proven-unreachable ({row['proof']})"
        elif want == "unproven-exempt":
            state = f"unproven-exempt({row['dec']}): rejection parity unproven, owner {row['owner']}"
            exemptions.append(row["dec"])
        elif key in covered:
            kinds = {s for _, _, s in covered[key]}
            label = "exercised" if "exercised" in kinds else "exercised-unverified (class unverified)"
            if "exercised" not in kinds:
                unverified.append(key)
            state = f"{label} by " + ", ".join(f"{t} ({c})" for t, c, _ in sorted(covered[key]))
        else:
            missing.append(key)
            state = f"UNCOVERED (requires {want}" + (f" or {row['or']}, {row['slice']}" if "or" in row else "") + ")"
        lines.append(f"row   {key}: {state}")
    for case in manifest.get("cases", []):
        if case["id"] in cases:
            lines.append(f"case  {case['id']}: exercised by {', '.join(sorted(cases[case['id']]))}")
        else:
            missing.append(case["id"])
            lines.append(f"case  {case['id']}: UNCOVERED ({case['slice']})")
    for d in manifest.get("deferred", []):
        lines.append(f"defer {deferred_name(d)}: {deferred_label(d)}, " + ("untested" if "plane" in d else "not covered"))
    if problems:
        verdict = "failed"
    elif missing:
        verdict = f"incomplete ({len(missing)} required rows and cases uncovered)"
    elif exemptions:
        verdict = f"incomplete-with-exemption ({', '.join(sorted(exemptions))})"
    else:
        verdict = "complete"
    if unverified and not problems:
        # §5.2: they count, and are listed for L3 to escalate (a10 Q-B).
        verdict += f"; class-unverified, L3 escalates: {', '.join(sorted(unverified))}"
    return problems, lines, verdict + plane_notes(manifest.get("deferred", []))


# --- O10 (§4.2): the call tree and its precedence model ------------------------------------------
#
# Per mode, a tree of a10's 16 functions under the RPC roots, built from the bodies by a small
# statement parser over the blanked source. Nodes, each with its position among its parent's
# children in source order:
#   call    a call of one of the 16 (or a bridge), the callee's body expanded by the reader at each
#           site; `pruned` keeps the position of a re-call a10 rules X-unr, with no body;
#   loop    a `for`/`while`, its domain from LOOPS (an unknown header fails the scan);
#   block   a guarded group: the nodes under an `if` whose condition GUARDS resolves (its `else`
#           gets the negation), or under a `catch` (the `exception` fact);
#   row     an a10 R row (terminating), W row (never wins: the callee's R row is the bound one),
#           an RPC precheck (`pre`, terminating) or the queue's wait (`wait`, fires if a deferred
#           script check is pending);
#   return  an early `return true` (or the return of an external check's result) that leaves the
#           function; skip a `continue`; defer the queued form of a script check (DEFERS).
# A row's own `if` is its trigger and needs no table; any other `if` around nodes must resolve
# (GUARDS, TRIGGERS, DEFERS), or the scan fails. Guards are [attr, value] terms over ATTRS.

CALLTREE_VERSION = 1
MINING = "src/rpc/mining.cpp"


def known_call(required):
    """A call of one of `required`'s functions, by bare name."""
    names = sorted({name.split("::")[-1] for names in required.values() for name in names}, key=len, reverse=True)
    return re.compile(r"(?<![\w])(" + "|".join(names) + r")\s*\(")

# Calls that reach one of the 16 through code the census does not scan: (caller, name) -> callee.
BRIDGES = {
    # v:4911 ActivateBestChain -> ActivateBestChainStep -> ConnectTip -> ConnectBlock.
    ("ProcessNewBlock", "ActivateBestChain"): "ConnectBlock",
}
BRIDGE_CALL = re.compile(r"(?<![\w])(\w+)\s*\(")

# The guard attributes. kind: `mode` (a static value per mode), `fact` (a measured block or chain
# fact, `default` the a10 regtest regime's), `iter` (true at the measured indices of its domain's
# current iteration, `default` the indices), `par` (true iff -par > 1) or `pass` (true wherever the
# walk gets: the guard tests that the checks before it passed).
ATTRS = {
    "fCheckPOW": {"kind": "mode", "values": {"Submit": True, "Proposal": False, "Header": True},
                  "cite": "vh:367 default true (PNB v:4885, AB v:4830, ABH v:4634); TBV with false (mi:722)"},
    "fCheckMerkleRoot": {"kind": "mode", "values": {"Submit": True, "Proposal": True, "Header": True},
                         "cite": "vh:367 default true; TBV with true (mi:722)"},
    "fJustCheck": {"kind": "mode", "values": {"Submit": False, "Proposal": True, "Header": False},
                   "cite": "TBV calls ConnectBlock with true (v:4979); ConnectTip with false"},
    "fRequested": {"kind": "mode", "values": {"Submit": True, "Proposal": True, "Header": True},
                   "cite": "submitblock passes force_processing=true (mi:1055)"},
    "par": {"kind": "par", "cite": "pvChecks = parallel_script_checks ? &vChecks : nullptr (v:2763); "
                                   "HasThreads iff -par > 1 (in:1544-1555)"},
    "pass": {"kind": "pass", "cite": "PNB's `if (ret)` after CheckBlock (v:4886)"},
    "fChecked": {"kind": "fact", "default": False, "cite": "v:4386 block.fChecked (a fresh block)"},
    "merkle_checked": {"kind": "fact", "default": False, "cite": "v:4337 m_checked_merkle_root"},
    "not_genesis": {"kind": "fact", "default": True, "cite": "v:4618 hash != hashGenesisBlock"},
    "known_header": {"kind": "fact", "default": False, "cite": "v:4619 miSelf found"},
    "prev_not_valid_scripts": {"kind": "fact", "default": False, "cite": "v:4688 !pindexPrev->IsValid(BLOCK_VALID_SCRIPTS)"},
    "already_have": {"kind": "fact", "default": False, "cite": "v:4815 BLOCK_HAVE_DATA"},
    "prior_pruned": {"kind": "fact", "default": False, "cite": "v:4817"},
    "less_work": {"kind": "fact", "default": False, "cite": "v:4818"},
    "too_far_ahead": {"kind": "fact", "default": False,
                      "cite": "v:4819; fTooFarAhead (v:4805, BLOCKS_NEAR_TIP) is read only inside "
                              "!fRequested (v:4816), which L3's routes never reach: submitblock passes "
                              "force_processing=true (mi:1055) and a proposal runs TestBlockValidity"},
    "below_min_work": {"kind": "fact", "default": False, "cite": "v:4825"},
    "genesis": {"kind": "fact", "default": False, "cite": "v:2547"},
    "bip30": {"kind": "fact", "default": True, "cite": "v:2601-2614 fEnforceBIP30"},
    "v24": {"kind": "fact", "default": False, "cite": "v:2698 fV24Active_context; st:916 rules.v24"},
    "fScriptChecks": {"kind": "fact", "default": True, "cite": "v:2553-2582 (assumevalid off: true)"},
    "not_ibd": {"kind": "fact", "default": True, "cite": "v:2812 (a10's legs leave IBD)"},
    "script_cached": {"kind": "fact", "default": False, "cite": "v:2185 g_scriptExecutionCache"},
    "dip3": {"kind": "fact", "default": False, "cite": "v:357/v:361 fDIP0003Active_context (DIP3 at 2000, D49)"},
    "heightincb": {"kind": "fact", "default": True,
                   "cite": "v:4591 DeploymentActiveAfter(HEIGHTINCB) (chainparams.cpp:819 regtest BIP34Height 1, consensus/params.h:212)"},
    "cb_type5": {"kind": "fact", "default": False, "cite": "st:876 vtx[0]->nType == TRANSACTION_COINBASE"},
    "cbtx_decoded": {"kind": "fact", "default": False, "cite": "st:984 opt_cbTx.has_value()"},
    "exception": {"kind": "fact", "default": False, "cite": "a catch clause (st:268, st:1053)"},
    "coinbase": {"kind": "iter", "domain": "block.vtx", "default": [0], "cite": "tx.IsCoinBase() (v:2705, v:2757, tc:60, v:2170)"},
    "special_version": {"kind": "iter", "domain": "block.vtx", "default": [], "cite": "v:358 tx.IsSpecialTxVersion()"},
    "asset_unlock": {"kind": "iter", "domain": "block.vtx", "default": [], "cite": "tv:166 an asset unlock"},
    "has_lock_inputs": {"kind": "iter", "domain": "block.vtx", "default": [], "cite": "v:2816 instantsend::HasLockInputs"},
    "cb_type5_at": {"kind": "iter", "domain": "block.vtx", "default": [], "cite": "st:925 i == 0 && nType == TRANSACTION_COINBASE"},
    "has_payload": {"kind": "iter", "domain": "block.vtx", "default": [], "cite": "st:224 HasExtraPayloadField()"},
}

# (function, normalized condition) -> the guard's terms (a conjunction). Its `else` is the
# negation of a one-term guard.
GUARDS = {
    ("CheckBlock", "block.fChecked"): [["fChecked", True]],
    ("CheckMerkleRoot", "block.m_checked_merkle_root"): [["merkle_checked", True]],
    ("ContextualCheckTransaction", "fDIP0003Active_context"): [["dip3", True]],
    ("ContextualCheckBlock", "DeploymentActiveAfter(pindexPrev, chainman.GetConsensus(), Consensus::DEPLOYMENT_HEIGHTINCB) && !fDIP0003Active_context"):
        [["heightincb", True], ["dip3", False]],
    ("ContextualCheckBlock", "fDIP0003Active_context"): [["dip3", True]],
    ("ContextualCheckTransaction", "tx.IsSpecialTxVersion()"): [["special_version", True]],
    ("AcceptBlockHeader", "hash != GetConsensus().hashGenesisBlock"): [["not_genesis", True]],
    ("AcceptBlockHeader", "miSelf != m_blockman.m_block_index.end()"): [["known_header", True]],
    ("AcceptBlockHeader", "!pindexPrev->IsValid(BLOCK_VALID_SCRIPTS)"): [["prev_not_valid_scripts", True]],
    ("AcceptBlock", "fAlreadyHave"): [["already_have", True]],
    ("AcceptBlock", "!fRequested"): [["fRequested", False]],
    ("AcceptBlock", "pindex->nTx != 0"): [["prior_pruned", True]],
    ("AcceptBlock", "!fHasMoreOrSameWork"): [["less_work", True]],
    ("AcceptBlock", "fTooFarAhead"): [["too_far_ahead", True]],
    ("AcceptBlock", "pindex->nChainWork < MinimumChainWork()"): [["below_min_work", True]],
    ("ProcessNewBlock", "ret"): [["pass", True]],
    ("ConnectBlock", "block_hash == params.GetConsensus().hashGenesisBlock"): [["genesis", True]],
    ("ConnectBlock", "fEnforceBIP30"): [["bip30", True]],
    ("ConnectBlock", "!tx.IsCoinBase()"): [["coinbase", False]],
    ("ConnectBlock", "fV24Active_context"): [["v24", True]],
    ("ConnectBlock", "!IsInitialBlockDownload()"): [["not_ibd", True]],
    ("ConnectBlock", "!instantsend::HasLockInputs(*tx)"): [["has_lock_inputs", False]],
    ("ConnectBlock", "fJustCheck"): [["fJustCheck", True]],
    ("CheckInputScripts", "tx.IsCoinBase()"): [["coinbase", True]],
    ("CheckInputScripts", "g_scriptExecutionCache.contains(hashCacheEntry, !cacheFullScriptStore)"): [["script_cached", True]],
    ("CheckTransaction", "tx.IsCoinBase()"): [["coinbase", True]],
    ("CheckTxInputs", "bool isAssetUnlockTx = (tx.IsSpecialTxVersion() && tx.nType == TRANSACTION_ASSET_UNLOCK); isAssetUnlockTx"): [["asset_unlock", True]],
    ("CheckSpecialTxInner", "!tx.HasExtraPayloadField()"): [["has_payload", False]],
    ("ProcessSpecialTxsInBlock", "fCheckCbTxMerkleRoots && block.vtx.size() > 0 && block.vtx[0]->nType == TRANSACTION_COINBASE"):
        [["fScriptChecks", True], ["cb_type5", True]],
    ("ProcessSpecialTxsInBlock", "i == 0 && block.vtx[i]->nType == TRANSACTION_COINBASE"): [["cb_type5_at", True]],
    ("ProcessSpecialTxsInBlock", "opt_cbTx.has_value()"): [["cbtx_decoded", True]],
}
# (function, normalized condition): an `if` around several rows whose condition is part of each
# one's trigger (an instance of any of them asserts it).
TRIGGERS = {
    ("CheckInputScripts", "!check()"),
}
# (function, normalized condition) of an if/else whose `then` queues the check (the defer) and
# whose `else` runs it inline: the deferred row and the wait row that reports it.
DEFERS = {
    ("CheckInputScripts", "pvChecks"): {
        "row": "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)",
        "wait": "validation.cpp:ConnectBlock#7:block-validation-failed",
        "guard": [["par", True]],
    },
}
WAITS = {"validation.cpp:ConnectBlock#7:block-validation-failed"}
# (function, normalized loop header) -> (domain, first index).
LOOPS = {
    ("CheckBlock", "unsigned int i = 1; i < block.vtx.size(); i++"): ("block.vtx", 1),
    ("CheckBlock", "const auto& tx : block.vtx"): ("block.vtx", 0),
    ("ContextualCheckBlock", "const auto& tx : block.vtx"): ("block.vtx", 0),
    ("AcceptBlockHeader", "const CBlockIndex* failedit : m_failed_blocks"): ("other", 0),
    ("AcceptBlockHeader", "invalid_walk != failedit"): ("other", 0),
    ("ConnectBlock", "const auto& tx : block.vtx"): ("block.vtx", 0),
    ("ConnectBlock", "size_t o = 0; o < tx->vout.size(); o++"): ("tx.vout", 0),
    ("ConnectBlock", "unsigned int i = 0; i < block.vtx.size(); i++"): ("block.vtx", 0),
    ("ConnectBlock", "size_t j = 0; j < tx.vin.size(); j++"): ("tx.vin", 0),
    ("ConnectBlock", "auto conflictLockOpt = m_chain_helper->ConflictingISLockIfAny(*tx)"): ("other", 0),
    ("CheckInputScripts", "unsigned int i = 0; i < tx.vin.size(); i++"): ("tx.vin", 0),
    ("CheckTransaction", "const auto& txout : tx.vout"): ("tx.vout", 0),
    ("CheckTransaction", "const auto& txin : tx.vin"): ("tx.vin", 0),
    ("CheckTxInputs", "unsigned int i = 0; i < tx.vin.size(); ++i"): ("tx.vin", 0),
    ("ProcessSpecialTxsInBlock", "size_t i = 0; i < block.vtx.size(); ++i"): ("block.vtx", 0),
}
# (caller, callee) -> why the call is pruned (a10 §6.2's X-unr edges); it keeps its position.
PRUNED_CALLS = {
    ("AcceptBlock", "CheckBlock"): "X-unr re-call: fChecked (a10 E row AcceptBlock@E:CheckBlock, v:4830)",
    ("ConnectBlock", "CheckBlock"): "X-unr re-call: already passed with the same flags (a10 E row ConnectBlock@E:CheckBlock, v:2516)",
}
# (function, normalized switch head) -> why the switch is pruned.
PRUNED_SWITCHES = {
    ("CheckSpecialTxInner", "tx.nType"): "E-evo:Validators: the validator arms past the DIP3 gate (st:228; a10 §6.1, Q-F)",
}
# (caller, callee) -> the guard terms in the call's own condition before it, and the source token
# each must show.
CALL_GUARDS = {
    ("CheckBlock", "CheckMerkleRoot"): ([["fCheckMerkleRoot", True]], "fCheckMerkleRoot"),
    ("ConnectBlock", "CheckInputScripts"): ([["fScriptChecks", True]], "fScriptChecks"),
}
# A row key -> the guard terms in its own condition, and the source token they need.
ROW_GUARDS = {
    "validation.cpp:CheckBlockHeader#1:high-hash": ([["fCheckPOW", True]], "fCheckPOW"),
    # v:381 `tx.nVersion < 0` is never IsSpecialTxVersion (`nVersion >= 3`, primitives/transaction.h:334).
    "validation.cpp:ContextualCheckTransaction#4:bad-txns-version": ([["special_version", False]], "nVersion"),
}
# The RPC roots (§4.1): each mode's prechecks, as rows keyed by their reply, then the entry call.
ROOTS = {
    "Submit": [
        ("pre", 1033, "block.vtx.empty() || !block.vtx[0]->IsCoinBase()", "Block does not start with a coinbase"),
        ("pre", 1043, "pindex->IsValid(BLOCK_VALID_SCRIPTS)", "duplicate"),
        ("pre", 1046, "pindex->nStatus & BLOCK_FAILED_MASK", "duplicate-invalid"),
        ("call", 1055, "chainman.ProcessNewBlock(", "ProcessNewBlock"),
    ],
    "Proposal": [
        ("pre", 710, "pindex->IsValid(BLOCK_VALID_SCRIPTS)", "duplicate"),
        ("pre", 712, "pindex->nStatus & BLOCK_FAILED_MASK", "duplicate-invalid"),
        ("pre", 714, 'return "duplicate-inconclusive"', "duplicate-inconclusive"),
        ("pre", 719, "block.hashPrevBlock != pindexPrev->GetBlockHash()", "inconclusive-not-best-prevblk"),
        ("call", 722, "TestBlockValidity(state", "TestBlockValidity"),
    ],
    "Header": [
        ("pre", 1091, "!chainman.m_blockman.LookupBlockIndex(h.hashPrevBlock)", "Must submit previous header"),
        # mi:1097 ProcessNewBlockHeaders -> v:4747 AcceptBlockHeader, one header.
        ("call", 1097, "chainman.ProcessNewBlockHeaders(", "AcceptBlockHeader"),
    ],
}
MODES = ("Submit", "Proposal", "Header")


def root_key(mode, line, reply):
    rpc = {"Submit": "submitblock", "Proposal": "getblocktemplate", "Header": "submitheader"}[mode]
    return f"mining.cpp:{rpc}@pre{line}:{reply}"


# The table entries a build used: an entry the source no longer needs fails the scan (stale).
USED = set()


class Stmt:
    """A parsed statement: `kind` (simple, block, if, loop, switch, try), its span, and its parts."""

    def __init__(self, kind, start, end, **parts):
        self.kind, self.start, self.end, self.parts = kind, start, end, parts


STMT_KEYWORD = re.compile(r"(if|for|while|do|switch|try|else|catch)\b")


def skip_space(code, i, end):
    while i < end:
        if code[i].isspace():
            i += 1
        elif code[i] == "#" and (i == 0 or code[code.rfind("\n", 0, i) + 1 : i].strip() == ""):
            i = code.find("\n", i)
            i = end if i == -1 else i
        else:
            break
    return i


def parse_stmts(code, i, end):
    stmts = []
    i = skip_space(code, i, end)
    while i < end:
        stmt, i = parse_stmt(code, i, end)
        stmts.append(stmt)
        i = skip_space(code, i, end)
    return stmts


def paren(code, i, end):
    """(open, close) of the parenthesis group starting at or after i."""
    i = skip_space(code, i, end)
    if code[i] != "(":
        raise ScanError(f"offset {i}: expected '('")
    return i, match(code, i, "(", ")")


def parse_stmt(code, i, end):
    start = i
    if code[i] == "{":
        close = match(code, i, "{", "}")
        return Stmt("block", start, close, body=parse_stmts(code, i + 1, close - 1)), close
    kw = STMT_KEYWORD.match(code, i)
    word = kw.group(1) if kw and not code[kw.end():kw.end() + 1].isalnum() else None
    if word in ("if",):
        a, b = paren(code, kw.end(), end)
        then, j = parse_stmt(code, skip_space(code, b, end), end)
        k = skip_space(code, j, end)
        other = None
        if code.startswith("else", k) and not code[k + 4 : k + 5].isalnum() and code[k + 4 : k + 5] != "_":
            other, j = parse_stmt(code, skip_space(code, k + 4, end), end)
        return Stmt("if", start, j, cond=(a + 1, b - 1), then=then, other=other), j
    if word in ("for", "while"):
        a, b = paren(code, kw.end(), end)
        body, j = parse_stmt(code, skip_space(code, b, end), end)
        return Stmt("loop", start, j, head=(a + 1, b - 1), body=body), j
    if word == "do":
        raise ScanError(f"offset {i}: a do-while loop (unsupported)")
    if word == "switch":
        a, b = paren(code, kw.end(), end)
        k = skip_space(code, b, end)
        close = match(code, k, "{", "}")
        return Stmt("switch", start, close, head=(a + 1, b - 1)), close
    if word == "try":
        k = skip_space(code, kw.end(), end)
        body, j = parse_stmt(code, k, end)
        catches = []
        while True:
            k = skip_space(code, j, end)
            if not code.startswith("catch", k):
                break
            a, b = paren(code, k + 5, end)
            handler, j = parse_stmt(code, skip_space(code, b, end), end)
            catches.append(((a + 1, b - 1), handler))
        if not catches:
            raise ScanError(f"offset {i}: a try with no catch")
        return Stmt("try", start, j, body=body, catches=catches), j
    if word in ("else", "catch"):
        raise ScanError(f"offset {i}: a dangling {word}")
    # A simple statement: up to the `;` at depth 0.
    depth, j = 0, i
    while j < end:
        c = code[j]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == ";" and depth == 0:
            return Stmt("simple", start, j + 1), j + 1
        j += 1
    raise ScanError(f"offset {i}: a statement with no ';'")


class Builder:
    """The nodes of one function body (§4.2), with the a10 rows placed by their spans."""

    def __init__(self, name, text, code, bare, rows, spans, tables, known):
        self.name, self.text, self.code, self.bare = name, text, code, bare
        self.t, self.known = tables, known
        self.rows = rows  # key -> a10 row
        self.spans = spans  # key -> (start, end)
        self.placed = set()
        self.pruned = []  # (key, reason)

    def norm(self, a, b):
        return " ".join(self.bare[a:b].split())

    def line(self, i):
        return line_of(self.text, i)

    def fail(self, i, why):
        raise ScanError(f"{self.name} line {self.line(i)}: {why}")

    def rows_in(self, a, b):
        return sorted((s, k) for k, (s, e) in self.spans.items() if a <= s < b)

    def calls_in(self, a, b):
        """The calls of the 16 and the bridges in code[a:b], in order: (offset, callee, end)."""
        found = []
        for m in self.known.finditer(self.code, a, b):
            found.append((m.start(1), m.group(1), match(self.code, m.end() - 1, "(", ")")))
        bridges = {name for _, name in self.t["BRIDGES"]}
        for m in BRIDGE_CALL.finditer(self.code, a, b) if bridges else ():
            if m.group(1) not in bridges:
                continue
            callee = self.t["BRIDGES"].get((self.name, m.group(1)))
            if callee is None:
                self.fail(m.start(), f"a bridge call {m.group(1)} not declared for {self.name}")
            USED.add(("BRIDGES", (self.name, m.group(1))))
            found.append((m.start(1), callee, match(self.code, m.end() - 1, "(", ")")))
        return sorted(found)

    def call_node(self, at, callee, guards):
        node = {"n": "call", "line": self.line(at), "callee": callee}
        if guards:
            node["guards"] = guards
        if (reason := self.t["PRUNED_CALLS"].get((self.name, callee))) is not None:
            USED.add(("PRUNED_CALLS", (self.name, callee)))
            node["pruned"] = reason
        return node

    def row_node(self, key, at, guards=None):
        row = self.rows[key]
        self.placed.add(key)
        kind = "wait" if key in self.t["WAITS"] else row["kind"]
        node = {"n": "row", "line": self.line(at), "key": key, "kind": kind}
        if key in self.t["ROW_GUARDS"]:
            USED.add(("ROW_GUARDS", key))
            terms, token = self.t["ROW_GUARDS"][key]
            gov = self.code.rfind("if", 0, at)
            if token not in self.bare[gov:at]:
                self.fail(at, f"{key}'s guard token {token} is not in its condition")
            guards = (guards or []) + terms
        if guards:
            node["guards"] = guards
        return node

    def simple(self, stmt):
        """The nodes of a simple statement: calls and rows by offset, returns and continues."""
        a, b = stmt.start, stmt.end
        items = [(at, "call", callee) for at, callee, _ in self.calls_in(a, b)]
        items += [(at, "row", key) for at, key in self.rows_in(a, b)]
        text = " ".join(self.code[a:b].split())
        if text.startswith("continue"):
            items.append((a, "skip", None))
        elif text.startswith("return") and not items:
            rest = text[len("return"):].strip().rstrip(";").strip()
            if rest == "true" or (re.match(r"^[\w:]+\(", rest) and not rest.startswith(("AbortNode(",))):
                items.append((a, "return", None))
            elif rest != "false" and not re.match(r"^\w+\.Error\(", rest) and not rest.startswith("AbortNode("):
                self.fail(a, f"an unknown return form: {text}")
        nodes = []
        for at, what, arg in sorted(items, key=lambda t: t[0]):
            if what == "call":
                nodes.append(self.call_node(at, arg, None))
            elif what == "row":
                nodes.append(self.row_node(arg, at))
            else:
                nodes.append({"n": what, "line": self.line(at)})
        return nodes

    def stmts(self, stmts):
        nodes = []
        for stmt in stmts:
            nodes.extend(self.stmt(stmt))
        return nodes

    def stmt(self, stmt):
        if stmt.kind == "simple":
            return self.simple(stmt)
        if stmt.kind == "block":
            return self.stmts(stmt.parts["body"])
        if stmt.kind == "loop":
            a, b = stmt.parts["head"]
            head = self.norm(a, b)
            if self.calls_in(a, b):
                self.fail(a, f"a call in a loop header: {head}")
            if (self.name, head) not in self.t["LOOPS"]:
                self.fail(a, f"an unresolved loop domain: ({self.name!r}, {head!r})")
            USED.add(("LOOPS", (self.name, head)))
            domain, first = self.t["LOOPS"][(self.name, head)]
            node = {"n": "loop", "line": self.line(stmt.start), "domain": domain}
            if first:
                node["start"] = first
            node["body"] = self.stmt(stmt.parts["body"])
            return [node]
        if stmt.kind == "switch":
            a, b = stmt.parts["head"]
            head = self.norm(a, b)
            reason = self.t["PRUNED_SWITCHES"].get((self.name, head))
            if reason is None:
                self.fail(a, f"an unresolved switch: ({self.name!r}, {head!r})")
            USED.add(("PRUNED_SWITCHES", (self.name, head)))
            for _, key in self.rows_in(stmt.start, stmt.end):
                self.placed.add(key)
                self.pruned.append((key, reason))
            return [{"n": "block", "line": self.line(stmt.start), "pruned": reason}]
        if stmt.kind == "try":
            nodes = self.stmt(stmt.parts["body"])
            for (a, b), handler in stmt.parts["catches"]:
                inner = self.stmt(handler)
                if inner:
                    nodes.append({"n": "block", "line": self.line(a), "guards": [["exception", True]], "body": inner})
            return nodes
        if stmt.kind == "if":
            return self.if_stmt(stmt)
        raise AssertionError(stmt.kind)

    def if_stmt(self, stmt):
        a, b = stmt.parts["cond"]
        cond = self.norm(a, b)
        then = self.stmt(stmt.parts["then"])
        other = self.stmt(stmt.parts["other"]) if stmt.parts["other"] else []
        calls = self.calls_in(a, b)
        if calls:
            return self.call_condition(stmt, cond, calls, then, other)
        if (self.name, cond) in self.t["DEFERS"] and stmt.parts["other"] is not None:
            d = self.t["DEFERS"][(self.name, cond)]
            USED.add(("DEFERS", (self.name, cond)))
            if then:
                self.fail(a, "a defer branch with nodes")
            neg = [[t[0], not t[1]] for t in d["guard"]]
            defer = {"n": "defer", "line": self.line(stmt.start), "row": d["row"], "wait": d["wait"]}
            nodes = [{"n": "block", "line": self.line(stmt.start), "guards": d["guard"], "body": [defer]}]
            if other:
                nodes.append({"n": "block", "line": self.line(stmt.parts["other"].start), "guards": neg, "body": other})
            return nodes
        if not then and not other:
            return []
        if (self.name, cond) in self.t["GUARDS"]:
            USED.add(("GUARDS", (self.name, cond)))
            terms = self.t["GUARDS"][(self.name, cond)]
            nodes = []
            if then:
                nodes.append({"n": "block", "line": self.line(stmt.start), "guards": terms, "body": then})
            if other:
                if len(terms) != 1:
                    self.fail(a, f"an else under a guard of {len(terms)} terms: {cond}")
                neg = [[terms[0][0], not terms[0][1]]]
                nodes.append({"n": "block", "line": self.line(stmt.parts["other"].start), "guards": neg, "body": other})
            return nodes
        if (self.name, cond) in self.t["TRIGGERS"]:
            USED.add(("TRIGGERS", (self.name, cond)))
            if other:
                self.fail(a, f"a trigger condition with an else: {cond}")
            if any(n["n"] != "row" and not rowless_loop(n) for n in then):
                self.fail(a, f"a trigger condition around non-row nodes: {cond}")
            return then
        for branch in (then, other):
            rows = [n for n in branch if n["n"] == "row"]
            if len(rows) > 1 or any(n["n"] != "row" and not rowless_loop(n) for n in branch):
                self.fail(a, f"an unresolved condition: ({self.name!r}, {cond!r})")
        return then + other

    def call_condition(self, stmt, cond, calls, then, other):
        """An `if` whose condition calls one of the 16: `!A(...) || !B(...)`, or a declared guard
        prefix `g && !A(...)`. The calls are sibling nodes; the `then` (failure handling) holds W
        rows only."""
        a, b = stmt.parts["cond"]
        rest, cut = "", a
        for at, _, close in calls:
            begin = expr_start(self.code, a, at)
            rest += self.code[cut:begin] + " "
            cut = close
        rest += self.code[cut:b]
        rest = " ".join(rest.replace("||", " ").replace("&&", " ").split())
        nodes = []
        for at, callee, _ in calls:
            guards = None
            if (self.name, callee) in self.t["CALL_GUARDS"]:
                USED.add(("CALL_GUARDS", (self.name, callee)))
                guards, token = self.t["CALL_GUARDS"][(self.name, callee)]
                if rest != token:
                    self.fail(a, f"call {callee}: guard tokens {rest!r}, declared {token!r}")
                rest = ""
            nodes.append(self.call_node(at, callee, guards))
        if rest:
            self.fail(a, f"an unresolved call condition: ({self.name!r}, {cond!r}) leaves {rest!r}")
        if any(n["n"] != "row" or n["kind"] != "W" for n in then):
            self.fail(a, f"a call condition whose failure branch holds more than W rows: {cond}")
        if other:
            self.fail(a, f"a call condition with an else: {cond}")
        return nodes + then


def expr_start(code, floor, at):
    """The start of the call expression whose callee name is at `at`: its object chain
    (`a.`, `a->`, `A::`, `f().`) and a leading `!`."""
    i = at
    while True:
        j = i
        while j > floor and code[j - 1].isspace():
            j -= 1
        if code[j - 2 : j] in ("->", "::"):
            j -= 2
        elif code[j - 1 : j] == ".":
            j -= 1
        else:
            break
        while j > floor and code[j - 1].isspace():
            j -= 1
        if code[j - 1] == ")":
            depth = 0
            while True:
                j -= 1
                depth += {")": 1, "(": -1}.get(code[j], 0)
                if depth == 0:
                    break
        k = j
        while k > floor and (code[k - 1].isalnum() or code[k - 1] == "_"):
            k -= 1
        if k == j:
            raise ScanError(f"offset {at}: an unparsed call object")
        i = k
    j = i
    while j > floor and code[j - 1].isspace():
        j -= 1
    return j - 1 if code[j - 1 : j] == "!" else i


def rowless_loop(node):
    return node["n"] == "loop" and not any(n["n"] != "loop" or not rowless_loop(n) for n in node["body"])


def build_function(path, name, text, a10_rows, spans, tables, known):
    code, bare = blank(text), blank(text, strings=False)
    start, end = find_function(code, name)
    file = path.rsplit("/", 1)[-1]
    rows = {r["key"]: r for r in a10_rows if r["kind"] in ("R", "W") and r["function"] == name and r["file"] == file}
    fspans = {k: (a, b) for k, (p, a, b) in spans.items() if k in rows}
    builder = Builder(name.split("::")[-1], text, code, bare, rows, fspans, tables, known)
    body = builder.stmts(parse_stmts(code, start + 1, end - 1))
    missing = sorted(set(rows) - builder.placed)
    if missing:
        raise ScanError(f"{name}: a10 rows the tree does not place: {missing}")
    return {"file": file, "line": line_of(text, start), "body": body}, builder.pruned


def build_roots(mining, table):
    lines = mining.split("\n")
    roots = {}
    for mode, entries in table.items():
        nodes = []
        for what, line, token, arg in entries:
            if token not in lines[line - 1]:
                raise ScanError(f"mining.cpp:{line}: {token!r} not found ({mode} root)")
            if what == "pre":
                nodes.append({"n": "row", "line": line, "key": root_key(mode, line, arg), "kind": "pre"})
            else:
                nodes.append({"n": "call", "line": line, "callee": arg})
        roots[mode] = nodes
    return roots


def expand(functions, nodes, seen=()):
    """`nodes` with every unpruned call's body expanded (a reader's view; the fixture keeps each
    body once)."""
    out = []
    for node in nodes:
        node = dict(node)
        if node["n"] == "call" and "pruned" not in node:
            if node["callee"] in seen:
                raise ScanError(f"recursion through {node['callee']}")
            node["body"] = expand(functions, functions[node["callee"]]["body"], (*seen, node["callee"]))
        elif "body" in node:
            node["body"] = expand(functions, node["body"], seen)
        out.append(node)
    return out


def reached_rows(nodes, out):
    for node in nodes:
        if node["n"] == "row":
            out.add(node["key"])
        elif node["n"] == "defer":
            out.add(node["row"])
        for child in node.get("body", []):
            reached_rows([child], out)
    return out


TABLES = {"GUARDS": GUARDS, "TRIGGERS": TRIGGERS, "DEFERS": DEFERS, "LOOPS": LOOPS,
          "PRUNED_CALLS": PRUNED_CALLS, "PRUNED_SWITCHES": PRUNED_SWITCHES, "CALL_GUARDS": CALL_GUARDS,
          "ROW_GUARDS": ROW_GUARDS, "BRIDGES": BRIDGES, "WAITS": WAITS, "ATTRS": ATTRS, "ROOTS": ROOTS}
# The tables whose entries must each be used by a build.
USE_CHECKED = ("GUARDS", "TRIGGERS", "DEFERS", "LOOPS", "PRUNED_CALLS", "PRUNED_SWITCHES", "CALL_GUARDS",
               "ROW_GUARDS", "BRIDGES")


def stale_entries(tables):
    """The table entries the last build did not use."""
    return sorted(f"{name} {key!r}" for name in USE_CHECKED for key in tables[name] if (name, key) not in USED)


def calltree_sources(sources, mining, a10_rows, spans, required=REQUIRED, tables=TABLES):
    """The call-tree model of `sources` (path -> text) under `mining`'s roots; `tables` are the
    resolution tables (the real ones by default)."""
    USED.clear()
    functions, pruned = {}, []
    known = known_call(required)
    for path, names in required.items():
        for name in names:
            tree, cut = build_function(path, name, sources[path], a10_rows, spans, tables, known)
            functions[name.split("::")[-1]] = tree
            pruned += cut
    roots = build_roots(mining, tables["ROOTS"]) if mining is not None else {}
    reach = {}
    for mode, nodes in roots.items():
        for key in reached_rows(expand(functions, nodes), set()):
            reach.setdefault(key, []).append(mode)
    for name, tree in functions.items():
        for key in reached_rows(tree["body"], set()):
            reach.setdefault(key, [])
    walk_problems = []
    for name, tree in functions.items():
        def check_domains(nodes, open_):
            for node in nodes:
                if node["n"] == "loop" and node["domain"] != "other" and node["domain"] in open_:
                    walk_problems.append(f"{name}: loop domain {node['domain']} nested in itself")
                inner = (*open_, node["domain"]) if node["n"] == "loop" else open_
                check_domains(node.get("body", []), inner)
        check_domains(tree["body"], ())
    if stale := stale_entries(tables):
        walk_problems.append("stale table entries: " + ", ".join(stale))
    if walk_problems:
        raise ScanError("; ".join(walk_problems))
    return {
        "attrs": tables["ATTRS"],
        "modes": roots,
        "functions": functions,
        "pruned": [{"key": k, "reason": r} for k, r in pruned],
        "structural_reach": {k: v for k, v in sorted(reach.items())},
    }


def calltree_scan(tree, tag):
    paths = [*REQUIRED, SCRIPT_ERRORS]
    sources = {path: git(tree, "show", f"{tag}:{path}") for path in paths}
    mining = git(tree, "show", f"{tag}:{MINING}")
    spans = {}
    a10_rows, _ = scan_sources(sources, REQUIRED, spans)
    model = calltree_sources(sources, mining, a10_rows, spans)
    blobs = {path: git(tree, "rev-parse", f"{tag}:{path}").strip() for path in [*REQUIRED, MINING]}
    commit = git(tree, "rev-parse", "--verify", "--quiet", f"{tag}^{{commit}}").strip()
    inventory = (FIXTURE_DIR / f"inventory-{tag}.json").read_bytes()
    prov = {
        "tag": tag,
        "commit": commit,
        "blobs": blobs,
        "calltree_version": CALLTREE_VERSION,
        "inventory_sha256": hashlib.sha256(inventory).hexdigest(),
        "note": "l3-census.py calltree (O10 plan §4.2): per mode, the RPC roots over a10's 16 functions; "
                "each node's position is its index among its parent's children; node-l1's l3::precedence "
                "walks it (§4.3)",
    }
    return {"provenance": prov, **model}


def exempt(row):
    return row.get("decision", "").startswith("X-")


def evo_keys(fixture):
    """Every row's E-evo key, as node-l1's `Inventory::evo_keys`."""
    return {r["evo_key"] for r in fixture["rows"] if "evo_key" in r}


def reported_evo_keys(fixture):
    """The keys a leg reports: each fixture E-evo key in every one of EVO_MODES."""
    return {prefix + k for k in evo_keys(fixture) for prefix in EVO_MODES.values()}


def counted(state):
    """Whether a reported E-evo `state` is a count."""
    return isinstance(state, int) and not isinstance(state, bool)


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
            # An excluded row (group M's, plan rev1 §4) never ran and must say why.
            if m.get("status") == "excluded" and m.get("reason"):
                continue
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
            want = reported_evo_keys(fixture)
            if set(keys) != want:
                added, removed = sorted(set(keys) - want), sorted(want - set(keys))
                problems.append(f"{where}: E-evo keys differ from the fixture's: added {added}, removed {removed}")
            for key, state in sorted(keys.items()):
                twin = key.removeprefix(EVO_MODES["Proposal"])
                if not counted(state):
                    problems.append(f"{where}: E-evo key {key} = {state!r}, expected counted")
                elif twin.startswith("g.") and state != 0:
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


def is_devnet(report):
    """Whether `report` is the devnet leg's (l3_devnet_walk's report, plan §7.2)."""
    return report.get("name") == "devnet-walk" or report.get("set") == "devnet"


def av_header_ops(reports):
    """The AV-on fixtures' header-op parent states (OD-L3-25 (a), an M-13 deviation): per leg,
    the counts recorded, unreadable and n/a, and the AV-on legs that carry none."""
    lines, missing = [], []
    for r in reports.values():
        if r.get("set") != "av":
            continue
        for leg in r.get("legs", []):
            if not leg.get("name", "").startswith("av-on-"):
                continue
            gates = ((leg.get("av") or {}).get("header_gates")) or {}
            if not all(k in gates for k in ("headers", "recorded", "unreadable", "n/a")):
                missing.append(f"{leg['name']}: no header-op parent-state counts (OD-L3-25 (a))")
                continue
            lines.append(
                f"l3-census: {leg['name']} header ops (M-13 deviation): {gates['headers']}, dashd recorded "
                f"{gates['recorded']}, unreadable {gates['unreadable']}; V n/a {gates['n/a']}"
            )
    return lines, missing


def require_full(fixture, reports, expect_head=None):
    """`--require-full` (plan §6.3): `partial`'s problems over the census union, plus a devnet
    report passed as an input, a census set missing or repeated, every uncovered C row, and a
    union not run at one rdc head (OD-L3-72 m5): a report without `rdc_head`, two reports at
    different heads, or (`expect_head`) a head other than the expected one."""
    problems = [f"{name}: a devnet report is not a census input (M-12)" for name, r in reports.items() if is_devnet(r)]
    census = {name: r for name, r in reports.items() if not is_devnet(r)}
    heads = {name: r.get("rdc_head") for name, r in census.items()}
    problems += [f"{name}: no rdc_head" for name, head in heads.items() if not head]
    if len({head for head in heads.values() if head}) > 1:
        problems.append(f"the census union's reports come from different rdc heads: {heads}")
    if expect_head:
        problems += [
            f"{name}: rdc_head {head}, not the expected head {expect_head}"
            for name, head in heads.items()
            if head and head != expect_head
        ]
    sets = sorted(str(r.get("set")) for r in census.values())
    if sets != sorted(CENSUS_SETS):
        problems.append(f"the census union takes one report per set {list(CENSUS_SETS)}, got {sets}")
    more, covered = partial(fixture, census)
    problems += more + av_header_ops(census)[1]
    problems += [
        f"C row {r['key']} is uncovered" for r in fixture["rows"] if r.get("decision") == "C" and r["key"] not in covered
    ]
    return problems, covered


def class_summary(fixture, covered):
    """The covering trials of the C rows, counted by class."""
    counts = {}
    for row in fixture["rows"]:
        if row.get("decision") == "C":
            for _, cls in covered.get(row["key"], []):
                counts[cls] = counts.get(cls, 0) + 1
    return ", ".join(f"{cls} {n}" for cls, n in sorted(counts.items())) or "none"


# Why a C row's coverage can only be site-only (no runtime class exists for it on either side),
# printed beside its covering trials.
SITE_ONLY_REASONS = {
    "validation.cpp:CheckBlock#2:bad-cb-missing":
        "submitblock refuses a block without a coinbase before validation (mi:1033): proposals only",
    "validation.cpp:AcceptBlockHeader#1:duplicate":
        "header op: no BlockChecked on either side, no Core class (OD-L3-24; BL-600)",
}


def status_lines(fixture, covered, reasons=SITE_ONLY_REASONS):
    """One line per fixture row: its key, kind, decision, scan status and coverage (each
    covering trial with its class, and for a row covered only site-only, why)."""
    lines = []
    for row in fixture["rows"]:
        decision = row.get("decision") or row.get("class") or "-"
        scanned_ = "scanned" if row.get("scanned", True) else "annotated"
        if row["key"] in covered:
            state = "covered by " + ", ".join(f"{t} ({cls})" for t, cls in sorted(covered[row["key"]]))
            if all(cls == "site-only" for _, cls in covered[row["key"]]):
                state += f"; site-only: {reasons.get(row['key'], 'no reason listed')}"
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
SELFTEST_EVO = list(EVO_KEYS)
# A passing leg's E-evo keys in both modes: wrappers live or idle, gated ones shut.
SELFTEST_EVO_STATE = {prefix + k: 0 for k in EVO_KEYS for prefix in EVO_MODES.values()}
SELFTEST_EVO_STATE.update({"w.stx": 3, "w.cbtx": 1, "w.quorum": 2, "w.mnhf": 2, "p.w.stx": 1})


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
                    set_="tier1", cls="runtime", head="h" * 40):
    """A report at rdc head `head` whose legs `legs` all carry `evo`; the first leg's trials
    bind `bindings` (trial id -> occurrence)."""
    trials = [{"trial_id": t, "class": cls, "occurrence": o} for t, o in bindings.items()]
    return {
        "set": set_,
        "rdc_head": head,
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
    # A row covered only site-only prints why (OD-L3-24), or that no reason is listed.
    site_only = {first: [("t1+header", "site-only")]}
    why = status_lines(fixture, site_only, {first: "header op"})
    expect("a site-only row prints its reason",
           any(l.endswith(f"{first}: covered by t1+header (site-only); site-only: header op") for l in why))
    expect("a site-only row without a reason says so",
           any(l.endswith("site-only: no reason listed") for l in status_lines(fixture, site_only, {})))
    # An ambiguous binding gets no site credit.
    _, covered = partial(fixture, {"r": selftest_report({"t1+cold": {"ambiguous": [first, second]}})})
    expect("ambiguous covers nothing", covered == {})
    # A strprintf reason with an unhandled conversion fails the scan.
    bad = SELFTEST_SOURCE.replace("bad-version(0x%08x)", "bad-version(%d)")
    expect("an unhandled conversion fails", "unhandled conversion %d" in (selftest_error(bad) or ""))
    # An E-evo edge added or removed fails, in either mode.
    for problems in (evo_problems({"g.new": 0}), evo_problems(drop=("w.stx",)),
                     evo_problems({"p.g.new": 0}), evo_problems(drop=("p.g.dmn",))):
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
    expect("a proposal's entered gated edge fails",
           any("p.g.mnhf was entered" in p for p in evo_problems({"p.g.mnhf": 1})))
    problems, covered = partial(fixture, {"r": selftest_report({"t1+cold": {"bound": first}}, status="failed")})
    expect("a failed leg fails and covers nothing", problems != [] and covered == {})
    problems = report_problems(selftest_report({"t1+cold": {"bound": first}}, ran="not run"))
    expect("a manifest row not run fails", any("not run" in p for p in problems))
    excluded = selftest_report({"t1+cold": {"bound": first}})
    excluded["manifest"].append({"trial_id": "m_x/par1", "status": "excluded", "reason": "why"})
    expect("an excluded row with its reason passes", report_problems(excluded) == [])
    del excluded["manifest"][-1]["reason"]
    expect("an excluded row without a reason fails", any("m_x/par1" in p for p in report_problems(excluded)))

    # m-6: the report's key states and leg set are asserted, not trusted.
    for key in ("g.stx", "g.dmn", "p.g.quorum"):
        for bad_state in ("derived: x", "not observed", True, None):
            problems = evo_problems({key: bad_state})
            expect(f"{key} given as {bad_state!r} fails", any(f"E-evo key {key} = " in p for p in problems))
    expect("a counted key given as a string fails", any("E-evo key w.cbtx" in p for p in evo_problems({"w.cbtx": "1"})))
    expect("a proposal's wrapper given as a string fails",
           any("E-evo key p.w.mnhf" in p for p in evo_problems({"p.w.mnhf": "2"})))
    expect("idle wrappers in both modes pass", evo_problems({"w.quorum": 0, "p.w.stx": 0}) == evo_problems())
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

    # --require-full (plan §6.3): the census union covers every C row, or fails.
    c_rows = [r["key"] for r in fixture["rows"] if r.get("decision") == "C"]
    every = {f"t{i}+cold": {"bound": k} for i, k in enumerate(c_rows)}
    av = list(SET_LEGS["av"])
    union = {
        "full": selftest_report(every, set_="full"),
        "warn": selftest_report({}, legs=("warn",), set_="warn"),
        "av": selftest_report({}, legs=av, set_="av"),
    }
    union["warn"]["manifest"] = union["av"]["manifest"] = [{"trial_id": "x", "status": "ran"}]
    counts = {"headers": 3, "recorded": 1, "unreadable": 2, "n/a": 3}
    for leg in union["av"]["legs"]:
        leg["av"] = {"header_gates": counts} if leg["name"].startswith("av-on-") else None
    expect("the full union passes", require_full(fixture, union)[0] == [])
    devnet = {**union, "walk": {"name": "devnet-walk", "status": "done"}}
    expect("a devnet report is rejected (M-12)",
           require_full(fixture, devnet)[0] == ["walk: a devnet report is not a census input (M-12)"])
    no_av = {k: v for k, v in union.items() if k != "av"}
    expect("a missing census set fails", any("one report per set" in p for p in require_full(fixture, no_av)[0]))
    lines, missing = av_header_ops(union)
    expect("the AV-on legs' header-op counts print (OD-L3-25 (a))", len(lines) == 3 and missing == []
           and lines[0].endswith("3, dashd recorded 1, unreadable 2; V n/a 3"))
    uncounted = json.loads(json.dumps(union))
    uncounted["av"]["legs"][1]["av"] = {}
    expect("an AV-on leg without the counts fails", require_full(fixture, uncounted)[0]
           == ["av-on-d1: no header-op parent-state counts (OD-L3-25 (a))"])
    short = {**union, "full": selftest_report(dict(list(every.items())[1:]), set_="full")}
    expect("an uncovered C row fails", require_full(fixture, short)[0] == [f"C row {c_rows[0]} is uncovered"])
    # OD-L3-72 m5: one rdc head per union.
    mixed = json.loads(json.dumps(union))
    mixed["warn"]["rdc_head"] = "w" * 40
    expect("a union from two rdc heads fails", [p for p in require_full(fixture, mixed)[0] if "rdc head" in p]
           == [f"the census union's reports come from different rdc heads: "
               f"{ {'full': 'h' * 40, 'warn': 'w' * 40, 'av': 'h' * 40} }"])
    absent = json.loads(json.dumps(union))
    del absent["av"]["rdc_head"]
    expect("a report without an rdc head fails", require_full(fixture, absent)[0] == ["av: no rdc_head"])
    expect("the expected head passes", require_full(fixture, union, "h" * 40)[0] == [])
    expect("another expected head fails", require_full(fixture, union, "e" * 40)[0]
           == [f"{n}: rdc_head {'h' * 40}, not the expected head {'e' * 40}" for n in ("full", "warn", "av")])

    selftest_tree(expect)
    selftest_overlay(expect, fixture)
    selftest_calltree(expect)
    selftest_claims(expect)
    selftest_tag_move(expect)

    for name in failures:
        print(f"l3-census: selftest FAILED: {name}", file=sys.stderr)
    if not failures:
        print("l3-census: selftest passed")
    return 1 if failures else 0


SELFTEST_TREE_ATMP = """
bool MemPoolAccept::PreChecks(ATMPArgs& args, Workspace& ws)
{
    if (ws.m_ptx->vin.size() > 9) return state.Invalid(TxValidationResult::TX_NOT_STANDARD, "bad-atmp");
    // a note on "bad-cb-missing", in a comment
    return state.Invalid(TxValidationResult::TX_CONFLICT, "txn-mempool-conflict");
}
"""
SELFTEST_TREE_EVO = """
bool CheckEvo(const CTransaction& tx, TxValidationState& state)
{
    if (tx.vin.empty()) return state.Invalid(TxValidationResult::TX_BAD_SPECIAL, "bad-evo");
    return state.Invalid(TxValidationResult::TX_BAD_SPECIAL, "bad-prevblk");
}
"""


def selftest_tree_census(validation=SELFTEST_SOURCE + SELFTEST_TREE_ATMP):
    """The tree census over the synthetic validation.cpp and an evo file."""
    sources = {"src/validation.cpp": validation, "src/evo/x.cpp": SELFTEST_TREE_EVO}
    a10 = {"src/validation.cpp": validation, SCRIPT_ERRORS: SELFTEST_ERRORS}
    return tree_scan_sources(sources, a10, SELFTEST_REQUIRED)


def selftest_tree(expect):
    """The tree-mode self-tests (O10 plan §5.1): buckets, comments and the a10 subset rule."""
    census, a10_rows = selftest_tree_census()
    buckets = {t["template"]: t["bucket"] for t in census["templates"]}
    expect("tree: a block-function string is block", buckets.get("bad-cb-missing") == "block")
    expect("tree: a string only in MemPoolAccept is atmp", buckets.get("bad-atmp") == "atmp")
    expect("tree: a string only in src/evo is evo", buckets.get("bad-evo") == "evo")
    expect("tree: a string also in src/evo is evo", buckets.get("bad-prevblk") == "evo")
    comments = [o for t in census["templates"] for o in t["occurrences"] if o.get("comment")]
    expect("tree: a string in a comment is an occurrence, flagged", len(comments) == 1)
    expect("tree: the counts", census["summary"]["templates"] == 5 and census["summary"]["occurrences"] == 8)
    info = [r["template"] for r in census["informational"]]
    expect("tree: a non-bad reason is informational", info == ["txn-mempool-conflict"])
    expect("tree: the a10 subset rule holds", a10_subset_problems(census, a10_rows) == [])
    # An a10 row without its tree occurrence, and an occurrence binding no a10 row, fail.
    dropped = json.loads(json.dumps(census))
    for t in dropped["templates"]:
        for o in t["occurrences"]:
            if o.get("a10_key", "").endswith("#4:bad-cb-missing"):
                del o["a10_key"]
    expect("tree: an a10 row with no occurrence fails",
           any("has no tree occurrence" in p for p in a10_subset_problems(dropped, a10_rows)))
    stray = json.loads(json.dumps(census))
    stray["templates"][0]["occurrences"][0]["a10_key"] = "validation.cpp:Twin#9:bad-x"
    expect("tree: an occurrence binding no a10 row fails",
           any("not an a10 R row" in p for p in a10_subset_problems(stray, a10_rows)))
    # A file's own buckets regenerate exactly: one byte changed in the source changes the census.
    moved, _ = selftest_tree_census("\n" + SELFTEST_SOURCE + SELFTEST_TREE_ATMP)
    expect("tree: a moved line changes the census", dumps(moved) != dumps(census))


def selftest_overlay(expect, fixture):
    """The overlay self-tests (§5.1): a passing overlay, and each rule's failure."""
    prov = {"commit": fixture["provenance"]["commit"]}
    exempt_key = "validation.cpp:Twin#4:bad-cb-missing"
    good = {"key": exempt_key, "a10_decision": "X-reg", "status": "unproven-exempt", "dec": "DEC-1",
            "owner": "L3", "milestone": "M3", "reason": "r", "core": "c", "rdc": "r", "parity": "unproven"}

    def problems(*rows, commit=prov["commit"]):
        return overlay_problems({"provenance": {"commit": commit}, "rows": list(rows)}, fixture)

    expect("overlay: a passing overlay", problems(good) == [])
    expect("overlay: a key a10 lacks fails", any("not an a10 key" in p for p in problems(dict(good, key="x"))))
    expect("overlay: a stale a10 decision fails",
           any("a10 has 'X-reg'" in p for p in problems(dict(good, a10_decision="X-unr"))))
    c_row = dict(good, key="validation.cpp:Twin#1:bad-prevblk", a10_decision="C", status="deferred",
                 slice="O10-5", dep=["x"], milestone="M4")
    expect("overlay: re-deciding a C row fails", any("weakens a10's C decision" in p for p in problems(c_row)))
    expect("overlay: an exemption without a DEC id fails",
           any("without a DEC id" in p for p in problems(dict(good, dec="OD-1"))))
    expect("overlay: an exemption without its rdc site fails",
           any("without rdc" in p for p in problems(dict(good, rdc=""))))
    expect("overlay: a counted status name for a reason fails",
           any("unknown status 'regime-excluded'" in p for p in problems(dict(good, status="regime-excluded"))))
    expect("overlay: a key twice fails", any("twice" in p for p in problems(good, good)))
    expect("overlay: another commit fails", any("commit" in p for p in problems(good, commit="d" * 40)))


def selftest_claims(expect):
    """The claim manifest tests (§5.3, M-2, M-3) over the checked-in v24.0.0 fixtures (no tree):
    reports built to cover every required row and case but what each test leaves out."""
    tag = "v24.0.0"
    values, texts = claim_inputs(tag)
    inventory = values["inventory"]
    manifests = claims(inventory, values["overlay"], values["tree"], texts)
    m3 = manifests["m3"]
    expect("claims: the checked-in manifests are the generated ones",
           all(load_fixture(f"{CLAIMS_DIR}/{m}.json") == v for m, v in manifests.items()))
    expect("claims: the generated manifest stands", manifest_problems(m3, inventory, values["tree"], texts) == [])
    prov = inventory["provenance"]
    sets = {"o10_pairs": ["par1", "par4"], "o10_regime": ["regime"]}
    exercised = [r["key"] for r in m3["rows"] if r["require"] == "exercised"]
    regime = {r["key"] for r in m3["rows"] if "or" in r}
    o10_5 = "validation.cpp:ContextualCheckBlock#5:bad-cb-type"
    # Q-13: every verdict states the peer-penalty plane untested.
    peer = "; peer-penalty untested, deferred(O10-9, P4+P2b+SP1+GV1b, M5)"

    def trial(key, cls="runtime"):
        return {"trial_id": f"t:{key}", "class": cls, "occurrence": {"bound": key}}

    def a10(keys, head="h" * 40, cls="runtime", only=None, **over):
        evo = dict(SELFTEST_EVO_STATE)
        legs = [{"name": n, "status": "done", "evo_keys": evo, "only": only,
                 "trials": [trial(k, cls) for k in keys] if n == "par1" else []}
                for n in ("par1", "par4")]
        report = {"set": "tier1", "rdc_head": head, "dashd": {"tag": prov["tag"], "commit": prov["commit"]},
                  "manifest": [{"trial_id": f"t:{k}", "status": "ran"} for k in keys], "legs": legs}
        return {**report, **over}

    def o10(set_, legs, keys=(), cases=None, head="h" * 40, **over):
        report = {"set": set_, "rdc_head": head, "complete": True,
                  "dashd": {"tag": prov["tag"], "commit": prov["commit"]},
                  "legs": [{"name": n, "status": "done", "trials": [trial(k) for k in keys] if i == 0 else []}
                           for i, n in enumerate(legs)],
                  "o10_cases": {c["id"]: "pass" for c in m3["cases"]} if cases is None else cases}
        return {**report, **over}

    def full(omit=None, **over):
        """Reports covering every required row and case, but `omit`."""
        reports = {
            "a10": a10([k for k in exercised if k not in regime and k != omit]),
            "pairs": o10("o10_pairs", sets["o10_pairs"],
                         cases={c["id"]: "pass" for c in m3["cases"] if c["id"] != omit}),
            "regime": o10("o10_regime", sets["o10_regime"],
                          keys=[k for k in regime if k != omit], cases={}),
        }
        reports.update(over)
        return claim(m3, inventory, reports, o10_sets=sets)

    problems, lines, verdict = full()
    expect("claims: every row and case covered, bad-sb-start exempt: incomplete-with-exemption (DEC-988)",
           problems == [] and verdict == "incomplete-with-exemption (DEC-988)" + peer)
    expect("claims: --partial prints every row, case and deferred row",
           len(lines) == len(m3["rows"]) + len(m3["cases"]) + len(m3["deferred"]))
    # M-2: an M3 row omitted from the reports fails the claim; so does a case.
    for omit in (exercised[0], sorted(regime)[0], "o10.w.W5.solos"):
        problems, lines, verdict = full(omit=omit)
        expect(f"claims: omitting {omit} leaves the claim incomplete",
               problems == [] and verdict.startswith("incomplete (1 ") and any(f"{omit}: UNCOVERED" in l for l in lines))
    # A deferred M4 row with no report passes M3 and prints deferred(O10-5, …), never covered.
    expect("claims: a deferred O10-5 row prints deferred(O10-5, …)",
           any(l == f"defer {o10_5}: deferred(O10-5, MN0-2a+CH7a-4a+P-1, M4), not covered" for l in lines))
    expect("claims: the deferred O10-5 row is in the M4 skeleton, unfilled",
           any(r.get("key") == o10_5 and r["status"] == "unfilled" for r in manifests["m4"]["rows"]))
    # A deferred row submitted as covered fails (an a10 leg or an O10 leg).
    problems, _, verdict = full(extra=o10("o10_pairs", sets["o10_pairs"], keys=[o10_5], cases={}))
    expect("claims: a deferred row submitted as covered fails",
           verdict == "failed" + peer and any(f"reports the deferred row {o10_5}" in p for p in problems))
    # M-3: replay never counts; a refused set (O10-5, devnet) is no union input.
    for set_ in ("replay", "o10_dip3", "devnet"):
        problems, _, verdict = full(extra=o10(set_, ["x"], cases={}))
        expect(f"claims: a {set_} report is refused", verdict == "failed" + peer and any("refused" in p for p in problems))
    problems, _, verdict = full(extra=dict(o10(None, ["x"], cases={}), name="devnet-walk"))
    expect("claims: a devnet-walk report with no set is refused",
           verdict == "failed" + peer and any("refused" in p for p in problems))
    # OD-L3-29: warn is a10's regtest census set (SET_LEGS), a union input like tier1.
    warn = a10([exercised[0]], set="warn")
    warn["legs"] = [dict(warn["legs"][0], name="warn")]
    problems, _, verdict = full(extra=warn)
    expect("claims: a warn report is accepted as a union input",
           problems == [] and verdict == "incomplete-with-exemption (DEC-988)" + peer)
    problems, _, _ = full(extra=dict(o10("o10_pairs", sets["o10_pairs"], cases={}), replay_case="c.json"))
    expect("claims: a replay-case report is refused", any("replay is never coverage" in p for p in problems))
    problems, lines, verdict = claim(m3, inventory, {"a": a10([exercised[0]], cls="n/a")}, o10_sets=sets)
    expect("claims: a binding with no class covers nothing", any(f"{exercised[0]}: UNCOVERED" in l for l in lines))
    _, lines, _ = claim(m3, inventory, {"a": a10([exercised[0]], cls="site-only")}, o10_sets=sets)
    expect("claims: a site-only binding is exercised-unverified",
           any(f"{exercised[0]}: exercised-unverified" in l for l in lines))
    # §5.2: a row covered only with the class unverified counts, and the verdict names it.
    reports = {"a10": a10([k for k in exercised if k not in regime and k != exercised[0]]),
               "u": a10([exercised[0]], cls="unverified"),
               "pairs": o10("o10_pairs", sets["o10_pairs"]),
               "regime": o10("o10_regime", sets["o10_regime"], keys=sorted(regime), cases={})}
    problems, _, verdict = claim(m3, inventory, reports, o10_sets=sets)
    expect("claims: a class-unverified row counts and the verdict lists it for L3",
           problems == [] and verdict.startswith("incomplete-with-exemption (DEC-988)")
           and verdict.endswith(f"class-unverified, L3 escalates: {exercised[0]}" + peer))
    # Each report carries its head, its pin and its completeness.
    problems, _, _ = full(extra=a10([], head="g" * 40))
    expect("claims: mixed heads fail", any("mixed heads" in p for p in problems))
    problems, _, _ = claim(m3, inventory, {"a": a10([])}, head="g" * 40, o10_sets=sets)
    expect("claims: a report off the claimed head fails", any("not the claimed head" in p for p in problems))
    problems, _, _ = full(extra=a10([], dashd={"tag": "v24.0.0-rc.3", "commit": "c" * 40}))
    expect("claims: a report from another dashd fails", any("dashd v24.0.0-rc.3" in p for p in problems))
    problems, _, _ = full(extra=o10("o10_pairs", sets["o10_pairs"], cases={}, complete=False))
    expect("claims: an incomplete O10 report fails", any("not complete" in p for p in problems))
    problems, _, _ = full(extra=a10([exercised[0]], manifest=[{"trial_id": "x", "status": "not run"}]))
    expect("claims: an a10 report with a row not run fails", any("incomplete: " in p for p in problems))
    problems, _, _ = full(extra=a10([exercised[0]], only="m_bip68"))
    expect("claims: a filtered a10 run (L3_ONLY) fails", any("filtered to 'm_bip68'" in p for p in problems))
    old = a10([exercised[0]])
    old["legs"] = [{k: v for k, v in leg.items() if k != "only"} for leg in old["legs"]]
    problems, _, _ = full(extra=old)
    expect("claims: an a10 report that does not say it ran unfiltered fails",
           any("filtered to 'unknown'" in p for p in problems))
    problems, _, _ = claim(m3, inventory, {"g": o10("o10_alt_smoke", ["par1"], cases={})})
    expect("claims: an O10 set whose slice has not defined its legs is refused",
           any("not defined yet" in p for p in problems))
    # O10-3 defines o10_gen: each -par over four shards; a generated trial credits no row (§3.4),
    # but one binding a proven-unreachable row still fails its proof; the minimums gate it.
    gsets = dict(sets, o10_gen=O10_SETS["o10_gen"])
    met = {"n_per_par": 2000, "minimums": {"met": True, "short": []}}

    def gen(keys=(), legs=None, generator=None, **over):
        report = o10("o10_gen", O10_SETS["o10_gen"] if legs is None else legs, cases={},
                     generator=met if generator is None else generator, **over)
        report["legs"][0]["trials"] = [dict(trial(k), trial_id=f"{GEN_TRIAL_PREFIX}10003.{i}@Reg/par1+cold")
                                       for i, k in enumerate(keys)]
        return report

    def gen_full(**reports):
        rs = {"a10": a10([k for k in exercised if k not in regime]),
              "pairs": o10("o10_pairs", sets["o10_pairs"]),
              "regime": o10("o10_regime", sets["o10_regime"], keys=sorted(regime), cases={})}
        rs.update(reports)
        return claim(m3, inventory, rs, o10_sets=gsets)

    expect("claims: o10_gen runs par1 and par4 over four shards",
           O10_SETS["o10_gen"] == ["par1.s0", "par1.s1", "par1.s2", "par1.s3",
                                   "par4.s0", "par4.s1", "par4.s2", "par4.s3"])
    problems, _, verdict = gen_full(g=gen())
    expect("claims: a complete o10_gen report is a union input",
           problems == [] and verdict == "incomplete-with-exemption (DEC-988)" + peer)
    problems, _, _ = gen_full(g=gen(legs=["par1", "par4"]))
    expect("claims: an o10_gen report without its shards is incomplete",
           any("set o10_gen has exactly" in p for p in problems))
    problems, _, _ = gen_full(g=gen(generator={"n_per_par": 2000, "minimums": {"met": False, "short": ["x"]}}))
    expect("claims: an o10_gen report below a minimum is incomplete",
           any("minimums are not met" in p for p in problems))
    problems, _, _ = gen_full(g=gen(generator={"n_per_par": 40, "minimums": {"met": True, "short": []}}))
    expect("claims: an o10_gen report below the nightly's N is incomplete",
           any("N = 40 per -par" in p for p in problems))
    rs = {"a10": a10([k for k in exercised if k not in regime and k != exercised[0]]),
          "pairs": o10("o10_pairs", sets["o10_pairs"]),
          "regime": o10("o10_regime", sets["o10_regime"], keys=sorted(regime), cases={}),
          "g": gen(keys=[exercised[0]])}
    problems, lines, verdict = claim(m3, inventory, rs, o10_sets=gsets)
    expect("claims: a generated trial credits no row",
           problems == [] and any(f"{exercised[0]}: UNCOVERED" in l for l in lines))
    unreachable = "specialtxman.cpp:ProcessSpecialTxsInBlock#3:bad-cbtx-version"
    problems, _, verdict = gen_full(g=gen(keys=[unreachable]))
    expect("claims: a generated trial binding a proven-unreachable row fails its proof",
           verdict == "failed" + peer and any(f"binds {unreachable}, a proven-unreachable row" in p for p in problems))
    # O10-2 defines o10_pairs: exactly par1 and par4 (§4.5), every class case named by its class.
    expect("claims: o10_pairs runs par1 and par4", O10_SETS["o10_pairs"] == ["par1", "par4"])
    problems, _, _ = claim(m3, inventory, {"p": o10("o10_pairs", ["par1"], cases={})})
    expect("claims: an o10_pairs report without par4 is incomplete",
           any("legs ['par1'], set o10_pairs has exactly ['par1', 'par4']" in p for p in problems))
    problems, _, _ = claim(m3, inventory, {"p": o10("o10_pairs", ["par1", "par4"], cases={"o10.pair.class6": "fail"})})
    expect("claims: a failed pair class fails", any("case o10.pair.class6 fail" in p for p in problems))
    classes = [c for c in m3["cases"] if c["id"].startswith("o10.pair.class") and "class" in c]
    expect("claims: the m3 cases name the ten pair classes",
           [c["class"] for c in classes] == list(PAIR_CLASSES.values()))
    # An unknown string, an unknown case and an unknown set fail.
    problems, _, _ = full(extra=a10(["validation.cpp:CheckBlock#9:bad-nonsense"]))
    expect("claims: an unknown string fails", any("an unknown string" in p for p in problems))
    problems, _, _ = full(extra=o10("o10_pairs", sets["o10_pairs"], cases={"o10.nonsense": "pass"}))
    expect("claims: an unknown case fails", any("unknown case o10.nonsense" in p for p in problems))
    problems, _, _ = full(extra=o10("tier9", ["x"], cases={}))
    expect("claims: an unknown set fails", any("unknown set 'tier9'" in p for p in problems))
    # A trial binding a proven-unreachable row refutes its proof; one binding an exempt row is
    # evidence against the exemption's premise: both fail the claim, whatever the set.
    sb = "validation.cpp:ConnectBlock#3:bad-sb-start"
    pstib3 = "specialtxman.cpp:ProcessSpecialTxsInBlock#3:bad-cbtx-version"
    problems, lines, verdict = full(extra=o10("o10_pairs", sets["o10_pairs"], keys=[pstib3], cases={}))
    expect("claims: a binding of a proven-unreachable row fails its proof",
           verdict == "failed" + peer
           and any(f"binds {pstib3}, a proven-unreachable row on Submit, Proposal: the proof (o10 overlay: {pstib3}) fails"
                   in p for p in problems))
    unr = next(r["key"] for r in m3["rows"] if r["require"] == "proven-unreachable" and r["key"] != pstib3)
    problems, _, verdict = full(extra=o10("o10_regime", sets["o10_regime"], keys=[unr], cases={}))
    expect("claims: a binding of an a10 X-unr row fails its proof",
           verdict == "failed" + peer and any(f"binds {unr}, a proven-unreachable row: the proof" in p for p in problems))
    problems, _, verdict = full(extra=o10("o10_pairs", sets["o10_pairs"], keys=[sb], cases={}))
    expect("claims: a binding of an unproven-exempt row is refused",
           verdict == "failed" + peer and any(f"binds {sb}, an unproven-exempt row (DEC-988)" in p for p in problems))
    # A bound occurrence that is not a string is a problem naming the trial, not a traceback.
    odd = o10("o10_pairs", sets["o10_pairs"], cases={})
    odd["legs"][0]["trials"] = [{"trial_id": "t:odd", "class": "runtime", "occurrence": {"bound": None}}]
    problems, _, verdict = full(extra=odd)
    expect("claims: a non-string bound occurrence is a problem",
           verdict == "failed" + peer and any("extra par1 t:odd: binds None, not a row key" in p for p in problems))
    # Q-13: the plane is deferred in M3, carried by M4, unfilled in M5, and printed untested.
    _, lines, _ = full()
    expect("claims: the peer-penalty plane prints untested",
           "defer plane peer-penalty: deferred(O10-9, P4+P2b+SP1+GV1b, M5), untested" in lines)
    expect("claims: M4 carries the peer-penalty plane, M5 holds it unfilled",
           [r["status"] for r in manifests["m4"]["rows"] if r.get("plane") == "peer-penalty"] == ["deferred"]
           and [r["status"] for r in manifests["m5"]["rows"] if r.get("plane") == "peer-penalty"] == ["unfilled"])
    expect("claims: a skeleton's verdict states the plane too",
           claim(manifests["m4"], inventory, {}, o10_sets=sets)[2] == "failed" + peer)
    for m, field in (("m3", "deferred"), ("m4", "rows"), ("m5", "rows")):
        man = manifests[m]
        lost = dict(man, **{field: [d for d in man[field] if "plane" not in d]})
        expect(f"claims: {m} without the peer-penalty plane fails",
               any(p.startswith("plane peer-penalty: missing") for p in manifest_problems(lost, inventory, values["tree"], texts)))
    odd = dict(m3, deferred=[*m3["deferred"], dict(m3["deferred"][-1], plane="gossip")])
    expect("claims: an unknown plane fails",
           any("plane gossip: not a known plane" in p for p in manifest_problems(odd, inventory, values["tree"], texts)))
    # The manifest itself: an exemption without a DEC id, a stale manifest, coverage on a deferral.
    bad = dict(m3, rows=[dict(r, dec="OD-1") if r["key"] == sb else r for r in m3["rows"]])
    expect("claims: an unproven-exempt row without a DEC id fails",
           any("without a DEC id" in p for p in manifest_problems(bad, inventory, values["tree"], texts)))
    stale = dict(texts, **{f"inventory-{tag}.json": texts[f"inventory-{tag}.json"] + " "})
    expect("claims: a manifest over changed inputs is stale",
           any(p.startswith("stale: the manifest's input shas") for p in manifest_problems(m3, inventory, values["tree"], stale)))
    gone = dict(m3, rows=[*m3["rows"], {"key": "validation.cpp:CheckBlock#9:bad-gone", "require": "exercised"}])
    expect("claims: a manifest row not in the tree is stale",
           any("stale: validation.cpp:CheckBlock#9:bad-gone" in p for p in manifest_problems(gone, inventory, values["tree"], texts)))
    gone = dict(m3, deferred=[*m3["deferred"], {"template": "bad-gone", "slice": "O10-7a", "dep": [], "milestone": "M4"}])
    expect("claims: a deferred string not in the tree is stale",
           any("stale: bad-gone is not a tree string" in p for p in manifest_problems(gone, inventory, values["tree"], texts)))
    covered = dict(m3, deferred=[dict(d, covered_by=["t"]) if d.get("key") == o10_5 else d for d in m3["deferred"]])
    expect("claims: a deferred row with a coverage field fails",
           any("coverage field" in p for p in manifest_problems(covered, inventory, values["tree"], texts)))
    expect("claims: a skeleton manifest never passes",
           any("skeleton" in p for p in claim(manifests["m4"], inventory, {}, o10_sets=sets)[0]))
    # Generation: a tree string no slice rule names fails, so a new tag's strings get a decision.
    new = dict(values["tree"], templates=[*values["tree"]["templates"],
                                          {"template": "bad-novel", "bucket": "evo",
                                           "occurrences": [{"file": "evo/x.cpp", "line": 1}]}])
    try:
        claims(inventory, values["overlay"], new, texts)
        expect("claims: a tree string with no slice fails the generation", False)
    except ScanError as e:
        expect("claims: a tree string with no slice fails the generation", "bad-novel" in str(e))


SELFTEST_CT_INNER = """
bool Inner(const CTransaction& tx, TxValidationState& state)
{
    if (tx.vin.empty()) return state.Invalid(TxValidationResult::TX_CONSENSUS, "bad-inner");
    return true;
}
"""
SELFTEST_CT_OUTER = """
bool Outer(const CBlock& block, BlockValidationState& state)
{
    if (block.vtx.empty()) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-outer");
    for (const auto& tx : block.vtx) {
        TxValidationState tx_state;
        if (!Inner(*tx, tx_state)) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, tx_state.GetRejectReason());
    }
    return true;
}
"""
SELFTEST_CT_LOOP = "    for (const auto& tx : block.vtx) {"


def selftest_tables(**over):
    """Empty resolution tables over the synthetic sources, with `over`'s entries."""
    tables = {name: (set() if name in ("TRIGGERS", "WAITS") else {}) for name in TABLES}
    tables["LOOPS"] = {("Outer", "const auto& tx : block.vtx"): ("block.vtx", 0)}
    tables["ROOTS"] = {"Submit": [("call", 2, "Outer(", "Outer")]}
    tables["ATTRS"] = {}
    tables.update(over)
    return tables


def selftest_calltree_build(outer=SELFTEST_CT_OUTER, **over):
    """The call tree of the synthetic Inner and Outer, or the ScanError text."""
    source = SELFTEST_CT_INNER + outer
    required = {"src/validation.cpp": ["Inner", "Outer"]}
    spans = {}
    try:
        rows, _ = scan_sources({"src/validation.cpp": source, SCRIPT_ERRORS: SELFTEST_ERRORS}, required, spans)
        return calltree_sources({"src/validation.cpp": source}, "x\nOuter(\n", rows, spans, required,
                                selftest_tables(**over))
    except ScanError as e:
        return str(e)


def selftest_calltree(expect):
    """The call-tree scanner self-tests (§4.2): a baseline tree, and a witness for each way the
    scan fails closed."""
    model = selftest_calltree_build()
    shape = None
    if isinstance(model, dict):
        body = model["functions"]["Outer"]["body"]
        shape = [(n["n"], [c["n"] for c in n.get("body", [])]) for n in body]
    expect("calltree: the baseline tree", shape == [("row", []), ("loop", ["call", "row"]), ("return", [])])
    expect("calltree: a mode root calls its entry", isinstance(model, dict)
           and model["modes"]["Submit"] == [{"n": "call", "line": 2, "callee": "Outer"}])

    def fails(text, outer=SELFTEST_CT_OUTER, **over):
        got = selftest_calltree_build(outer, **over)
        return isinstance(got, str) and text in got

    def edit(old, new):
        assert old in SELFTEST_CT_OUTER, old
        return SELFTEST_CT_OUTER.replace(old, new)

    expect("calltree: an unknown loop header fails",
           fails("an unresolved loop domain", edit(SELFTEST_CT_LOOP, "    for (auto& tx : block.vtx) {")))
    guarded_call = "    if (block.nVersion > 1) {\n        Inner(*block.vtx[0], s);\n    }\n"
    expect("calltree: an undeclared guard around a call fails",
           fails("an unresolved condition", edit("    return true;\n}", guarded_call + "    return true;\n}")))
    two_rows = ('    if (block.nVersion > 1) {\n'
                '        if (block.nTime > 9) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-a");\n'
                '        if (block.nTime > 7) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-b");\n'
                '    }\n')
    expect("calltree: an undeclared condition around two rows fails",
           fails("an unresolved condition", edit("    return true;\n}", two_rows + "    return true;\n}")))
    expect("calltree: a declared guard resolves it", isinstance(selftest_calltree_build(
        edit("    return true;\n}", two_rows + "    return true;\n}"),
        GUARDS={("Outer", "block.nVersion > 1"): [["v2", True]]}), dict))
    expect("calltree: a call condition with an undeclared prefix fails",
           fails("an unresolved call condition", edit("if (!Inner(*tx, tx_state))", "if (fFlag && !Inner(*tx, tx_state))")))
    expect("calltree: an undeclared bridge fails",
           fails("a bridge call Activate not declared",
                 edit("    return true;\n}", "    if (!Activate(state)) return false;\n    return true;\n}"),
                 BRIDGES={("Other", "Activate"): "Inner"}))
    expect("calltree: a stale table entry fails",
           fails("stale table entries", LOOPS={("Outer", "const auto& tx : block.vtx"): ("block.vtx", 0),
                                                ("Outer", "gone"): ("other", 0)}))
    expect("calltree: an undeclared switch fails",
           fails("an unresolved switch", edit("    return true;\n}", "    switch (block.nVersion) {\n    }\n    return true;\n}")))
    expect("calltree: an unknown return form fails",
           fails("an unknown return form", edit("    return true;\n}", "    return accepted;\n}")))
    nested = edit("        TxValidationState tx_state;", "        for (const auto& tx : block.vtx) {}\n        TxValidationState tx_state;")
    expect("calltree: a loop nested in its own domain fails",
           fails("nested in itself", nested, LOOPS={("Outer", "const auto& tx : block.vtx"): ("block.vtx", 0)}))
    expect("calltree: a failure branch holding more than W rows fails",
           fails("holds more than W rows", edit("tx_state.GetRejectReason());\n", "tx_state.GetRejectReason()); }\n").replace(
                "if (!Inner(*tx, tx_state)) return", "if (!Inner(*tx, tx_state)) { Inner(*tx, tx_state); return")))
    expect("calltree: a root whose source line moved fails",
           fails("not found (Submit root)", ROOTS={"Submit": [("call", 1, "Outer(", "Outer")]}))
    witnesses = selftest_witnesses()
    expect("calltree: the synthetic witness models build", isinstance(witnesses, dict))
    expect("calltree: precedence-selftest.json is the generated one",
           load_fixture(SELFTEST_FIXTURE) == witness_fixture(witnesses))


# §4.4's synthetic witnesses: each W structure and each further pair, rebuilt in a synthetic
# source, scanned into a model by the same builder, with the winner §4.3 predicts. node-l1's
# `l3::precedence` walks them (precedence-selftest.json).
SELFTEST_W_LOOPS = """
bool Inner(const CTransaction& tx, TxValidationState& state)
{
    if (tx.vin.empty()) return state.Invalid(TxValidationResult::TX_CONSENSUS, "bad-body");
    return true;
}
bool Outer(const CBlock& block, BlockValidationState& state)
{
    if (block.nSize > 9) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-pre");
    for (const auto& tx : block.vtx) {
        TxValidationState tx_state;
        if (!Inner(*tx, tx_state)) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, tx_state.GetRejectReason());
    }
    for (const auto& tx : block.vtx) {
        if (tx->nLockTime > 3) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-loop2");
    }
    if (block.nSigOps > 9) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-post");
    return true;
}
"""
SELFTEST_W_INPUTS = """
bool Inputs(const CBlock& block, BlockValidationState& state)
{
    for (const auto& tx : block.vtx) {
        for (const auto& txin : tx->vin) {
            if (coin.IsCoinBase() && young) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-premature");
            if (!MoneyRange(nValueIn)) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-range");
        }
    }
    return true;
}
"""
SELFTEST_W_CONNECT = """
bool Belowout(const CTransaction& tx, TxValidationState& state)
{
    if (tx.nIn < tx.nOut) return state.Invalid(TxValidationResult::TX_CONSENSUS, "bad-belowout");
    return true;
}
bool Scripts(const CTransaction& tx, TxValidationState& state, std::vector<CScriptCheck>* pvChecks)
{
    for (unsigned int i = 0; i < tx.vin.size(); i++) {
        CScriptCheck check(tx, i);
        if (pvChecks) {
            pvChecks->emplace_back(std::move(check));
        } else if (!check()) {
            return state.Invalid(TxValidationResult::TX_CONSENSUS, "bad-script");
        }
    }
    return true;
}
bool Connect(const CBlock& block, BlockValidationState& state)
{
    for (const auto& tx : block.vtx) {
        if (!tx->IsCoinBase()) {
            TxValidationState tx_state;
            if (!Belowout(*tx, tx_state)) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, tx_state.GetRejectReason());
            if (tx->nLockTime > 3) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-seqlock");
            if (!Scripts(*tx, tx_state, pvChecks)) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, tx_state.GetRejectReason());
        }
    }
    if (!control.Wait()) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-wait");
    if (block.nFee > 9) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-after");
    return true;
}
"""
VTX = "block.vtx"


def shape(inst, domains, iters, setting=None):
    """Each loop domain's length as node-l1's walker reads it (`Env::length`): the measured one
    in `setting`, else one past the highest index an instance's trigger or an `iter`
    attribute's default (`iters`: domain -> indices) names, else 0."""
    measured = (setting or {}).get("lengths", {})
    out = {}
    for d in domains:
        named = [i for _, trig in inst for dd, i in trig if dd == d] + list(iters.get(d, ()))
        out[d] = measured.get(d, max(named) + 1 if named else 0)
    return out


def with_solos(cases, domains, defers=None, iters=None):
    """`cases` and, after them, a single-instance control for every constituent of every
    multi-instance case at each `-par` it runs at (plan §4.3, DEC-998 M-1), in the case's whole
    setting: its facts and every loop length (`shape`), made explicit. A single-instance case
    already holding it (the same instance and setting, binding its own row, or at par > 1 the
    wait its `defers` entry names, deferred) is the control. A case may end with its setting,
    {"facts": {...}, "lengths": {...}}."""
    defers, iters = defers or {}, iters or {}

    def own(template, par):
        return (defers[template], True) if template in defers and par > 1 else (template, False)

    def setting(inst, rest):
        given = rest[0] if rest else {}
        facts = tuple(sorted(given.get("facts", {}).items()))
        return facts, tuple(shape(inst, domains, iters, given).items())

    have = {(inst[0][0], tuple(inst[0][1]), p, setting(inst, rest))
            for _, inst, pars, _, _, *rest in cases if len(inst) == 1 for p in pars}
    solos = {}  # (template, trigger, setting, want, deferred) -> its -par values, in first-seen order
    for _, inst, pars, _, _, *rest in cases:
        if len(inst) < 2:
            continue
        ctx = setting(inst, rest)
        for template, trigger in inst:
            for p in pars:
                if (template, tuple(trigger), p, ctx) not in have:
                    have.add((template, tuple(trigger), p, ctx))
                    solos.setdefault((template, tuple(trigger), ctx, *own(template, p)), []).append(p)

    def solo(t, trig, ctx, ps, want, deferred):
        facts, lengths = ctx
        where = ", ".join(f"{d} {i}" for d, i in trig) or "no loop"
        lens = ", ".join(f"{d} {n}" for d, n in lengths)
        given = {"facts": dict(facts), "lengths": dict(lengths)} if facts else {"lengths": dict(lengths)}
        return (f"solo: {t} at {where} (of {lens})", [(t, list(trig))], tuple(ps), want, deferred, given)
    return [*cases, *(solo(t, trig, ctx, ps, want, deferred) for (t, trig, ctx, want, deferred), ps in solos.items())]


# name -> (source, functions in order, entry, tables, cases). A case: (name, instances as
# (template, trigger), par values, the winning template or None, deferred). `selftest_witnesses`
# appends each group's generated solo controls ([`with_solos`]).
SELFTEST_WITNESSES = {
    "loops": (SELFTEST_W_LOOPS, ["Inner", "Outer"], "Outer",
              {"LOOPS": {("Outer", "const auto& tx : block.vtx"): (VTX, 0)}},
              [("pre-loop row vs a body row at index 0 (W5)", [("bad-pre", []), ("bad-body", [(VTX, 0)])], (1, 4), "bad-pre", False),
               ("pre-loop row vs a body row above its position (W5)", [("bad-body", [(VTX, 5)]), ("bad-pre", [])], (1, 4), "bad-pre", False),
               ("post-loop row vs a body row at a high index (W6)", [("bad-post", []), ("bad-body", [(VTX, 11)])], (1, 4), "bad-body", False),
               ("sibling loop 2 at 0 vs sibling loop 1 at 5", [("bad-loop2", [(VTX, 0)]), ("bad-body", [(VTX, 5)])], (1, 4), "bad-body", False),
               ("solo: the body row", [("bad-body", [(VTX, 11)])], (1, 4), "bad-body", False),
               ("solo: the post-loop row", [("bad-post", [])], (1, 4), "bad-post", False)]),
    "inputs": (SELFTEST_W_INPUTS, ["Inputs"], "Inputs",
               {"LOOPS": {("Inputs", "const auto& tx : block.vtx"): (VTX, 0),
                          ("Inputs", "const auto& txin : tx->vin"): ("tx.vin", 0)}},
               [("nested: outer 1 inner 0 vs outer 0 inner 7", [("bad-range", [(VTX, 1), ("tx.vin", 0)]),
                                                                ("bad-premature", [(VTX, 0), ("tx.vin", 7)])], (1, 4), "bad-premature", False),
                ("W3: [MAX, MAX, immature]: range at j = 1", [("bad-range", [(VTX, 1), ("tx.vin", 1)]),
                                                             ("bad-premature", [(VTX, 1), ("tx.vin", 2)])], (1, 4), "bad-range", False),
                ("W3 reversed: [immature, MAX, MAX]: premature at j = 0", [("bad-range", [(VTX, 1), ("tx.vin", 2)]),
                                                                          ("bad-premature", [(VTX, 1), ("tx.vin", 0)])], (1, 4), "bad-premature", False),
                ("W3b: [MAX mature, immature]: premature at j = 1", [("bad-range", [(VTX, 1), ("tx.vin", 1)]),
                                                                    ("bad-premature", [(VTX, 1), ("tx.vin", 1)])], (1, 4), "bad-premature", False)]),
    "connect": (SELFTEST_W_CONNECT, ["Belowout", "Scripts", "Connect"], "Connect",
                {"LOOPS": {("Connect", "const auto& tx : block.vtx"): (VTX, 0),
                           ("Scripts", "unsigned int i = 0; i < tx.vin.size(); i++"): ("tx.vin", 0)},
                 "GUARDS": {("Connect", "!tx->IsCoinBase()"): [["coinbase", False]]},
                 "TRIGGERS": {("Scripts", "!check()")},
                 "DEFERS": {("Scripts", "pvChecks"): {"row": "@bad-script", "wait": "@bad-wait", "guard": [["par", True]]}},
                 "WAITS": {"@bad-wait"},
                 "ATTRS": {"coinbase": {"kind": "iter", "domain": VTX, "default": [0]}, "par": {"kind": "par"}}},
                [("W1: tx1 sequence lock, tx2 below-out", [("bad-belowout", [(VTX, 2)]), ("bad-seqlock", [(VTX, 1)])], (1, 4), "bad-seqlock", False),
                 ("W1 reversed", [("bad-belowout", [(VTX, 1)]), ("bad-seqlock", [(VTX, 2)])], (1, 4), "bad-belowout", False),
                 ("W2: a callee row in tx1 vs a caller row in tx2", [("bad-seqlock", [(VTX, 2)]), ("bad-belowout", [(VTX, 1)])], (1, 4), "bad-belowout", False),
                 ("W2: call site before the caller's later row, one tx", [("bad-seqlock", [(VTX, 1)]), ("bad-belowout", [(VTX, 1)])], (1, 4), "bad-belowout", False),
                 ("W4: tx1 script fault, tx2 clean, par1", [("bad-script", [(VTX, 1), ("tx.vin", 0)])], (1,), "bad-script", False),
                 ("W4: tx1 script fault, tx2 clean, par4", [("bad-script", [(VTX, 1), ("tx.vin", 0)])], (4,), "bad-wait", True),
                 ("W4 reversed: tx1 script + tx2 sequence lock, par1", [("bad-script", [(VTX, 1), ("tx.vin", 0)]), ("bad-seqlock", [(VTX, 2)])], (1,), "bad-script", False),
                 ("deferred vs a later-iteration inline row: inline wins at par4", [("bad-script", [(VTX, 1), ("tx.vin", 0)]), ("bad-seqlock", [(VTX, 2)])], (4,), "bad-seqlock", False),
                 ("deferred vs a post-Wait row: the deferral wins", [("bad-script", [(VTX, 1), ("tx.vin", 0)]), ("bad-after", [])], (4,), "bad-wait", True),
                 ("a guarded-out iteration (the coinbase) holds no instance", [("bad-seqlock", [(VTX, 0)])], (1, 4), None, False),
                 ("the same row past the coinbase fires", [("bad-seqlock", [(VTX, 1)])], (1, 4), "bad-seqlock", False)]),
}


def selftest_witnesses():
    """The synthetic witness models with their cases (precedence-selftest.json's value), or the
    ScanError text."""
    out = {}
    for name, (source, functions, entry, over, cases) in SELFTEST_WITNESSES.items():
        required = {"src/validation.cpp": functions}
        spans = {}
        try:
            rows, _ = scan_sources({"src/validation.cpp": source, SCRIPT_ERRORS: SELFTEST_ERRORS}, required, spans)
        except ScanError as e:
            return f"{name}: {e}"
        keys = {r["template"]: r["key"] for r in rows if r["kind"] == "R"}
        def key(t):
            return keys[t[1:]] if isinstance(t, str) and t.startswith("@") else t
        tables = dict(over)
        if "DEFERS" in tables:
            tables["DEFERS"] = {k: {**d, "row": key(d["row"]), "wait": key(d["wait"])} for k, d in tables["DEFERS"].items()}
        if "WAITS" in tables:
            tables["WAITS"] = {key(w) for w in tables["WAITS"]}
        line = 2
        tables["ROOTS"] = {"Submit": [("call", line, f"{entry}(", entry)]}
        try:
            model = calltree_sources({"src/validation.cpp": source}, f"x\n{entry}(\n", rows, spans, required,
                                     selftest_tables(**tables))
        except ScanError as e:
            return f"{name}: {e}"
        model.pop("structural_reach")
        domains = sorted({d for d, _ in over.get("LOOPS", {}).values()})
        iters = {a["domain"]: a["default"] for a in over.get("ATTRS", {}).values() if a["kind"] == "iter"}
        defers = {d["row"][1:]: d["wait"][1:] for d in over.get("DEFERS", {}).values()}
        model["cases"] = [
            {"name": c, "instances": [{"row": keys[t], "trigger": [[d, i] for d, i in trig]} for t, trig in inst],
             "par": list(par), "want": keys[want] if want else None, "deferred": deferred, **(rest[0] if rest else {})}
            for c, inst, par, want, deferred, *rest in with_solos(cases, domains, defers, iters)]
        out[name] = model
    return out


SELFTEST_FIXTURE = "precedence-selftest.json"


def witness_fixture(witnesses):
    if isinstance(witnesses, str):
        raise ScanError(f"the synthetic witnesses: {witnesses}")
    prov = {"calltree_version": CALLTREE_VERSION,
            "note": "l3-census.py o10-check (O10 plan §4.4): the synthetic witnesses, scanned by the call-tree "
                    "builder, each case with the winner §4.3 predicts; node-l1's l3::precedence walks them"}
    return {"provenance": prov, "witnesses": witnesses}


FIXTURE_DIR = Path(__file__).resolve().parent / "l3-census"


def claim_inputs(tag, tree_value=None):
    """The claim manifests' inputs at `tag` (a10's inventory, the overlay, the tree census) and
    their fixture texts; `tree_value` replaces the checked-in tree census (o10-check's own scan)."""
    names = {"inventory": f"inventory-{tag}.json", "overlay": f"o10-overlay-{tag}.json",
             "tree": f"inventory-tree-{tag}.json"}
    texts = {k: (FIXTURE_DIR / v).read_text() for k, v in names.items()}
    if tree_value is not None:
        texts["tree"] = dumps(tree_value)
    values = {k: json.loads(t) for k, t in texts.items()}
    return values, {names[k]: t for k, t in texts.items()}


def o10_fixtures(tree, tag):
    """O10's generated fixtures at `tag`: file name -> its regenerated value."""
    census = tree_scan(tree, tag)
    values, texts = claim_inputs(tag, census)
    manifests = claims(values["inventory"], values["overlay"], values["tree"], texts)
    return {
        f"inventory-tree-{tag}.json": census,
        f"precedence-{tag}.json": calltree_scan(tree, tag),
        SELFTEST_FIXTURE: witness_fixture(selftest_witnesses()),
        **{f"{CLAIMS_DIR}/{m}.json": v for m, v in manifests.items()},
    }


def o10_tree(args):
    sys.stdout.write(dumps(tree_scan(args.tree, args.tag)))
    return 0


def load_fixture(name):
    with open(FIXTURE_DIR / name) as f:
        return json.load(f)


def o10_check(args):
    """Each O10 fixture regenerated from the tag equals its checked-in file byte for byte; the
    overlay stands over a10's inventory."""
    failed = []
    inventory = load_fixture(f"inventory-{args.tag}.json")
    if problems := overlay_problems(load_fixture(f"o10-overlay-{args.tag}.json"), inventory):
        failed.append("overlay")
        print(f"l3-census: o10-overlay-{args.tag}.json fails:", *problems, sep="\n  ", file=sys.stderr)
    else:
        print(f"l3-census: o10-overlay-{args.tag}.json stands over the a10 inventory")
    for name, value in o10_fixtures(args.tree, args.tag).items():
        path = FIXTURE_DIR / name
        if not path.exists() or path.read_text() != dumps(value):
            failed.append(name)
            print(f"l3-census: {path} does not regenerate from {args.tag}", file=sys.stderr)
        else:
            print(f"l3-census: {name} regenerates from {args.tag}")
    return 1 if failed else 0


def o10_calltree(args):
    sys.stdout.write(dumps(calltree_scan(args.tree, args.tag)))
    return 0


def o10_claim(args):
    """`claim <m>` over the reports: the manifest stands over the fixtures, then every row's and
    case's status and the verdict. Exit 0 iff complete (with --partial: iff no problems), 2 iff
    incomplete only by DEC exemptions, else 1."""
    values, texts = claim_inputs(args.tag)
    manifest = load_fixture(f"{CLAIMS_DIR}/{args.milestone}.json")
    if problems := manifest_problems(manifest, values["inventory"], values["tree"], texts):
        print(f"l3-census: claims/{args.milestone}.json fails:", *problems, sep="\n  ", file=sys.stderr)
        return 1
    reports = {}
    for path in args.reports:
        with open(path) as f:
            reports[path] = json.load(f)
    problems, lines, verdict = claim(manifest, values["inventory"], reports, head=args.head)
    print(*lines, sep="\n")
    print(f"l3-census: claim {args.milestone}: {verdict}")
    if problems:
        print(f"l3-census: claim {args.milestone} fails:", *problems, sep="\n  ", file=sys.stderr)
        return 1
    if args.partial or verdict.startswith("complete"):
        return 0
    return 2 if verdict.startswith("incomplete-with-exemption") else 1


# --- Tag moves (OD-L3-37, OD-L3-38): the inventory carried, citations remapped ------------------
#
# A Core tag move (rc.4 -> rc.5, then the final) moves lines; occurrences are kept or added. Both
# commands map each file's lines through the diff of its two blobs and take a mapped line only
# where its text is the old line's (whitespace-trimmed); anything else fails, listed, for a hand
# decision. Neither ever matches by key or line number: a line-keyed F row's key can name
# different rows at the two tags (AcceptBlock@F4850 at rc.4 and rc.5).

# Citation prefixes (the O10 plan's and the a10 plan's): prefix -> Core path.
CITE_PATHS = {
    "v": "src/validation.cpp",
    "vh": "src/validation.h",
    "tc": "src/consensus/tx_check.cpp",
    "tv": "src/consensus/tx_verify.cpp",
    "st": "src/evo/specialtxman.cpp",
    "mi": "src/rpc/mining.cpp",
    "mp": "src/rpc/mempool.cpp",
    "cp": "src/chainparams.cpp",
    "bp": "src/llmq/blockprocessor.cpp",
    "cm": "src/llmq/commitment.cpp",
    "cb": "src/evo/cbtx.cpp",
    "mh": "src/evo/mnhftx.cpp",
    "al": "src/evo/assetlocktx.cpp",
    "pt": "src/evo/providertx.cpp",
    "in": "src/init.cpp",
    "vb": "src/versionbits.cpp",
    "pay": "src/masternode/payments.cpp",
}
CITE = re.compile(
    r"(?<![\w.-])(" + "|".join(sorted(CITE_PATHS, key=len, reverse=True)) + r"):(\d+)(?:-(\d+))?(?![\w])"
)
# The long form (OD-L3-72 m4): a Core file by name or path (`<file>.cpp:N-M`,
# `rpc/<file>.cpp:N`), resolved through `core_path` or the tag's tree.
LONG_CITE = re.compile(r"(?<![\w./-])((?:[a-z_0-9]+/)*[a-z_0-9]+\.(?:cpp|h)):(\d+)(?:-(\d+))?(?![\w])")
# A bare continuation (`:381`, `:1041-1062`) after a citation on the same line names that file.
CONTINUATION = re.compile(r"(?<=[\s(,/]):(\d+)(?:-(\d+))?(?![\w])")
CITE_FORMS = ("all", "long")
COMMENT_MARKERS = ("//!", "///", "//", "#")
# An explicit exclusion (spelled apart here so this comment holds no marker): a line holding
# "remap-cites" + ": skip", or the lines from one holding its "-begin" form through one holding
# its "-end" form, keep their cite-shaped text as written (test literals, not citations).
SKIP_MARK = re.compile("remap-cites" + r": skip(?:-(begin|end))?(?![\w-])")


class LineMap:
    """Old line -> new line of one file across a tag move: the diff's equal runs, each mapped line
    checked to hold the old line's text."""

    def __init__(self, old, new):
        self.old, self.new = old.splitlines(), new.splitlines()
        self.to = {}
        if self.old == self.new:
            self.to = {n: n for n in range(1, len(self.old) + 1)}
            return
        matcher = difflib.SequenceMatcher(None, self.old, self.new, autojunk=False)
        for tag, i1, i2, j1, _ in matcher.get_opcodes():
            if tag == "equal":
                self.to.update({i1 + k + 1: j1 + k + 1 for k in range(i2 - i1)})

    def line(self, n):
        """Old line `n`'s new line, or None where the move changed or removed it."""
        m = self.to.get(n)
        if m is None or self.old[n - 1].strip() != self.new[m - 1].strip():
            return None
        return m

    def span(self, a, b):
        """Old lines a..b as new lines, or None unless every line maps and the run stays whole."""
        new = [self.line(n) for n in range(a, b + 1)]
        if None in new or new != list(range(new[0], new[0] + len(new))):
            return None
        return new[0], new[-1]


def names_tag(tag):
    """A pattern for text that names `tag` itself ("at rc.4", "rc.4's", "v24.0.0-rc.4"), not a
    file named after it (`…-rc.4.json`)."""
    short = tag.split("-", 1)[1] if "-" in tag else tag.lstrip("v")
    return re.compile(r"(?<![\w.])(?:v?\d+\.\d+\.\d+-)?" + re.escape(short) + r"(?!\w|\.\w)")


def paragraphs(lines):
    """Runs of consecutive comment lines with the same marker; every other line alone."""
    out, i = [], 0
    while i < len(lines):
        marker = next((m for m in COMMENT_MARKERS if lines[i].lstrip().startswith(m)), None)
        j = i + 1
        while marker and j < len(lines) and next(
            (m for m in COMMENT_MARKERS if lines[j].lstrip().startswith(m)), None
        ) == marker:
            j += 1
        out.append((i, j))
        i = j
    return out


def cites(line, forms="all"):
    """The citations on `line` in order, as (span, file key, first, last or None): short and
    long forms, and each bare continuation with the file of the citation before it. `forms`
    "long" leaves short citations and their continuations out."""
    found = sorted([(m.span(), m.group(1), m) for m in CITE.finditer(line)]
                   + [(m.span(), m.group(1), m) for m in LONG_CITE.finditer(line)]
                   + [(m.span(), None, m) for m in CONTINUATION.finditer(line)], key=lambda f: f[0])
    out, file, short, end = [], None, False, 0
    for span, key, m in found:
        if span[0] < end:
            continue
        end = span[1]
        if key is None:
            if file is None:
                continue
            first, last = m.group(1), m.group(2)
        else:
            file, short = key, key in CITE_PATHS
            first, last = m.group(2), m.group(3)
        if forms == "long" and short:
            continue
        out.append((span, file, int(first), int(last) if last else None))
    return out


def remap_text(text, line_map, keep, hand=(), forms="all"):
    """`text` with every citation (`cites`, of `forms`) mapped through `line_map(file key)` (a
    LineMap); a paragraph that names the old tag (`keep`, a pattern or a list of them) keeps its
    citations as written. A `hand` decision ({anchor, from,
    to, text}: the cite `from` on a line holding `anchor` becomes `to`, whose line at the new tag
    must read `text`) wins over both and counts its uses in `hand_uses`. -> (text, moved, kept,
    failed), each listing (line, old cite, new cite or reason)."""
    keeps = keep if isinstance(keep, (list, tuple)) else [keep]
    lines = text.split("\n")
    moved, kept, failed = [], [], []
    skipped, begin = set(), None
    for k, line in enumerate(lines):
        if not (mark := SKIP_MARK.search(line)):
            continue
        if mark.group(1) == "begin":
            if begin is not None:
                failed.append((k + 1, mark.group(0), f"a skip-begin inside the one at line {begin + 1}"))
            begin = k
        elif mark.group(1) == "end":
            if begin is None:
                failed.append((k + 1, mark.group(0), "a skip-end with no skip-begin"))
            else:
                skipped.update(range(begin, k + 1))
            begin = None
        else:
            skipped.add(k)
    if begin is not None:
        failed.append((begin + 1, "skip-begin", "a skip-begin with no skip-end"))
    for a, b in paragraphs(lines):
        stays = any(p.search(lines[k]) for p in keeps for k in range(a, b))
        for k in range(a, b):

            def one(found, k=k):
                span, file, first, last = found
                cite = lines[k][span[0]:span[1]]
                if k in skipped:
                    kept.append((k + 1, cite, "skip marker"))
                    return cite
                decided = [h for h in hand if h["from"] == cite and h["anchor"] in lines[k]]
                if decided:
                    h = decided[0]
                    h["uses"] = h.get("uses", 0) + 1
                    prefix, _, number = h["to"].partition(":")
                    new = line_map(prefix).new
                    reads = number.isdigit() and 0 < int(number) <= len(new) and new[int(number) - 1].strip()
                    if reads != h["text"]:
                        failed.append((k + 1, cite, f"the hand decision's line {h['to']} does not read {h['text']!r}"))
                        return cite
                    moved.append((k + 1, cite, f"{h['to']} (hand)"))
                    return h["to"]
                if stays:
                    kept.append((k + 1, cite, "names a kept tag"))
                    return cite
                try:
                    lm = line_map(file)
                except ScanError as e:
                    failed.append((k + 1, cite, str(e)))
                    return cite
                end = last if last is not None else first
                new = lm.span(first, end) if end >= first else None
                if new is None:
                    failed.append((k + 1, cite, "the line changed or was removed at the new tag"))
                    return cite
                head = cite[: cite.index(":") + 1]
                out = f"{head}{new[0]}" + (f"-{new[1]}" if last is not None else "")
                if out != cite:
                    moved.append((k + 1, cite, out))
                return out

            pieces, at = [], 0
            for found in cites(lines[k], forms):
                pieces += [lines[k][at:found[0][0]], one(found)]
                at = found[0][1]
            lines[k] = "".join(pieces) + lines[k][at:]
    return "\n".join(lines), moved, kept, failed


def line_maps(tree, old_tag, new_tag):
    """prefix, Core path or long-form file (`chainparams.cpp`, `rpc/mining.cpp`) -> its LineMap
    across the move, read lazily with git show. A long-form file is the Core path ending in it:
    `src/<file>` where the old tag has it, else the one tracked path that does, else a ScanError."""
    cache, files = {}, []

    def resolve(key):
        if key in CITE_PATHS:
            return CITE_PATHS[key]
        if key.startswith("src/"):
            return key
        if not files:
            files.extend(git(tree, "ls-tree", "-r", "--name-only", old_tag, "src").splitlines())
        if f"src/{key}" in files:
            return f"src/{key}"
        ends = [f for f in files if f.endswith("/" + key)]
        if len(ends) != 1:
            raise ScanError(f"no single Core path for {key} at {old_tag}: {ends}")
        return ends[0]

    def get(key):
        path = resolve(key)
        if path not in cache:
            cache[path] = LineMap(git(tree, "show", f"{old_tag}:{path}"), git(tree, "show", f"{new_tag}:{path}"))
        return cache[path]

    return get


def core_path(file):
    """A fixture row's file (`validation.cpp`) as its Core path."""
    paths = [p for p in [*REQUIRED, *CITE_PATHS.values()] if p.rsplit("/", 1)[-1] == file]
    if len(set(paths)) != 1:
        raise ScanError(f"no single Core path for {file}: {sorted(set(paths))}")
    return paths[0]


def row_identity(row):
    """What `carry` matches a scanned row by: never its key or line."""
    return (row["file"], row["function"], row["sha"], row.get("template"))


def annotation_fields(row):
    return [k for k in row if k not in SCANNED]


def carry(old, scan_new, line_map, annotations):
    """The inventory `old` carried onto `scan_new` (OD-L3-37). Scanned rows match by (file,
    function, sha, template); a group of equal identities pairs only through the line map; new
    rows take their annotations from `annotations` ({"rows": [{key, sha, ...}]}) only. Unscanned
    rows keep their annotations, their lines mapped. Every citation in an annotation string is
    remapped (a string naming the old tag keeps its own). -> (inventory, report, problems)."""
    old_tag, new_tag = old["provenance"]["tag"], scan_new["provenance"]["tag"]
    keep = names_tag(old_tag)
    problems = []
    report = {"from": old_tag, "to": new_tag, "unchanged": 0, "shifted": 0, "rekeyed": [], "new": [],
              "unscanned": [], "cites": []}

    def remap_annotations(row, key):
        for field, value in list(row.items()):
            if field in SCANNED or not isinstance(value, str):
                continue
            text, moved, _, failed = remap_text(value, line_map, keep)
            row[field] = text
            report["cites"] += [{"key": key, "field": field, "from": a, "to": b} for _, a, b in moved]
            problems.extend(f"{key} {field}: {cite}: {why}" for _, cite, why in failed)

    olds = [r for r in old["rows"] if r.get("scanned", True)]
    news = scan_new["rows"]
    groups = {}
    for side, rows in (("old", olds), ("new", news)):
        for r in rows:
            groups.setdefault(row_identity(r), {"old": [], "new": []})[side].append(r)
    pair = {}  # id(old row) -> new row
    for ident, g in groups.items():
        if len(g["old"]) == 1 and len(g["new"]) == 1:
            pair[id(g["old"][0])] = g["new"][0]
            continue
        by_line = {r["line"]: r for r in g["new"]}
        for r in g["old"]:
            target = by_line.get(line_map(core_path(r["file"])).line(r["line"]))
            if target is None:
                what = "ambiguous" if len(g["old"]) + len(g["new"]) > 2 else "unmatched"
                problems.append(f"{what}: {r['key']} (sha {r['sha']}) has no counterpart at {new_tag}")
            elif target in pair.values():
                problems.append(f"ambiguous: {r['key']} pairs with {target['key']}, already taken")
            else:
                pair[id(r)] = target
    paired_new = {id(r) for r in pair.values()}

    given = {(a["key"], a["sha"]): a for a in annotations.get("rows", [])}
    used = set()
    shapes = {}
    for r in olds:
        shapes.setdefault(r["kind"], []).append(r)

    def new_row(n):
        entry = given.get((n["key"], n["sha"]))
        if entry is None:
            problems.append(f"new row {n['key']} (sha {n['sha']}) at {new_tag} has no annotation")
            return None
        used.add((n["key"], n["sha"]))
        fields = [k for k in entry if k not in ("key", "sha")]
        like = next((r for r in shapes.get(n["kind"], []) if sorted(annotation_fields(r)) == sorted(fields)), None)
        if like is None:
            problems.append(f"new row {n['key']}: annotation fields {sorted(fields)} match no {n['kind']} row's")
            return None
        row = {k: (n[k] if k in SCANNED else entry[k]) for k in like if k in n or k in entry}
        report["new"].append(n["key"])
        return row

    out = []
    placed = set()
    for r in old["rows"]:
        if not r.get("scanned", True):
            row = dict(r)
            if "line" in r:
                file, _, rest = r["key"].partition(":")
                n = line_map(core_path(file)).line(r["line"])
                if n is None:
                    problems.append(f"unscanned {r['key']}: line {r['line']} changed at {new_tag}")
                    n = r["line"]
                row["line"] = n
                if re.search(rf"@F{r['line']}$", rest):
                    row["key"] = re.sub(rf"@F{r['line']}$", f"@F{n}", r["key"])
                if n != r["line"]:
                    report["unscanned"].append({"from": r["key"], "to": row["key"], "line": [r["line"], n]})
            remap_annotations(row, row["key"])
            out.append(row)
            continue
        n = pair.get(id(r))
        if n is None:
            continue
        row = {k: (n.get(k) if k in SCANNED else v) for k, v in r.items()}
        row.update({k: n[k] for k in SCANNED if k in n and k not in row})
        if scanned(row) == scanned(r):
            report["unchanged"] += 1
        elif row["key"] == r["key"]:
            report["shifted"] += 1
        else:
            report["rekeyed"].append({"from": r["key"], "to": row["key"], "sha": r["sha"]})
        remap_annotations(row, row["key"])
        out.append(row)
        placed.add(id(n))
        # The new rows the scan puts right after this one.
        i = news.index(n) + 1
        while i < len(news) and id(news[i]) not in paired_new:
            if (added := new_row(news[i])) is not None:
                out.append(added)
            placed.add(id(news[i]))
            i += 1
    for n in news:
        if id(n) not in placed and id(n) not in paired_new:
            if (added := new_row(n)) is not None:
                out.insert(0, added)
    for key, sha in sorted(given.keys() - used):
        problems.append(f"annotation for {key} (sha {sha}) matches no new row")

    prov = dict(old["provenance"])
    prov.update({k: scan_new["provenance"][k] for k in ("tag", "commit", "blobs", "scanner_version")})
    if new_tag != old_tag and "note" in prov:
        prov["note"] += f" Carried from {old_tag} by l3-census.py carry (OD-L3-37)."
    inventory = {"provenance": prov, "script_errors": scan_new["script_errors"], "rows": out}
    keys = [r["key"] for r in out]
    problems += [f"duplicate key {k} after the carry" for k in sorted({k for k in keys if keys.count(k) > 1})]
    if not problems:
        diff, _, _ = check(inventory, scan_new)
        problems += [f"the carried inventory fails check: {p}" for p in diff]
    return inventory, report, problems


def tag_carry(args):
    with open(args.inventory) as f:
        old = json.load(f)
    annotations = {"rows": []}
    if args.annotations:
        with open(args.annotations) as f:
            annotations = json.load(f)
    new = scan(args.tree, args.tag)
    inventory, report, problems = carry(old, new, line_maps(args.tree, old["provenance"]["tag"], args.tag),
                                        annotations)
    if args.report:
        with open(args.report, "w") as f:
            f.write(dumps(report))
    if problems:
        print(f"l3-census: carry {old['provenance']['tag']} -> {args.tag} fails:", *problems, sep="\n  ",
              file=sys.stderr)
        return 1
    sys.stdout.write(dumps(inventory))
    print(f"l3-census: carry {old['provenance']['tag']} -> {args.tag}: {report['unchanged']} unchanged, "
          f"{report['shifted']} shifted, {len(report['rekeyed'])} re-keyed, {len(report['new'])} new, "
          f"{len(report['unscanned'])} unscanned moved, {len(report['cites'])} cites", file=sys.stderr)
    return 0


def tag_remap_cites(args):
    """remap-cites: every citation in the files, old tag -> new tag; nothing written unless every
    citation maps (and `--write`)."""
    get = line_maps(args.tree, args.from_tag, args.to_tag)
    # A second pass over one move (`--forms long` after the short forms moved) also keeps what
    # names the new tag: it was written there.
    keep = [names_tag(args.from_tag)] + ([names_tag(args.to_tag)] if args.forms != "all" else [])
    # `--keep none` (BL-672 N2): an identity pass checks every cite; skip markers alone exempt.
    if args.keep == "none":
        keep = []
    hand = []
    if args.hand:
        with open(args.hand) as f:
            hand = json.load(f)["rows"]
    texts, failed_any = {}, False
    totals = [0, 0]
    for path in args.paths:
        with open(path) as f:
            text = f.read()
        new, moved, kept, failed = remap_text(text, get, keep, [h for h in hand if h["path"] == path], args.forms)
        for line, a, b in moved:
            print(f"{path}:{line}: {a} -> {b}")
        for line, a, why in kept:
            print(f"{path}:{line}: {a} kept ({why})")
        for line, a, why in failed:
            print(f"{path}:{line}: {a}: {why}", file=sys.stderr)
        failed_any |= bool(failed)
        totals[0] += len(moved)
        totals[1] += len(kept)
        if new != text:
            texts[path] = new
    for h in hand:
        if h.get("uses", 0) != 1:
            print(f"{h['path']}: hand decision {h['from']} on {h['anchor']!r}: {h.get('uses', 0)} uses, not 1",
                  file=sys.stderr)
            failed_any = True
    print(f"l3-census: remap-cites {args.from_tag} -> {args.to_tag}: {totals[0]} moved, {totals[1]} kept, "
          f"{len(texts)} files change", file=sys.stderr)
    if failed_any:
        print("l3-census: remap-cites fails: the citations above need a hand decision", file=sys.stderr)
        return 1
    if args.write:
        for path, text in texts.items():
            with open(path, "w") as f:
                f.write(text)
    return 0


TAG_COMMANDS = {"carry": tag_carry, "remap-cites": tag_remap_cites}

# The tag-move selftest's two tags of one synthetic file: rc.2 inserts one R row before
# `bad-size` (its ordinal 3 -> 4), so every later line moves by one and WriteA's F row takes
# WriteB's old key (the F4850 shape), and appends a comment to the second `bad-prevblk` line in
# the ambiguous variant.
SELFTEST_MOVE_OLD = """
bool Twin(const CBlock& block, BlockValidationState& state, const CBlockIndex* prev)
{
    if (prev == nullptr) return state.Invalid(BlockValidationResult::BLOCK_INVALID_PREV, "bad-prevblk");
    if (prev->nStatus & BLOCK_FAILED_MASK) {
        return state.Invalid(BlockValidationResult::BLOCK_INVALID_PREV, "bad-prevblk");
    }
    if (block.vtx.size() > 9) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-size");
    if (!WriteA(block)) return AbortNode(state, "a failed");
    if (!WriteB(block)) return AbortNode(state, "b failed");
    if (!Other(block)) return false;
    return true;
}
"""
SELFTEST_MOVE_ADDED = (
    '    if (block.nTime == 0) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-new");\n'
)


# remap-cites: skip-begin (the tag-move selftest's cite-shaped literals are test data)
def selftest_tag_move(expect):
    old_src = SELFTEST_MOVE_OLD
    size_line = '    if (block.vtx.size() > 9)'
    new_src = old_src.replace(size_line, SELFTEST_MOVE_ADDED + size_line)

    def scan_of(source, tag):
        got = selftest_scan(source)
        got["provenance"] = {**got["provenance"], "tag": tag}
        return got

    old_scan, new_scan = scan_of(old_src, "v9.0.0-rc.1"), scan_of(new_src, "v9.0.0-rc.2")
    by = {r["key"]: r for r in old_scan["rows"]}
    f_a, f_b = "validation.cpp:Twin@F9", "validation.cpp:Twin@F10"
    expect("tag move: the F4850 shape (WriteB's key is WriteA's at the new tag)",
           {r["key"] for r in new_scan["rows"]} >= {"validation.cpp:Twin@F10", "validation.cpp:Twin@F11"}
           and by[f_a]["sha"] != by[f_b]["sha"])
    old = json.loads(json.dumps(old_scan))
    old["provenance"]["note"] = "a note"
    for r in old["rows"]:
        r["decision"] = "C"
        r["note"] = f"was {r['key']}"
    old["rows"][0]["note"] = "v:9 here; at rc.1 v:9 stays"
    old["rows"] += [
        {"key": "validation.cpp:Twin@F11", "kind": "F", "line": 11, "decision": "F", "note": "Other, v:11",
         "scanned": False},
        {"key": "validation.cpp:Twin@E:Other", "kind": "E", "line": 11, "scanned": False},
    ]
    maps = {"src/validation.cpp": LineMap(old_src, new_src)}

    def line_map(key):
        return maps[CITE_PATHS.get(key, key)]

    added = next(r for r in new_scan["rows"] if r.get("template") == "bad-new")
    note = {"key": added["key"], "sha": added["sha"], "decision": "C", "note": "new at rc.2"}
    inv, report, problems = carry(old, new_scan, line_map, {"rows": [note]})
    rows = {r["key"]: r for r in inv["rows"]}
    expect("tag move: carry passes with the new row annotated", problems == [])
    expect("tag move: F rows follow their sha, not their key",
           rows.get("validation.cpp:Twin@F10", {}).get("note") == f"was {f_a}"
           and rows.get("validation.cpp:Twin@F11", {}).get("note") == f"was {f_b}")
    expect("tag move: the renumbered R row keeps its annotations",
           rows.get("validation.cpp:Twin#4:bad-size", {}).get("note") == "was validation.cpp:Twin#3:bad-size"
           and {"from": "validation.cpp:Twin#3:bad-size", "to": "validation.cpp:Twin#4:bad-size",
                "sha": by["validation.cpp:Twin#3:bad-size"]["sha"]} in report["rekeyed"])
    expect("tag move: the equal-sha twins pair through the line map",
           rows.get("validation.cpp:Twin#2:bad-prevblk", {}).get("note") == "was validation.cpp:Twin#2:bad-prevblk")
    expect("tag move: the new row sits after its scan predecessor with its given annotation",
           [r["key"] for r in inv["rows"]].index(added["key"])
           == [r["key"] for r in inv["rows"]].index("validation.cpp:Twin#2:bad-prevblk") + 1
           and rows[added["key"]]["note"] == "new at rc.2")
    expect("tag move: unscanned rows move, a line-keyed one re-keyed",
           rows.get("validation.cpp:Twin@F12", {}).get("line") == 12
           and rows.get("validation.cpp:Twin@E:Other", {}).get("line") == 12)
    expect("tag move: annotation cites move; a string naming the old tag keeps its own",
           rows["validation.cpp:Twin#1:bad-prevblk"]["note"] == "v:9 here; at rc.1 v:9 stays"
           and rows["validation.cpp:Twin@F12"]["note"] == "Other, v:12")
    expect("tag move: the provenance is the new scan's", inv["provenance"]["tag"] == "v9.0.0-rc.2"
           and inv["provenance"]["note"].endswith("(OD-L3-37)."))

    same, _, none = carry(old, scan_of(old_src, "v9.0.0-rc.1"), lambda k: LineMap(old_src, old_src), {"rows": []})
    expect("tag move: carrying onto the same tag reproduces the inventory", none == [] and dumps(same) == dumps(old))
    _, _, problems = carry(old, new_scan, line_map, {"rows": []})
    expect("tag move: an unannotated new row fails",
           problems == [f"new row {added['key']} (sha {added['sha']}) at v9.0.0-rc.2 has no annotation"])
    stray = {"key": "validation.cpp:Twin#9:bad-none", "sha": "0" * 16, "decision": "C", "note": ""}
    _, _, problems = carry(old, new_scan, line_map, {"rows": [note, stray]})
    expect("tag move: an annotation for no new row fails", any("matches no new row" in p for p in problems))
    _, _, problems = carry(old, new_scan, line_map, {"rows": [{**note, "decision": "C", "note": "", "x": 1}]})
    expect("tag move: annotation fields unlike the kind's fail", any("match no R row's" in p for p in problems))
    gone_src = new_src.replace("    if (!WriteB(block)) return AbortNode(state, \"b failed\");\n", "")
    _, _, problems = carry(old, scan_of(gone_src, "v9.0.0-rc.2"),
                           lambda k: LineMap(old_src, gone_src), {"rows": [note]})
    expect("tag move: an old row gone at the new tag fails", any(p.startswith(f"unmatched: {f_b}") for p in problems))
    twin = '        return state.Invalid(BlockValidationResult::BLOCK_INVALID_PREV, "bad-prevblk");\n'
    moved_src = new_src.replace(twin, twin.replace(";\n", "; // moved\n"))
    _, _, problems = carry(old, scan_of(moved_src, "v9.0.0-rc.2"),
                           lambda k: LineMap(old_src, moved_src), {"rows": [note]})
    expect("tag move: equal-sha rows the line map cannot pair fail as ambiguous",
           any(p.startswith("ambiguous: validation.cpp:Twin#2:bad-prevblk") for p in problems))

    lm = LineMap(old_src, new_src)
    text = ("// The rules are rc.1's (v:9).\n// Still that paragraph: v:9.\nlet x = 1; // v:9-10, tc:3\n"
            "/// v:4 and v:7-9; vh:12 b:02x\n")
    got, moved, kept, failed = remap_text(text, lambda p: lm, names_tag("v9.0.0-rc.1"))
    expect("remap-cites: a paragraph naming the old tag keeps its cites", len(kept) == 2
           and got.startswith("// The rules are rc.1's (v:9).\n// Still that paragraph: v:9.\n"))
    expect("remap-cites: cites and ranges move; an unchanged line keeps its number",
           "let x = 1; // v:10-11, tc:3" in got and "/// v:4 and" in got)
    expect("remap-cites: a range across a changed line fails; non-cites are left alone",
           [(c, w) for _, c, w in failed] == [("v:7-9", "the line changed or was removed at the new tag")]
           and "b:02x" in got and "vh:13" in got)
    changed = new_src.replace('"a failed"', '"a failed!"')
    _, _, _, failed = remap_text("v:9", lambda p: LineMap(old_src, changed), names_tag("v9.0.0-rc.1"))
    expect("remap-cites: a cite whose line changed fails", [c for _, c, _ in failed] == ["v:9"])
    size = {"anchor": "size", "from": "v:7-9", "to": "v:9", "text": "not that line"}
    size_line = 'if (block.vtx.size() > 9) return state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, "bad-size");'
    hand = [{**size, "text": size_line}]
    rc1 = names_tag("v9.0.0-rc.1")
    got, moved, _, failed = remap_text("// size: v:7-9, v:7-9 elsewhere\n", lambda p: lm, rc1, hand)
    expect("remap-cites: a hand decision wins on its anchored line, its new line's text checked",
           got.startswith("// size: v:9, v:9 elsewhere") and failed == [] and hand[0]["uses"] == 2)
    _, _, _, failed = remap_text("// size: v:7-9\n", lambda p: lm, rc1, [dict(size)])
    expect("remap-cites: a hand decision whose line reads otherwise fails",
           [w for _, _, w in failed] == [f"the hand decision's line v:9 does not read {size['text']!r}"])
    skip = "remap-cites: " + "skip"
    marked = f"a = 'v:9'  # {skip}\n# {skip}-begin\nb = 'v:9-10'\n# {skip}-end\nc = 'v:9'\n"
    got, moved, kept, failed = remap_text(marked, lambda p: lm, rc1)
    expect("remap-cites: marked lines keep their cites across a shift; the rest move",
           got == marked.replace("c = 'v:9'", "c = 'v:10'") and failed == []
           and [w for _, _, w in kept] == ["skip marker"] * 2)
    for broken, why in ((f"# {skip}-begin\nv:9\n", "a skip-begin with no skip-end"),
                        (f"v:9\n# {skip}-end\n", "a skip-end with no skip-begin"),
                        (f"# {skip}-begin\n# {skip}-begin\n# {skip}-end\n", "a skip-begin inside the one at line 1")):
        expect(f"remap-cites: {why} fails", [w for _, _, w in remap_text(broken, lambda p: lm, rc1)[3]] == [why])
    expect("remap-cites: a file named after the tag does not keep a cite",
           not names_tag("v24.0.0-rc.4").search("planes-v24.0.0-rc.4.json")
           and names_tag("v24.0.0-rc.4").search("at rc.4, v:1") is not None)
    # OD-L3-72 m4: long forms and continuations.
    mixed = "// validation.cpp:9, :4 and :9-10 (src/validation.cpp:9); v:9 :9\nlet at = 1; // :9\n"
    got, moved, _, failed = remap_text(mixed, lambda p: lm, rc1)
    expect("remap-cites: long forms and their continuations move; a bare number with no cite before it on its line stays",
           got == "// validation.cpp:10, :4 and :10-11 (src/validation.cpp:10); v:10 :10\nlet at = 1; // :9\n"
           and failed == [])
    got, moved, _, failed = remap_text(mixed, lambda p: lm, rc1, forms="long")
    expect("remap-cites: --forms long leaves short cites and their continuations",
           got == "// validation.cpp:10, :4 and :10-11 (src/validation.cpp:10); v:9 :9\nlet at = 1; // :9\n"
           and [c for _, c, _ in moved] == ["validation.cpp:9", ":9-10", "src/validation.cpp:9"])
    got, _, kept, _ = remap_text("// at rc.2: validation.cpp:10\n", lambda p: lm, [rc1, names_tag("v9.0.0-rc.2")],
                                 forms="long")
    expect("remap-cites: a paragraph naming either tag keeps its long cites",
           got == "// at rc.2: validation.cpp:10\n" and len(kept) == 1)

    eof = "// at rc.1: v:999\n"
    _, _, kept, failed = remap_text(eof, lambda p: lm, rc1)
    _, _, kept_none, failed_none = remap_text(eof, lambda p: lm, [])
    expect("remap-cites: keeping nothing maps a kept paragraph's cite, which fails past the end",
           len(kept) == 1 and failed == [] and kept_none == []
           and [c for _, c, _ in failed_none] == ["v:999"])

    def unknown(key):
        raise ScanError(f"no single Core path for {key}")

    _, _, _, failed = remap_text("// nowhere.cpp:3, :4\n", unknown, rc1)
    expect("remap-cites: a long form naming no single Core file fails, its continuation too",
           [(c, w) for _, c, w in failed] == [("nowhere.cpp:3", "no single Core path for nowhere.cpp"),
                                               (":4", "no single Core path for nowhere.cpp")])
# remap-cites: skip-end


O10_COMMANDS = {"tree": o10_tree, "calltree": o10_calltree, "o10-check": o10_check, "claim": o10_claim}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command")
    s = sub.add_parser("scan")
    s.add_argument("--tree", required=True)
    s.add_argument("--tag", required=True)
    c = sub.add_parser("check")
    c.add_argument("--tree", required=True)
    c.add_argument("--fixture", required=True)
    modes = c.add_mutually_exclusive_group()
    modes.add_argument("--partial", nargs="+", metavar="REPORT", help="L3 run reports to give each row's status from")
    modes.add_argument("--require-full", nargs="+", metavar="REPORT",
                       help="the census union (full, warn and av reports): every C row covered")
    c.add_argument("--expect-head", metavar="SHA",
                   help="with --require-full: the rdc head every census report must carry")
    t = sub.add_parser("tree", help="print the whole-tree census (O10 plan §5.1)")
    t.add_argument("--tree", required=True)
    t.add_argument("--tag", required=True)
    ct = sub.add_parser("calltree", help="print the call-tree precedence model (O10 plan §4.2)")
    ct.add_argument("--tree", required=True)
    ct.add_argument("--tag", required=True)
    o = sub.add_parser("o10-check", help="regenerate O10's fixtures from the tag; each must equal its file")
    o.add_argument("--tree", required=True)
    o.add_argument("--tag", required=True)
    cl = sub.add_parser("claim", help="a milestone claim over L3 run reports (O10 plan §5.3)")
    cl.add_argument("milestone", choices=["m3", "m4", "m5"])
    cl.add_argument("reports", nargs="*", metavar="REPORT")
    cl.add_argument("--tag", default="v24.0.0")
    cl.add_argument("--head", required=True, help="the exact rdc head every report must carry")
    cl.add_argument("--partial", action="store_true", help="print every status; fail only on problems")
    ca = sub.add_parser("carry", help="the inventory carried onto a new tag (OD-L3-37)")
    ca.add_argument("--tree", required=True)
    ca.add_argument("--inventory", required=True, help="the old tag's inventory fixture")
    ca.add_argument("--tag", required=True, help="the new tag")
    ca.add_argument("--annotations", help="the new rows' annotations: {\"rows\": [{key, sha, ...}]}")
    ca.add_argument("--report", help="where the carry report goes")
    rc = sub.add_parser("remap-cites", help="citations in files moved to a new tag, by content (OD-L3-38)")
    rc.add_argument("--tree", required=True)
    rc.add_argument("--from", dest="from_tag", required=True)
    rc.add_argument("--to", dest="to_tag", required=True)
    rc.add_argument("--write", action="store_true", help="rewrite the files (only if every citation maps)")
    rc.add_argument("--hand", help='hand decisions: {"rows": [{path, anchor, from, to, text, reason}]}, each used once')
    rc.add_argument("--keep", choices=("tags", "none"), default="tags",
                    help="none: keep no paragraph for naming a tag, so every cite outside a skip "
                         "marker is mapped (the identity pass, BL-672 N2)")
    rc.add_argument("--forms", choices=CITE_FORMS, default="all",
                    help="long: only long-form citations (validation.cpp:N) and their continuations (OD-L3-72 m4)")
    rc.add_argument("paths", nargs="+", metavar="PATH")
    parser.add_argument("--selftest", action="store_true", help="run the census self-tests")
    argv = sys.argv[1:]
    if argv[:1] == ["--claim"]:  # the plan's spelling, `--claim m3`
        argv[0] = "claim"
    args = parser.parse_args(argv)
    if args.selftest:
        return selftest()
    if args.command is None:
        parser.error("a command or --selftest")
    if args.command in O10_COMMANDS or args.command in TAG_COMMANDS:
        try:
            return {**O10_COMMANDS, **TAG_COMMANDS}[args.command](args)
        except ScanError as e:
            print(f"l3-census: {e}", file=sys.stderr)
            return 1
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
    paths = args.partial or args.require_full
    if not paths:
        return 0
    mode = "--partial" if args.partial else "--require-full"
    reports = {}
    for path in paths:
        with open(path) as f:
            reports[path] = json.load(f)
    if args.expect_head and not args.require_full:
        print("l3-census: --expect-head goes with --require-full", file=sys.stderr)
        return 2
    if args.partial:
        problems, covered = partial(fixture, reports)
    else:
        problems, covered = require_full(fixture, reports, args.expect_head)
    print(*status_lines(fixture, covered), sep="\n")
    c_rows = [r["key"] for r in fixture["rows"] if r.get("decision") == "C"]
    uncovered = [k for k in c_rows if k not in covered]
    print(
        f"l3-census: {mode}: {len(c_rows) - len(uncovered)} of {len(c_rows)} C rows covered, "
        f"{len(uncovered)} uncovered"
    )
    print(f"l3-census: {mode}: covering trials by class over C rows: {class_summary(fixture, covered)}")
    print(*av_header_ops(reports)[0], sep="\n")
    if problems:
        print(f"l3-census: {mode} fails:", *problems, sep="\n  ", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
