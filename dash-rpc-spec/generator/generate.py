#!/usr/bin/env python3
"""Generate dash-rpc-spec's table (src/generated.rs) from Dash Core.

Inputs, all produced by Dash Core (see ../PROVENANCE.md):
- the rpcspec_dump JSON (`build-dump`, then running the tool): every registered command with
  its RPCHelpMan arguments, category and wallet registration;
- help text captured from a running regtest dashd (`capture`): `help`, `help <cmd>` for every
  command with a wallet loaded, the same again under -disablewallet, and
  `help dump_all_command_conversions`;
- src/rpc/client.cpp of the pin: vRPCConvertParams, the dash-cli conversion table;
- the PRD §5.4 matrix (docs/node-program/PRD.md): the class of every command.

Usage:
  generate.py regenerate CORE_BUILD DASHD [--work DIR] [--rpcport N]
      all of the below, then rustfmt; CORE_BUILD is a Core tree built by build-core.sh
  generate.py build-dump CORE_BUILD OUT
  generate.py capture --dashd PATH --dump DUMP.json --out CAPTURE.json [--rpcport N]
  generate.py emit --dump DUMP.json --capture CAPTURE.json --client client.cpp --prd PRD.md
                   --provenance TEXT --out generated.rs

`emit` fails if any input disagrees with another: the dump's RPCHelpMan::ToString() and the
live help, the wallet and -disablewallet help of a command registered in both, client.cpp and
the server's conversion map, the dump's argument map and the live one, or the PRD's command
list and Core's.

Needs Python 3.8+, git, make and a C++ toolchain only (no GNU coreutils or Bash 4).
"""

import argparse
import base64
import hashlib
import json
import os
import re
import secrets
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(HERE)
REPO = os.path.dirname(CRATE)


class RpcWarmup(Exception):
    pass


class NotOurs(Exception):
    """The endpoint rejected this instance's credentials: another process holds the port."""


