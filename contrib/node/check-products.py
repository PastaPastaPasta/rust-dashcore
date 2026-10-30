#!/usr/bin/env python3
"""The global-allocator proof (SB0 L5; DEC-181, DEC-188).

  check-products.py [--registry | <package>...]

layers.toml's [[product]] registry lists every artifact the program ships or
tests as a product, each row with the exact features, profile and target
triples it is built with. Every cdylib, staticlib and bin target of a
workspace member, whatever its Cargo kind (a lib, a bin, or an example, test
or bench by its crate types), needs a row or a [[product-exclusion]] with a
reason, and every row and named exclusion must name one (`--registry` checks
only this). An exclusion by `kind` ("example", "test" or "bench") covers only
that kind's plain executables; a library artifact is excluded by name.

For each row, target and triple, cargo builds the product as configured, and
the same crate is compiled again as an rlib, with the arguments and
environment cargo gave it. A canary binary then has to link against that rlib:

    extern crate <crate>;
    #[global_allocator] static DASH_ALLOC_CANARY: std::alloc::System = std::alloc::System;
    fn main() {}

rustc admits one global allocator per product, so the link fails, naming the
crate (`conflicts with global allocator in: <crate>`), if anything the product
links installs another. That holds whatever put the allocator there: a cfg,
a `#[path]` file, a macro, a build script, or a feature another crate of the
product enables. Failures name the package ids cargo built under that crate
name. A row with `allocator = "own"` installs its own allocator, which must be
its own package's (DEC-188 (1)): rustc names crates, not packages, so its
product canary must conflict, and a second canary, linking every dependency
the product unit is given except its own package's, must link. A row's
`kind` ("lib", "bin" or "example") picks between targets of one name.

Those canaries see what the product's rustc arguments name, so the arguments
are allow-listed, not parsed for known-bad shapes (DEC-188 am. 1). Every
rustc unit the canary build compiles (the wrapper records each; a unit cargo
finds fresh was compiled with the same rustflags, which its fingerprint
holds and the product unit's arguments show, and whose crates reach both
canaries through its rlib; an own row's own-package units, which the second
canary leaves out, are always recompiled, as the fingerprint omits
RUSTC_WRAPPER), the product's included, and
every build script's linked paths, libraries and cfgs are read the way
rustc's getopts reads them, into options and values: `--opt val`,
`--opt=val`, `-Xval`, `-X val`, grouped short flags and the long aliases.
Each option must be in ALLOWED, the options the registered products' units
use, with its rule: an --extern path must be a file cargo reported for a
package of the unit graph (a pathless one a sysroot crate's alone); -L takes
native and framework paths, and a dependency path only to cargo's deps/
(a crate or all path would let a bare `extern crate` load an allocator no
--extern names); --emit no output path; --target the row's triple; -C only
the CODEGEN keys (linker and link-arg reach only the native linker, whose
inputs are BL-92's residual, not the canaries'). Any other option,
`--sysroot`, `-Z`, an @argfile or `--` fails the row, naming the option and
the unit or build script that gave it.

Consumers outside the registry are out of scope. So are the nested
workspaces, which are harnesses rather than products, and RUSTFLAGS a
product's build might add (cargo hfuzz's instrumentation): the canary uses the
workspace's own cargo configuration. A C library that replaces malloc sets no
#[global_allocator]: link-probe.py requires every native package in a shared
or pure crate's closure to be reviewed in layers.toml's [[native]] (DEC-188
(3)); a product's other native packages (ring, rocksdb, jemalloc in its own
row) are not reviewed. Named packages restrict the canaries to their rows. The registry check
always covers every row.

As RUSTC, with DASH_ALLOC_CANARY_RUSTC set, this script is the wrapper
through which cargo compiles the product (see wrap()).
"""

