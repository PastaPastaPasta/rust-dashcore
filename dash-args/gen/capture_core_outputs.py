#!/usr/bin/env python3
"""Capture the Dash Core outputs dash-args is tested against, from the release binaries.

Usage:
    python3 -I capture_core_outputs.py --tarball dashcore-24.0.0-rc.5-x86_64-linux-gnu.tar.gz \
        --core-src <v24.0.0-rc.5 src dir> --work <empty scratch dir> \
        --out dash-args/tests/data/core-v24 [--qt-libs <dir>] [--skip-qt]
    python3 -I capture_core_outputs.py --win-zip dashcore-24.0.0-rc.5-win64.zip \
        --work <empty scratch dir> --out dash-args/tests/data/core-v24

Writes into --out:
  help/<program>-help[-debug].txt   `-help` / `-help-debug` output (dash-qt needs an X server:
                                    run under xvfb-run, with --qt-libs naming a directory of
                                    extra libxcb libraries if the host lacks them)
  args_merge.txt, chain_merge.txt,  the outputs of test_dash's argsman_tests/util_ArgsMerge,
  settings_merge.txt                argsman_tests/util_ChainMerge and settings_tests/Merge
  recognized.txt                    every option name of the generated tables, as accepted
                                    or rejected by the program's command-line parser
  init_cases.json                   dashd's exit code, stderr and resulting files for the
                                    command line and config cases in CASES (run with -version,
                                    which exits right after config initialization)

With --win-zip, runs the win64 release under `wine` instead and writes:
  help-win64/<program>-help[-debug].txt   as help/, read with universal newlines (no
                                          dash-qt: on Windows it shows its help in a dialog)
  init_cases_win64.json                   the CASES that name -datadir, then WIN_CASES, with
                                          {T} a Windows path ({D} its drive, {R} the rest)

See PROVENANCE.md.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

sys.dont_write_bytecode = True  # no __pycache__ in the source tree
sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_options  # noqa: E402

TARBALL_SHA256 = "54cc952f79e4733d20b950c93b9c0824ae4410acf9a4972cd9361e2a107cb62d"
WIN_ZIP_SHA256 = "e6087458b618dd260a40c04257e88ed6f46da03e40f955d7a8cf1bd0937ef7f6"
VERSION_DIR = "dashcore-24.0.0-rc.5"

# Each case: name, argv (after the program name and -version; "{T}" is the case's scratch
# root) and files to create (relative path -> contents; a trailing "/" makes a directory).
# {T}/d and {T}/home (HOME, so the default datadir is {T}/home/.dashcore) always exist.
CASES = [
    # Datadir and config file resolution.
    ("datadir_missing", ["-datadir={T}/missing"], {}),
    ("datadir_ok_creates_files", ["-datadir={T}/d"], {}),
    ("datadir_trailing_slash", ["-datadir={T}/d/"], {}),
    ("conf_missing_absolute", ["-datadir={T}/d", "-conf={T}/none.conf"], {}),
    ("conf_missing_relative", ["-datadir={T}/d", "-conf=none.conf"], {}),
    ("conf_relative_to_datadir", ["-datadir={T}/d", "-conf=other.conf"], {"d/other.conf": "regtest=1\n"}),
    ("conf_datadir_missing", ["-conf={T}/c.conf"], {"c.conf": "datadir={T}/new\n"}),
    ("conf_datadir_exists", ["-conf={T}/c.conf"], {"c.conf": "datadir={T}/new\n", "new/": ""}),
    ("cmdline_datadir_overrides_conf", ["-datadir={T}/d", "-conf={T}/c.conf"],
     {"c.conf": "datadir={T}/new\n", "new/": ""}),
    ("ignored_conf", ["-datadir={T}/d", "-conf={T}/other.conf"], {"d/dash.conf": "", "other.conf": ""}),
    ("ignored_conf_allowed", ["-datadir={T}/d", "-conf={T}/other.conf", "-allowignoredconf"],
     {"d/dash.conf": "", "other.conf": ""}),
    ("redundant_conf_same_file", ["-datadir={T}/d", "-conf={T}/d/dash.conf"], {"d/dash.conf": ""}),
    ("ignored_default_conf", [], {"home/.dashcore/dash.conf": "datadir={T}/d\n", "d/dash.conf": ""}),
    ("default_datadir", [], {"home/.dashcore/": ""}),
    ("default_datadir_missing", [], {}),
    # Config file parser.
    ("conf_sets_conf", ["-datadir={T}/d"], {"d/dash.conf": "conf=some.conf\n"}),
    ("included_sets_conf", ["-datadir={T}/d"], {"d/dash.conf": "includeconf=inc.conf\n", "d/inc.conf": "conf=some.conf\n"}),
    ("unknown_conf_key_ignored", ["-datadir={T}/d"], {"d/dash.conf": "dash_conf=1\n"}),
    ("reindex_in_conf", ["-datadir={T}/d"], {"d/dash.conf": "reindex=0\n"}),
    ("leading_dash_in_conf", ["-datadir={T}/d"], {"d/dash.conf": "-dash=1\n"}),
    ("bare_negation_in_conf", ["-datadir={T}/d"], {"d/dash.conf": "nono\n"}),
    ("bare_word_in_conf", ["-datadir={T}/d"], {"d/dash.conf": "\n\nserver\n"}),
    ("rpcpassword_hash", ["-datadir={T}/d"], {"d/dash.conf": "server=1\nrpcuser=someuser\nrpcpassword=some#pass"}),
    ("rpcpassword_hash_dotted", ["-datadir={T}/d"], {"d/dash.conf": "server=1\nrpcuser=someuser\nmain.rpcpassword=some#pass"}),
    ("rpcpassword_hash_section", ["-datadir={T}/d"], {"d/dash.conf": "server=1\nrpcuser=someuser\n[main]\nrpcpassword=some#pass"}),
    ("comments_and_whitespace", ["-datadir={T}/d"], {"d/dash.conf": "  # comment\r\n\t regtest = 1 # trailing\r\n[regtest]\r\n settings = s.json \r\n"}),
    ("negation_forbidden_in_conf", ["-datadir={T}/d"], {"d/dash.conf": "nopowtargetspacing=1\n"}),
    ("double_negative_in_conf", ["-datadir={T}/d"], {"d/dash.conf": "noregtest=0\n"}),
    ("default_section_network_only", ["-datadir={T}/d"], {"d/dash.conf": "regtest=1\nwallet=foo\n"}),
    # Command line parser.
    ("invalid_parameter", ["-datadir={T}/d", "-dash_cli=1"], {}),
    ("dotted_parameter", ["-datadir={T}/d", "-regtest.server=1"], {}),
    ("double_dash", ["--datadir={T}/d", "--regtest"], {}),
    ("lone_dash_stops_parsing", ["-datadir={T}/d", "-", "-dash_cli=1"], {}),
    ("proxy_without_value", ["-datadir={T}/d", "-proxy"], {}),
    ("negation_forbidden", ["-datadir={T}/d", "-nopowtargetspacing"], {}),
    ("double_negative", ["-datadir={T}/d", "-noconnect=0"], {}),
    ("unexpected_token", ["-datadir={T}/d", "foo", "-regtest"], {}),
    ("includeconf_cmdline", ["-datadir={T}/d", "-includeconf=relative2.conf", "-includeconf=no_warn.conf"], {}),
    ("includeconf_cmdline_double_negative", ["-datadir={T}/d", "-noincludeconf=0"], {}),
    ("noincludeconf_cmdline", ["-datadir={T}/d", "-noincludeconf"], {"d/dash.conf": "includeconf=missing.conf\n"}),
    # includeconf.
    ("include_missing", ["-datadir={T}/d"], {"d/dash.conf": "includeconf=relative.conf\n"}),
    ("include_ok", ["-datadir={T}/d"], {"d/dash.conf": "includeconf=relative.conf\n", "d/relative.conf": "regtest=1\n"}),
    ("include_recursive", ["-datadir={T}/d"],
     {"d/dash.conf": "includeconf=relative.conf\n", "d/relative.conf": "includeconf=relative2.conf\n", "d/relative2.conf": ""}),
    ("include_chain_section_first", ["-datadir={T}/d"],
     {"d/dash.conf": "regtest=1\nincludeconf=a.conf\n[regtest]\nincludeconf=b.conf\n",
      "d/a.conf": "settings=from_default_include.json\n", "d/b.conf": "settings=from_chain_include.json\n"}),
    ("include_switches_chain", ["-datadir={T}/d"],
     {"d/dash.conf": "includeconf=a.conf\n[regtest]\nincludeconf=c.conf\n", "d/a.conf": "regtest=1\n", "d/c.conf": ""}),
    ("include_negated_in_conf", ["-datadir={T}/d"],
     {"d/dash.conf": "includeconf=a.conf\nnoincludeconf=1\nincludeconf=b.conf\n", "d/b.conf": "regtest=1\n"}),
    # Chain selection.
    ("chain_regtest", ["-datadir={T}/d", "-regtest"], {}),
    ("chain_testnet", ["-datadir={T}/d", "-testnet"], {}),
    ("chain_arg", ["-datadir={T}/d", "-chain=test"], {}),
    ("chain_unknown", ["-datadir={T}/d", "-chain=foo"], {}),
    ("chain_conflict", ["-datadir={T}/d", "-regtest", "-testnet"], {}),
    ("chain_conflict_chain_arg", ["-datadir={T}/d", "-chain=regtest", "-testnet"], {}),
    ("chain_conflict_conf", ["-datadir={T}/d", "-testnet"], {"d/dash.conf": "regtest=1\n"}),
    ("chain_negated_cmdline_ignored", ["-datadir={T}/d", "-noregtest"], {"d/dash.conf": "regtest=1\n"}),
    ("chain_negated_conf", ["-datadir={T}/d", "-testnet"], {"d/dash.conf": "noregtest=1\n"}),
    ("chain_regtest_zero", ["-datadir={T}/d", "-regtest=0"], {}),
    ("devnet_named", ["-datadir={T}/d", "-devnet=abc"], {}),
    ("devnet_unnamed", ["-datadir={T}/d", "-devnet"], {}),
    ("devnet_conflict", ["-datadir={T}/d", "-devnet=", "-regtest"], {}),
    # settings.json.
    ("settings_values", ["-datadir={T}/d", "-regtest"],
     {"d/regtest/settings.json": '{"string": "string", "num": 5, "bool": true, "null": null, "list": [6, 7]}'}),
    ("settings_invalid_json", ["-datadir={T}/d", "-regtest"], {"d/regtest/settings.json": "invalid json"}),
    ("settings_non_object", ["-datadir={T}/d", "-regtest"], {"d/regtest/settings.json": '"string"'}),
    ("settings_duplicate_key", ["-datadir={T}/d", "-regtest"], {"d/regtest/settings.json": '{"key": 1, "key": 2}'}),
    ("settings_empty_file", ["-datadir={T}/d", "-regtest"], {"d/regtest/settings.json": ""}),
    ("settings_empty_directory", ["-datadir={T}/d", "-regtest"], {"d/regtest/settings.json/": ""}),
    ("settings_nonempty_directory", ["-datadir={T}/d", "-regtest"], {"d/regtest/settings.json/x": ""}),
    ("settings_trailing_garbage", ["-datadir={T}/d"], {"d/settings.json": "{} x"}),
    ("settings_disabled_cmdline", ["-datadir={T}/d", "-regtest", "-nosettings"], {"d/regtest/settings.json": "invalid json"}),
    ("settings_disabled_conf", ["-datadir={T}/d"],
     {"d/dash.conf": "regtest=1\n[regtest]\nnosettings=1\n", "d/regtest/settings.json": "invalid json"}),
    ("settings_alternate_path", ["-datadir={T}/d", "-settings={T}/alt.json"], {"alt.json": '{"key": "value"}'}),
    ("settings_relative_path", ["-datadir={T}/d", "-regtest", "-settings=sub/../alt.json"], {}),
    ("settings_write_fails", ["-datadir={T}/d", "-settings={T}/nodir/s.json"], {}),
    ("settings_warning_kept", ["-datadir={T}/d"], {"d/settings.json": '{"_warning_": "custom", "a": "b"}'}),
    ("settings_escapes", ["-datadir={T}/d"], {"d/settings.json": '{"k": "a\\u0001\\u007f\\"\\\\/\\u00e9\\ud83d\\ude00", "n": -1.50e+3}'}),
]


# Windows-only cases. A file given as {"hardlink": <path>} is a hard link to that file.
WIN_CASES = [
    # Drive-relative, root-relative, verbatim and UNC-like paths (relative to libstdc++).
    ("win_datadir_drive_relative", ["-datadir={D}d"], {"d/dash.conf": "regtest=1\n"}),
    ("win_datadir_root_relative", ["-datadir={R}\\d"], {"d/dash.conf": "regtest=1\n"}),
    ("win_datadir_verbatim", ["-datadir=\\\\?\\{T}\\d"], {}),
    ("win_conf_drive_relative", ["-datadir={T}\\d", "-conf={D}other.conf"], {"d/other.conf": "regtest=1\n"}),
    ("win_conf_root_relative", ["-datadir={T}\\d", "-conf={R}\\c.conf"], {"c.conf": "regtest=1\n"}),
    ("win_conf_double_separator", ["-datadir={T}\\d", "-conf=\\{R}\\c.conf"], {"c.conf": "regtest=1\n"}),
    ("win_include_drive_relative", ["-datadir={T}\\d"], {"d/dash.conf": "includeconf={D}inc.conf\n", "d/inc.conf": "regtest=1\n"}),
    ("win_settings_drive_relative", ["-datadir={T}\\d", "-settings={D}s.json"], {"d/s.json": '{"a": "b"}'}),
    # Hard links are the same file ...
    ("win_hardlinked_conf", ["-datadir={T}\\d", "-conf={T}\\d\\other.conf"],
     {"d/dash.conf": "", "d/other.conf": {"hardlink": "d/dash.conf"}}),
    # ... unless msvcrt's stat modes differ: an executable extension makes them two files.
    ("win_hardlinked_conf_exe", ["-datadir={T}\\d", "-conf={T}\\d\\other.exe"],
     {"d/dash.conf": "", "d/other.exe": {"hardlink": "d/dash.conf"}}),
    # Text-mode streams: input ends at Ctrl-Z, CR LF reads as LF and LF writes as CR LF.
    ("win_conf_ctrl_z", ["-datadir={T}\\d"], {"d/dash.conf": "regtest=1\r\n\x1a\r\nnono\r\n"}),
    ("win_settings_ctrl_z", ["-datadir={T}\\d"], {"d/settings.json": '{"a": "b"}\x1a garbage'}),
    ("win_settings_ctrl_z_first", ["-datadir={T}\\d"], {"d/settings.json": '\x1a{"a": "b"}'}),
    ("win_settings_crlf", ["-datadir={T}\\d"], {"d/settings.json": '{\r\n    "a": "b"\r\n}\r\n'}),
]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


HELP_JOBS = [
    ("dashd-help.txt", ["dashd", "-help"]),
    ("dashd-help-debug.txt", ["dashd", "-help-debug"]),
    ("dash-cli-help.txt", ["dash-cli", "-help"]),
    ("dash-tx-help.txt", ["dash-tx", "-help"]),
    ("dash-wallet-help.txt", ["dash-wallet", "-help"]),
    ("dash-util-help.txt", ["dash-util", "-help"]),
]


def capture_help(bindir, home, out, qt_libs, with_qt):
    out.mkdir(parents=True, exist_ok=True)
    # dash-tx and dash-util have no -datadir; HOME places the default datadir, which must
    # exist (Core cannot create its empty dash.conf otherwise).
    (home / ".dashcore").mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, HOME=str(home))
    for name, (prog, *args) in HELP_JOBS:
        r = run([str(bindir / prog), *args], env=env)
        if r.returncode != 0:
            sys.exit(f"{prog} {args}: exit {r.returncode}: {r.stderr}")
        (out / name).write_text(r.stdout)
    if not with_qt:
        return
    qt_env = dict(env)
    if qt_libs:
        qt_env["LD_LIBRARY_PATH"] = str(qt_libs)
    for name, arg in (("dash-qt-help.txt", "-help"), ("dash-qt-help-debug.txt", "-help-debug")):
        r = run(["xvfb-run", "-a", str(bindir / "dash-qt"), arg], env=qt_env)
        if r.returncode != 0 or "Options:" not in r.stdout:
            sys.exit(f"dash-qt {arg}: exit {r.returncode}: {r.stderr}")
        (out / name).write_text(r.stdout)


def capture_merge(bindir, work, out):
    for env_var, test, name in (
        ("ARGS_MERGE_TEST_OUT", "argsman_tests/util_ArgsMerge", "args_merge.txt"),
        ("CHAIN_MERGE_TEST_OUT", "argsman_tests/util_ChainMerge", "chain_merge.txt"),
        ("SETTINGS_MERGE_TEST_OUT", "settings_tests/Merge", "settings_merge.txt"),
    ):
        target = work / name
        r = run([str(bindir / "test_dash"), f"--run_test={test}"], env=dict(os.environ, **{env_var: str(target)}))
        if r.returncode != 0:
            sys.exit(f"test_dash {test}: exit {r.returncode}: {r.stdout}{r.stderr}")
        shutil.copyfile(target, out / name)


def capture_recognized(bindir, core_src, work, out):
    """Check every option name of the tables against the real parsers."""
    functions = gen_options.parse_functions(core_src, gen_options.UNIX)
    programs = {
        "dashd": ["setup_server_args"],
        "dash-cli": ["setup_cli_args"],
        "dash-tx": ["setup_bitcoin_tx_args"],
        "dash-wallet": ["setup_wallet_tool_args"],
        "dash-util": ["setup_bitcoin_util_args"],
    }
    lines = []
    for prog, roots in programs.items():
        dd = work / f"recognized-{prog}"
        (dd / ".dashcore").mkdir(parents=True)
        cats = gen_options.available_args(gen_options.flatten(functions, roots))
        names = sorted({name for cat, entries in cats.items() if cat != "COMMANDS" or prog != "dash-tx"
                        for name in entries}, key=lambda s: s.encode())
        for name in names:
            if name.startswith("-"):
                argv = [f"{name}=1", "-version"]
            elif "COMMANDS" in cats and name in cats["COMMANDS"]:
                argv = ["-version", name]
            else:
                continue
            r = run([str(bindir / prog), *argv], env=dict(os.environ, HOME=str(dd)))
            if r.returncode < 0:
                sys.exit(f"{prog} {argv}: killed by signal {-r.returncode}")
            rejected = "Invalid parameter" in r.stderr or "Invalid command" in r.stderr
            lines.append(f"{prog} {name} {'rejected' if rejected else 'accepted'}\n")
        # A control: an unknown option must be rejected.
        r = run([str(bindir / prog), "-nosuchoption", "-version"], env=dict(os.environ, HOME=str(dd)))
        if "Invalid parameter -nosuchoption" not in r.stderr:
            sys.exit(f"{prog}: unknown option not rejected: {r.stderr}")
    (out / "recognized.txt").write_text("".join(lines))


def tree(root):
    """Files and directories under root: path -> contents (text, line ends as stored) or None
    for a directory."""
    result = {}
    for dirpath, dirnames, filenames in os.walk(root):
        for d in dirnames:
            result[os.path.relpath(os.path.join(dirpath, d), root)] = None
        for f in filenames:
            p = os.path.join(dirpath, f)
            with open(p, newline="") as f:
                result[os.path.relpath(p, root)] = f.read()
    return dict(sorted(result.items()))


def capture_cases(dashd, cases, work, out, name, windows=False):
    """Run each case's dashd -version in a fresh directory; record exit code, stderr and files."""
    results = []
    for case, argv, files in cases:
        root = work / "cases" / case
        root.mkdir(parents=True)
        (root / "d").mkdir()
        (root / "home").mkdir()
        t = "Z:" + str(root).replace("/", "\\") if windows else str(root)
        fill = lambda s: s.replace("{T}", t).replace("{D}", t[:2]).replace("{R}", t[2:])
        for rel, contents in files.items():
            path = root / rel
            if rel.endswith("/"):
                path.mkdir(parents=True, exist_ok=True)
            elif isinstance(contents, dict):
                os.link(root / contents["hardlink"], path)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with open(path, "w", newline="") as f:
                    f.write(fill(contents))
        before = tree(root)
        # -version first: anything after a lone "-" or a command token is not parsed.
        args = ["-version"] + [fill(a) for a in argv]
        # HOME names the default datadir on Unix; on Windows it is Wine's, which would create
        # its desktop folders under a new one.
        env = os.environ if windows else dict(os.environ, HOME=str(root / "home"))
        r = run([*dashd, *args], env=env, cwd=root)
        after = tree(root)
        results.append({
            "name": case,
            "argv": ["-version"] + argv,
            "files": files,
            "exit": r.returncode,
            "stderr": r.stderr.replace(t, "{T}"),
            "created": {k: (v.replace(t, "{T}") if v is not None else None)
                        for k, v in after.items() if k not in before},
            "changed": {k: v.replace(t, "{T}") for k, v in after.items()
                        if k in before and v is not None and before[k] != v},
            "removed": sorted(k for k in before if k not in after),
        })
    (out / name).write_text(json.dumps({"cases": results}, indent=1, ensure_ascii=False) + "\n")