def read_json(path):
    with open(path) as f:
        return json.load(f)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Node:
    """A regtest dashd on its own datadir, RPC port and random credentials, stopped on exit.

    The endpoint counts as this instance's only once it accepts the instance's credentials, the
    child is still running, and `getrpcinfo` reports a log file inside the instance's datadir.
    Only then is `stop` sent to it; otherwise the child is killed by PID. With no `rpcport` a
    free port is picked, and picked again if dashd cannot bind it; an explicit port that is
    taken is an error.
    """

    ATTEMPTS = 5

    def __init__(self, dashd, rpcport, extra_args):
        for attempt in range(1, self.ATTEMPTS + 1):
            port = rpcport or free_port()
            try:
                self._start(dashd, port, extra_args)
                return
            except DashdExited as e:
                if rpcport or attempt == self.ATTEMPTS:
                    raise SystemExit(f"dashd on RPC port {port} exited during startup: {e}")
                print(f"dashd could not use RPC port {port}; retrying on another", file=sys.stderr)

    def _start(self, dashd, port, extra_args):
        self.datadir = tempfile.mkdtemp(prefix="dash-rpc-spec-")
        self.rpcport = port
        self.owned = False
        user, password = "rpcspec", secrets.token_hex(16)
        self.auth = "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()
        args = [
            dashd, "-regtest", f"-datadir={self.datadir}", f"-rpcport={port}",
            "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1", "-listen=0", "-dnsseed=0",
            "-fixedseeds=0", "-server", "-printtoconsole=0", f"-rpcuser={user}",
            f"-rpcpassword={password}",
        ] + extra_args
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            self._wait_owned()
        except BaseException:
            self.stop()
            raise

    def _wait_owned(self):
        deadline = time.time() + 120
        while True:
            try:
                self.call("getblockcount")
                if self.proc.poll() is None:
                    break
            except (urllib.error.URLError, ConnectionError, RpcWarmup, NotOurs):
                pass
            if self.proc.poll() is not None:
                raise DashdExited(self.proc.stderr.read().decode().strip())
            if time.time() > deadline:
                raise SystemExit("dashd did not start within 120 s")
            time.sleep(0.25)
        logpath = os.path.realpath(self.call("getrpcinfo")["logpath"])
        if os.path.commonpath([logpath, os.path.realpath(self.datadir)]) != os.path.realpath(self.datadir):
            raise SystemExit(f"RPC port {self.rpcport} answers for another dashd (log {logpath})")
        self.owned = True

    def call(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)})
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.rpcport}/", data=body.encode(),
            headers={"Authorization": self.auth, "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as e:
            if e.code == 401:
                raise NotOurs()
            raw = e.read()
        try:
            reply = json.loads(raw)
        except json.JSONDecodeError:
            raise SystemExit(f"{method}: not a JSON-RPC reply: {raw[:200]!r}")
        if reply.get("error"):
            if reply["error"]["code"] == -28:
                raise RpcWarmup()
            raise SystemExit(f"{method} {params}: {reply['error']}")
        return reply["result"]

    def stop(self):
        if self.owned and self.proc.poll() is None:
            try:
                self.call("stop")
            except Exception:
                self.proc.kill()
        else:
            self.proc.kill()
        try:
            self.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        shutil.rmtree(self.datadir, ignore_errors=True)


class DashdExited(Exception):
    pass


def capture_config(dashd, rpcport, wallet, names):
    node = Node(dashd, rpcport, [] if wallet else ["-disablewallet"])
    try:
        if wallet:
            node.call("createwallet", "rpcspec")
        out = {"overview": node.call("help")}
        # Composite names go as two params, as `dash-cli help protx register` sends them.
        out["help"] = {name: node.call("help", *name.split(" ", 1)) for name in names}
        out["unknown"] = node.call("help", "foo")
        out["arg_map"] = node.call("help", "dump_all_command_conversions")
        return out
    finally:
        node.stop()


def capture(dashd, dump, out_path, rpcport):
    cmds = read_json(dump)["commands"]
    with_wallet = [c["name"] for c in cmds if c["registered_with_wallet"]]
    without_wallet = [c["name"] for c in cmds if c["registered_without_wallet"]]
    out = {
        "dashd_version": dashd_version(dashd),
        "with_wallet": capture_config(dashd, rpcport, True, with_wallet),
        "without_wallet": capture_config(dashd, rpcport, False, without_wallet),
    }
    with open(out_path, "w") as f:
        json.dump(out, f, indent=1, sort_keys=True)
        f.write("\n")


def dashd_version(dashd):
    return subprocess.run([dashd, "-version"], capture_output=True, text=True,
                          check=True).stdout.splitlines()[0]


def build_dump(core, out):
    """Compiles rpcspec_dump.cpp with dashd-bitcoind.o's flags and links it with dashd's link
    line, so it sees the RPC tables of the tree's dashd. `make -n -W bitcoind.cpp` prints exactly
    those two commands; each piece replaced must be present."""
    core, out = os.path.abspath(core), os.path.abspath(out)
    src_dir = os.path.join(core, "src")
    with open(os.path.join(src_dir, "config", "bitcoin-config.h")) as f:
        config = f.read()
    for flag in ("ENABLE_WALLET", "ENABLE_ZMQ"):
        if f"#define {flag} 1" not in config:
            raise SystemExit(f"Core was built without {flag}")
    lines = subprocess.run([os.environ.get("MAKE", "make"), "-n", "-W", "bitcoind.cpp", "V=1", "dashd"],
                           cwd=src_dir, capture_output=True, text=True, check=True).stdout.splitlines()
    compile_cmd = [l for l in lines if "-c -o dashd-bitcoind.o" in l]
    link_cmd = [l for l in lines if "--mode=link" in l and " -o dashd " in l]
    if len(compile_cmd) != 1 or len(link_cmd) != 1:
        raise SystemExit("could not find dashd's compile and link commands")

    def swap(cmd, old, new):
        if old not in cmd:
            raise SystemExit(f"unexpected command shape: no {old!r}")
        return cmd.replace(old, new, 1)

    obj = out + ".o"
    src = os.path.join(HERE, "rpcspec_dump.cpp")
    compile_cmd = swap(compile_cmd[0], " -MT dashd-bitcoind.o -MD -MP -MF .deps/dashd-bitcoind.Tpo", "")
    compile_cmd = swap(compile_cmd, " -o dashd-bitcoind.o ", f" -o {shlex.quote(obj)} ")
    compile_cmd = swap(compile_cmd, "`test -f 'bitcoind.cpp' || echo './'`bitcoind.cpp", shlex.quote(src))
    link_cmd = swap(link_cmd[0], " -o dashd ", f" -o {shlex.quote(out)} ")
    link_cmd = swap(link_cmd, " dashd-bitcoind.o ", f" {shlex.quote(obj)} ")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    for cmd in (compile_cmd, link_cmd):
        subprocess.run(cmd, shell=True, cwd=src_dir, check=True)
    os.remove(obj)
    shutil.rmtree(os.path.join(os.path.dirname(out), ".libs"), ignore_errors=True)
    print(f"built {out}")


def git_blob(path):
    return subprocess.run(["git", "hash-object", path], capture_output=True, text=True,
                          check=True).stdout.strip()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def regenerate(core, dashd, work, rpcport):
    core, dashd = os.path.abspath(core), os.path.abspath(dashd)
    work = os.path.abspath(work or tempfile.mkdtemp(prefix="dash-rpc-spec-work-"))
    os.makedirs(work, exist_ok=True)
    prd = os.path.join(REPO, "docs", "node-program", "PRD.md")
    client = os.path.join(core, "src", "rpc", "client.cpp")
    tool, dump, cap = (os.path.join(work, n) for n in ("rpcspec_dump", "dump.json", "capture.json"))

    build_dump(core, tool)
    with open(dump, "w") as f:
        subprocess.run([tool], stdout=f, check=True)
    capture(dashd, dump, cap, rpcport)
    provenance = (
        f"Generated from Dash Core ({dashd_version(dashd)}):\n"
        f"- help text: `help` and `help <cmd>` of a running regtest dashd, sha256\n"
        f"  {sha256_file(dashd)}, with a wallet loaded (commands registered only\n"
        f"  under -disablewallet: none);\n"
        f"- arguments, categories and wallet gating: generator/rpcspec_dump.cpp linked against a\n"
        f"  build of that tag's source;\n"
        f"- CONVERT_PARAMS: src/rpc/client.cpp, git blob {git_blob(client)};\n"
        f"- classes: PRD §5.4 of docs/node-program/PRD.md, git blob {git_blob(prd)}.")
    out = os.path.join(CRATE, "src", "generated.rs")
    emit(dump, cap, client, prd, provenance, out)
    subprocess.run(["rustfmt", "--edition", "2024", out], check=True)
    print(f"intermediate files in {work}")


def parse_client_cpp(path):
    """vRPCConvertParams in order: (method, index, name, preserve_str, miner_only).

    Uses rpc_help.py's line pattern, and tracks the `#if ENABLE_MINER` block, the only
    preprocessor conditional inside the table at v24."""
    with open(path) as f:
        text = f.read()
    start = text.index("static const CRPCConvertParam vRPCConvertParams[] =")
    end = text.index("};", start)
    entries, miner = [], False
    for line in text[start:end].splitlines()[1:]:
        s = line.strip()
        if s.startswith("#if"):
            if s != "#if ENABLE_MINER":
                raise SystemExit(f"client.cpp: unexpected conditional {s!r}")
            miner = True
        elif s.startswith("#endif"):
            miner = False
        elif s.startswith("#"):
            raise SystemExit(f"client.cpp: unexpected directive {s!r}")
        elif s.startswith("{ \""):
            m = re.fullmatch(r'{ *"([^"]*)", *([0-9]+) *, *"([^"]*)"(?:, *(true|false))? *},?(?: *//.*)?', s)
            if not m:
                raise SystemExit(f"client.cpp: unparsed entry {s!r}")
            entries.append((m[1], int(m[2]), m[3], m[4] == "true", miner))
        elif s not in ("{", "") and not s.startswith("//"):
            raise SystemExit(f"client.cpp: unparsed line {s!r}")
    return entries


def parse_prd(path):
    with open(path) as f:
        text = f.read()
    s = text[text.index("### 5.4 Matrix"):]
    s = s[:s.index("\n## ")]
    classes = {}
    for name, cls in re.findall(r"^\| `([^`]+)` \| (MUST|SHOULD|WON'T) \|", s, re.M):
        if name in classes:
            raise SystemExit(f"PRD: {name} classified twice")
        classes[name] = cls
    return classes


def rust_str(s):
    out = ['"']
    for ch in s:
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\t":
            out.append("\\t")
        elif ch == "\r":
            out.append("\\r")
        elif ord(ch) < 0x20 or ord(ch) == 0x7f:
            out.append(f"\\u{{{ord(ch):x}}}")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


ARG_TYPES = {
    "OBJ": "Obj", "ARR": "Arr", "STR": "Str", "NUM": "Num", "BOOL": "Bool",
    "OBJ_USER_KEYS": "ObjUserKeys", "AMOUNT": "Amount", "STR_HEX": "StrHex", "RANGE": "Range",
}
# Where PRD §5.4 and Core's v24 command list disagree: name -> (class to use, reason). A class
# of None drops a PRD row that names no v24 command. Reported for a PRD fix; `emit` fails when
# an entry no longer applies.
PRD_GAPS = {
    "protx shared_register_prepare_evo": (
        "MUST", "missing from PRD §5.4; classified as its non-evo twin `protx shared_register_prepare` "
        "(MUST, US-SHR-01/02)"),
    "getprioritisedtransactions": (None, "in PRD §5.4 (mining, MUST) but not registered by v24.0.0-rc.4"),
}

CLASSES = {"MUST": "Class::Must", "SHOULD": "Class::Should", "WON'T": "Class::Wont"}


def rust_bool(b):
    return "true" if b else "false"


def rust_args(args):
    """The arguments as a Rust slice literal; rustfmt lays it out."""
    parts = []
    for a in args:
        fb = a["fallback"]
        if fb["kind"] == "required":
            fallback = "Fallback::Required"
        elif fb["kind"] == "omitted":
            fallback = "Fallback::Omitted"
        elif fb["kind"] == "hint":
            fallback = f"Fallback::DefaultHint({rust_str(fb['value'])})"
        else:
            fallback = f"Fallback::Default({rust_str(fb['value'])})"
        parts.append(
            f"RpcArg {{ names: {rust_str(a['names'])}, ty: ArgType::{ARG_TYPES[a['type']]}, "
            f"fallback: {fallback}, skip_type_check: {rust_bool(a['skip_type_check'])}, "
            f"hidden: {rust_bool(a['hidden'])}, inner: {rust_args(a['inner'])} }},\n")
    return "&[\n" + "".join(parts) + "]" if parts else "&[]"


def overview(cmds, wallet_enabled):
    """`help` with no argument (CRPCTable::help), recomputed from the dump."""
    key = "registered_with_wallet" if wallet_enabled else "registered_without_wallet"
    listed = sorted((c for c in cmds if c[key] and c["category"] != "hidden" and " " not in c["name"]),
                    key=lambda c: (c["category"] + c["name"]).encode())
    out, category = "", ""
    for c in listed:
        if c["category"] != category:
            if category:
                out += "\n"
            category = c["category"]
            out += f"== {category[:1].upper()}{category[1:]} ==\n"
        out += c["help"].split("\n", 1)[0] + "\n"
    return out[:-1]


def server_arg_map(cmds):
    """help dump_all_command_conversions, recomputed from the dump (RPCHelpMan::GetArgMap)."""
    rows = []
    for c in sorted(cmds, key=lambda c: c["name"]):
        if " " in c["name"]:
            continue
        for i, a in enumerate(c["args"]):
            for n in a["names"].split("|"):
                rows.append([c["name"], i, n, a["type"] in ("STR", "STR_HEX")])
    return rows


def emit(dump_path, capture_path, client_path, prd_path, provenance, out_path):
    dump = read_json(dump_path)
    cap = read_json(capture_path)
    cmds = dump["commands"]
    wl, nwl = cap["with_wallet"], cap["without_wallet"]
    errors = []

    # Help: live text is the table's; the dump's ToString() and the other wallet mode must agree.
    for c in cmds:
        name = c["name"]
        live = wl["help"][name] if c["registered_with_wallet"] else nwl["help"][name]
        c["live_help"] = live
        if c["help"] != live:
            errors.append(f"{name}: RPCHelpMan::ToString() differs from dashd's help")
        if c["registered_with_wallet"] and c["registered_without_wallet"] and nwl["help"][name] != live:
            errors.append(f"{name}: help differs with and without the wallet")
        if not c["registered_with_wallet"]:
            errors.append(f"{name}: registered only without the wallet (unexpected at v24)")

    # Core's help overview lists a handler once (setDone); the table assumes one name each.
    fn_ids = [c["fn_id"] for c in cmds]
    if len(set(fn_ids)) != len(fn_ids):
        errors.append("a handler is registered under two names; help() would need setDone")

    # Conversion tables: client.cpp vs the live server map, as rpc_help.py checks it.
    convert = parse_client_cpp(client_path)
    # rpc_help.py's filters: echojson converts on the client only, and the server's map leaves
    # out sub-commands. The full table is compared with client.cpp by the crate's tests.
    server = [tuple(r[:3]) for r in wl["arg_map"] if not r[3]]
    client = [(m, i, n) for (m, i, n, _, _) in convert if m != "echojson" and " " not in m]
    if sorted(client) != sorted(server):
        errors.append(f"client.cpp and the server conversion map differ: "
                      f"{sorted(set(client) ^ set(server))}")
    for mode, live, key, wallet in (("wallet", wl, "registered_with_wallet", True),
                                    ("-disablewallet", nwl, "registered_without_wallet", False)):
        if server_arg_map([c for c in cmds if c[key]]) != live["arg_map"]:
            errors.append(f"dump's argument map differs from dashd's ({mode})")
        # Catches a command the release registers and the dump's build does not.
        if overview(cmds, wallet) != live["overview"]:
            errors.append(f"help overview recomputed from the dump differs from dashd's ({mode})")
        if live["unknown"] != "help: unknown command: foo":
            errors.append(f"unexpected unknown-command help ({mode}): {live['unknown']!r}")
    if dump["arg_map"] != wl["arg_map"]:
        errors.append("dump_all_command_conversions differs between the dump and dashd")

    # PRD classes. Core registers some names twice (node and wallet-aware); the PRD counts once.
    prd = parse_prd(prd_path)
    names = {c["name"] for c in cmds}
    for n, (cls, reason) in PRD_GAPS.items():
        print(f"PRD gap: {n}: {reason}", file=sys.stderr)
        if cls is None:
            if n not in prd or n in names:
                errors.append(f"{n}: PRD_GAPS entry is stale")
            prd.pop(n, None)
        else:
            if n in prd or n not in names:
                errors.append(f"{n}: PRD_GAPS entry is stale")
            prd[n] = cls
    for n in sorted(names - prd.keys()):
        errors.append(f"{n}: not classified in PRD §5")
    for n in sorted(prd.keys() - names):
        errors.append(f"{n}: classified in PRD §5 but not registered by Core")

    version = cap["dashd_version"].rsplit(" ", 1)[-1]
    if not re.fullmatch(r"v\d+\.\d+\.\d+(-rc\.\d+)?", version):
        errors.append(f"unexpected dashd version line {cap['dashd_version']!r}")

    if errors:
        print("\n".join(errors), file=sys.stderr)
        raise SystemExit(f"{len(errors)} inconsistencies; nothing written")

    out = [
        "// @generated by dash-rpc-spec/generator/generate.py. Do not edit.\n",
        "//\n",
    ]
    out += [f"// {line}\n" if line else "//\n" for line in provenance.splitlines()]
    out += [
        "\n",
        "use crate::{ArgType, Class, ConvertParam, Fallback, Gating, RpcArg, RpcCommand};\n",
        "\n",
        "/// The Dash Core release the table was generated from.\n",
        f"pub const CORE_VERSION: &str = {rust_str(version)};\n",
        "\n",
        f"/// Every command Dash Core registers, sorted by name ({len(cmds)} entries).\n",
        "pub static COMMANDS: &[RpcCommand] = &[\n",
    ]
    for c in sorted(cmds, key=lambda c: c["name"]):
        if c["from_wallet_table"]:
            gating = "Gating::Hybrid" if c["registered_without_wallet"] else "Gating::Wallet"
        else:
            gating = "Gating::Always"
        out.append(
            "    RpcCommand {\n"
            f"        name: {rust_str(c['name'])},\n"
            f"        category: {rust_str(c['category'])},\n"
            f"        whitelist_name: {rust_str(c['name'].replace(' ', ''))},\n"
            f"        gating: {gating},\n"
            f"        class: {CLASSES[prd[c['name']]]},\n"
            f"        args: {rust_args(c['args'])},\n"
            f"        help: {rust_str(c['live_help'])},\n"
            "    },\n")
    out.append("];\n\n")
    out.append(f"/// `vRPCConvertParams` of `src/rpc/client.cpp`, in file order ({len(convert)} entries).\n")
    out.append("pub static CONVERT_PARAMS: &[ConvertParam] = &[\n")
    for m, i, n, p, miner in convert:
        out.append(
            f"    ConvertParam {{ method: {rust_str(m)}, index: {i}, name: {rust_str(n)}, "
            f"preserve_str: {rust_bool(p)}, miner_only: {rust_bool(miner)} }},\n")
    out.append("];\n")
    with open(out_path, "w") as f:
        f.write("".join(out))
    print(f"wrote {out_path}: {len(cmds)} commands, {len(convert)} conversion entries")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("regenerate")
    r.add_argument("core_build")
    r.add_argument("dashd")
    r.add_argument("--work")
    r.add_argument("--rpcport", type=int)
    r.set_defaults(func=lambda a: regenerate(a.core_build, a.dashd, a.work, a.rpcport))
    b = sub.add_parser("build-dump")
    b.add_argument("core_build")
    b.add_argument("out")
    b.set_defaults(func=lambda a: build_dump(a.core_build, a.out))
    c = sub.add_parser("capture")
    for arg in ("--dashd", "--dump", "--out"):
        c.add_argument(arg, required=True)
    c.add_argument("--rpcport", type=int)
    c.set_defaults(func=lambda a: capture(a.dashd, a.dump, a.out, a.rpcport))
    e = sub.add_parser("emit")
    for arg in ("--dump", "--capture", "--client", "--prd", "--provenance", "--out"):
        e.add_argument(arg, required=True)
    e.set_defaults(func=lambda a: emit(a.dump, a.capture, a.client, a.prd, a.provenance, a.out))
    a = p.parse_args()
    a.func(a)


if __name__ == "__main__":
    main()
