#!/usr/bin/env python3
"""FT-MAP: each Dash Core functional test -> PRD story IDs (O11; PRD §0.4, A1 §4.8).

  ft-map.py generate --core ~/workspace/dash --prd PRD.md --out compat/FT-MAP.md
  ft-map.py check    --core ~/workspace/dash --prd PRD.md --out compat/FT-MAP.md

The tests are the pin's test_runner.py scripts (ALL_SCRIPTS), read from the v24.0 pin's branch
(func-compat.py's PINS). A test maps to a story by three sources, each marked in the table:
- `a`: the story's acceptance names the test (or a glob that matches it);
- `r`: a reviewed rule in ft-map-rules.toml (test-family patterns);
- `t`: the story's "Dash Core" column names an RPC the test calls (`.name(` for one of the pin's
  `RPCHelpMan` names) or an option it passes (`-name`), leaving out tokens more than NOISE of
  the tests or more than STORY_NOISE stories use.
`t` is the skeleton's guess and the place to review first; `a` and `r` are reviewed text.

Beside it, the asserted-log extraction (R4 V6): the debug-log fragments each test asserts, read
by contrib/node/asserted-logs.py's resolver at the v24.0 branch and at the v23.1.8 tag (REFS),
and the fragments that differ between them (O17: `ERROR:` -> `[error]`, WAL-28, P2P-25).
"""

import argparse
import ast
import fnmatch
import importlib.util
import re
import sys
import tomllib
from collections import Counter, defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
# A token more tests than this share (getblockcount, -debug) says nothing about a test's subject,
# nor one more stories than STORY_NOISE name (sendrawtransaction).
NOISE = 0.08
STORY_NOISE = 2


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


compat = load_module("func_compat", HERE / "func-compat.py")
asserted_logs = load_module("asserted_logs", HERE.parent / "asserted-logs.py")
# What the asserted-log columns read: the v24.0 pin's branch, and v23.1's release tag for O17's
# per-pin deltas (no v23.1 branch is run: DEC-82, DEC-359).
REFS = {"v24.0": compat.PINS["v24.0"].branch, "v23.1": "v23.1.8"}


# ---- the PRD ---------------------------------------------------------------------------------

def stories(prd_text):
    """§2's story rows -> {id: (title, dash core column, acceptance)}."""
    section = prd_text.split("\n## 2. ", 1)[1].split("\n## 3. ", 1)[0]
    out = {}
    for line in section.splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if re.fullmatch(r"US-[A-Z]+-\d+", cells[0]) and len(cells) >= 7:
            out[cells[0]] = (cells[1], cells[4], cells[6])
    return out


def backticked(text):
    return re.findall(r"`([^`]+)`", text)


def core_tokens(column, rpcs):
    """The RPCs and options a "Dash Core" cell names: `getblockchaininfo.blocks` -> getblockchaininfo."""
    tokens = set()
    for span in backticked(column):
        for word in re.split(r"[\s,/]+", span):
            word = word.split(".", 1)[0].split("=", 1)[0].strip("()")
            if re.fullmatch(r"-[a-z][a-z0-9]+", word) or word in rpcs:
                tokens.add(word)
    return tokens


# ---- the tests -------------------------------------------------------------------------------

def rpc_names(core, ref):
    """Every RPC (and sub-command group) the pin registers: its `RPCHelpMan` names."""
    names = set()
    for path in compat.git(core, "grep", "-l", "RPCHelpMan", ref, "--", "src/*.cpp").split():
        source = compat.git(core, "show", path)  # `ref:src/...`
        names.update(re.findall(r'RPCHelpMan\{\s*"([a-z0-9_]+)"', source))
    return names


def test_tokens(source, rpcs):
    """The RPCs a test calls (`.name(`, not a logger's) and the options it names (`-name`)."""
    calls = set(re.findall(r"(?<!log)\.([a-z][a-z0-9]+)\(", source)) & rpcs
    options = set(re.findall(r"""["'\s](-[a-z][a-z0-9]+)(?=[=\s"'])""", source))
    return calls | options


