#!/usr/bin/env python3
"""Generate the oracle's relic call-log wrappers (BS11a D59, D60).

usage: wrap.py --cc CC [--include DIR]... --symbols FILE --out FILE.c

Every symbol in FILE (one per line) gets __wrap_<symbol>, with the prototype the
compiler sees in relic.h under the given include directories. The wrapper calls
__real_<symbol>; when that call returns to code outside relic (a thread-local depth
counter back at 0), it reports the symbol to dash_oracle_relic_note(), which the
oracle shim defines. The object also defines dash_oracle_relic_wrapped, the number of
wrapped symbols.

Fails when a symbol has no single prototype it can parse; build-oracle.sh compares the
object's __wrap_ symbols with FILE (the drift guard).
"""
import argparse
import re
import subprocess
import sys

KEYWORDS = {
    "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned",
    "_Bool", "const", "volatile", "restrict", "__restrict", "struct", "union", "enum",
}
IDENT = r"[A-Za-z_]\w*"


def preprocess(cc, includes):
    cmd = [cc, "-E", "-P", "-x", "c", "-D_GNU_SOURCE"]
    for inc in includes:
        cmd += ["-I", inc]
    cmd.append("-")
    return subprocess.run(cmd, input='#include "relic.h"\n', capture_output=True, text=True, check=True).stdout


def strip_balanced(text, opener):
    """Remove every `opener(...)`, parentheses balanced (attributes, asm labels)."""
    out, i = [], 0
    while True:
        j = text.find(opener, i)
        if j < 0:
            out.append(text[i:])
            return "".join(out)
        out.append(text[i:j])
        k = text.index("(", j)
        depth = 0
        while True:
            depth += {"(": 1, ")": -1}.get(text[k], 0)
            k += 1
            if depth == 0:
                break
        i = k


def statements(text):
    """Top-level declarations, split at semicolons. A function body ends its
    statement; any other brace body (struct, union, enum) stays as a placeholder."""
    text = strip_balanced(strip_balanced(text, "__attribute__"), "__asm__")
    flat, depth = [], 0
    for ch in text:
        if ch == "{":
            if depth == 0:
                stmt = "".join(flat).rsplit(";", 1)[-1].rstrip()
                flat.append(";" if stmt.endswith(")") else " BODY ")
            depth += 1
        elif ch == "}":
            depth -= 1
        elif depth == 0:
            flat.append(ch)
    for stmt in "".join(flat).split(";"):
        stmt = " ".join(stmt.split())
        if stmt:
            yield stmt


def split_params(params):
    parts, depth, cur = [], 0, []
    for ch in params:
        if ch == "," and depth == 0:
            parts.append("".join(cur).strip())
            cur = []
            continue
        depth += {"(": 1, ")": -1}.get(ch, 0)
        cur.append(ch)
    parts.append("".join(cur).strip())
    return parts


def declared_name(decl):
    """The name a declarator declares: a function pointer's, else the last identifier
    before any array bounds. The match, so that callers can see what precedes it."""
    return re.search(r"\(\s*\*\s*(%s)\s*\)" % IDENT, decl) or re.search(r"(%s)\s*(\[[^\]]*\]\s*)*$" % IDENT, decl)


def param_name(param, types):
    m = declared_name(param)
    if m and m.group().startswith("("):
        return m.group(1)
    if not m or m.group(1) in KEYWORDS or m.group(1) in types or not param[: m.start()].strip():
        return None  # no name: the prototype gives only a type
    return m.group(1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cc", required=True)
    ap.add_argument("--include", action="append", default=[])
    ap.add_argument("--symbols", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    symbols = sorted({s.strip() for s in open(args.symbols) if s.strip()})
    stmts = list(statements(preprocess(args.cc, args.include)))
    types = set()
    for st in stmts:
        if st.startswith("typedef "):
            m = declared_name(st)
            if m:
                types.add(m.group(1))

    errors, wrappers = [], []
    for sym in symbols:
        pattern = re.compile(r"^(?:extern )?(?P<ret>[\w\s\*]*?[\w\*])\s*\b%s\s*\((?P<params>.*)\)$" % re.escape(sym))
        protos = {(m.group("ret").strip(), m.group("params").strip()) for m in map(pattern.match, stmts) if m}
        protos = {p for p in protos if not re.search(r"\b(static|inline|typedef)\b", p[0])}
        if len(protos) != 1:
            errors.append("%s: %d prototypes" % (sym, len(protos)))
            continue
        ret, params = protos.pop()
        if params in ("", "void"):
            names, decl = [], "void"
        else:
            parts = split_params(params)
            names = [param_name(p, types) for p in parts]
            if None in names:
                errors.append("%s: a parameter without a name in (%s)" % (sym, params))
                continue
            decl = ", ".join(parts)
        call = "__real_%s(%s)" % (sym, ", ".join(names))
        is_void = " ".join(ret.split()) == "void"
        body = [
            "%s __real_%s(%s);" % (ret, sym, decl),
            "%s __wrap_%s(%s)" % (ret, sym, decl),
            "{",
            "\trelic_wrap_depth++;",
            ("\t%s;" % call) if is_void else ("\t%s result = %s;" % (ret, call)),
            '\tif (--relic_wrap_depth == 0) dash_oracle_relic_note("%s");' % sym,
        ]
        if not is_void:
            body.append("\treturn result;")
        body.append("}")
        wrappers.append("\n".join(body))
    if errors:
        sys.exit("wrap.py: no parsable prototype:\n  " + "\n  ".join(errors))

    with open(args.out, "w") as out:
        out.write("/* Generated by contrib/node/oracle-relic/wrap.py from relic.h; do not edit. */\n")
        out.write('#include "relic.h"\n\n')
        out.write("void dash_oracle_relic_note(const char *symbol);\n")
        out.write("const unsigned dash_oracle_relic_wrapped = %d;\n" % len(wrappers))
        out.write("static _Thread_local unsigned relic_wrap_depth;\n\n")
        out.write("\n\n".join(wrappers) + "\n")
    print("wrap.py: %d wrappers" % len(wrappers))


if __name__ == "__main__":
    main()