import json
import os
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONFLICT = "conflicts with global allocator in:"
TAG = "(SB0 L5; DEC-181, DEC-188)"
PRODUCT_KINDS = {"cdylib", "staticlib", "bin"}
HARNESS_KINDS = ("example", "test", "bench")
ROW_KEYS = {"package", "targets", "features", "default-features", "profile", "triples", "allocator", "kind"}
DEFAULTS = {"features": [], "default-features": True, "profile": "release"}
# -L kinds; a bare path is `all`. crate, dependency and all paths load crates.
SEARCH_KINDS = ("native", "framework", "crate", "dependency", "all")
# rustc's options as its getopts table parses them (`rustc --help -v`, 1.98.1):
# each spelling's canonical name and whether it takes a value.
SHORT = {"h": ("--help", False), "L": ("-L", True), "l": ("-l", True), "g": ("-g", False), "O": ("-O", False),
         "o": ("-o", True), "A": ("-A", True), "W": ("-W", True), "D": ("-D", True), "F": ("-F", True),
         "C": ("-C", True), "V": ("--version", False), "v": ("--verbose", False), "Z": ("-Z", True)}
LONG = {"help": ("--help", False), "test": ("--test", False), "version": ("--version", False),
        "verbose": ("--verbose", False), "allow": ("-A", True), "warn": ("-W", True), "deny": ("-D", True),
        "forbid": ("-F", True), "codegen": ("-C", True)} | {
    name: (f"--{name}", True) for name in (
        "cfg", "check-cfg", "crate-type", "crate-name", "edition", "emit", "print", "out-dir", "explain", "target",
        "force-warn", "cap-lints", "extern", "sysroot", "error-format", "json", "color", "diagnostic-width",
        "remap-path-prefix", "remap-path-scope")}
# DEC-188 am. 1: exactly the options the 30 registered products' rustc units
# use (a cold capture of every unit, 2026-10-11), each with its rule (see
# check()); anything else fails closed. The -C keys likewise; linker and
# link-arg reach only the native linker (BL-92).
ALLOWED = {"--extern": "extern", "-L": "search", "--target": "target", "-C": "codegen", "--emit": "emit",
           "": "input"} | {
    name: "any" for name in (
        "-l", "--cfg", "--check-cfg", "--crate-type", "--crate-name", "--edition", "--error-format",
        "--json", "--cap-lints", "--out-dir", "-W", "-A", "-D")}
CODEGEN = {"opt-level", "embed-bitcode", "debuginfo", "debug-assertions", "codegen-units", "strip", "metadata",
           "extra-filename", "linker", "link-arg", "prefer-dynamic"}
# The options and -C keys of the product unit the canary link reuses.
LINK_OPTIONS, LINK_CODEGEN = ("-L", "--target"), ("linker", "link-arg")
# The product unit's options its rlib's compile drops.
RLIB_DROPPED = ("--crate-type", "--out-dir", "-o", "--emit", "--error-format", "--json", "--cap-lints")
# A build-script-executed record's keys and the options they give the package's units.
SCRIPT_OPTIONS = (("linked_paths", "-L"), ("linked_libs", "-l"), ("cfgs", "--cfg"))