def mapping(tests, sources, prd, rules, rpcs):
    """{test: {story: set of sources}}; raises on a rule naming an unknown story or no test."""
    out = {t: defaultdict(set) for t in tests}
    for story, (_, _, acceptance) in prd.items():
        for name in backticked(acceptance):
            for t in tests:
                if fnmatch.fnmatchcase(t, name.removesuffix(".py")):
                    out[t][story].add("a")
    for pattern, ids in rules.items():
        unknown = [i for i in ids if i not in prd]
        if unknown:
            raise ValueError(f"rule {pattern!r}: no such story {unknown}")
        hits = [t for t in tests if fnmatch.fnmatchcase(t, pattern)]
        if not hits:
            raise ValueError(f"rule {pattern!r} matches no test at the pin")
        for t in hits:
            for i in ids:
                out[t][i].add("r")
    used = {t: test_tokens(sources[t], rpcs) for t in tests}
    named = {story: core_tokens(column, rpcs) for story, (_, column, _) in prd.items()}
    df = Counter(tok for toks in used.values() for tok in toks)
    sf = Counter(tok for toks in named.values() for tok in toks)
    for story in prd:
        wanted = {tok for tok in named[story] if df[tok] <= NOISE * len(tests) and sf[tok] <= STORY_NOISE}
        for t in tests:
            if wanted & used[t]:
                out[t][story].add("t")
    return out


# ---- asserted-log extraction (R4 V6) ---------------------------------------------------------

def asserted(core, ref):
    """{test stem: {(sense, fragment)}} at `ref`, by asserted-logs.py's resolver."""
    reasons = asserted_logs.templates(asserted_logs.git(core, "show", f"{ref}:{asserted_logs.TEMPLATES}"))
    used, unresolved, out = set(), [], defaultdict(set)
    for path, source in asserted_logs.core_files(core, ref):
        for sense, _, _, text in asserted_logs.fragments(path, source, reasons, used, unresolved):
            if text:
                out[Path(path).stem].add((sense, text))
    if unresolved:
        raise ValueError(f"{ref}: assertions the resolver cannot follow: {unresolved}")
    return out


# ---- the document ----------------------------------------------------------------------------

def phase_of(phases, test):
    for name, phase in phases.items():
        if any(fnmatch.fnmatchcase(test, p) for p in phase["tests"]):
            return name
    return ""


def cell(text):
    return text.replace("|", "\\|").replace("\n", " ")


