#!/usr/bin/env python3
"""Extracts the Dash Core v24 protocol tables that the `dash` crate must equal (BS18).

Reads Dash Core's sources at a pinned tag, straight from git (no checkout needed), and writes
`dash/tests/data/core_v24_protocol_tables.json`, which `dash/tests/core_v24_protocol_tables.rs`
checks the crate's constants against:

- `version.h`: every protocol-version constant, in Core's order;
- `protocol.h`: the `GetDataMsg` inventory enum and the `ServiceFlags` enum;
- `protocol.cpp`: `CInv::GetCommandInternal` (inventory type -> command), `serviceFlagToStr`, and
  the `NetMsgType` command strings they refer to, plus the `CInv::ToString` formats;
- `net.h`, `serialize.h`, `net.cpp`: `MAX_PROTOCOL_MESSAGE_LENGTH`, `MAX_SIZE`, and the lines
  where Core applies them;
- `script/script.h`, `script/script.cpp`: the `opcodetype` enum and `GetOpName`, expanded to the
  256 names a byte value gets.

Usage:
  core-protocol-tables.py --core <Dash Core clone>            # rewrite the JSON
  core-protocol-tables.py --core <Dash Core clone> --check    # fail if the JSON is stale

The tag must resolve to the recorded commit (`EXPECTED_COMMIT`); moving the pin is a deliberate
edit of this file. The script only reads the clone (`git show`).
"""

import argparse
import json
import os
import re
import subprocess
import sys

TAG = "v24.0.0-rc.3"
# The commit the (annotated) tag peels to, as `git rev-parse <tag>^{commit}` prints it.
EXPECTED_COMMIT = "0364ed626ec2eaa518bf1ae4a0566ce638c7fe18"

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "..", "dash", "tests", "data", "core_v24_protocol_tables.json"))


def git(core, *args):
    return subprocess.run(["git", "-C", core, *args], check=True, capture_output=True, text=True).stdout


def show(core, path):
    return git(core, "show", f"{TAG}:{path}")


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


def enum_body(text, header):
    start = text.index(header)
    start = text.index("{", start) + 1
    return strip_comments(text[start:text.index("};", start)])


def line_of(text, needle):
    """1-based line number of the first line containing `needle`; fails if there is none."""
    for i, line in enumerate(text.splitlines(), 1):
        if needle in line:
            return i
    raise SystemExit(f"core-protocol-tables: {needle!r} not found")


def parse_int(expr):
    expr = expr.strip().rstrip(",")
    m = re.fullmatch(r"\(?\s*1\s*<<\s*(\d+)\s*\)?", expr)
    if m:
        return 1 << int(m.group(1))
    if re.fullmatch(r"\d+(\s*\*\s*\d+)*", expr):
        n = 1
        for f in expr.split("*"):
            n *= int(f)
        return n
    return int(expr, 0)


def version_constants(core):
    text = strip_comments(show(core, "src/version.h"))
    return [[n, int(v)] for n, v in re.findall(r"static const int (\w+) = (\d+);", text)]


def msg_strings(core):
    text = show(core, "src/protocol.cpp")
    return dict(re.findall(r'MAKE_MSG\((\w+), "([^"]*)"\);', text))


def inventory(core):
    h = show(core, "src/protocol.h")
    cpp = show(core, "src/protocol.cpp")
    members = [[n, int(v)] for n, v in re.findall(r"(\w+) = (\d+),", enum_body(h, "enum GetDataMsg"))]
    strings = msg_strings(core)
    body = cpp[cpp.index("const char* CInv::GetCommandInternal"):cpp.index("std::string CInv::GetCommand()")]
    commands = [[name, strings[cmd]] for name, cmd in re.findall(r"case (MSG_\w+):\s*return NetMsgType::(\w+);", strip_comments(body))]
    names = {n for n, _ in members}
    for name, _ in commands:
        if name not in names:
            raise SystemExit(f"core-protocol-tables: {name} has a command but is not in GetDataMsg")
    fmt = re.search(r'std::string CInv::ToString\(\) const.*?\n}\n', cpp, re.S).group(0)
    unknown_fmt = re.search(r'strprintf\("(0x%08x %s)"', fmt).group(1)
    known_fmt = re.search(r'strprintf\("(%s %s)"', fmt).group(1)
    return {
        "members": members,
        "commands": commands,
        "to_string_known": known_fmt,
        "to_string_unknown": unknown_fmt,
    }


