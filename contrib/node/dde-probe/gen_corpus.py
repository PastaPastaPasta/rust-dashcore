#!/usr/bin/env python3
"""Renders base-sdk's pkgs/p2p_core/corpus/decode_domain.json5 from the probes' output.

Usage: gen_corpus.py PROBE.json GATES.json V2.json > decode_domain.json5
(probe_p2p.py, probe_gates.py and probe_v2.py). Fails unless both releases
agree on every outcome used.
"""
import json
import sys

GROUPS = {"cut_46_mandatory_only": 0, "cut_80_through_nonce": 1, "cut_99_through_subver": 2, "cut_103_through_height": 3,
          "cut_104_through_relay": 4, "cut_136_through_challenge": 5}
HEADERS = ["headers_txcount_5", "headers_txcount_fd", "headers_txcount_noncanonical", "headers_count_2001",
           "headers_count_2000_short", "headers2_count_2001_short", "headers2_count_8001", "headers_trailing",
           "headers2_count_2001_at_70230", "headers2_count_2001_at_70234", "headers2_count_2001_at_70235"]
# Pongs whose bytes depend on Core's random ping nonce, with a fixed stand-in.
NONCE_PONGS = {"mismatched_nonce": "fe02030405060708", "matching_nonce_trailing": "010203040506070899"}
CONTROL = ["senddsq_empty", "senddsq_02", "qsendrecsigs_empty", "qsendrecsigs_02"]


def same(res, f):
    vals = [f(r) for r in res["nodes"].values()]
    assert all(v == vals[0] for v in vals), vals
    return vals[0]


def outcome(case, command):
    """Labels one case from the peer's own log lines and connection state."""
    log = " ".join(case["log"])
    if "ProcessMessages(%s," % command in log and "Exception" in log:
        assert not case["closed"], case
        return "throw"
    for key, label in (("obsolete version", "disconnect: obsolete version"),
                       ("inbound masternode connection", "disconnect: inbound masternode")):
        if key in log:
            assert case["closed"], case
            return label
    assert case["synced"] and not case["closed"], case
    if "headers message size" in log:
        return "misbehaving 20"
    assert "Exception" not in log and "Misbehaving" not in log, case
    return "accept"


def line(entry):
    return "    " + json.dumps(entry) + ","