def render(pins, tests, prd, mapped, logs, present, phases):
    v24, v23 = "v24.0", "v23.1"
    by_story = defaultdict(list)
    for t in tests:
        for s in mapped[t]:
            by_story[s].append(t)
    only_dev = [t for t in tests if set(mapped[t]) == {"US-DEV-07"}]
    unmapped = [s for s in prd if not by_story[s]]
    lines = [
        "# FT-MAP: Dash Core functional tests -> PRD stories",
        "",
        "Generated by rust-dashcore `contrib/node/func-compat/ft-map.py` (O11); do not edit by hand.",
        "Rules: `contrib/node/func-compat/ft-map-rules.toml`. Refs: "
        + ", ".join(f"{p} `{c[:12]}`" for p, c in pins.items()) + ".",
        "",
        f"{len(tests)} tests at {v24} (test_runner.py's scripts); {len(prd)} stories.",
        f"{len(tests) - len(only_dev)} tests map to a story besides US-DEV-07; "
        f"{len(prd) - len(unmapped)} stories have a test.",
        "",
        "Sources: `a` the story's acceptance names the test; `r` a reviewed rule; `t` the story's",
        "Dash Core column names an RPC or option the test uses (the skeleton's guess: review it).",
        f"Asserted logs: the fragments the test asserts in debug.log at {v24} / {v23} (R4 V6).",
        "Phase: A1 Appendix D (`phases.toml`).",
        "",
        "## Tests",
        "",
        "| test | phase | stories | asserted logs |",
        "|---|---|---|---|",
    ]
    for t in tests:
        stories_cell = ", ".join(f"{s} {''.join(sorted(src))}" for s, src in sorted(mapped[t].items()))
        counts = f"{len(logs[v24].get(t, ()))} / {len(logs[v23].get(t, ())) if t in present[v23] else '–'}"
        lines.append(f"| `{t}` | {phase_of(phases, t)} | {stories_cell} | {counts} |")
    lines += ["", "## Stories", "", "| story | tests | title |", "|---|---|---|"]
    for s, (title, _, _) in prd.items():
        names = by_story[s]
        shown = ", ".join(f"`{t}`" for t in names[:12]) + (f" (+{len(names) - 12})" if len(names) > 12 else "")
        lines.append(f"| {s} | {shown or '—'} | {cell(title[:90])} |")
    lines += ["", f"## Asserted-log changes {v23} -> {v24}", "",
              "| test | sense | change | fragment |", "|---|---|---|---|"]
    for t in sorted(set(logs[v24]) | set(logs[v23])):
        new, old = logs[v24].get(t, set()), logs[v23].get(t, set())
        for change, frags in (("added", new - old), ("removed", old - new)):
            for sense, text in sorted(frags):
                lines.append(f"| `{t}` | {sense} | {change} | `{cell(text)}` |")
    return "\n".join(lines) + "\n"


def generate(core, prd_path):
    prd = stories(Path(prd_path).read_text(encoding="utf8"))
    with open(HERE / "ft-map-rules.toml", "rb") as f:
        rules = tomllib.load(f)
    pins, logs, present = {}, {}, {}
    for name, ref in REFS.items():
        pins[name] = compat.git(core, "rev-parse", f"{ref}^{{commit}}").strip()
        logs[name] = asserted(core, ref)
        names = compat.git(core, "ls-tree", "--name-only", f"{ref}:test/functional").split()
        present[name] = {n.removesuffix(".py") for n in names if n.endswith(".py")}
    branch = compat.PINS["v24.0"].branch
    runner = compat.git(core, "show", f"{branch}:test/functional/test_runner.py")
    tests = sorted({compat.stem(s) for s in scripts_in(runner)})
    sources = {t: compat.git(core, "show", f"{branch}:test/functional/{t}.py") for t in tests}
    mapped = mapping(tests, sources, prd, rules, rpc_names(core, branch))
    return render(pins, tests, prd, mapped, logs, present, compat.load_phases())


def scripts_in(runner_source):
    """test_runner.py's ALL_SCRIPTS, read without importing it."""
    names = {}
    for node in ast.parse(runner_source).body:
        if getattr(node, "targets", None) and getattr(node.targets[0], "id", "") in ("BASE_SCRIPTS", "EXTENDED_SCRIPTS"):
            names[node.targets[0].id] = [e.value for e in node.value.elts]
    if set(names) != {"BASE_SCRIPTS", "EXTENDED_SCRIPTS"}:
        raise ValueError("test_runner.py: BASE_SCRIPTS or EXTENDED_SCRIPTS not found")
    return names["EXTENDED_SCRIPTS"] + names["BASE_SCRIPTS"]


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("mode", choices=["generate", "check"])
    p.add_argument("--core", type=Path, default=Path.home() / "workspace/dash")
    p.add_argument("--prd", type=Path, default=Path.home() / "workspace/node-program/PRD.md")
    p.add_argument("--out", type=Path, default=Path.home() / "workspace/node-program/compat/FT-MAP.md")
    a = p.parse_args()
    text = generate(a.core, a.prd)
    if a.mode == "generate":
        a.out.write_text(text, encoding="utf8")
        print(f"{a.out}: {len(text.splitlines())} lines")
        return 0
    if not a.out.exists() or a.out.read_text(encoding="utf8") != text:
        print(f"{a.out} is out of date; run ft-map.py generate", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