def service_flags(core):
    h = show(core, "src/protocol.h")
    cpp = show(core, "src/protocol.cpp")
    members = [[n, parse_int(v)] for n, v in re.findall(r"(NODE_\w+) = ([^,\n]+),", enum_body(h, "enum ServiceFlags"))]
    body = cpp[cpp.index("static std::string serviceFlagToStr"):cpp.index("std::vector<std::string> serviceFlagsToStr")]
    names = [[n, s] for n, s in re.findall(r'case (NODE_\w+):\s+return "([^"]+)";', strip_comments(body))]
    unknown = re.search(r'return strprintf\("(UNKNOWN\[2\^%u\])", bit\);', body).group(1)
    return {"members": members, "names": names, "unknown_format": unknown}


def limits(core):
    net_h = show(core, "src/net.h")
    ser_h = show(core, "src/serialize.h")
    net_cpp = show(core, "src/net.cpp")
    proto = parse_int(re.search(r"MAX_PROTOCOL_MESSAGE_LENGTH = ([^;]+);", net_h).group(1))
    max_size = parse_int(re.search(r"MAX_SIZE = ([^;]+);", ser_h).group(1))
    sites = {
        "v1_header": "hdr.nMessageSize > MAX_SIZE || hdr.nMessageSize > MAX_PROTOCOL_MESSAGE_LENGTH",
        "v2_contents": "std::min<size_t>(MAX_SIZE, MAX_PROTOCOL_MESSAGE_LENGTH)",
        "compact_size": None,
    }
    applied = {
        "v1_header": ["src/net.cpp", line_of(net_cpp, sites["v1_header"])],
        "v2_contents": ["src/net.cpp", line_of(net_cpp, sites["v2_contents"])],
        "compact_size": ["src/serialize.h", line_of(ser_h, "if (range_check && nSizeRet > MAX_SIZE)")],
    }
    return {
        "MAX_PROTOCOL_MESSAGE_LENGTH": proto,
        "MAX_SIZE": max_size,
        "applied_at": applied,
    }


def opcodes(core):
    h = strip_comments(show(core, "src/script/script.h"))
    enum = h[h.index("enum opcodetype"):]
    enum = enum[enum.index("{") + 1:enum.index("};")]
    values = {}
    members = []
    for name, rhs in re.findall(r"(\w+)\s*=\s*(\w+)\s*,", enum):
        value = values[rhs] if rhs in values else int(rhs, 0)
        values[name] = value
        members.append([name, value])
    cpp = show(core, "src/script/script.cpp")
    body = cpp[cpp.index("std::string GetOpName"):cpp.index("unsigned int CScript::GetSigOpCount(bool")]
    body = strip_comments(body)
    default = re.search(r'default:\s*return "([^"]+)";', body).group(1)
    names = [default] * 256
    seen = set()
    for label, text in re.findall(r'case (\w+)\s*:\s*return "([^"]*)";', body):
        value = values[label]
        if value in seen:
            raise SystemExit(f"core-protocol-tables: GetOpName names byte {value:#x} twice")
        seen.add(value)
        names[value] = text
    return {"members": members, "get_op_name": names}


def build(core):
    resolved = git(core, "rev-parse", f"{TAG}^{{commit}}").strip()
    if resolved != EXPECTED_COMMIT:
        raise SystemExit(f"core-protocol-tables: {TAG} is {resolved}, expected {EXPECTED_COMMIT}")
    return {
        "source": {"repo": "dashpay/dash", "tag": TAG, "commit": EXPECTED_COMMIT},
        "version": version_constants(core),
        "inventory": inventory(core),
        "service_flags": service_flags(core),
        "limits": limits(core),
        "opcodes": opcodes(core),
    }


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--core", required=True, help="a Dash Core clone that has the tag")
    p.add_argument("--check", action="store_true", help="compare with the checked-in file instead of writing it")
    args = p.parse_args()
    text = json.dumps(build(args.core), indent=1) + "\n"
    if args.check:
        with open(OUT) as f:
            if f.read() != text:
                print(f"core-protocol-tables: {OUT} is stale; rerun without --check", file=sys.stderr)
                return 1
        print("core-protocol-tables: up to date")
        return 0
    with open(OUT, "w") as f:
        f.write(text)
    print(f"core-protocol-tables: wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
