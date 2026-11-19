#!/usr/bin/env python3
"""Citation checker for a Dash Core re-pin (SW-V24 R15, impact item 15).

Finds the Core `file:line` citations in this tree that point at different line content at
the new pin than at the old one, and reports them. It only reads: nothing in the tree or in
the Core checkout is edited, so the rewrite of the cites (and the pin constants) stays a
separate, reviewed change.

    python3 -I contrib/node/pin-delta/check-citations.py \\
        --core ~/workspace/dash --old v24.0.0-rc.3 --new v24.0.0-rc.4 --out citations-rc3-rc4.md

A citation is `<core path>:<lines>` in any tracked text file that mentions the old pin
(`rc.3` or the full tag): `net_processing.cpp:3637`, `src/test/evo_sharedmn_tests.cpp:522`,
`llmq/snapshot.cpp` rc.3:173-210, `netinfo.cpp:24, 90-113`. The path may be the full path,
the path without `src/`, or a path suffix down to the bare file name; it is looked up in the
old pin's tree. A cite whose name matches several Core files is "ambiguous" and is judged
against every candidate that is long enough to hold the cited lines. A path that is a tracked file of this tree is that file's own cite
and is skipped.

Each cited range is compared between the pins (`git diff -U0 old new`, and the two blobs):
    changed   a hunk touches the cited lines (or inserts inside the range, or the file is
              gone at the new pin): the cited content differs, rewrite or re-verify the cite
    shifted   no hunk touches the lines but earlier hunks move them, so the same line numbers
              hold other content at the new pin (a cite rewritten to the new pin must be
              renumbered; an old-pin cite stays right)
    same      the same content at the same line numbers
A cite with several ranges takes the worst of them. "Differs" in the sense of the task is
changed + shifted.

Which pin a cite names: `pin` is "attached" when the cite carries the label (`rc.3:173`),
"line" when its source line names the old pin, "file" when only the file does (the usual
case: a header says "Core v24.0.0-rc.3" and the cites below it give bare `file:line`), and
"other" when its line names only other Core releases (`v23.1.8`, "Core 23.1.8"; the new pin is
ignored, and a bare dotted number such as a section number or crate version is not a release);
other-pin cites are listed apart and not counted. Cites in files that never name the old pin are
not judged, but the report counts them.

Exit status: 0 (the report is the product), 1 with --strict when any cite changed or is
ambiguous with a changed candidate, 2 on git or usage errors.
"""

import argparse
import collections
import os
import re
import subprocess
import sys

# Diff output must not depend on the user's git config (diff.noprefix, diff.mnemonicPrefix, diff.algorithm,
# diff.interHunkContext, ...): hunks must be the same minimal, unfused ones every run.
DIFF_OPTS = ["--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--src-prefix=a/", "--dst-prefix=b/",
             "--diff-algorithm=myers", "--inter-hunk-context=0",
             # attributes (or a NUL byte) can make git print "Binary files differ" with no hunks: every cite in
             # the file would read "same"
             "--text"]
MAX_BYTES = 4 << 20

CORE_PATH = r"[\w.+-]+(?:/[\w.+-]+)*\.(?:cpp|hpp|cc|c|h|py|json|txt|am|ac|sh|mk|in)"
DASH = "-\u2013\u2014"  # hyphen, en dash, em dash: the docs write ranges both ways
LINES = rf"\d+(?:[{DASH}]\d+)?(?:\s*,\s*:?\d+(?:[{DASH}]\d+)?(?![\w.]))*"
# path, optional closing backtick, optional pin label, then ":<lines>"
CITE_RE = re.compile(
    rf"(?<![\w/.+-])(?P<path>{CORE_PATH})`?(?P<label>\s+(?:v\d+\.\d+\.\d+-)?rc\.\d+|\s+v\d+\.\d+\.\d+)?`?:(?P<lines>{LINES})(?![\w])")
# A Core release named in prose: v24.0.0, v24.0.0-rc.3, 24.0.0-rc.3, "Core 23.1.8", "dashd 23.1.8", rc.3.
# A bare dotted triple is not one (a section number §3.5.1, an address 127.0.0.1, a crate version 2.10.1).
VERSION = r"\d+\.\d+\.\d+(?:-rc\.\d+)?(?![\w]|\.\d)"
PIN_RE = re.compile(rf"(?<![\w.\u00a7/-])v{VERSION}|(?<![\w.\u00a7/-])\d+\.\d+\.\d+-rc\.\d+(?![\w]|\.\d)"
                    rf"|\b(?:Core|dashd|dashcore)\s+(?P<bare>v?{VERSION})|(?<![\w.])rc\.\d+(?!\d)")


