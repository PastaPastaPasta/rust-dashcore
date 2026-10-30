#!/usr/bin/env python3
"""CH3 §7.1's reject census for rdashd's ConnectBlock, DisconnectBlock and TestBlockValidity
(CH3a slices a3, a4, a5, a6).

Reads Dash Core's `ConnectBlock` (`validation.cpp:2469-2935` at v24.0.0-rc.3) and the undo write
it calls (`node/blockstorage.cpp`: `UndoWriteToDisk` :509-538, `FlushUndoFile` :577-583, and
:726-803 less `WriteBlockToDisk` :748-771, which is `AcceptBlock`'s), then `DisconnectBlock`
(`validation.cpp:2282-2378`), the `ApplyTxInUndo` it calls (:2252-2278) and its undo read
(`node/blockstorage.cpp:540-575` `UndoReadFromDisk`), then `TestBlockValidity` (`v:4923-4974`:
its ChainLock reject and the three pre-check `LogError` lines). It lists every `state.Invalid`,
`AbortNode`, `LogError` and `LogPrintf("ERROR: …")` in them, plus the two pre-DIP3 rejects
`NoEvo` makes inside `ProcessSpecialTxsInBlock` (`bad-tx-type-dip3-inactive`, D32;
`bad-cbtx-payload`, D54). Comments are stripped from both sides first, so a reason or a text
that only a comment holds does not count. It fails unless:

- every reject reason is a string literal of the port and of the directed tests;
- every `AbortNode` text is a constant of the port that the tests name;
- every log line's fixed text is in the port (the `%s` fields are free), or, for a line the port
  composes from a prefix and an error's text (`COMPOSED`), each of its pieces is;
- in the disconnect's ranges, which only log, and in `TestBlockValidity`'s, every log line's
  fixed text is also in the tests, or is a constant of the port that the tests name;
- every reject whose reason Core computes is resolved through `COMPUTED`, which names the
  reasons each call can give and the ones a test must hold;
- every exemption in `EXEMPT` still matches a site, so a stale one fails too. An `untested`
  exemption waives only the directed-test check; its site's texts are still checked in the port.
  No exemption applies in `TestBlockValidity`, whose every site is directed-tested (`ALL_TESTED`).

  ch3a-connect-census.py --core <Dash Core tree or its src/>
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PORT = [
    ROOT / "dashd-chainstate/src/connect/serial.rs",
    ROOT / "dashd-chainstate/src/connect/evo.rs",
    ROOT / "dashd-chainstate/src/connect/disconnect.rs",
    ROOT / "dashd-store/src/flatfile/blocks.rs",
    ROOT / "dashd-chainstate/src/connect/connector.rs",
    # CS6's `check_queue_failed` holds `block-validation-failed` (slice a4, CH3a D127).
    ROOT / "dashd-chainstate/src/checkqueue/script.rs",
]
TESTS = [
    ROOT / "dashd-chainstate/src/chain/tests/connector.rs",
    ROOT / "dashd-chainstate/src/chain/tests/connector/disconnect.rs",
    ROOT / "dashd-chainstate/src/chain/tests/connector/proposal.rs",
    ROOT / "dashd-chainstate/src/chain/tests/connector/queue.rs",
]

# (Core file, function, first line, anchor text on it, last line, whether its log lines need a test)
RANGES = [
    ("validation.cpp", "ConnectBlock", 2469, "bool Chainstate::ConnectBlock(", 2935, False),
    ("node/blockstorage.cpp", "UndoWriteToDisk", 509, "static bool UndoWriteToDisk(", 538, False),
    ("node/blockstorage.cpp", "FlushUndoFile", 577, "void BlockManager::FlushUndoFile(", 583, False),
    ("node/blockstorage.cpp", "FindUndoPos", 726, "bool BlockManager::FindUndoPos(", 746, False),
    (
        "node/blockstorage.cpp",
        "WriteUndoDataForBlock",
        773,
        "bool BlockManager::WriteUndoDataForBlock(",
        803,
        False,
    ),
    ("validation.cpp", "ApplyTxInUndo", 2252, "int ApplyTxInUndo(", 2278, True),
    (
        "validation.cpp",
        "DisconnectBlock",
        2282,
        "DisconnectResult Chainstate::DisconnectBlock(",
        2378,
        True,
    ),
    ("node/blockstorage.cpp", "UndoReadFromDisk", 540, "bool UndoReadFromDisk(", 575, True),
    ("validation.cpp", "TestBlockValidity", 4923, "bool TestBlockValidity(", 4974, True),
]

# Functions whose every site is directed-tested: `EXEMPT` does not apply in them.
ALL_TESTED = {"TestBlockValidity"}

# Log lines the port composes: `UndoReadFromDisk: {e}` over ST2's `UndoReadError` (CH3a D104).
COMPOSED = {
    "UndoReadFromDisk: OpenUndoFile failed": ["UndoReadFromDisk: {e}", "OpenUndoFile failed"],
    "UndoReadFromDisk: Deserialize or I/O error - ": [
        "UndoReadFromDisk: {e}",
        "Deserialize or I/O error - {0}",
    ],
    "UndoReadFromDisk: Checksum mismatch": ["UndoReadFromDisk: {e}", "Checksum mismatch"],
}

# Rejects NoEvo makes inside ProcessSpecialTxsInBlock (evo/specialtxman.cpp), below DIP3.
EXTRA_REASONS = ["bad-tx-type-dip3-inactive", "bad-cbtx-payload"]

# A reject whose reason is computed, by the check it follows: the reasons a directed test must
# hold, and the ones it does not with why.
COMPUTED = {
    "CheckTxInputs": (
        [
            "bad-txns-inputs-missingorspent",
            "bad-txns-premature-spend-of-coinbase",
            "bad-txns-inputvalues-outofrange",
            "bad-txns-in-belowout",
        ],
        {
            "bad-txns-fee-outofrange": "unreachable: the fee is at most the inputs, already in "
            "range (consensus/tx_verify.cpp:189-203); CS5b's tests own it",
            "GetAssetUnlockFee": "asset unlocks are special transactions: below DIP3 H4 rejects "
            "them first (bad-tx-type-dip3-inactive); CH7a",
        },
    ),
    "CheckSharedCollateralSpends": (
        [],
        {"*": "hook H8, V24 and complete EvoHooks only (G1 refuses V24 before it): CH7a"},
    ),
    "CheckInputScripts": (["mandatory-script-verify-flag-failed"], {}),
}

# Sites the directed tests do not reach, with why. Each key must match a site's text. A `later`
# site is not a3's port at all; an `untested` one is, and its texts are still checked.
EXEMPT = {
    "Consensus::CheckBlock: %s": (
        "untested",
        "a stored block that passed AcceptBlock's CheckBlock reads back the same or mutated (its "
        "header commits to the merkle root): the mutated branch is tested",
    ),
    "ftell failed": (
        "later",
        "ST2 writes at a position it computed; there is no ftell to fail (CH3a D77)",
    ),
    "no undo data available": (
        "untested",
        "the disconnected block is the tip or its ancestor, whose ConnectBlock wrote its undo "
        "before it joined the chain, and pruning never removes a tip-side block's undo",
    ),
}


def strip_comments(text, name):
    """`text` without its `//` and `/* */` comments, string and character literals kept, and
    every newline kept, so line numbers hold. Serves C++ and Rust (whose `'a` lifetimes are not
    literals). It refuses what it does not parse: a Rust raw string or a nested block comment."""
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
        elif text.startswith("/*", i):
            j = text.find("*/", i + 2)
            j = n if j < 0 else j + 2
            if "/*" in text[i + 2 : j]:
                sys.exit(f"{name}: a nested block comment; the census cannot strip it")
            out.append("\n" * text.count("\n", i, j))
            i = j
        elif re.compile(r'(?<![\w])r#*"').match(text, i) and (i == 0 or not text[i - 1].isalnum()):
            sys.exit(f"{name}: a raw string; the census cannot strip it")
        elif c == '"':
            m = re.compile(r'"(?:[^"\\]|\\.)*"', re.S).match(text, i)
            j = m.end() if m else n
            out.append(text[i:j])
            i = j
        elif c == "'":
            m = re.compile(r"'(?:[^'\\\n]|\\.[^']*)'").match(text, i)
            j = m.end() if m else i + 1
            out.append(text[i:j])
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def statements(lines, first, last):
    """The statements in [first, last] (1-based), joined over lines, with their first line."""
    out, buf, start = [], [], None
    for n in range(first, last + 1):
        text = lines[n - 1].strip()
        if not text:
            continue
        if start is None:
            start = n
        buf.append(text)
        if text.endswith((";", "{", "}")):
            out.append((start, " ".join(buf)))
            buf, start = [], None
    return out


def rust_text(path, port=False):
    """A Rust file without comments, its string continuations (`\\` then a newline) joined; for
    the port, without its unit tests (`#[cfg(test)] mod tests`, the file's last item)."""
    text = path.read_text()
    if port:
        m = re.search(r"^#\[cfg\(test\)\]\s*\nmod tests\b", text, re.M)
        text = text[: m.start()] if m else text
    return re.sub(r"\\\n\s*", "", strip_comments(text, path.name))


def literal_after(text, prefix):
    m = re.search(re.escape(prefix) + r'\s*"((?:[^"\\]|\\.)*)"', text)
    return m.group(1) if m else None


def fixed_parts(fmt, func):
    """The fixed text of a Core format: `__func__`'s `%s` filled in, the other fields dropped."""
    fmt = fmt.replace("\\n", "")
    if func:
        fmt = fmt.replace("%s", func, 1)
    return [p for p in re.split(r"%[sdiu]", fmt) if p.strip()]


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--core", type=Path, required=True)
    args = ap.parse_args()
    src = args.core / "src" if (args.core / "src/validation.cpp").exists() else args.core
    core = {}
    port = "\n".join(rust_text(p, port=True) for p in PORT)
    tests = "\n".join(rust_text(p) for p in TESTS)
    failures, exempt_used, composed_used = [], set(), set()
    counts = {"reject": 0, "abort": 0, "log": 0, "computed": 0}

    def need(cond, msg):
        if not cond:
            failures.append(msg)

    for file, func, first, anchor, last, log_tests in RANGES:
        if file not in core:
            core[file] = strip_comments((src / file).read_text(), file).splitlines()
        lines = core[file]
        if anchor not in lines[first - 1]:
            sys.exit(f"{file}:{first} is not `{anchor}`: not the pinned Core")
        prev_check = None
        for n, text in statements(lines, first, last):
            where = f"v:{n}" if file == "validation.cpp" else f"{file}:{n}"
            m = re.search(r"if \(.*?!\s*(?:Consensus::)?(\w+)\(", text)
            if m:
                prev_check = m.group(1)
            exempt = None if func in ALL_TESTED else next((k for k in EXEMPT if k in text), None)
            if exempt:
                exempt_used.add(exempt)
                if EXEMPT[exempt][0] == "later":
                    continue
            tested = exempt is None
            if "AbortNode(" in text:
                counts["abort"] += 1
                msg = literal_after(text, "AbortNode(state,") or literal_after(text, "AbortNode(")
                if msg is None:
                    call = re.search(r"AbortNode\((?:state, )?(\w+)\(\)\)", text).group(1)
                    body = "\n".join(lines)
                    m = re.search(call + r'\(\)\s*\{.*?\n    return "([^"]*)";', body, re.S)
                    msg = m.group(1)
                const = re.search(r'pub const (\w+): &str =\s*"' + re.escape(msg) + '"', port)
                need(const, f"{where}: AbortNode text not a port constant: {msg!r}")
                if const and tested:
                    need(const.group(1) in tests, f"{where}: no test names {const.group(1)}")
            for log in ("LogError(", "LogPrintf("):
                fmt = literal_after(text, log)
                if fmt is None or (log == "LogPrintf(" and not fmt.startswith("ERROR: ")):
                    continue
                counts["log"] += 1
                func_arg = func if re.search(r'",\s*__func__', text) else None
                for part in fixed_parts(fmt, func_arg):
                    if part in COMPOSED:
                        composed_used.add(part)
                        for piece in COMPOSED[part]:
                            need(piece in port, f"{where}: log text not in the port: {piece!r}")
                    else:
                        need(part in port, f"{where}: log text not in the port: {part!r}")
                    if log_tests and tested:
                        const = re.search(r'pub const (\w+): &str =\s*"' + re.escape(part) + '"', port)
                        named = part in tests or (const and const.group(1) in tests)
                        need(named, f"{where}: no test names the log line {part!r}")
            if "state.Invalid(" in text:
                m = re.search(r'state\.Invalid\(BlockValidationResult::\w+,\s*"([^"]*)"', text)
                reason = m.group(1) if m else None
                if reason is not None:
                    counts["reject"] += 1
                    need(f'"{reason}"' in port, f"{where}: reason not in the port: {reason}")
                    if tested:
                        need(f'"{reason}"' in tests, f"{where}: no directed test for {reason}")
                    continue
                counts["computed"] += 1
                if prev_check not in COMPUTED:
                    failures.append(f"{where}: computed reason after {prev_check}: add to COMPUTED")
                    continue
                for r in COMPUTED[prev_check][0]:
                    need(f'"{r}' in tests, f"{where}: no directed test for {prev_check}'s {r}")
    for reason in EXTRA_REASONS:
        counts["reject"] += 1
        need(f'"{reason}"' in port, f"NoEvo: reason not in the port: {reason}")
        need(f'"{reason}"' in tests, f"NoEvo: no directed test for {reason}")
    for key in EXEMPT:
        need(key in exempt_used, f"stale exemption: {key!r}")
    for key in COMPOSED:
        need(key in composed_used, f"stale composed line: {key!r}")

    for f in failures:
        print(f"ch3a-connect-census: {f}", file=sys.stderr)
    print(
        f"ch3a-connect-census: {counts['reject']} literal rejects, {counts['computed']} computed, "
        f"{counts['abort']} AbortNode, {counts['log']} log lines; {len(EXEMPT)} exempt; "
        f"{len(failures)} failures"
    )
    for check, (_, notes) in COMPUTED.items():
        for k, why in notes.items():
            print(f"  not tested: {check} {k}: {why}")
    for k, (scope, why) in EXEMPT.items():
        print(f"  exempt ({scope}): {k}: {why}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
