#!/usr/bin/env python3
"""Core's V2 short-ID tables and send gate at both pins, from source.

Usage: bs7_short_ids.py [DASH_REPO] > short_ids.json5
Reads, with `git show` at v23.1.8 and v24.0.0-rc.3 (the checkout is not
touched): NetMsgType's strings and allNetMessageTypes (protocol.cpp),
V2_BITCOIN_IDS, V2_DASH_IDS, V2_NEW_SHORT_ID_VERSIONS, IsValidV2ShortID,
V2MessageMap and SetMessageToSend (net.cpp), V2Transport's m_peer_version
(net.h) and the version constants (version.h). It asserts the code shapes the
derivation relies on, fails if the two tags differ, and prints the table
base-sdk's `ShortId::for_send` and `split_v2` must equal: for every command
Core knows, and the empty type, the slot Core sends it by and from which
common version; and for every first byte, the type Core reads.
"""
import json
import re
import subprocess
import sys

TAGS = ["v23.1.8", "v24.0.0-rc.3"]


def show(repo, tag, path):
    return subprocess.run(["git", "-C", repo, "show", "%s:%s" % (tag, path)], capture_output=True, text=True,
                          check=True).stdout


def need(text, pattern, what):
    m = re.search(pattern, text, re.S)
    if not m:
        sys.exit("FAIL: %s not found" % what)
    return m


def table(repo, tag):
    proto = show(repo, tag, "src/protocol.cpp")
    net = show(repo, tag, "src/net.cpp")
    neth = show(repo, tag, "src/net.h")
    version = show(repo, tag, "src/version.h")
    names = dict(re.findall(r'MAKE_MSG\((\w+), "([^"]*)"\);', proto))
    all_types = [names[n] for n in re.findall(r"NetMsgType::(\w+)",
                 need(proto, r"allNetMessageTypes\[\] = \{(.*?)\};", "allNetMessageTypes").group(1))]

    def ids(var):
        body = need(net, r"std::array<std::string, \d+> %s = \{(.*?)\n\};" % var, var).group(1)
        out = []
        for line in body.splitlines():
            line = re.sub(r"/\*.*?\*/|//.*", "", line).strip().rstrip(",").strip()
            if not line:
                continue
            out.append("" if line == '""' else names[need(line, r"NetMsgType::(\w+)", line).group(1)])
        return out

    btc, dash = ids("V2_BITCOIN_IDS"), ids("V2_DASH_IDS")
    consts = {k: int(v) for k, v in re.findall(r"static const int (\w+) = (\d+);", version)}
    newer = {names[k]: consts[v] for k, v in re.findall(r"\{NetMsgType::(\w+), (\w+)\}",
             need(net, r"V2_NEW_SHORT_ID_VERSIONS = \{(.*?)\};", "V2_NEW_SHORT_ID_VERSIONS").group(1))}
    # The code the derivation below mirrors, asserted as text.
    need(net, r"return first_byte < std::size\(V2_BITCOIN_IDS\) \|\|\s*\(first_byte >= 128 && "
              r"static_cast<uint8_t>\(first_byte - 128\) < std::size\(V2_DASH_IDS\)\);", "IsValidV2ShortID")
    need(net, r"for \(size_t i = 1; i < std::size\(V2ShortIDs\(\)\); \+\+i\) \{\s*if \(IsValidV2ShortID\(i\)\) \{"
              r"\s*m_map\.emplace\(V2ShortIDs\(\)\[i\], i\);", "V2MessageMap")
    need(net, r"return BIP324_DASH_BASELINE_VERSION;", "GetMessageMinVersion baseline")
    need(net, r"if \(short_message_id\.has_value\(\) && m_peer_version >= GetMessageMinVersion\(msg\.m_type\)\)",
         "SetMessageToSend gate")
    need(neth, r"int m_peer_version GUARDED_BY\(m_send_mutex\) \{INIT_PROTO_VERSION\};", "m_peer_version init")
    slots = {}
    for i in list(range(1, len(btc))) + [128 + j for j in range(len(dash))]:
        name = btc[i] if i < 128 else dash[i - 128]
        slots.setdefault(name, i)  # emplace keeps the first
    send = {}
    for name in all_types + [""]:
        if name in slots:
            send[name] = {"id": slots[name], "min_version": newer.get(name, consts["BIP324_DASH_BASELINE_VERSION"])}
        else:
            send[name] = None
    recv = {}
    for b in range(256):
        if b == 0:
            recv[b] = "long"
        elif b < len(btc):
            recv[b] = btc[b]
        elif b >= 128 and b - 128 < len(dash):
            recv[b] = dash[b - 128]
        else:
            recv[b] = None
    return {"send": send, "recv": recv, "init_version": consts["INIT_PROTO_VERSION"]}


def main():
    repo = sys.argv[1] if len(sys.argv) > 1 else "/work/home/agent/workspace/dash"
    tables = {tag: table(repo, tag) for tag in TAGS}
    if tables[TAGS[0]] != tables[TAGS[1]]:
        sys.exit("FAIL: the short-ID tables differ between %s and %s" % tuple(TAGS))
    t = tables[TAGS[0]]
    print("// Dash Core's V2 short IDs at v23.1.8 and v24.0.0-rc.3 (identical), derived")
    print("// from the tags' protocol.cpp, net.cpp, net.h and version.h. \"send\": for")
    print("// each command in allNetMessageTypes and the empty type, Core's")
    print("// V2MessageMap slot and the common version from which SetMessageToSend uses")
    print("// it (null: always the long form). \"recv\": the type GetMessageType reads")
    print("// for each first byte (\"long\": 12 type bytes follow; \"\": a reserved slot,")
    print("// the empty type; null: dropped).")
    print("// \"init_version\": m_peer_version before the peer's version arrives.")
    print("{")
    print('  "init_version": %d,' % t["init_version"])
    print('  "send": {')
    for name, v in t["send"].items():
        print("    %s: %s," % (json.dumps(name), json.dumps(v)))
    print("  },")
    print('  "recv": {')
    for b, v in t["recv"].items():
        print('    "%d": %s,' % (b, json.dumps(v)))
    print("  },")
    print("}")


if __name__ == "__main__":
    main()