class DiffFormatError(Exception):
    """git diff output this script cannot tie to a file: stopping beats reading it as "same"."""


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True).stdout


def git_text(repo, *args):
    return git(repo, *args).decode("utf-8", "replace")


def pins(text):
    """The Core releases a line of prose names, as written (without a "Core " lead-in)."""
    return [m.group("bare") or m.group(0) for m in PIN_RE.finditer(text)]


def parse_ranges(spec):
    out = []
    for part in re.split(r"\s*,\s*", spec):
        part = part.lstrip(":")
        a, *rest = re.split(rf"[{DASH}]", part, maxsplit=1)
        a = int(a)
        b = int(rest[0]) if rest else a
        if a >= 1 and b >= a:
            out.append((a, b))
    return out


def diff_hunks(core, old, new):
    """({path: [(old_start, old_len, new_len)]}, {deleted paths}) for old..new, from -U0 hunks.
    A pure insertion has old_len 0 and old_start = the old line it follows."""
    hunks = collections.defaultdict(list)
    deleted = set()
    path = None
    for line in git_text(core, "diff", *DIFF_OPTS, "-U0", old, new).splitlines():
        m = re.match(r"diff --git a/(.+) b/(.+)$", line)
        if m:
            path = m.group(2)
            hunks[path]
        elif line.startswith("diff --git "):
            raise DiffFormatError(f"cannot read the file names in {line!r}")
        elif line.startswith("Binary files ") and line.endswith(" differ"):
            raise DiffFormatError(f"a binary diff, with no hunks to judge cites by: {line!r}")
        elif line.startswith("+++ /dev/null") and path is not None:
            deleted.add(path)
        elif line.startswith("@@ "):
            m = re.match(r"@@ -(\d+)(?:,(\d+))? \+\d+(?:,(\d+))? @@", line)
            if path is None or m is None:
                raise DiffFormatError(f"a hunk header with no file or in an unknown form: {line!r}")
            hunks[path].append((int(m.group(1)), *(1 if g is None else int(g) for g in m.group(2, 3))))
    return hunks, deleted


def judge(a, b, hunks):
    """"changed", "shifted" or "same" for old lines a..b against the file's hunks."""
    shift = 0
    for start, old_len, new_len in hunks:
        if old_len == 0:
            if a <= start < b:
                return "changed"
            if start < a:
                shift += new_len
        else:
            if start <= b and start + old_len - 1 >= a:
                return "changed"
            if start + old_len - 1 < a:
                shift += new_len - old_len
    return "shifted" if shift else "same"


class Core:
    """The two pins' trees and their differences."""

    def __init__(self, path, old, new):
        self.path = os.path.expanduser(path)
        self.old, self.new = old, new
        self.paths = git_text(self.path, "ls-tree", "-r", "--name-only", old).splitlines()
        self.by_name = collections.defaultdict(list)
        for p in self.paths:
            self.by_name[p.rsplit("/", 1)[-1]].append(p)
        self.hunks, self.deleted = diff_hunks(self.path, old, new)
        self._lines = {}

    def resolve(self, cited):
        """The old-pin paths a cited path names: [] when none."""
        names = self.by_name.get(cited.rsplit("/", 1)[-1], [])
        for exact in (cited, "src/" + cited):
            if exact in names:
                return [exact]
        return [p for p in names if p.endswith("/" + cited)]

    def length(self, path):
        key = (self.old, path)
        if key not in self._lines:
            self._lines[key] = git(self.path, "show", f"{self.old}:{path}").count(b"\n")
        return self._lines[key]

    def verdict(self, path, ranges):
        """(kind, [per-range kind]) of a cite's ranges in one old-pin file; kind is also
        "gone" (file deleted at the new pin) or "past-end" (a range beyond the old file)."""
        if path in self.deleted:
            return "gone", []
        if any(b > self.length(path) for _, b in ranges):
            return "past-end", []
        kinds = [judge(a, b, self.hunks.get(path, [])) for a, b in ranges]
        for worst in ("changed", "shifted"):
            if worst in kinds:
                return worst, kinds
        return "same", kinds


