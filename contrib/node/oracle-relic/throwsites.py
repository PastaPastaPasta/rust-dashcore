#!/usr/bin/env python3
"""Static check that no reachable relic error site fires on a normal input (BS11a D65, D71).

usage: throwsites.py --cc CC [--include DIR]... --relic-archive librelic.a --relic-src DIR
                     --dashbls-src DIR --wrap-set FILE --classes FILE --indirect FILE
                     [--report FILE]

The call graph is every instruction of `objdump -dr` of the relic archive that names
another function, by relocation or by direct target (calls, tail jumps and conditional
jumps), with compiler clones (.part, .isra, .constprop, .cold) folded into their
function. Its roots are the wrap set minus the symbols that run only at initialization.

Sources are read as the flavor's build compiles them: `CC -E -fdirectives-only` with
relic's include directories (and the build's, given with --include) decides which lines are live, so a branch of `#if` the pin
does not build counts neither as a site nor toward brace depth. A live RLC_THROW belongs
to the function whose body encloses it. One inside a macro body (a `#define` in the
source or in any relic header a compiled source includes) belongs to every function that uses
the macro, through other macros too, including the functions a macro invocation outside
any function defines (found by expanding that line with `CC -E`). A static function with
no symbol was inlined, so it is reachable when a reachable function of the same file
names it. Every function a reachable body in the same file names is followed too, to a
fixpoint. An indirect call or jump (a `*` operand no GOT relocation names) in a reachable
function must have its row in the --indirect FILE, whose targets join the graph.

Each reachable site, and each RLC_THROW in the dashbls sources and headers, must have a
class: `caught` (directly inside RLC_CATCH_ANY, which is `if (0)` with CHECK off),
`no_memory` (ERR_NO_MEMORY, out of scope), or the class FILE gives its path and line. A
reachable site without a class, a FILE row that names no reachable site, an unaudited
indirect call, an --indirect row with no reachable indirect call or a target not in the
archive, a live site in a compiled source that belongs to no function, a live site in a
header function (inlined, so its callers are unknown here), a source whose braces do not
balance, or a relic configuration with CHECK on or QUIET off fails the check: relic
moved and the table needs a review.
"""
import argparse
import collections
import os
import re
import subprocess
import sys