def capture_windows(zip_path, work, out):
    """The win64 release under wine: help (no dash-qt) and the cases that name -datadir."""
    with zipfile.ZipFile(zip_path) as zf:
        zf.extractall(work / "x")
    bindir = work / "x" / VERSION_DIR / "bin"
    # dash-tx and dash-util have no -datadir, and the default datadir (in the shell's
    # application data folder) must exist.
    r = run(["wine", "cmd", "/c", "if not exist %APPDATA%\\DashCore mkdir %APPDATA%\\DashCore"])
    if r.returncode != 0:
        sys.exit(f"cannot create the default datadir: {r.stderr}")
    help_out = out / "help-win64"
    help_out.mkdir(parents=True, exist_ok=True)
    for name, (prog, *args) in HELP_JOBS:
        r = run(["wine", str(bindir / f"{prog}.exe"), *args])
        if r.returncode != 0:
            sys.exit(f"{prog} {args}: exit {r.returncode}: {r.stderr}")
        (help_out / name).write_text(r.stdout)
    cases = [c for c in CASES if any(a.startswith(("-datadir", "--datadir")) for a in c[1])] + WIN_CASES
    capture_cases(["wine", str(bindir / "dashd.exe")], cases, work, out, "init_cases_win64.json", windows=True)


def main():
    ap = argparse.ArgumentParser()
    release = ap.add_mutually_exclusive_group(required=True)
    release.add_argument("--tarball", type=Path)
    release.add_argument("--win-zip", type=Path)
    ap.add_argument("--core-src", type=Path)
    ap.add_argument("--work", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--qt-libs", type=Path)
    ap.add_argument("--skip-qt", action="store_true", help="keep the existing dash-qt captures")
    a = ap.parse_args()

    if a.work.exists() and any(a.work.iterdir()):
        sys.exit("--work must be empty")
    if a.win_zip:
        if sha256(a.win_zip) != WIN_ZIP_SHA256:
            sys.exit("zip sha256 mismatch")
        a.work.mkdir(parents=True, exist_ok=True)
        a.out.mkdir(parents=True, exist_ok=True)
        capture_windows(a.win_zip, a.work, a.out)
        return
    if a.core_src is None:
        sys.exit("--core-src is required with --tarball")
    if sha256(a.tarball) != TARBALL_SHA256:
        sys.exit("tarball sha256 mismatch")
    a.work.mkdir(parents=True, exist_ok=True)
    with tarfile.open(a.tarball) as tf:
        tf.extractall(a.work / "x", filter="data")
    bindir = a.work / "x" / VERSION_DIR / "bin"
    home = a.work / "home"
    home.mkdir()

    a.out.mkdir(parents=True, exist_ok=True)
    capture_help(bindir, home, a.out / "help", a.qt_libs, not a.skip_qt)
    capture_merge(bindir, a.work, a.out)
    capture_recognized(bindir, a.core_src, a.work, a.out)
    capture_cases([str(bindir / "dashd")], CASES, a.work, a.out, "init_cases.json")


if __name__ == "__main__":
    main()
