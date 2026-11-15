#!/usr/bin/env python3
"""Judges an S crate's clippy runs for check-shared.sh against layers.toml's
[[l5-exception]] records (SB0 L5; DEC-205).

Usage: l5-exceptions.py <crate> <manifest> <exit status>:<json file>...

Each json file is one `cargo clippy --message-format=json` run of the crate's
library with L5's forbidden lints. An error diagnostic is exempt only when a
record names the crate, the disallowed path, the file and the function, and
the diagnostic is a direct call (`path(`) written in that function's body, not
produced by a macro. The function must be the file's only `fn` of that name,
and its body may declare no item (`fn`, `impl`, `macro_rules`) through which
the call could be reached from elsewhere. Every other error fails. Each record
must cover exactly one call site over all the runs: none means the call it
was granted for is gone (stale), more than one means a second call took cover
under it. A run that failed is accepted only if it linted the crate (one
exempt diagnostic at least) and reported no other error.

A closure in the body can still carry the call out (the real record's call
sits in the future abort_on_panic returns, by design), so a change to an
exempt function's body needs review like the record itself.
"""

import json
import re
import sys
import tomllib
from pathlib import Path

KEYS = {"crate", "path", "file", "function", "decision", "until", "note"}
LINTS = {"clippy::disallowed_methods", "clippy::disallowed_types"}
ITEMS = {"fn", "impl", "macro_rules"}
STRING = re.compile(r'(?:b|c)?(r)(#*)"|(?:b|c)?"')


def records(errors):
    with open("contrib/node/layers.toml", "rb") as f:
        layers = tomllib.load(f)
    crates = layers.get("crates", {})
    out = []
    for i, rec in enumerate(layers.get("l5-exception", [])):
        where = f"layers.toml [[l5-exception]] #{i + 1}"
        if set(rec) != KEYS:
            errors.append(f"{where} must have exactly the keys {', '.join(sorted(KEYS))}")
            continue
        layer = crates.get(rec["crate"])
        if (layer.get("layer") if isinstance(layer, dict) else layer) != "S":
            errors.append(f"{where}: {rec['crate']} is not an S crate")
        if not re.fullmatch(r"DEC-\d+", rec["decision"]) or not re.fullmatch(r"BL-\d+", rec["until"]):
            errors.append(f"{where}: decision must be a DEC row and until a BL row")
        out.append(rec)
    return out


def functions(src):
    """(name, start, end, items) for every `fn` with a body, nested ones too:
    byte offsets of its `fn` keyword and past its closing brace, and whether
    its body declares an item. `src` is the file decoded as Latin-1, so string
    offsets are byte offsets. Raises ValueError on source it cannot follow."""
    out, stack, words, pending, depth, i, n = [], [], [], None, 0, 0, len(src)
    while i < n:
        c = src[i]
        prev = src[i - 1] if i else " "
        if src.startswith("//", i):
            i = n if (j := src.find("\n", i)) < 0 else j
            continue
        if src.startswith("/*", i):
            level, i = 1, i + 2
            while level:
                if i >= n:
                    raise ValueError("unterminated block comment")
                if src.startswith("/*", i):
                    level, i = level + 1, i + 2
                elif src.startswith("*/", i):
                    level, i = level - 1, i + 2
                else:
                    i += 1
            continue
        if not (prev.isalnum() or prev == "_") and (m := STRING.match(src, i)):
            if m.group(1):  # raw: no escapes, ends at `"` and the same number of #
                i = src.index('"' + m.group(2), m.end()) + 1 + len(m.group(2))
            else:
                i = m.end()
                while src[i] != '"':
                    i += 2 if src[i] == "\\" else 1
                i += 1
            continue
        if c == "'":
            if src[i + 1] == "\\":
                i = src.index("'", i + 3) + 1
            elif src[i + 2] == "'":
                i += 3
            elif ord(src[i + 1]) > 127:  # a multi-byte UTF-8 char literal
                i = src.index("'", i + 2) + 1
            else:
                i += 1  # a lifetime or label
            continue
        if (c.isalpha() or c == "_") and not (prev.isalnum() or prev == "_"):
            word = re.match(r"\w+", src[i:i + 64]).group(0)
            if word in ITEMS:
                words.append(i)
            if word == "fn" and (m := re.match(r"fn\s+(\w+)", src[i:i + 128])):
                pending, depth = (m.group(1), i), 0
            i += len(word)
            continue
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif c == ";" and depth <= 0:
            pending = None  # a bodiless declaration
        elif c == "{":
            stack.append((pending, i))
            pending = None
        elif c == "}":
            if not stack:
                raise ValueError(f"unbalanced `}}` at byte {i}")
            (opened, body) = stack.pop()
            if opened:
                out.append((opened[0], opened[1], i + 1, any(body < w < i for w in words)))
        i += 1
    if stack:
        raise ValueError("unclosed `{`")
    return out