def main():
    res = json.load(open(sys.argv[1]))
    first = next(iter(res["nodes"].values()))
    out = ["""// P2P payloads Dash Core reads, for the decode domain of base-sdk's
// P2P messages. Each was sent to a regtest dashd of each pinned release,
// v23.1.8 and v24.0.0-rc.3 (release binaries, -debug=net), over a raw V1
// connection from 127.0.0.1, and "core" is the outcome both reported.
// "throw" is the "ProcessMessages(...): Exception ... caught" log line,
// which Core handles without a penalty.
{
  // A version payload one v23.1.8 node sent another (-capturemessages),
  // then cut at the offsets or edited at the fields in "name". For accepted
  // payloads, "subver", "startingheight" and "relaytxes" are getpeerinfo's
  // values. "groups" is not a Core observation: it counts the optional field
  // groups the cut leaves (addrFrom+nonce, subver, start height, relay,
  // mnauth challenge, fMasternode), following Core's read order.
  "version": ["""]
    for name in first["version"]:
        core = same(res, lambda r: outcome(r["version"][name], "version"))
        entry = {"name": name, "raw": first["version"][name]["raw"], "core": core}
        if core == "accept":
            for key in ("subver", "startingheight", "relaytxes"):
                entry[key] = same(res, lambda r: r["version"][name]["peer"][key])
            entry["groups"] = GROUPS.get(name, 6)
        out.append(line(entry))
    out.append("  ],")
    out.append("""  // After a full handshake at common version 70240. The headers are the
  // regtest genesis header (getblockheader false); "count" payloads stop
  // after the count. "misbehaving 20" is "headers message size = N".
  "headers": [""")
    for name in HEADERS:
        m = first["messages"][name]
        core = same(res, lambda r: outcome(r["messages"][name], m["command"]))
        out.append(line({"name": name, "command": m["command"], "version": m["version"], "raw": m["raw"], "core": core}))
    out.append("  ],")
    unknown = same(res, lambda r: ("Unknown command" in " ".join(r["messages"]["unknown_command"]["log"]),
                                   r["messages"]["unknown_command"]["synced"], r["messages"]["unknown_command"]["closed"]))
    assert unknown == (True, True, False), unknown
    assert same(res, lambda r: outcome(r["messages"]["sendheaders2_with_payload"], "sendheaders2")) == "accept"
    assert same(res, lambda r: r["verack_with_payload"]["synced"]
                and {"sendheaders2", "sendcmpct", "senddsq"} <= set(r["verack_with_payload"]["replies"]))
    out.append("""  // After a full handshake. Core logs an unknown command and keeps the peer;
  // a verack with a payload still completes the handshake (the node sends
  // its post-verack messages); sendheaders2 never reads its payload (no
  // exception logged).
  "ignored_payload": [""")
    out.append(line({"name": "unknown_command", "command": "bs4aprobe", "raw": "000102", "core": "accept: Unknown command"}))
    out.append(line({"name": "verack_with_payload", "command": "verack", "raw": "deadbeef", "core": "accept"}))
    out.append(line({"name": "sendheaders2_with_payload", "command": "sendheaders2", "raw": "0707", "core": "accept"}))
    out.append("  ],")
    assert all(v == ["01"] for v in res["senddsq_payloads"].values()), res["senddsq_payloads"]
    assert len(res["senddsq_payloads"]) == 2, res["senddsq_payloads"]
    out.append("""  // Messages carrying one bool. "captured" is senddsq as each release sent
  // it to the other (-capturemessages); the rest were sent after a full
  // handshake.
  "control": [""")
    out.append(line({"name": "captured", "command": "senddsq", "raw": "01", "core": "accept"}))
    for name in CONTROL:
        m = first["messages"][name]
        core = same(res, lambda r: outcome(r["messages"][name], m["command"]))
        out.append(line({"name": name, "command": m["command"], "raw": m["raw"], "core": core}))
    out.append("  ],")
    gates = json.load(open(sys.argv[2]))
    out.append("""  // pong sent while Core's own ping was outstanding (probe_gates.py), then
  // getpeerinfo: "ping cancelled" means pingwait went away without a
  // pingtime, "ping pending" that pingwait stayed, "ping measured" that
  // pingtime appeared. The two entries built from Core's random nonce show
  // 0102030405060708 in its place (its first byte flipped for the mismatch).
  "pong": [""")
    first_gates = next(iter(gates["nodes"].values()))
    for name in first_gates["pong"]:
        core = same(gates, lambda r: r["pong"][name]["core"])
        same(gates, lambda r: len(r["pong"][name]["raw"]))
        raw = first_gates["pong"][name]["raw"] if name not in NONCE_PONGS else NONCE_PONGS[name]
        out.append(line({"name": name, "raw": raw, "core": core}))
    out.append("  ],")
    v2 = json.load(open(sys.argv[3]))
    out.append("""  // V2 (BIP324) packets over an encrypted connection (probe_v2.py, Core's
  // test framework): the contents are a ping type, short ID 18 or the
  // 13-byte long form, then the nonce 0x01245678 and zeros up to
  // "payload_len". "unassigned" uses short ID 0xfe, "unassigned_33" 33 and
  // "reserved_5" 5, a slot Core keeps without a message; "long_<name>" uses
  // the "v1_commands" command <name> as a long-form type. "accept" means
  // Core read the ping and kept the peer; "Unknown command" is Core's log
  // line; "dropped" means the transport dropped it and kept the peer.
  "v2_contents": [""")
    for i, row in enumerate(next(iter(v2["nodes"].values()))):
        core = same(v2, lambda r: r[i]["core"])
        out.append(line({"form": row["form"], "payload_len": row["payload_len"], "contents_len": row["contents_len"],
                         "core": core}))
    out.append("  ],")
    out.append("""  // V1 messages after a full handshake whose 12-byte command (hex, NUL-padded)
  // breaks or meets Core's IsCommandValid (probe_gates.py), payload 010203. "dropped"
  // is the "Header error: Invalid message type" line with the peer kept and
  // no handler reached; "accept: Unknown command" reached the handlers.
  "v1_commands": [""")
    for name, case in first_gates["v1_commands"].items():
        dropped = same(gates, lambda r: any("Header error: Invalid message type" in l for l in r["v1_commands"][name]["log"]))
        unknown = same(gates, lambda r: r["v1_commands"][name]["unknown_command"])
        assert dropped != (unknown is not None), (name, dropped, unknown)
        core = "dropped" if dropped else "accept: Unknown command"
        command = bytes.fromhex(case["command"]).ljust(12, b"\0").hex()
        out.append(line({"name": name, "command": command, "raw": "010203", "core": core}))
    out.append("  ],")
    out.append("}")
    print("\n".join(out))


if __name__ == "__main__":
    main()
