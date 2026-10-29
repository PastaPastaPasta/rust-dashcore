#!/usr/bin/env python3
"""Renders base-sdk's BS7 corpora from bs7_probe.py and the oracle samples.

Usage: gen_bs7_corpus.py PROBE.json SAMPLE_DIR ORACLE_MAINNET.json ORACLE_TESTNET.json OUT_DIR
Writes OUT_DIR/core_headers2.json5, core_messages.json5 and core_verdicts.json5
(base-sdk pkgs/p2p_core/corpus) from the v24.0.0-rc.3 node under test, the only
parity reference (DEC-82). It fails if a case's log and connection state do not
fit the label it derives.
"""
import itertools
import json
import os
import re
import struct
import sys

from bs7_probe import framing_frames

PIN = "v24.0.0-rc.3"
# The peer that synced with the node under test; its capture is v24's too.
PEER = PIN + "-peer"
EXCEPTION = re.compile(r"ProcessMessages\((\w+), \d+ bytes\): Exception '([^']*)'")
MISBEHAVING = re.compile(r"Misbehaving: peer=\d+ \(\d+ -> (\d+)\)(?: DISCOURAGE THRESHOLD EXCEEDED)?:? ?([^.]*)")
# Commands Core knows but has no handler for (DDE-SWEEP §4.0).
NO_READER = {"merkleblock", "cfilter", "cfheaders", "cfcheckpt"}
# Handlers that catch their own read failure and score it; only that path
# appends "peer=N error=WHAT" to the reason (a missing filter for filteradd
# scores the bare reason after a good read).
CATCH = {"filterload": "misformatted bloom filter", "filteradd": "bad filteradd message"}
# Gates before the read in the probe's node state: sendtxrcncl with
# reconciliation off, ssc once the masternode sync has finished.
GATED = {"sendtxrcncl", "ssc"}
# Messages Core's sync manager sends on its own timer, not as replies.
TIMED = {"getsporks", "govsync"}


def rle(hexstr):
    """Hex, with runs of one byte over 64 long as {"b": byte, "n": count}."""
    b = bytes.fromhex(hexstr)
    if len(b) <= 4096:
        return hexstr
    out = []
    for byte, group in itertools.groupby(b):
        n = len(list(group))
        if n > 64:
            out.append({"b": "%02x" % byte, "n": n})
        elif out and isinstance(out[-1], str):
            out[-1] += bytes([byte]).hex() * n
        else:
            out.append(bytes([byte]).hex() * n)
    return out


def clean(log):
    """Core's log lines without their category and the disconnect echo."""
    return [l.split("] ", 1)[-1] for l in log if "disconnecting peer" not in l]


def read_label(case):
    """What Core's handler did with the payload, from its log and state."""
    cmd, log = case["command"], case["log"]
    exc = [m for l in log for m in EXCEPTION.findall(l)]
    mis = [m for l in log for m in MISBEHAVING.findall(l)]
    if exc:
        assert all(c == cmd for c, _ in exc), case["name"]
        assert case["core"] == "accept", case["name"]
        return "throw"
    if any("headers message size" in r for _, r in mis):
        return "too_many"
    if cmd in CATCH and any(CATCH[cmd] + ". peer=" in l and " error=" in l for l in log):
        return "catch"
    if cmd in NO_READER:
        assert case["core"] == "accept" and not log, case["name"]
        return "unread"
    if cmd in GATED and case.get("mnsync_synced") is not False:
        return "gated"
    return "read"


def exception(case):
    """The text of the exception Core's read threw, for a rejected case: the
    caught one, or the one a catching handler names after `error=`."""
    for l in case["log"]:
        m = EXCEPTION.search(l)
        if m:
            return re.search(r"Exception '([^']*)'", l).group(1)
        if " error=" in l:
            return l.split(" error=", 1)[1]
    return None