def tracked_text(tree):
    """[(path, lines)] of the tracked text files of the tree, read from the working tree."""
    out = []
    for rel in git(tree, "ls-files", "-z").decode("utf-8", "replace").split("\0"):
        full = os.path.join(tree, rel)
        if not rel or not os.path.isfile(full) or os.path.islink(full) or os.path.getsize(full) > MAX_BYTES:
            continue
        with open(full, "rb") as f:
            raw = f.read()
        if b"\0" in raw:
            continue
        out.append((rel, raw.decode("utf-8", "replace").splitlines()))
    return out


def run(args):
    tree = os.path.expanduser(args.tree)
    core = Core(args.core, args.old, args.new)
    def names(tag):
        return {tag, tag.lstrip("v"), tag.split("-", 1)[1] if "-" in tag else tag}

    old_names, new_names = names(args.old), names(args.new)
    short = args.old.split("-", 1)[1] if "-" in args.old else args.old
    old_re = re.compile(r"(?<![\w.])" + "|".join(re.escape(n) for n in sorted(old_names, key=len, reverse=True))
                        + r"(?![\w])")

    files = tracked_text(tree)
    own = {rel for rel, _ in files}
    scoped = [(rel, lines) for rel, lines in files if any(old_re.search(l) for l in lines)]
    pin_lines = sum(1 for _, lines in scoped for l in lines if args.old in l)

    # Core-shaped cites in the files that never name the old pin: not judged, but counted so that the
    # edge of the scope shows in the report.
    unscoped = collections.Counter()
    scoped_rel = {rel for rel, _ in scoped}
    for rel, lines in files:
        if rel not in scoped_rel:
            for text in lines:
                for m in CITE_RE.finditer(text):
                    if m.group("path") not in own and parse_ranges(m.group("lines")) and core.resolve(m.group("path")):
                        unscoped[rel] += 1

    cites = []
    skipped_own = 0
    for rel, lines in scoped:
        for n, text in enumerate(lines, 1):
            for m in CITE_RE.finditer(text):
                if m.group("path") in own:
                    skipped_own += 1
                    continue
                label = (m.group("label") or "").strip()
                # A mention of the new pin on the line is prose ("rewritten at rc.4"), not the cite's pin.
                tokens = [t for t in pins(text) if t not in new_names]
                if label:
                    pin = "attached" if label in old_names or label.lstrip("v") in old_names else "other"
                elif any(t in old_names for t in tokens):
                    pin = "line"
                elif tokens:
                    pin = "other"
                else:
                    pin = "file"
                ranges = parse_ranges(m.group("lines"))
                if ranges:
                    cites.append(dict(src=rel, line=n, cited=m.group("path"), lines=m.group("lines"),
                                      ranges=ranges, pin=pin, text=text.strip()))

    rows = []       # judged cites of the old pin
    other = []
    unresolved = []
    for c in cites:
        if c["pin"] == "other":
            other.append(c)
            continue
        cands = core.resolve(c["cited"])
        if not cands:
            unresolved.append(c)
            continue
        judged = [(p, *core.verdict(p, c["ranges"])) for p in cands]
        # A name shared with a shorter file: the candidates that can hold the lines are the ones meant.
        judged = [j for j in judged if j[1] != "past-end"] or judged
        c["cands"] = judged
        kinds = {k for _, k, _ in judged}
        c["ambiguous"] = len(cands) > 1
        for kind in ("changed", "gone", "shifted", "past-end", "same"):
            if kind in kinds:
                c["kind"] = kind
                break
        rows.append(c)

    count = collections.Counter((r["kind"], r["ambiguous"]) for r in rows)

    def n(kind, amb=None):
        return sum(v for (k, a), v in count.items() if k == kind and (amb is None or a == amb))

    differs = [r for r in rows if r["kind"] in ("changed", "gone", "shifted")]
    out = [f"# Citation check {args.old} -> {args.new}\n",
           f"Core {args.old} = {git_text(core.path, 'rev-parse', '--short=10', args.old + '^{commit}').strip()}, "
           f"{args.new} = {git_text(core.path, 'rev-parse', '--short=10', args.new + '^{commit}').strip()}; "
           f"tree {git_text(tree, 'rev-parse', '--short=10', 'HEAD').strip()}. Report only: nothing was edited.\n",
           "| count | what |", "|---|---|",
           f"| {len(files)} | tracked text files scanned |",
           f"| {len(scoped)} | of them mention `{short}` ({pin_lines} lines name `{args.old}`) |",
           f"| {len(rows)} | Core citations of the old pin, resolved in its tree |",
           f"| **{n('changed') + n('gone')}** | **changed**: the cited lines differ (of which {n('gone')} in a file gone at the new pin, "
           f"{n('changed', True)} ambiguous by name) |",
           f"| **{n('shifted')}** | **shifted**: same lines, other content at the new pin ({n('shifted', True)} ambiguous) |",
           f"| {n('same')} | same content at the same line numbers |",
           f"| {n('past-end')} | cite past the end of the old file (wrong line or not this pin's) |",
           f"| {len(unresolved)} | no Core file of that name at the old pin |",
           f"| {len(other)} | name only another pin on their line |",
           f"| {skipped_own} | name a tracked file of this tree |",
           f"| {sum(unscoped.values())} | Core citations in {len(unscoped)} files that never name `{short}` (**not judged**, last section) |",
           f"| {len({r['src'] for r in differs})} | files holding a changed or shifted cite |",
           f"| {sum(1 for r in differs if r['pin'] in ('attached', 'line'))} | changed or shifted cites whose own line names `{short}` "
           f"(the rest, {sum(1 for r in differs if r['pin'] == 'file')}, take it from their file) |", ""]
    summary = "\n".join(out)
    for kind, title, note in (
            ("changed", "Changed", "Rewrite to the new pin's lines, or re-verify. `gone` = file deleted at the new pin."),
            ("shifted", "Shifted", "The content is the same lines further on; right as an old-pin cite, renumber when rewritten.")):
        group = [r for r in rows if r["kind"] == kind or (kind == "changed" and r["kind"] == "gone")]
        out += [f"## {title} ({len(group)})\n", note + "\n", "| source | cited | pin | ranges | verdict (per candidate file) | source line |", "|---|---|---|---|---|---|"]
        for r in sorted(group, key=lambda r: (r["src"], r["line"])):
            verdict = "; ".join(f"{p} {k}" + (f" ({'/'.join(ks)})" if len(ks) > 1 else "") for p, k, ks in r["cands"])
            if r["ambiguous"]:
                verdict = "AMBIGUOUS " + verdict
            excerpt = r["text"][:110].replace("|", "\\|")
            out.append(f"| {r['src']}:{r['line']} | `{r['cited']}:{r['lines']}` | {r['pin']} | "
                       f"{'; '.join(f'{a}-{b}' if a != b else str(a) for a, b in r['ranges'])} | {verdict} | "
                       f"{excerpt} |")
        out.append("")
    for title, group, note in (
            ("Past the end of the old file", [r for r in rows if r["kind"] == "past-end"],
             "Check these by hand: the line does not exist at the old pin."),
            ("No Core file of that name", unresolved, "Not Core files (or Core files this pin does not have)."),
            ("Name another pin", other, "Left alone: their line names v23.1.8 or another rc.")):
        out += [f"## {title} ({len(group)})\n", note + "\n"]
        if title.startswith("Past"):
            out += [f"- {r['src']}:{r['line']} `{r['cited']}:{r['lines']}` ({r['cands'][0][0]}, {core.length(r['cands'][0][0])} lines)"
                    for r in sorted(group, key=lambda r: (r["src"], r["line"]))] + [""]
            continue
        per = collections.Counter(r["src"] for r in group)
        out += [f"- {src} ({k})" for src, k in sorted(per.items())] + [""]
    out += [f"## Not judged: files that never name `{short}` ({sum(unscoped.values())} cites in {len(unscoped)} files)\n",
            "They resolve to a Core file but carry no claim to be of this pin. The 15 with most cites:\n"]
    out += [f"- {src} ({k})" for src, k in unscoped.most_common(15)] + [""]
    text = "\n".join(out) + "\n"
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(text)
    print(summary if args.out else text)
    return 1 if args.strict and (n("changed") or n("gone")) else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--core", required=True, help="Dash Core checkout (read-only use)")
    ap.add_argument("--old", required=True, help="the pin the tree cites (tag or commit)")
    ap.add_argument("--new", required=True, help="the pin being moved to (tag or commit)")
    ap.add_argument("--tree", default=".", help="the repository to scan (default: the working directory)")
    ap.add_argument("--out", help="write the report here (default: standard output)")
    ap.add_argument("--strict", action="store_true", help="exit 1 when any cite changed")
    args = ap.parse_args()
    try:
        return run(args)
    except subprocess.CalledProcessError as e:
        stderr = e.stderr.decode("utf-8", "replace").strip() if e.stderr else ""
        print(f"error: {' '.join(e.cmd)} failed: {stderr}", file=sys.stderr)
        return 2
    except DiffFormatError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
