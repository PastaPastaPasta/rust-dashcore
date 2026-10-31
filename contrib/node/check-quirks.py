#!/usr/bin/env python3
"""Checks that the quirk register and the quirk tests agree (PLAN task O5; FR-CON-34).

Reads docs/node-program/quirks.toml, every Rust file in the workspace (skipping target/ and
hidden directories; comments stripped) and the compiled test inventory: every test binary
`cargo test --workspace --no-run` builds, with the features
`cargo test --workspace` uses, each listed with `--list` and `--list --ignored`. It checks:
- ids run 1..N with no gaps or repeats, and every entry has its fields;
- every quirk that is not dropped has exactly one test, a `#[test]` (or `#[rstest]`) function
  `quirk_NN_<slug>` in the file the register names, not conditionally compiled, and no other
  function is named `quirk_NN_*`; that test is in the compiled inventory exactly once, from a
  test target whose root file is the register file or reaches it through `mod` declarations
  (`#[path]` included) with the module path of its name, and nothing else compiled is named
  `quirk_NN_*` (so a test hidden by a file- or module-level `cfg`, or replaced by a same-named
  test in another target, fails); a `base-sdk:<path>::<fn>` test is read from the base SDK
  fork at the pinned revision (contrib/node/base_sdk_pins.py) and is not compiled here;
- a dropped quirk has no test, in the source or compiled;
- status "scaffold" ⇔ exactly `#[ignore = "quirk N: owner TASK"]` with the register's owner, and
  status "tested" ⇔ no `ignore` in any attribute (`cfg_attr` included), in the source and in the
  compiled inventory alike;
- owners and `also` tasks are task rows of docs/node-program/PLAN.md §4.
With --core <Dash Core repo>, it also checks that every anchor's file exists at the v24 pin and has
the lines cited.

Prints one summary line for STATUS and exits 1 on any disagreement, listing each.

--source-only skips the compiled inventory (quick, but blind to `cfg`); the gate runs without it.

Usage: contrib/node/check-quirks.py [--root <repo>] [--source-only]
                                    [--core <dash repo> [--core-ref <ref>]]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

REGISTER = "docs/node-program/quirks.toml"
PLAN = "docs/node-program/PLAN.md"
CORE_REF = "v24.0.0-rc.3"  # the v24.0 pin (DEC-82)
BASE_SDK = "base-sdk:"
STATUSES = ("tested", "scaffold", "dropped (DEC-82)")
QUIRK_1 = "quirk 1: see PLAN §7 handling"

FN = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:async[ \t]+)?fn[ \t]+(?P<name>quirk_(?P<id>\d+)_\w*)")
STRING = re.compile(r'r(#*)".*?"\1|"(?:[^"\\]|\\.)*"', re.S)
IS_TEST = re.compile(r"(?:\w+::)*(?:test|rstest)\b")
REASON = re.compile(r'ignore[ \t]*=[ \t]*"(?P<reason>[^"\\]*)"')
UNREADABLE = object()  # an ignore whose reason cannot be read exactly
COMPILED = re.compile(r"(?:\w+::)*(?P<name>quirk_(?P<id>\d+)_\w*)")


RAW_STRING = re.compile(r'b?r(#*)".*?"\1', re.S)
PLAIN_STRING = re.compile(r'b?"(?:[^"\\]|\\.)*"', re.S)
CHAR = re.compile(r"b?'(?:[^'\\\n]|\\[^\n]+?)'")  # not a lifetime: those have no closing quote


def strip_comments(text):
    """The text with `//` and (nested) `/* */` comments blanked, newlines kept, and string, raw
    string and char literals left intact."""
    out, i, n = [], 0, len(text)
    while i < n:
        word_start = i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")
        literal = word_start and (RAW_STRING.match(text, i) or PLAIN_STRING.match(text, i) or CHAR.match(text, i))
        if literal:
            out.append(literal[0])
            i = literal.end()
        elif text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
        elif text.startswith("/*", i):
            level, j = 1, i + 2
            while j < n and level:
                step = 2 if text.startswith(("/*", "*/"), j) else 1
                level += {"/*": 1, "*/": -1}.get(text[j:j + 2], 0) if step == 2 else 0
                j += step
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def rust_files(root):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d != "target" and not d.startswith("."))
        for f in sorted(filenames):
            if f.endswith(".rs"):
                yield Path(dirpath, f)


def depth(line):
    """The change in bracket depth over a line, outside string literals."""
    bare = STRING.sub("", line)
    return bare.count("[") - bare.count("]")


def parse(text, rel):
    """Every `fn quirk_NN_*` in a file outside comments, with the outer attributes above it (blank
    lines between them skipped): (id, name, rel, is_test, ignore, conditional), where ignore is
    None, a reason string, or UNREADABLE."""
    found, attrs, current, level = [], [], None, 0
    for line in strip_comments(text).splitlines():
        stripped = line.strip()
        if current is not None:
            current.append(line)
            level += depth(line)
            if level <= 0:
                attrs.append("\n".join(current))
                current = None
            continue
        if stripped.startswith("#[") and not stripped.startswith("#!["):
            level = depth(line)
            if level <= 0:
                attrs.append(stripped)
            else:
                current = [line]
            continue
        if not stripped:
            continue
        m = FN.match(line)
        if m:
            bodies = [STRING.sub('""', a) for a in attrs]
            is_test = any(IS_TEST.fullmatch(b[2:-1].strip()) for b in bodies)
            ignore = None
            if any(re.search(r"\bignore\b", b) for b in bodies):
                exact = [REASON.fullmatch(a[2:-1].strip()) for a in attrs]
                exact = [r["reason"] for r in exact if r]
                ignore = exact[0] if len(exact) == 1 else UNREADABLE
                if sum(bool(re.search(r"\bignore\b", b)) for b in bodies) > 1:
                    ignore = UNREADABLE
            conditional = any(re.match(r"#\[\s*cfg(_attr)?\s*\(", b) for b in bodies)
            found.append((int(m["id"]), m["name"], rel, is_test, ignore, conditional))
        attrs = []
    return found


def find_tests(root):
    found = []
    for path in rust_files(root):
        text = path.read_text(encoding="utf-8", errors="replace")
        if "quirk_" in text:
            found += parse(text, path.relative_to(root).as_posix())
    return found


def base_sdk_file(root, path, cache={}):
    """(text or None, where) of a file in the base SDK fork at the pinned revision."""
    if path not in cache:
        sys.path.insert(0, str(root / "contrib" / "node"))
        try:
            import base_sdk_pins as pins
            errors = []
            url, rev, _ = pins.pins(pins.load_toml(root / "Cargo.toml"), errors)
            if errors or not url.startswith("file://"):
                raise ValueError("; ".join(errors) or f"the fork is not a local clone ({url})")
            r = subprocess.run(["git", "-C", url[len("file://"):], "show", f"{rev}:{path}"],
                               capture_output=True, text=True)
            cache[path] = (r.stdout if r.returncode == 0 else None, f"the base SDK fork at {rev[:12]}")
        except Exception as e:  # noqa: BLE001 (any failure to read the pin is reported)
            cache[path] = (None, f"the base SDK pin ({e})")
    return cache[path]


MOD = re.compile(r'(?:#\[\s*path\s*=\s*"(?P<path>[^"]*)"\s*\]\s*)?(?:pub(?:\([^)]*\))?\s+)?\bmod\s+(?P<name>\w+)\s*;')


def module_files(root, src):
    """{file relative to root: [module path prefix, e.g. "a::b::"]} for a test target's root file
    `src` and the files its `mod name;` declarations reach (`#[path]` included; `cfg` is left to
    the compiled names, and a file included twice has one prefix per inclusion)."""
    files, todo = {}, [(root / src, "", True)]
    while todo:
        f, prefix, mod_rs = todo.pop()
        rel = f.resolve().relative_to(root.resolve()).as_posix() if f.exists() else None
        if rel is None:
            continue
        files.setdefault(rel, []).append(prefix)
        if len(files[rel]) > 1:
            continue
        for m in MOD.finditer(strip_comments(f.read_text(encoding="utf-8", errors="replace"))):
            if m["path"]:
                todo.append((f.parent / m["path"], f"{prefix}{m['name']}::", True))
            else:
                base = f.parent if mod_rs else f.with_suffix("")
                flat = base / f"{m['name']}.rs"
                child = flat if flat.exists() else base / m["name"] / "mod.rs"
                todo.append((child, f"{prefix}{m['name']}::", child.name == "mod.rs"))
    return files


def compiled_inventory(root):
    """[(test target's root file, test name, ignored)] for every test the workspace's test
    binaries list, or an error string. Default features, as the gate's `cargo test --workspace`
    builds them."""
    cmd = ["cargo", "test", "--locked", "--workspace", "--no-run", "--message-format=json"]
    r = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
    if r.returncode != 0:
        return f"`{' '.join(cmd)}` failed:\n" + "\n".join(r.stderr.splitlines()[-15:])
    # As `cargo test` does: proc-macro test binaries link the toolchain's libstd dynamically.
    libdir = subprocess.run(["rustc", "--print", "target-libdir"], cwd=root, capture_output=True, text=True)
    var = {"darwin": "DYLD_FALLBACK_LIBRARY_PATH", "win32": "PATH"}.get(sys.platform, "LD_LIBRARY_PATH")
    env = dict(os.environ)
    env[var] = os.pathsep.join(filter(None, [libdir.stdout.strip(), os.environ.get(var)]))
    inventory = []
    for line in r.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        exe = msg.get("executable")
        if msg.get("reason") != "compiler-artifact" or not exe or not msg.get("profile", {}).get("test"):
            continue
        listed = {}
        for ignored in (False, True):
            args = [exe, "--list", "--format", "terse"] + (["--ignored"] if ignored else [])
            out = subprocess.run(args, cwd=Path(msg["manifest_path"]).parent, env=env,
                                 capture_output=True, text=True)
            if out.returncode != 0:
                return f"`{' '.join(args)}` failed: {out.stderr.strip()[-500:]}"
            for row in out.stdout.splitlines():
                name, _, kind = row.rpartition(": ")
                if kind == "test":
                    listed[name] = ignored
        src = Path(msg["target"]["src_path"]).resolve().relative_to(root.resolve()).as_posix()
        inventory += [(src, name, ignored) for name, ignored in listed.items()]
    return inventory


def plan_tasks(root):
    """Task ids of PLAN.md §4's tables, or None when there is no PLAN.md."""
    path = root / PLAN
    if not path.exists():
        return None
    text = path.read_text(encoding="utf-8")
    section = re.search(r"^## 4\..*?(?=^## 5\.)", text, re.S | re.M)
    rows = re.findall(r"^\| ([A-Z][A-Za-z0-9-]*) \|", section[0] if section else "", re.M)
    return set(rows) - {"ID"}


def anchors(core):
    """(path, [line numbers]) for each `path:lines` anchor in a `core` field."""
    for part in core.split(";"):
        path, _, lines = part.strip().partition(":")
        nums = []
        for span in lines.split(","):
            nums.extend(int(n) for n in span.split("-") if n.strip().isdigit())
        yield path, nums


def check_core(entries, repo, ref):
    errors, lengths = [], {}
    for q in entries:
        if q.get("core") == QUIRK_1:
            continue
        for path, nums in anchors(q.get("core", "")):
            if path not in lengths:
                r = subprocess.run(["git", "-C", repo, "show", f"{ref}:{path}"], capture_output=True)
                lengths[path] = r.stdout.count(b"\n") if r.returncode == 0 else None
            n = lengths[path]
            if n is None:
                errors.append(f"quirk {q['id']}: core anchor {path} does not exist at {ref}")
            elif not nums or max(nums) > n:
                errors.append(f"quirk {q['id']}: core anchor {path}:{nums} is outside the file's {n} lines at {ref}")
    return errors


def check_entries(entries, tasks):
    errors = []
    ids = [q.get("id") for q in entries]
    if ids != list(range(1, len(entries) + 1)):
        errors.append(f"{REGISTER}: ids must run 1..{len(entries)} in order, found {ids}")
    for q in entries:
        qid = q.get("id")
        for key in ("title", "core", "owner", "status", "source"):
            if not isinstance(q.get(key), str) or not q[key].strip() or "\n" in q[key]:
                errors.append(f"quirk {qid}: `{key}` must be one non-empty line")
        status = q.get("status")
        if status not in STATUSES:
            errors.append(f"quirk {qid}: status {status!r} is not one of {', '.join(STATUSES)}")
        also = q.get("also", [])
        if not isinstance(also, list) or not all(isinstance(t, str) for t in also):
            errors.append(f"quirk {qid}: `also` must be a list of task ids")
            also = []
        if tasks is not None:
            for task in [q.get("owner")] + also:
                if task not in tasks:
                    errors.append(f"quirk {qid}: task {task!r} is not a task row of {PLAN} §4")
        test = q.get("test")
        if status == "dropped (DEC-82)":
            if test is not None:
                errors.append(f"quirk {qid}: a dropped quirk has no test, but the register names {test}")
        elif not isinstance(test, str) or "::" not in test:
            errors.append(f"quirk {qid}: `test` must be <path>::<fn>")
        elif not re.fullmatch(rf"quirk_{qid:02d}_[a-z0-9_]+", test.rpartition("::")[2]):
            errors.append(f"quirk {qid}: test {test} is not named quirk_{qid:02d}_<slug>")
    first = next((q for q in entries if q.get("id") == 1), {})
    if first.get("title") != QUIRK_1 or first.get("core") != QUIRK_1:
        errors.append(f"quirk 1: its title and core must be exactly {QUIRK_1!r}")
    return errors


def check(root, core_repo=None, core_ref=CORE_REF, source_only=False):
    try:
        entries = tomllib.loads((root / REGISTER).read_text(encoding="utf-8")).get("quirk", [])
    except (OSError, tomllib.TOMLDecodeError) as e:
        return [f"{REGISTER}: {e}"], None
    errors = check_entries(entries, plan_tasks(root))
    by_id = {q.get("id"): q for q in entries}

    per_id = {}
    in_tree = find_tests(root)
    for qid, name, path, is_test, ignore, conditional in in_tree:
        per_id.setdefault(qid, []).append((name, path, is_test, ignore, conditional))
        if qid not in by_id:
            errors.append(f"{path}: {name} is not in the register (no quirk {qid})")

    compiled = None  # quirk id -> [(test target's root file, test name, ignored)]
    if not source_only:
        inventory = compiled_inventory(root)
        if isinstance(inventory, str):
            errors.append(f"cannot list the compiled tests: {inventory}")
        else:
            compiled = {}
            for src, name, ignored in inventory:
                m = COMPILED.fullmatch(name)
                if m:
                    compiled.setdefault(int(m["id"]), []).append((src, name, ignored))
            for qid, rows in compiled.items():
                if qid not in by_id:
                    for src, name, _ in rows:
                        errors.append(f"{src}: compiled test {name} is not in the register (no quirk {qid})")
    for q in entries:
        test = q.get("test")
        if isinstance(test, str) and test.startswith(BASE_SDK):
            path, _, fn = test[len(BASE_SDK):].partition("::")
            text, where = base_sdk_file(root, path)
            if text is None:
                errors.append(f"quirk {q['id']}: cannot read {path} from {where}")
                continue
            for found in parse(text, BASE_SDK + path):
                if found[0] == q["id"]:
                    per_id.setdefault(found[0], []).append(found[1:])

    ignored = tested = dropped = 0
    for qid, q in by_id.items():
        status, owner, test = q.get("status"), q.get("owner"), q.get("test")
        tests = per_id.get(qid, [])
        if status == "dropped (DEC-82)":
            dropped += 1
            for name, path, *_ in tests:
                errors.append(f"quirk {qid} is dropped, but {path} has {name}")
            for src, name, _ in (compiled or {}).get(qid, []):
                errors.append(f"quirk {qid} is dropped, but the test target {src} compiles {name}")
            continue
        if len(tests) != 1:
            where = ", ".join(f"{p}::{n}" for n, p, *_ in tests) or "none"
            errors.append(f"quirk {qid}: needs exactly one test, found {len(tests)} ({where})")
            continue
        name, path, is_test, ignore, conditional = tests[0]
        if not isinstance(test, str) or f"{path}::{name}" != test:
            errors.append(f"quirk {qid}: the register names {test}, the tree has {path}::{name}")
        if compiled is not None:
            rows = compiled.get(qid, [])
            external = path.startswith(BASE_SDK)
            if external and rows:
                errors.append(f"quirk {qid}: its test is in the base SDK, but this workspace also compiles "
                              + ", ".join(f"{d}::{n}" for d, n, _ in rows))
            elif not external:
                if len(rows) != 1:
                    where = ", ".join(f"{d}::{n}" for d, n, _ in rows) or "none (a cfg or a missing target hides it)"
                    errors.append(f"quirk {qid}: needs exactly one compiled test, found {len(rows)}: {where}")
                elif rows[0][1] not in [p + name for p in module_files(root, rows[0][0]).get(path, [])]:
                    errors.append(f"quirk {qid}: the compiled test is {rows[0][1]} in the test target "
                                  f"{rows[0][0]}, which does not compile it from {path}::{name}")
                elif rows[0][2] != (status == "scaffold"):
                    errors.append(f"quirk {qid}: status {status}, but the compiled test "
                                  f"{rows[0][1]} is {'ignored' if rows[0][2] else 'not ignored'}")
        if not is_test:
            errors.append(f"quirk {qid}: {path}::{name} is not a #[test]")
        if conditional:
            errors.append(f"quirk {qid}: {path}::{name} is conditionally compiled (#[cfg]/#[cfg_attr])")
        want = f"quirk {qid}: owner {owner}"
        got = ("no #[ignore]" if ignore is None else "an ignore whose reason cannot be read exactly"
               if ignore is UNREADABLE else repr(ignore))
        if status == "scaffold":
            ignored += 1
            if ignore != want:
                errors.append(f"quirk {qid}: status scaffold needs #[ignore = \"{want}\"], {path}::{name} has {got}")
        elif status == "tested":
            tested += 1
            if ignore is not None:
                errors.append(f"quirk {qid}: status tested, but {path}::{name} has {got}; un-ignore it or set status = \"scaffold\"")

    if core_repo:
        errors += check_core(entries, core_repo, core_ref)
    summary = (
        f"quirk register: {len(entries)} quirks, {tested + ignored} tests: "
        f"{tested} tested, {ignored} ignored (scaffold), {dropped} dropped (DEC-82)"
        + (" [source only: compiled inventory not checked]" if source_only else "")
    )
    return errors, summary


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    ap.add_argument("--core", help="a Dash Core clone; checks every anchor at --core-ref")
    ap.add_argument("--core-ref", default=CORE_REF)
    ap.add_argument("--source-only", action="store_true", help="skip the compiled test inventory")
    args = ap.parse_args()
    errors, summary = check(args.root.resolve(), args.core, args.core_ref, args.source_only)
    for e in errors:
        print(f"check-quirks: {e}", file=sys.stderr)
    if summary:
        print(summary)
    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