def wrap(args):
    """RUSTC for the canary build. Every unit cargo compiles (CARGO_CRATE_NAME
    set; a build script's own rustc runs are not units) has its arguments and
    working directory recorded in DASH_ALLOC_CANARY_DIR/units/. A unit carrying
    `--cfg <marker>` is the product's: it compiles as cargo asked, without the
    marker, then again as an rlib into DASH_ALLOC_CANARY_DIR, recording its
    arguments in unit.json. Anything else runs rustc unchanged."""
    rustc, marker = os.environ["DASH_ALLOC_CANARY_RUSTC"], os.environ["DASH_ALLOC_CANARY_CFG"]
    out = Path(os.environ["DASH_ALLOC_CANARY_DIR"])
    at = next((i for i in range(len(args) - 1) if args[i] == "--cfg" and args[i + 1] == marker), None)
    if at is None:
        if os.environ.get("CARGO_CRATE_NAME"):
            (out / "units").mkdir(exist_ok=True)
            (out / "units" / f"{time.time_ns()}-{os.getpid()}.json").write_text(
                json.dumps({"cwd": os.getcwd(), "args": args}))
        os.execv(rustc, [rustc, *args])
    args = args[:at] + args[at + 2:]
    status = subprocess.run([rustc, *args]).returncode
    if status:
        return status
    (out / "unit.json").write_text(json.dumps({"cwd": os.getcwd(), "args": args}))
    pairs, bad = tokens(args)
    if bad:
        return 0  # canary() refuses the unit, before it looks for the rlib
    rlib = [arg for option, value, _ in pairs if option not in RLIB_DROPPED
            and not (option == "-C" and codegen_key(value) == "incremental") for arg in spelled(option, value)]
    rlib += ["--crate-type", "rlib", "--emit=link", "--out-dir", str(out), "--cap-lints", "allow"]
    built = subprocess.run([rustc, *rlib], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (out / "rlib.log").write_text(built.stdout)
    if built.returncode:
        sys.stderr.write(f"the product unit does not compile as an rlib:\n{built.stdout}")
        return built.returncode
    return 0


def names(value):
    return isinstance(value, list) and value and all(isinstance(v, str) and v for v in value)


def inventory(meta):
    """Every member target that builds a cdylib, staticlib or bin, by
    (package, target, class): "lib", "bin" or its harness kind."""
    members = set(meta["workspace_members"])
    products = {}
    for p in meta["packages"]:
        if p["id"] not in members:
            continue
        for t in p["targets"]:
            kinds = set(t["kind"])
            harness = next((k for k in HARNESS_KINDS if k in kinds), None)
            # An example's library crate types are in crate_types, not kind.
            types = PRODUCT_KINDS & set(t["crate_types"] if harness else t["kind"])
            if types:
                products[(p["name"], t["name"], harness or ("bin" if "bin" in kinds else "lib"))] = sorted(types)
    return products


def registry(rows, exclusions, meta):
    """The rows' (row, target, class) products, and errors for rows and
    exclusions that do not hold or name one and for products neither names."""
    products = inventory(meta)
    errors, named, resolved = [], set(), []
    for row in rows:
        what = f"layers.toml's [[product]] {row.get('package')!r}"
        if not isinstance(row, dict) or set(row) - ROW_KEYS or not {"package", "targets", "triples"} <= set(row):
            errors.append(f"{what} must have package, targets and triples, and only {sorted(ROW_KEYS)}")
            continue
        if (not (names(row["targets"]) and names(row["triples"]))
                or not isinstance(row.get("features", []), list)
                or not all(isinstance(v, str) for v in row.get("features", []))
                or not isinstance(row.get("default-features", True), bool)
                or not isinstance(row.get("profile", "release"), str)
                or row.get("allocator", "own") != "own" or row.get("kind", "lib") not in ("lib", "bin", "example")):
            errors.append(f"{what}: targets and triples are lists of names, features a list, default-features a "
                          f"bool, profile a name, allocator \"own\" or absent, kind \"lib\", \"bin\", \"example\" "
                          f"or absent")
            continue
        for target in row["targets"]:
            found = [c for c in ([row["kind"]] if "kind" in row else ["lib", "bin", "example"])
                     if (row["package"], target, c) in products]
            if not found:
                errors.append(f"{what} names {target}, which is no cdylib, staticlib or bin target of a workspace "
                              f"member")
            elif len(found) > 1:
                errors.append(f"{what} names {target}, which is a target of more than one kind "
                              f"({', '.join(found)}): give kind")
            else:
                named.add((row["package"], target, found[0]))
                resolved.append((DEFAULTS | row, target, found[0]))
    by_kind = {}
    for ex in exclusions:
        what = f"layers.toml's [[product-exclusion]] {ex!r}"
        if not isinstance(ex, dict) or not isinstance(ex.get("reason"), str) or not ex["reason"].strip():
            errors.append(f"{what} must give a reason")
        elif set(ex) == {"kind", "reason"} and ex["kind"] in HARNESS_KINDS:
            by_kind[ex["kind"]] = ex["reason"]
        elif set(ex) == {"package", "targets", "reason"} and isinstance(ex["package"], str) and names(ex["targets"]):
            for target in ex["targets"]:
                found = [k for k in products if k[:2] == (ex["package"], target)]
                if not found:
                    errors.append(f"{what} names {target}, which is no cdylib, staticlib or bin target of a "
                                  f"workspace member")
                named.update(found)
        else:
            errors.append(f"{what} is either kind (\"example\", \"test\" or \"bench\") and reason, or package, "
                          f"targets and reason")
    for key, types in sorted(products.items()):
        package, target, cls = key
        if key in named or (cls in by_kind and types == ["bin"]):
            continue
        kind = f" ({cls})" if cls in HARNESS_KINDS else ""
        errors.append(f"{package}'s {'/'.join(types)} target {target}{kind} is not in layers.toml's [[product]] "
                      f"registry or its exclusions")
    return errors, resolved


def inode(path):
    """A file's identity, the same for its hard links."""
    st = os.stat(path)
    return st.st_dev, st.st_ino


def spell(package_id):
    """A cargo package id as `name vX (source)`."""
    source, _, spec = package_id.rpartition("#")
    name, _, version = spec.rpartition("@")
    return f"{name or source.rstrip('/').rsplit('/', 1)[-1]} v{version} ({source})"


def conflict_in(text):
    """The crate a `conflicts with global allocator in:` line names, else None."""
    return next((l.split(CONFLICT, 1)[1].strip() for l in text.splitlines() if CONFLICT in l), None)


def link(out, name, externs, flags, rustc):
    """Links a canary that sets its own allocator and loads `externs`
    (alias, path); its compiler output, None if it links."""
    main = out / f"{name}.rs"
    main.write_text("".join(f"extern crate {alias};\n" for alias, _ in externs) +
                    "#[global_allocator] static DASH_ALLOC_CANARY: std::alloc::System = std::alloc::System;\n"
                    "fn main() {}\n")
    argv = [rustc, "--edition", "2021", "--crate-type", "bin", "-o", str(out / name), str(main), *flags]
    for alias, path in externs:
        argv += ["--extern", f"{alias}={path}"]
    linked = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return None if linked.returncode == 0 else linked.stdout


def search(entry):
    """A -L value as (kind, path)."""
    kind, sep, path = entry.partition("=")
    return (kind, path) if sep and kind in SEARCH_KINDS else ("all", entry)


def tokens(args):
    """rustc's argv as getopts reads it (DEC-188 am. 1 (1)): (option, value,
    spelling) for `--opt val`, `--opt=val`, `-Xval`, `-X val` and grouped
    short flags, with option "" for an input; or a refusal for an argument
    no such spelling covers, an @argfile or `--`."""
    # rustc expands an @argfile wherever it stands, an option's value included.
    argfile = next((arg for arg in args if arg.startswith("@")), None)
    if argfile:
        return [], f"the @argfile {argfile!r}"
    out, i = [], 0
    while i < len(args):
        arg, i = args[i], i + 1
        if arg == "--":
            return out, "`--`"
        if arg.startswith("--"):
            name, eq, value = arg[2:].partition("=")
            option, takes = LONG.get(name, (None, False))
            if option is None or eq and not takes:
                return out, f"the option {arg!r}"
            if not takes:
                out.append((option, None, arg))
            elif eq:
                out.append((option, value, arg))
            elif i < len(args):
                out.append((option, args[i], f"{arg} {args[i]}"))
                i += 1
            else:
                return out, f"the option {arg!r} without a value"
        elif arg.startswith("-") and arg != "-":
            at = 1
            while at < len(arg):
                if arg[at] not in SHORT:
                    return out, f"the option {arg!r}"
                option, takes = SHORT[arg[at]]
                at += 1
                if not takes:
                    out.append((option, None, arg))
                elif at < len(arg):
                    out.append((option, arg[at:], arg))
                    break
                elif i < len(args):
                    out.append((option, args[i], f"{arg} {args[i]}"))
                    i += 1
                    break
                else:
                    return out, f"the option {arg!r} without a value"
        else:
            out.append(("", arg, arg))
    return out, None


def spelled(option, value):
    """A token as rustc arguments."""
    return [value] if option == "" else [option] if value is None else [option, value]


def option_value(pairs, name, *default):
    """The value of the first `name` option of `pairs`."""
    return next((value for option, value, _ in pairs if option == name), *default)


def codegen_key(value):
    """A -C value's key, as rustc normalizes it."""
    return value.split("=", 1)[0].replace("_", "-")


def refusals(unit, source, context):
    """The reasons `unit`'s arguments ({"cwd", "args"}) fall outside the
    allow-list (DEC-188 am. 1 (2)), naming each and `source`."""
    pairs, bad = tokens(unit["args"])
    reasons = [f"{source} gives rustc {bad}, which the allow-list does not take"] if bad else []
    for option, value, spelling in pairs:
        why = check(option, value, Path(unit["cwd"]), context)
        if why:
            reasons.append(f"{source} gives rustc `{spelling}`: {why}")
    return reasons


def check(option, value, cwd, context):
    """None if the allow-list takes `option` with `value`, else why not."""
    rule = ALLOWED.get(option)
    if rule is None:
        return f"{option} is not an allow-listed option"
    if rule == "codegen":
        key = codegen_key(value)
        return None if key in CODEGEN else f"-C {key} is not an allow-listed codegen option"
    if rule == "emit":
        return "an output path" if "=" in value else None
    if rule == "target":
        return None if value == context["triple"] else f"the target is not the row's {context['triple']}"
    if rule == "search":
        kind, path = search(value)
        if kind in ("crate", "all") or kind == "dependency" and (cwd / path).resolve() not in context["deps"]:
            return ("a crate search path other than a dependency path to cargo's deps/, through which a bare "
                    "`extern crate` can load an allocator no --extern names")
    if rule == "extern":
        name, eq, path = value.partition("=")
        if ":" in name or not name.isidentifier():
            return "an extern other than `name` or `name=path`"
        if not eq:
            if name in context["sysroot"] and not any(any(d.glob(f"lib{name}-*")) for d in context["deps"]):
                return None
            return "a pathless extern that is no sysroot crate's alone"
        file = cwd / path
        if not file.is_file() or owned(file, context["owner"]) is None:
            return "a file that is no Cargo artifact of a package in the unit graph"
    return None


def owned(file, owner):
    """The package id of a Cargo artifact (an .rmeta by its .rlib), else None."""
    for f in (file, file.with_suffix(".rlib")):
        if f.is_file() and inode(f) in owner:
            return owner[inode(f)]
    return None


def allow_list(unit, out, scripts, triple, owner, by_crate, rustc):
    """The reasons the arguments of the product unit, of every other unit the
    canary build compiled (the rest are fresh: cargo's fingerprint holds
    their rustflags, which the product unit's are) and of the build scripts'
    outputs fall outside the allow-list (DEC-188 am. 1)."""
    # The product's profile directory holds deps/ (and examples/, an example's
    # --out-dir); a cross build's units also search the host's, for proc
    # macros and build scripts.
    out_dir = option_value(tokens(unit["args"])[0], "--out-dir", None)
    deps = set()
    if out_dir and Path(out_dir).name in ("deps", "examples"):
        profile = (Path(unit["cwd"]) / out_dir).resolve().parent
        deps = {profile / "deps"} | ({profile.parents[1] / profile.name / "deps"} if triple != "host" else set())
    context = {"triple": triple, "owner": owner, "deps": deps, "sysroot": sysroot_crates(rustc, triple)}
    reasons = refusals(unit, "the product unit", context)
    for f in sorted((out / "units").glob("*.json")):
        other = json.loads(f.read_text())
        name = option_value(tokens(other["args"])[0], "--crate-name", "?")
        ids = ", ".join(map(spell, sorted(by_crate.get(name, ())))) or "no package cargo built"
        reasons += refusals(other, f"the {name} unit ({ids})", context)
    for record in scripts:
        script = [arg for key, option in SCRIPT_OPTIONS for value in record[key] for arg in (option, value)]
        reasons += refusals({"cwd": unit["cwd"], "args": script}, f"{spell(record['package_id'])}'s build script",
                            context)
    return reasons


def compiled(out):
    """The artifact stems (`lib<crate><extra-filename>`) of the units the
    canary build compiled."""
    stems = set()
    for f in (out / "units").glob("*.json"):
        pairs = tokens(json.loads(f.read_text())["args"])[0]
        extra = [v.partition("=")[2] for o, v, _ in pairs if o == "-C" and codegen_key(v) == "extra-filename"]
        stems.add(f"lib{option_value(pairs, '--crate-name', '')}{extra[-1] if extra else ''}")
    return stems


def sysroot_crates(rustc, triple):
    """The crate names of the sysroot's libraries for `triple` and the host."""
    names = set()
    for target in {"host", triple}:
        where = [rustc, "--print", "target-libdir"] + ([] if target == "host" else ["--target", target])
        libdir = Path(subprocess.run(where, cwd=ROOT, stdout=subprocess.PIPE, check=True, text=True).stdout.strip())
        names |= {f.name[3:].split("-", 1)[0] for f in libdir.glob("lib*.rlib")}
    return names


def canary(row, target, kind, triple, package_id, rustc):
    """None if the row holds for `target` on `triple`, else the reason."""
    features = ",".join(row["features"])
    parts = [features.replace(",", "+").replace("/", "."), "" if row["default-features"] else "no-default"]
    out = ROOT / "target" / "node-alloc-canary" / triple / row["package"] / f"{kind}-{target}" / (
        "-".join(p for p in parts if p) or "default")
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    # A marker new to each run makes cargo recompile the product unit (cargo
    # rustc's arguments are in its fingerprint), so the wrapper always runs.
    marker = f"dash_alloc_canary_{time.time_ns()}"
    if row.get("allocator") == "own":
        # Cargo's fingerprint omits RUSTC_WRAPPER, so a fresh unit of the own
        # package may hold arguments an earlier build's wrapper added; cleaning
        # the package (under cargo's lock) makes cargo compile its units
        # through the wrapper, which records them.
        clean = ["cargo", "clean", "-q", "-p", row["package"], "--profile", row["profile"]]
        subprocess.run(clean + ([] if triple == "host" else ["--target", triple]), cwd=ROOT, check=True)
    select = {"lib": ["--lib"], "bin": ["--bin", target], "example": ["--example", target]}[kind]
    argv = ["cargo", "rustc", "--locked", "-q", "--message-format=json", "-p", row["package"], *select,
            "--profile", row["profile"]]
    if not row["default-features"]:
        argv.append("--no-default-features")
    if features:
        argv += ["--features", features]
    if triple != "host":
        argv += ["--target", triple]
    argv += ["--", "--cfg", marker]
    env = os.environ | {"RUSTC": str(Path(__file__).resolve()), "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": "",
                        "DASH_ALLOC_CANARY_RUSTC": rustc, "DASH_ALLOC_CANARY_CFG": marker,
                        "DASH_ALLOC_CANARY_DIR": str(out)}
    built = subprocess.run(argv, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    # Every unit cargo built or found fresh: its files' package ids, by inode
    # (cargo reports the copies it hard-links up, rustc gets deps/), and the
    # package ids by crate name, which is all rustc's diagnostic names.
    owner, by_crate, rendered, scripts = {}, {}, [], []
    for line in built.stdout.splitlines():
        msg = json.loads(line) if line.startswith("{") else {}
        if msg.get("reason") == "compiler-artifact":
            for f in msg["filenames"]:
                owner[inode(f)] = msg["package_id"]
            by_crate.setdefault(msg["target"]["name"].replace("-", "_"), set()).add(msg["package_id"])
        elif msg.get("reason") == "build-script-executed":
            scripts.append({key: msg[key] for key in ("package_id", "linked_paths", "linked_libs", "cfgs")})
        elif msg.get("reason") == "compiler-message" and msg["message"].get("rendered"):
            rendered.append(msg["message"]["rendered"])
    diagnostics = "".join(rendered) + built.stderr
    (out / "build-scripts.json").write_text(json.dumps(scripts))

    def named(crate, besides=None):
        ids = sorted(by_crate.get(crate, set()) - {besides})
        return f"{crate} ({', '.join(map(spell, ids)) or 'no package cargo built'})"

    if built.returncode:
        conflict = conflict_in(diagnostics)
        if conflict:
            return f"the product does not build: it installs two global allocators: {named(conflict)}"
        return "the product does not build:\n" + diagnostics
    if not (out / "unit.json").exists():
        return "cargo did not compile the product's unit through the canary wrapper"
    unit = json.loads((out / "unit.json").read_text())
    reasons = allow_list(unit, out, scripts, triple, owner, by_crate, rustc)
    if reasons:
        return "; ".join(reasons)
    pairs = tokens(unit["args"])[0]
    crate = option_value(pairs, "--crate-name")
    rlibs = list(out.glob(f"lib{crate}*.rlib"))
    if len(rlibs) != 1:
        return f"expected one rlib of {crate} in {out}, found {len(rlibs)}"
    flags = [arg for option, value, _ in pairs
             if option in LINK_OPTIONS or option == "-C" and codegen_key(value) in LINK_CODEGEN
             for arg in spelled(option, value)]
    externs = []
    for option, value, _ in pairs:
        name, eq, path = value.partition("=") if option == "--extern" else ("", "", "")
        if eq:
            externs.append((name, Path(unit["cwd"]) / path))
    own = row.get("allocator") == "own"
    failure = link(out, "canary", [(crate, rlibs[0])], flags, rustc)
    if failure is None:
        return "declares its own allocator, but the canary links" if own else None
    conflict = conflict_in(failure)
    if conflict is None:
        return "the canary does not link:\n" + failure
    if not own:
        return f"installs a global allocator: {named(conflict)}"
    # DEC-188 (1): the allocator is the product package's own when every
    # dependency the unit is given, its own package's aside, links without one.
    # A unit cargo finds fresh may hold arguments an earlier build's
    # RUSTC_WRAPPER added (the fingerprint omits it). Another package's crates
    # reach both canaries through its rlib, but the own package's are left out
    # of the second, so each must have been compiled, and checked, in this run.
    stale = [str(path) for alias, path in externs if owned(path, owner) == package_id
             and path.with_suffix("").name not in compiled(out)]
    if stale:
        return (f"the product's own package's {', '.join(stale)} is fresh: cargo did not compile it in this run, "
                f"so its arguments are not checked")
    others = [(alias, path) for alias, path in externs if owned(path, owner) != package_id]
    failure = link(out, "dependencies", others, flags, rustc)
    if failure is None:
        return None
    conflict = conflict_in(failure)
    if conflict is None:
        return "the canary over the product's dependencies does not link:\n" + failure
    return f"a global allocator in {named(conflict, package_id)}, not the product's own package {spell(package_id)}"


def main(argv):
    if os.environ.get("DASH_ALLOC_CANARY_RUSTC"):
        return wrap(argv)
    if "--registry" in argv and argv != ["--registry"]:
        sys.exit(__doc__)
    meta = json.loads(subprocess.run(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
                                     cwd=ROOT, stdout=subprocess.PIPE, check=True, text=True).stdout)
    spec = tomllib.loads((ROOT / "contrib" / "node" / "layers.toml").read_text())
    rows = spec.get("product", [])
    errors, products = registry(rows, spec.get("product-exclusion", []), meta)
    errors += [f"{p} matches no [[product]] row" for p in argv if p != "--registry"
               and not any(isinstance(r, dict) and r.get("package") == p for r in rows)]
    for error in errors:
        print(f"error: {error} {TAG}", file=sys.stderr)
    if errors:
        return 1
    if argv == ["--registry"]:
        return 0
    rustc = os.environ.get("RUSTC") or shutil.which("rustc")
    ids = {p["name"]: p["id"] for p in meta["packages"] if p["id"] in set(meta["workspace_members"])}
    failed = False
    for row, target, kind in products:
        if argv and row["package"] not in argv:
            continue
        for triple in row["triples"]:
            what = (f"{row['package']} {target} ({triple}, {','.join(row['features']) or 'default features'}"
                    f"{'' if row['default-features'] else ' only'})")
            began = time.monotonic()
            reason = canary(row, target, kind, triple, ids[row["package"]], rustc)
            if reason:
                print(f"error: {what}: {reason} {TAG}", file=sys.stderr, flush=True)
                failed = True
            else:
                verdict = "its own package's allocator only" if row.get("allocator") else "the canary links"
                print(f"ok: {what}: {verdict} ({time.monotonic() - began:.0f} s)", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