INIT = {"core_get", "core_init", "core_set_thread_initializer", "ep_param_set_any_pairf", "err_get_code"}
MEMBER = re.compile(r"^(\S+\.o):\s+file format")
HEADER = re.compile(r"^[0-9a-f]+ <([^>]+)>:$")
TARGET = re.compile(r"\s[0-9a-f]+ <([^>+]+)(?:\+0x[0-9a-f]+)?>")
RELOC = re.compile(r"R_X86_64_(?:PLT32|PC32|GOTPCREL\w*)\s+([A-Za-z_][\w.]*)")
INDIRECT = re.compile(r"\t(?:notrack\s+)?(?:call|jmp)q?\s+\*")
GOT_CALL = re.compile(r"R_X86_64_GOTPCREL\w*\s+[A-Za-z_]")
CLONE = re.compile(r"\.(?:part|isra|constprop|cold|lto_priv)(?:\.\d+)?")
FUNC_DEF = re.compile(r"^(?:static\s+)?(?:inline\s+)?[A-Za-z_][\w\s\*]*?\b([a-z_]\w*)\s*\([^;]*$")
THROW = re.compile(r"RLC_THROW\((\w+)\)")
LINEMARK = re.compile(r'^# (\d+) "([^"]*)"')
DEFINE = re.compile(r"^\s*#\s*define\s+(\w+)")
LITERAL = re.compile(r'"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'')
COMMENT = re.compile(r'("(?:\\.|[^"\\\n])*"|\'(?:\\.|[^\'\\\n])*\')|//[^\n]*|/\*.*?\*/', re.S)


def base(sym):
    return CLONE.sub("", sym)


def call_graph(archive):
    """The edges, the defined functions, and each function's count of indirect calls and
    jumps: a `*` operand whose next line is no GOT relocation naming the callee."""
    dis = subprocess.run(["objdump", "-dr", archive], capture_output=True, text=True, check=True).stdout
    edges, defined, by_member = collections.defaultdict(set), set(), collections.defaultdict(set)
    indirect = collections.Counter()
    member = cur = pending = None
    for line in dis.splitlines():
        if pending is not None:
            if not GOT_CALL.search(line):
                indirect[pending] += 1
            pending = None
        m = MEMBER.match(line)
        if m:
            member = m.group(1)
            continue
        m = HEADER.match(line)
        if m:
            cur = base(m.group(1))
            defined.add(cur)
            by_member[member].add(cur)
            continue
        if cur is None:
            continue
        for t in (TARGET.search(line), RELOC.search(line)):
            if t and not t.group(1).startswith(".") and base(t.group(1)) != cur:
                edges[cur].add(base(t.group(1)))
        if INDIRECT.search(line):
            pending = cur
    if pending is not None:
        indirect[pending] += 1
    return edges, defined, indirect


class Preprocessor:
    def __init__(self, cc, relic_src, includes):
        self.cc = cc
        self.root = os.path.realpath(relic_src)
        self.flags = ["-D_GNU_SOURCE"] + ["-I" + os.path.join(self.root, d)
                                          for d in ("include", "include/low", "src/tmpl")]
        self.flags += ["-I" + d for d in includes]
        self.live = collections.defaultdict(dict)
        self.expanded = {}

    def lines(self, path, *extra):
        """Every output line by file (relative to relic's root) and line number."""
        cmd = [self.cc, "-E", "-x", "c"] + list(extra) + self.flags + [os.path.join(self.root, path)]
        out, cur, n = collections.defaultdict(dict), None, 0
        for line in subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.split("\n"):
            m = LINEMARK.match(line)
            if m:
                n, cur = int(m.group(1)), os.path.relpath(os.path.realpath(m.group(2)), self.root)
                continue
            if cur is not None and line.strip():
                out[cur][n] = (out[cur][n] + " " + line) if n in out[cur] else line
            n += 1
        return out

    def read(self, path):
        """Records the live lines of a compiled source and of the headers it includes."""
        for f, lines in self.lines(path, "-fdirectives-only").items():
            self.live[f].update(lines)

    def expand(self, path, n):
        if path not in self.expanded:
            self.expanded[path] = self.lines(path)[path]
        return self.expanded[path].get(n, "")

    def config(self):
        cmd = [self.cc, "-E", "-dM", "-x", "c"] + self.flags + ["-"]
        return subprocess.run(cmd, input='#include "relic_conf.h"\n', capture_output=True, text=True,
                              check=True).stdout


def compiled_sources(archive, relic_src, arith):
    """The source of each archive member. A low-level file exists once per backend: the
    member is the configured backend's (relic_conf.h's ARITH), or easy's where that
    backend has none, as relic's build picks them."""
    members = subprocess.run(["ar", "t", archive], capture_output=True, text=True, check=True).stdout.split()
    by_base = collections.defaultdict(list)
    for dp, _, files in os.walk(os.path.join(relic_src, "src")):
        for f in files:
            if f.endswith(".c"):
                by_base[f[:-2]].append(os.path.relpath(os.path.join(dp, f), relic_src))
    out = []
    for m in members:
        name = re.sub(r"^librelic_la-", "", m)[:-2]
        for backend in (arith, "easy"):
            if len(by_base[name]) > 1:
                pick = [p for p in by_base[name] if p.startswith("src/low/%s/" % backend)]
                by_base[name] = pick or by_base[name]
        if len(by_base[name]) != 1:
            sys.exit("throwsites.py: member %s maps to %d sources" % (m, len(by_base[name])))
        out.append(by_base[name][0])
    return sorted(out)


def uncommented(path):
    """The file's lines with comments blanked (line breaks kept)."""
    text = open(path, errors="replace").read()
    return COMMENT.sub(lambda m: m.group(1) or "\n" * m.group().count("\n"), text).split("\n")


def defined_functions(text):
    """The functions an expanded line defines: `name(...) {` at brace depth 0."""
    text, names, depth = LITERAL.sub('""', text), [], 0
    for m in re.finditer(r"[{}]", text):
        if m.group() == "{":
            if depth == 0:
                f = re.search(r"\b([A-Za-z_]\w*)\s*\([^()]*\)\s*$", text[: m.start()])
                if f:
                    names.append(f.group(1))
            depth += 1
        else:
            depth -= 1
    return names


def in_catch(lines, k, throw):
    """Whether the RLC_THROW match on lines[k] directly follows `RLC_CATCH_ANY {`."""
    before = lines[k][: throw.start()].strip()
    if not before:
        before = next((p for p in (q.strip().rstrip("\\").strip() for q in reversed(lines[:k])) if p), "")
    return before.endswith("RLC_CATCH_ANY {")


class Source:
    """One file's live RLC_THROW sites, macros and function bodies (with the functions a
    macro invocation outside any function defines)."""

    def __init__(self, pp, path, compiled):
        raw = uncommented(os.path.join(pp.root, path))
        live = pp.live[path]
        self.path, self.sites, self.macros, self.bodies, self.errors = path, [], {}, {}, []
        depth, func, i = 0, None, 0
        while i < len(raw):
            n = i + 1
            text = live.get(n)
            if text is None:
                i += 1
                continue
            if raw[i].lstrip().startswith("#"):
                end = i
                while raw[end].rstrip().endswith("\\") and end + 1 < len(raw):
                    end += 1
                d = DEFINE.match(raw[i])
                if d and d.group(1) != "RLC_THROW":
                    body = raw[i:end + 1]
                    self.macros[d.group(1)] = DEFINE.sub("", "\n".join(body), count=1)
                    for k, line in enumerate(body):
                        for t in THROW.finditer(line):
                            self.sites.append((n + k, "macro", d.group(1), t.group(1), in_catch(body, k, t)))
                i = end + 1
                continue
            code = LITERAL.sub('""', raw[i])
            delta = code.count("{") - code.count("}")
            if depth == 0:
                m = FUNC_DEF.match(code)
                if m:
                    func = m.group(1)
                elif compiled and re.match(r"^\s*[A-Za-z_]\w*\s*\(", code):
                    generated = defined_functions(pp.expand(path, n))
                    for g in generated:
                        self.bodies[g] = code
                    # A signature whose return type is on the line before opens the body here.
                    func = generated[-1] if generated and delta > 0 else None
            elif func:
                self.bodies[func] = self.bodies.get(func, "") + "\n" + code
            if "RLC_THROW" in code:
                for t in THROW.finditer(raw[i]):
                    if not compiled:
                        self.errors.append("%s:%d: an RLC_THROW in a header function" % (path, n))
                    elif depth > 0 and func:
                        self.sites.append((n, "func", func, t.group(1), in_catch(raw, i, t)))
                    else:
                        self.errors.append("%s:%d: an RLC_THROW outside any function" % (path, n))
            depth += delta
            if depth < 0:
                self.errors.append("%s:%d: brace depth below 0" % (path, n))
                depth = 0
            if depth == 0 and delta < 0:
                func = None
            i += 1
        if depth != 0 and compiled:
            self.errors.append("%s: brace depth %d at the end of the live source" % (path, depth))


def scan_dashbls(root):
    """dashbls's own RLC_THROW sites (sources and headers), by raw text."""
    sites = []
    for dp, _, files in os.walk(root):
        for f in sorted(files):
            if not f.endswith((".cpp", ".hpp", ".h")):
                continue
            path = os.path.join(dp, f)
            lines = uncommented(path)
            for k, line in enumerate(lines):
                for t in THROW.finditer(line):
                    if not DEFINE.match(line):
                        sites.append(("dashbls/" + os.path.relpath(path, root), k + 1, "-", t.group(1),
                                      in_catch(lines, k, t)))
    return sites


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cc", required=True)
    ap.add_argument("--include", action="append", default=[])
    ap.add_argument("--relic-archive", required=True)
    ap.add_argument("--relic-src", required=True)
    ap.add_argument("--dashbls-src", required=True)
    ap.add_argument("--wrap-set", required=True)
    ap.add_argument("--classes", required=True)
    ap.add_argument("--indirect", required=True)
    ap.add_argument("--report")
    args = ap.parse_args()

    pp = Preprocessor(args.cc, args.relic_src, args.include)
    conf = dict(re.findall(r"^#define (\w+)(?: (.*))?$", pp.config(), re.M))
    errors = []
    if "CHECK" in conf or "QUIET" not in conf:
        errors.append("relic_conf.h: CHECK must be off and QUIET on (RLC_THROW only sets the code)")
    arith = (conf.get("ARITH") or "").lower()

    edges, defined, indirect = call_graph(args.relic_archive)
    audited = {}
    for row in open(args.indirect):
        if row.strip() and not row.startswith("#"):
            func, targets = row.rstrip("\n").split("\t")[:2]
            audited[func] = set() if targets == "-" else set(targets.split(","))
            edges[func] |= audited[func]
    roots = [s.strip() for s in open(args.wrap_set) if s.strip() and s.strip() not in INIT]
    seen = set()

    def reach(names):
        stack = list(names)
        while stack:
            f = stack.pop()
            if f not in seen:
                seen.add(f)
                stack.extend(edges.get(f, ()))

    reach(roots)
    paths = compiled_sources(args.relic_archive, args.relic_src, arith)
    for p in paths:
        pp.read(p)
    sources = [Source(pp, p, True) for p in paths]
    headers = [Source(pp, f, False) for f in sorted(pp.live) if not f.startswith("..") and f not in paths]
    for s in sources + headers:
        errors += s.errors
    macros = {}
    for s in headers + sources:
        macros.update(s.macros)
    bodies = {(s.path, f): b for s in sources for f, b in s.bodies.items()}

    changed = True
    while changed:
        changed = False
        for (path, f), body in bodies.items():
            if f not in seen:
                continue
            for (p2, g) in bodies:
                if p2 == path and g not in seen and re.search(r"\b%s\b" % re.escape(g), body):
                    reach([g])
                    changed = True

    for f in sorted(seen):
        if indirect[f] and f not in audited:
            errors.append("%s: %d indirect calls or jumps that %s does not audit" % (f, indirect[f], args.indirect))
    for f, targets in sorted(audited.items()):
        if f not in seen or not indirect[f]:
            errors.append("%s: an audited function with no reachable indirect call" % f)
        errors += ["%s: audited target %s is not in the archive" % (f, t) for t in sorted(targets - defined)]

    def reachable(path, func):
        if func in seen:
            return True
        if func in defined:
            return False
        return any(p == path and f in seen and re.search(r"\b%s\s*\(" % re.escape(func), b)
                   for (p, f), b in bodies.items())

    named = {m: {n for n in re.findall(r"\b[A-Za-z_]\w*\b", body) if n in macros and n != m}
             for m, body in macros.items()}
    users = collections.defaultdict(list)
    for (path, f), body in bodies.items():
        stack, done = [n for n in re.findall(r"\b[A-Za-z_]\w*\b", body) if n in macros], set()
        while stack:
            m = stack.pop()
            if m not in done:
                done.add(m)
                users[m].append((path, f))
                stack.extend(named[m])

    sites, live = [], []
    for s in sources + headers:
        for line, kind, name, err, caught in s.sites:
            owners = [(s.path, name)] if kind == "func" else users.get(name, [])
            if kind == "macro" and not owners:
                if s in sources:
                    errors.append("%s:%d: an RLC_THROW in macro %s, which no function uses" % (s.path, line, name))
                continue
            sites.append((s.path, line))
            hit = sorted({f for p, f in owners if reachable(p, f)})
            if hit:
                live.append((s.path, line, ",".join(hit), err, caught))
    dashbls = scan_dashbls(args.dashbls_src) + scan_dashbls(os.path.join(args.dashbls_src, "..", "include"))

    table = {}
    for row in open(args.classes):
        if row.strip() and not row.startswith("#"):
            path, line, cls = row.rstrip("\n").split("\t")[:3]
            table[(path, int(line))] = cls
    counts, used, rows = collections.Counter(), set(), []
    for path, line, func, kind, in_catch in live + dashbls:
        if in_catch and kind == "ERR_CAUGHT":
            cls = "caught"
        elif kind == "ERR_NO_MEMORY":
            cls = "no_memory"
        elif (path, line) in table:
            cls = table[(path, line)]
            used.add((path, line))
        else:
            errors.append("%s:%d %s %s has no class" % (path, line, func, kind))
            continue
        counts[cls] += 1
        rows.append("%s\t%d\t%s\t%s\t%s" % (path, line, func, kind, cls))
    for key in sorted(set(table) - used):
        errors.append("%s:%d is in the table but is no reachable site" % key)
    if args.report:
        with open(args.report, "w") as out:
            out.write("\n".join(rows) + "\n")
    print("throwsites.py: %d roots, %d reachable functions (%d indirect calls or jumps in %d, audited), "
          "%d of %d relic sites reachable, %d dashbls sites; %s"
          % (len(roots), len(seen), sum(indirect[f] for f in seen), sum(1 for f in seen if indirect[f]),
             len(live), len(sites), len(dashbls),
             " ".join("%s=%d" % kv for kv in sorted(counts.items()))))
    if errors:
        sys.exit("throwsites.py:\n  " + "\n  ".join(errors))


if __name__ == "__main__":
    main()