def main():
    crate, manifest, runs = sys.argv[1], Path(sys.argv[2]).resolve(), sys.argv[3:]
    errors = []
    mine = [r for r in records(errors) if r["crate"] == crate]
    bodies = {}
    for rec in mine:
        src = manifest.parent / rec["file"]
        text = src.read_bytes().decode("latin-1") if src.is_file() else ""
        try:
            found = [f for f in functions(text) if f[0] == rec["function"]]
        except (ValueError, IndexError) as e:
            errors.append(f"{crate}: l5-exception ({rec['decision']}): cannot read {rec['file']}: {e}")
            found = None
        if found is None:
            found = []
        elif not found:
            errors.append(f"{crate}: stale l5-exception ({rec['decision']}): {rec['file']} has no fn {rec['function']}")
        elif len(found) > 1:
            errors.append(f"{crate}: l5-exception ({rec['decision']}): {rec['file']} has {len(found)} fns named "
                          f"{rec['function']}; the record must name one")
            found = []
        elif found[0][3]:
            errors.append(f"{crate}: l5-exception ({rec['decision']}): fn {rec['function']} declares an item "
                          "(fn, impl or macro_rules) in its body, which could carry the call elsewhere")
            found = []
        bodies[id(rec)] = (src.resolve(), text, found[0][1:3] if found else None)
    sites = {id(rec): set() for rec in mine}
    for run in runs:
        status, _, path = run.partition(":")
        exempt = other = 0
        for line in Path(path).read_text().splitlines():
            msg = json.loads(line) if line.startswith("{") else {}
            diag = msg.get("message") if msg.get("reason") == "compiler-message" else None
            if not diag or not diag["level"].startswith("error") or diag["message"].startswith("aborting due to"):
                continue
            primary = next((s for s in diag["spans"] if s["is_primary"]), None)
            hit = None
            if (primary and primary.get("expansion") is None
                    and (diag.get("code") or {}).get("code") in LINTS):
                file = Path(primary["file_name"]).resolve()
                for rec in mine:
                    src, text, body = bodies[id(rec)]
                    if (body and file == src and f"`{rec['path']}`" in diag["message"]
                            and body[0] <= primary["byte_start"] and primary["byte_end"] <= body[1]
                            and re.match(r"\s*\(", text[primary["byte_end"]:])):
                        hit = rec
                        break
            if hit:
                exempt += 1
                sites[id(hit)].add(primary["byte_start"])
            else:
                other += 1
                print(diag.get("rendered") or diag["message"], file=sys.stderr, end="")
        if other or (status != "0" and not exempt):
            errors.append(f"{crate}: clippy failed{'' if other else ' before linting the crate'} ({run})")
    for rec in mine:
        n = len(sites[id(rec)])
        what = f"`{rec['path']}` in {rec['file']} fn {rec['function']}"
        if n == 0 and bodies[id(rec)][2]:
            errors.append(f"{crate}: stale l5-exception ({rec['decision']}): no direct call to {what} is left; "
                          "delete the record")
        elif n > 1:
            errors.append(f"{crate}: l5-exception ({rec['decision']}) covers one call, but {what} has {n}")
        elif n == 1:
            print(f"{crate}: {what} exempt by {rec['decision']} until {rec['until']}")
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