def verdicts(probe):
    cases, framing = [], []
    node = probe["nodes"][PIN]
    for c in node["cases"] + node["filters"]["cases"]:
        label = read_label(c)
        entry = {"name": c["name"], "command": c["command"], "core": c["core"], "read": label,
                 "log": clean(c["log"]),
                 "pre": [[x, rle(pl)] for x, pl in c["pre"]], "raw": rle(c["raw"]),
                 "replies": [x for x, _ in c["replies"] if x not in TIMED], "pin": PIN}
        if label in ("read", "unread"):
            entry["consumed"] = 0 if label == "unread" else c.get("core_reads", len(c["raw"]) // 2)
        if label in ("throw", "catch"):
            entry["exception"] = exception(c)
        if c.get("version"):
            entry["version"] = c["version"]
        if c in node["filters"]["cases"]:
            entry["mode"] = "peerblockfilters"
        cases.append(entry)
    for c in node["framing"]:
        chunks = framing_frames()[c["name"]]
        framing.append({"name": c["name"], "core": c["core"], "log": clean(c["log"]), "pongs": c["pongs"],
                        "pins": [PIN], "frame": rle(b"".join(chunks).hex()), "chunks": len(chunks)})
    return cases, framing


# Replies with their requested fields recorded ("expect"), by request.
EXPECTED = {"filtered_block": "merkleblock", "getcfilters": "cfilter", "getcfheaders": "cfheaders",
            "getcfcheckpt": "cfcheckpt"}


def messages(probe):
    """Every message each release sent, deduplicated, with its source, and
    for the replies in EXPECTED what the probe requested, in order."""
    seen, out = set(), []
    expect = {k: list(v) for k, v in probe["nodes"][PIN]["expect"].items()}

    def add(pin, source, cmd, raw):
        key = source.removeprefix("reply:")
        want = expect[key].pop(0) if EXPECTED.get(key) == cmd else None
        if (cmd, raw) in seen:
            return
        seen.add((cmd, raw))
        out.append({"pin": pin, "source": source, "command": cmd, "raw": rle(raw)})
        if want is not None:
            out[-1]["expect"] = want

    for capturer in (PIN, PEER):
        for cmd, raw in probe["captures"][capturer]["sent"]:
            add(PIN, "capture", cmd, raw)
    node = probe["nodes"][PIN]
    for key, msgs in node["originals"].items():
        for cmd, raw in msgs:
            add(PIN, "reply:" + key, cmd, raw)
    for key, msgs in node["filters"].items():
        if key != "cases":
            for cmd, raw in msgs:
                add(PIN, "reply:" + key, cmd, raw)
    left = {k: len(v) for k, v in expect.items() if v}
    if left:
        sys.exit("FAIL: requested replies Core did not send: %s" % left)
    return out


def compact_pairs(msgs):
    """Each cmpctblock with the block of the same header, both sent by Core."""
    blocks = {}
    for m in msgs:
        if m["command"] == "block" and isinstance(m["raw"], str):
            blocks.setdefault(m["raw"][:160], m)
    pairs = []
    for m in msgs:
        if m["command"] == "cmpctblock" and isinstance(m["raw"], str) and m["raw"][:160] in blocks:
            b = blocks[m["raw"][:160]]
            pairs.append({"pin": m["pin"], "cmpctblock": m["raw"], "block": b["raw"], "block_pin": b["pin"]})
    return pairs


def headers2(probe, sample_dir, oracles):
    capture = []
    for capturer in (PIN, PEER):
        for cmd, raw in probe["captures"][capturer]["sent"]:
            if cmd == "headers2":
                capture.append({"pin": PIN, "source": "capture", "mode": "either", "raw": raw})
    o = probe["nodes"][PIN]["originals"]
    for cmd, raw in o["getheaders2_genesis"]:
        if cmd == "headers2":
            capture.append({"pin": PIN, "source": "reply to getheaders2", "mode": "reply", "raw": raw})
    for cmd, raw in o["announce"]:
        if cmd == "headers2":
            capture.append({"pin": PIN, "source": "announcement", "mode": "announce", "raw": raw})
    networks = []
    for net, res in oracles.items():
        for name in res["samples"]:
            raw = open(os.path.join(sample_dir, name + ".hex")).read().strip()
            networks.append({"name": name, "start": int(name.split("-")[1]), "mode": "reply", "raw": raw})
    return capture, networks


def write(path, header, obj):
    with open(path, "w") as f:
        for line in header:
            f.write("// %s\n" % line if line else "//\n")
        f.write(json.dumps(obj, indent=1))
        f.write("\n")


def main():
    probe = json.load(open(sys.argv[1]))
    sample_dir, out_dir = sys.argv[2], sys.argv[5]
    oracles = {"mainnet": json.load(open(sys.argv[3])), "testnet": json.load(open(sys.argv[4]))}
    for net, res in oracles.items():
        if res["failures"]:
            sys.exit("FAIL: the %s walk had failures" % net)
    msgs = messages(probe)
    capture, networks = headers2(probe, sample_dir, oracles)
    cases, framing = verdicts(probe)
    # base-sdk's corpus headers name no tool of this repository (BSX §8).
    common = [
        "Produced by Dash Core: two regtest dashds of v24.0.0-rc.3 (release",
        "binaries, SHA256SUMS checked) and a raw P2P peer driving them.",
        "\"pin\" names the release.",
        "Payloads over 4 KiB are lists of hex and {\"b\": byte, \"n\": count} runs.",
    ]
    write(os.path.join(out_dir, "core_headers2.json5"), [
        "headers2 messages Dash Core built.",
        "",
        "\"capture\": sent by each node to the other (-capturemessages) while",
        "they synced a regtest chain whose block versions vary (-blockversion, 11",
        "versions), and Core's reply to a getheaders2 from genesis and its",
        "announcements after sendheaders2 over a raw V1 connection. \"mode\" is the",
        "encoder Core used where the exchange fixes it (\"either\" otherwise).",
        "\"networks\": short getheaders2 replies (300 headers, a stop hash) of",
        "v24.0.0-rc.3 nodes on mainnet and testnet, at the windows of their chains",
        "with the most distinct versions; \"start\" is the height of the first",
        "header. The whole chain of each also went through base-sdk (decode,",
        "re-encode, reply mode, continuity, the last hash of every reply against",
        "getblockhash) without a failure.",
    ] + common[:3], {"capture": capture, "networks": networks})
    write(os.path.join(out_dir, "core_messages.json5"), [
        "Every message the two nodes sent each other while syncing (-capturemessages)",
        "and every reply to a raw P2P peer's requests (getheaders, getheaders2,",
        "getblocks, getdata for whole, compact and filtered blocks, getblocktxn,",
        "mempool, and with -peerblockfilters getcfilters/getcfheaders/getcfcheckpt).",
        "\"source\" says which. \"expect\": for the merkleblock and BIP157 replies,",
        "the block requested, or the reply's fields from the request and the node's",
        "filter index (getblockhash, getblockfilter; hashes in RPC order).",
        "\"compact\" pairs each cmpctblock with the block message of the same",
        "header.",
    ] + common, {"messages": msgs, "compact": compact_pairs(msgs)})
    write(os.path.join(out_dir, "core_verdicts.json5"), [
        "What Dash Core's handler did with each payload, after a full handshake at",
        "common version 70242 over a raw V1 connection from 127.0.0.1 (-debug=net),",
        "with the probe's own user agent.",
        "\"core\": the peer was kept (a later ping answered) or disconnected; \"log\":",
        "Core's own lines about it; \"replies\": what Core sent back (without the",
        "getsporks and govsync its sync manager sends on a timer). \"read\" is the",
        "label derived from those: throw (\"Exception ... caught\"),",
        "catch (the handler's own +100 for a failed read), too_many (+20 before",
        "reading), unread (no handler), gated (a gate before the read in this",
        "state: sendtxrcncl with reconciliation off, ssc once mnsync finished),",
        "read (no exception). \"pre\" messages went first on the same connection.",
        "\"consumed\": for read cases, the bytes Core's handler reads, from how the",
        "probe built the payload (the whole payload less the bytes it appended, or",
        "none for a command that reads nothing); \"exception\": for rejected cases,",
        "Core's exception text. ssc cases with \"mnsync_synced\" false ran right after",
        "mnsync reset, so Core read them; inv_type cases record whether Core",
        "requested the object.",
        "mode \"peerblockfilters\": the node ran -peerblockfilters=1",
        "-blockfilterindex=1.",
        "\"framing\": V1 messages with a wrong magic, a length above 3 MiB, a 3 MiB",
        "unknown command, a bad checksum (its ping unanswered: only the probe's",
        "later ping nonce 0x5eedbeef is in \"pongs\"), an invalid command, and a",
        "ping (nonce 0xabcdef) sent byte by byte.",
    ] + common, {"cases": cases, "framing": framing})


if __name__ == "__main__":
    main()
