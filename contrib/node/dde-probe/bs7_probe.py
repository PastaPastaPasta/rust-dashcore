#!/usr/bin/env python3
"""BS7's Core-judged evidence: originals Core sends, handler verdicts, V1 framing.

Usage: DDE_PAIR=v24 bs7_probe.py OUT.json
Needs the v24 pair freshly started (DDE_PAIR=v24 nodes.py start
-debug=validation) with empty datadirs: v24.0.0-rc.3 is the only parity
reference (DEC-82). Builds one regtest chain whose block versions vary
(-blockversion, mined in segments on the node under test with restarts in
between) and syncs it to the peer over a connection both nodes capture
(-capturemessages), so Core's getheaders2 replies and block announcements are
captured. Then, against the node under test:

- originals: messages Core builds on request (getheaders2, getdata for a
  compact, filtered or whole block and a tx, getblocktxn, getblocks, mempool);
- cases: every BS7 command with valid, cut, oversized and trailing payloads,
  recording whether Core kept the peer and its own log lines;
- framing: V1 headers with a wrong magic, a length above 3 MiB, a bad
  checksum, an invalid command, and a message split byte by byte;
- filters: the BIP157 requests after a restart with -peerblockfilters=1
  -blockfilterindex=1, which makes Core serve cfilter, cfheaders, cfcheckpt;
- expect: for each reply to the filtered block and BIP157 requests, its
  fields as requested and from the node's own filter index (RPC), which the
  corpus test compares with base-sdk's decode.

Leaves the nodes running in the filter mode.
"""
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import time

import nodes
from p2p import MAGIC, Peer, frame, read_capture, sha256d

if os.environ.get("DDE_PAIR") != "v24":
    sys.exit("bs7_probe.py runs on the v24 pair: set DDE_PAIR=v24 (DEC-82)")
NODE, PEER = list(nodes.NODES)
# The probe's own user agent: it identifies as itself, not as Core (DEC-83).
USER_AGENT = b"/dde-probe:bs7/"

# Version segments of the shared chain: (version, blocks). Eleven versions,
# so the 7-entry list evicts, with repeats that hit it at every depth.
SEGMENTS = [
    (0x20000000, 12), (0x20000001, 3), (0x20000002, 2), (0x20000000, 2), (0x20000004, 4),
    (0x20000008, 1), (0x20000010, 2), (0x20000001, 1), (0x20000020, 3), (0x20000040, 1),
    (0x20000080, 2), (4, 2), (5, 1), (0x20000000, 1), (0x20000002, 2), (0x20000100, 1),
    (0x20000004, 2), (0x20000000, 130),
]
DESC = "raw(51)"
MSG_TX, MSG_BLOCK, MSG_FILTERED_BLOCK, MSG_CMPCT_BLOCK = 1, 2, 3, 20


def u32(n):
    return struct.pack("<I", n)


def u64(n):
    return struct.pack("<Q", n)


def compact(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    if n <= 0xFFFFFFFF:
        return b"\xfe" + u32(n)
    return b"\xff" + u64(n)


def h(display_hex):
    """A hash from RPC (display order) in wire order."""
    return bytes.fromhex(display_hex)[::-1]


def locator(hashes, version=70240):
    return struct.pack("<i", version) + compact(len(hashes)) + b"".join(hashes) + b"\0" * 32


def getdata(*items):
    return compact(len(items)) + b"".join(u32(t) + x for t, x in items)


def filterload(size, funcs, fill=0xFF):
    return compact(size) + bytes([fill]) * size + u32(funcs) + u32(0) + b"\x00"


def launch(node, *extra):
    """Starts one node (`nodes.start` starts both) and waits for its RPC."""
    release = nodes.NODES[node][0]
    d = nodes.datadir(node)
    subprocess.Popen([os.path.join(release, "bin", "dashd"), "-datadir=%s" % d, "-debug=validation", *extra],
                     stdout=open(d / "stdout.log", "a"), stderr=subprocess.STDOUT, start_new_session=True)
    for _ in range(900):
        try:
            nodes.cli(node, "getblockcount")
            return
        except RuntimeError:
            time.sleep(0.5)
    raise RuntimeError("%s did not start" % node)


def restart(node, *extra):
    nodes.cli(node, "stop")
    pid = nodes.datadir(node) / "regtest" / "dashd.pid"
    for _ in range(120):
        if not pid.exists():
            break
        time.sleep(0.5)
    launch(node, *extra)


def wait_synced(timeout=120):
    end = time.time() + timeout
    while time.time() < end:
        tips = [nodes.cli(n, "getbestblockhash") for n in (NODE, PEER)]
        if tips[0] == tips[1]:
            return tips[0]
        time.sleep(0.5)
    raise RuntimeError("nodes did not sync")


def connect(a, b):
    nodes.cli(a, "addnode", "127.0.0.1:%d" % nodes.p2p_port(b), "onetry")



def disconnect_all(node):
    for p in nodes.cli(node, "getpeerinfo"):
        nodes.cli(node, "disconnectnode", "", str(p["id"]))
    time.sleep(1)


def build_chain():
    """The version-diverse chain on the node under test, synced to the peer
    under capture."""
    for version, blocks in SEGMENTS[:-1]:
        restart(NODE, "-blockversion=%d" % version)
        nodes.cli(NODE, "generatetodescriptor", str(blocks), DESC)
    restart(NODE)
    nodes.cli(NODE, "generatetodescriptor", str(SEGMENTS[-1][1]), DESC)
    connect(PEER, NODE)
    wait_synced()
    # Transactions for compact, filtered and whole blocks, and one left in the
    # mempool: the node spends anyone-can-spend coinbases, the peer relays.
    nodes.cli(NODE, "createwallet", "w")
    addr = nodes.cli(NODE, "getnewaddress")
    nodes.cli(NODE, "generatetoaddress", "101", addr)
    wait_synced()
    for _ in range(6):
        nodes.cli(NODE, "sendtoaddress", nodes.cli(NODE, "getnewaddress"), "2.5")
    time.sleep(2)
    nodes.cli(NODE, "generatetoaddress", "1", addr)
    wait_synced()
    nodes.cli(NODE, "generatetoaddress", "3", addr)
    wait_synced()
    # Blocks the peer mines reach the node as announcements.
    nodes.cli(PEER, "generatetodescriptor", "2", DESC)
    wait_synced()
    nodes.cli(NODE, "sendtoaddress", nodes.cli(NODE, "getnewaddress"), "1.0")
    time.sleep(2)
    # A wiped peer re-syncs from the node, which sends every block whole, the
    # ones it announced as compact blocks included.
    disconnect_all(PEER)
    nodes.cli(PEER, "stop")
    time.sleep(3)
    for sub in ("blocks", "chainstate", "evodb", "llmq", "indexes"):
        shutil.rmtree(nodes.datadir(PEER) / "regtest" / sub, ignore_errors=True)
    launch(PEER)
    connect(PEER, NODE)
    wait_synced(300)
    nodes.cli(PEER, "generatetodescriptor", "2", DESC)
    wait_synced()


def captures():
    """Every message each node captured, by direction: (command, payload hex)."""
    out = {}
    for node in nodes.NODES:
        d = nodes.datadir(node) / "regtest" / "message_capture"
        out[node] = {"sent": [], "recv": []}
        for f in sorted(d.glob("*/msgs_*.dat")):
            way = "sent" if f.name == "msgs_sent.dat" else "recv"
            out[node][way] += [[cmd, payload.hex()] for _, cmd, payload in read_capture(f)]
    return out


def base_version():
    """The probe's version payload: the peer's captured one, with the probe's
    own user agent (its fields before the user agent take 80 bytes)."""
    for f in (nodes.datadir(PEER) / "regtest" / "message_capture").glob("*/msgs_sent.dat"):
        for _, cmd, payload in read_capture(f):
            if cmd == "version":
                n, at = payload[80], 81
                assert n < 0xFD, "user agent length"
                return payload[:80] + compact(len(USER_AGENT)) + USER_AGENT + payload[at + n:]
    raise RuntimeError("no captured version")


def handshake(node, base, version=None):
    p = Peer(node)
    p.send("version", base if version is None else struct.pack("<i", version) + base[4:])
    p.read(1.0)
    p.send("verack")
    assert p.sync(), "handshake"
    return p


def app_messages(p, before):
    """What Core sent after message `before`, without pings and pongs."""
    return [[c, pl.hex()] for c, pl in p.received[before:] if c not in ("ping", "pong")]


KEYS = ("Exception", "Misbehaving", "violation", "disconnect", "Unknown command", "Header error", "locator size",
        "invalid", "Ignoring", "unrequested", "out-of-bounds", "SYNCSTATUSCOUNT", "peer %d requested")


def notable(p):
    return [l for l in p.log() if any(k in l for k in KEYS)]


def run_case(node, base, name, msgs, version=None, raw=None, rpc=None, wait=0.0):
    """Sends `msgs` ((command, payload) pairs) after a handshake, then a ping.

    `raw` is sent as is instead (framing cases). Records whether Core kept the
    peer, its log lines and the commands it sent back.
    """
    p = handshake(node, base, version)
    before = len(p.received)
    synced_mn = None
    if rpc:
        nodes.cli(node, *rpc)
        synced_mn = nodes.cli(node, "mnsync", "status")["IsSynced"]
    if raw is not None:
        for chunk in raw:
            p.sock.sendall(chunk)
            time.sleep(0.002)
    else:
        for cmd, payload in msgs:
            p.send(cmd, payload)
    synced = p.sync(0x5EEDBEEF)
    time.sleep(0.4 + wait)
    p.read(0.4)
    replies = app_messages(p, before)
    pongs = [pl.hex() for c, pl in p.received[before:] if c == "pong"]
    out = {
        "name": name,
        "command": msgs[-1][0] if msgs else None,
        "raw": msgs[-1][1].hex() if msgs else None,
        "pre": [[c, pl.hex()] for c, pl in msgs[:-1]] if msgs else [],
        "version": version,
        "mnsync_synced": synced_mn,
        "core": "disconnect" if p.closed else ("accept" if synced else "no answer"),
        "log": notable(p),
        "replies": replies,
        "pongs": pongs,
    }
    p.close()
    return out


def request(node, base, msgs, wait_for, seconds=10):
    """Sends `msgs` and returns the messages Core sends until it has sent every
    command in `wait_for`."""
    p = handshake(node, base)
    before = len(p.received)
    for cmd, payload in msgs:
        p.send(cmd, payload)
    end = time.time() + seconds
    while time.time() < end:
        p.read(0.2)
        got = [c for c, _ in p.received[before:]]
        if all(w in got for w in wait_for):
            break
    p.read(0.3)
    out = app_messages(p, before)
    p.close()
    return out


def fresh_tx_block():
    """A block with transactions at the tip, and one transaction left in the
    mempool: Core serves getblocktxn only within 10 blocks of its tip."""
    # mnsync reset makes the sync re-run, and its tick drops the peer it
    # synced from: reconnect the two nodes.
    if not nodes.cli(PEER, "getpeerinfo"):
        connect(PEER, NODE)
        for _ in range(60):
            if nodes.cli(PEER, "getpeerinfo"):
                break
            time.sleep(0.5)
    try:
        nodes.cli(NODE, "loadwallet", "w")
    except RuntimeError:
        pass  # already loaded
    addr = nodes.cli(NODE, "getnewaddress")
    for _ in range(3):
        nodes.cli(NODE, "sendtoaddress", nodes.cli(NODE, "getnewaddress"), "1.5")
    nodes.cli(NODE, "generatetoaddress", "1", addr)
    wait_synced()
    nodes.cli(NODE, "sendtoaddress", nodes.cli(NODE, "getnewaddress"), "0.5")
    for _ in range(40):
        if all(nodes.cli(n, "getrawmempool") for n in (NODE, PEER)):
            break
        time.sleep(0.25)


def chain_facts(node):
    tip = nodes.cli(node, "getbestblockhash")
    height = int(nodes.cli(node, "getblockcount"))
    blk = nodes.cli(node, "getblock", tip, "2")
    # The newest block with more than one transaction.
    tx_block = blk
    while len(tx_block["tx"]) < 2:
        tx_block = nodes.cli(node, "getblock", tx_block["previousblockhash"], "2")
    mempool = nodes.cli(node, "getrawmempool")
    return {
        "tip": tip,
        "height": height,
        "genesis": nodes.cli(node, "getblockhash", "0"),
        "tx_block": tx_block["hash"],
        "tx_block_txs": len(tx_block["tx"]),
        "tx_block_depth": height - tx_block["height"],
        "mempool_tx": mempool[0] if mempool else None,
    }


def originals(node, base, facts):
    tip, txb, gen = h(facts["tip"]), h(facts["tx_block"]), h(facts["genesis"])
    out = {
        "getheaders2_genesis": request(node, base, [("getheaders2", locator([gen]))], ["headers2"]),
        "getheaders_genesis": request(node, base, [("getheaders", locator([gen]))], ["headers"]),
        "getblocks_genesis": request(node, base, [("getblocks", locator([gen]))], ["inv"]),
        "getdata_block": request(node, base, [("getdata", getdata((MSG_BLOCK, txb)))], ["block"]),
        "getdata_cmpct_tip": request(node, base, [("getdata", getdata((MSG_CMPCT_BLOCK, tip)))], ["cmpctblock"]),
        "getblocktxn": request(node, base, [("getblocktxn", txb + compact(1) + compact(1))], ["blocktxn"]),
        "filtered_block": request(
            node, base, [("filterload", filterload(64, 3)), ("getdata", getdata((MSG_FILTERED_BLOCK, txb)))],
            ["merkleblock", "tx"]),
        "mempool": request(node, base, [("mempool", b"")], ["inv"]),
    }
    if facts["mempool_tx"]:
        out["getdata_tx"] = request(node, base, [("getdata", getdata((MSG_TX, h(facts["mempool_tx"]))))], ["tx"])
    # Announcements: headers2 after sendheaders2 once Core knows we have its tip.
    p = handshake(node, base)
    p.send("sendheaders2")
    p.send("getheaders2", locator([tip]))
    p.read(1.0)
    before = len(p.received)
    nodes.cli(node, "generatetodescriptor", "1", DESC)
    p.read(2.0)
    nodes.cli(node, "generatetodescriptor", "3", DESC)
    p.read(3.0)
    out["announce"] = app_messages(p, before)
    p.close()
    return out


def core_payload(origs, key, cmd):
    for c, pl in origs.get(key, []):
        if c == cmd:
            return bytes.fromhex(pl)
    return None


def cases(node, base, facts, origs):
    tip, txb, gen = h(facts["tip"]), h(facts["tx_block"]), h(facts["genesis"])
    block = core_payload(origs, "getdata_block", "block")
    cmpct = core_payload(origs, "getdata_cmpct_tip", "cmpctblock")
    blocktxn = core_payload(origs, "getblocktxn", "blocktxn")
    reply2 = core_payload(origs, "getheaders2_genesis", "headers2")
    tx = core_payload(origs, "getdata_tx", "tx") or core_payload(origs, "filtered_block", "tx")
    header = block[:80]
    out = []

    def case(name, cmd, payload, pre=(), version=None, trailing=0, reads=None, rpc=None, wait=0.0):
        """`reads`: the bytes Core's handler reads when it reads the payload,
        from how the case is built: the whole payload less `trailing`."""
        r = run_case(node, base, name, list(pre) + [(cmd, payload)], version, rpc=rpc, wait=wait)
        r["core_reads"] = len(payload) - trailing if reads is None else reads
        out.append(r)

    # BIP152.
    case("sendcmpct_hb0_v1", "sendcmpct", b"\x00" + u64(1))
    case("sendcmpct_hb02_v1", "sendcmpct", b"\x02" + u64(1))
    case("sendcmpct_v2", "sendcmpct", b"\x01" + u64(2))
    case("sendcmpct_short", "sendcmpct", b"\x01\x01\x00\x00")
    case("sendcmpct_empty", "sendcmpct", b"")
    case("sendcmpct_trailing", "sendcmpct", b"\x00" + u64(1) + b"\xff", trailing=1)
    case("cmpctblock_core", "cmpctblock", cmpct)
    case("cmpctblock_cut", "cmpctblock", cmpct[:-3])
    case("cmpctblock_trailing", "cmpctblock", cmpct + b"\x00\x01", trailing=2)
    case("cmpctblock_prefilled_index_65535", "cmpctblock",
         header + u64(7) + compact(0) + compact(1) + b"\xfd\xff\xff" + tx)
    case("cmpctblock_prefilled_index_65536", "cmpctblock",
         header + u64(7) + compact(0) + compact(1) + b"\xfe\x00\x00\x01\x00" + tx)
    case("cmpctblock_total_65535", "cmpctblock", header + u64(7) + compact(65535) + b"\x11" * 6 * 65535 + compact(0))
    case("cmpctblock_total_65536", "cmpctblock", header + u64(7) + compact(65535) + b"\x11" * 6 * 65535 + compact(1)
         + compact(0) + tx)
    case("cmpctblock_shortids_count_past_end", "cmpctblock", header + u64(7) + compact(1000) + b"\x11" * 12)
    case("getblocktxn_tx_block_1", "getblocktxn", txb + compact(1) + compact(1))
    case("getblocktxn_out_of_range", "getblocktxn", txb + compact(1) + compact(500))
    case("getblocktxn_overflow", "getblocktxn", txb + compact(2) + compact(65535) + compact(0))
    case("getblocktxn_diff_65536", "getblocktxn", txb + compact(1) + b"\xfe\x00\x00\x01\x00")
    case("getblocktxn_unknown_block", "getblocktxn", b"\x42" * 32 + compact(1) + compact(0))
    case("getblocktxn_cut", "getblocktxn", txb + compact(3) + compact(0))
    case("blocktxn_unrequested", "blocktxn", blocktxn)
    case("blocktxn_cut", "blocktxn", blocktxn[:-1])
    # BIP37.
    case("filterload_36000", "filterload", filterload(36000, 1))
    case("filterload_36001", "filterload", filterload(36001, 1))
    case("filterload_funcs_50", "filterload", filterload(10, 50))
    case("filterload_funcs_51", "filterload", filterload(10, 51))
    case("filterload_cut", "filterload", filterload(10, 1)[:-1])
    case("filterload_trailing", "filterload", filterload(10, 1) + b"\x01\x02", trailing=2)
    case("filteradd_520", "filteradd", compact(520) + b"\x01" * 520, pre=[("filterload", filterload(10, 1))])
    case("filteradd_521", "filteradd", compact(521) + b"\x01" * 521, pre=[("filterload", filterload(10, 1))])
    case("filteradd_cut", "filteradd", compact(5) + b"\x01" * 4, pre=[("filterload", filterload(10, 1))])
    case("filteradd_no_filter", "filteradd", compact(3) + b"\x01" * 3)
    case("filterclear_payload", "filterclear", b"\x01\x02", reads=0)
    case("merkleblock_garbage", "merkleblock", b"\x01\x02\x03", reads=0)
    case("mempool_payload", "mempool", b"\x01", reads=0)
    # BIP157 without -peerblockfilters (the default).
    case("getcfilters_basic", "getcfilters", b"\x00" + u32(0) + tip)
    case("getcfilters_cut", "getcfilters", b"\x00" + u32(0) + tip[:-1])
    case("getcfheaders_basic", "getcfheaders", b"\x00" + u32(0) + tip)
    case("getcfheaders_cut", "getcfheaders", b"\x00\x00")
    case("getcfcheckpt_basic", "getcfcheckpt", b"\x00" + tip)
    case("getcfcheckpt_cut", "getcfcheckpt", b"\x00" + tip[:5])
    for cmd in ("cfilter", "cfheaders", "cfcheckpt"):
        case("%s_garbage" % cmd, cmd, b"\x09\x08\x07", reads=0)
    # Locators.
    for cmd in ("getheaders2", "getblocks", "getheaders"):
        case("%s_locator_101" % cmd, cmd, locator([gen] * 101))
        case("%s_locator_102" % cmd, cmd, locator([gen] * 102))
        case("%s_cut" % cmd, cmd, locator([gen])[:-1])
        case("%s_trailing" % cmd, cmd, locator([gen]) + b"\x05", trailing=1)
    # headers2: a Core reply sent back, cut and with trailing bytes.
    case("headers2_core_reply", "headers2", reply2)
    case("headers2_core_reply_cut", "headers2", reply2[:-2])
    case("headers2_core_reply_trailing", "headers2", reply2 + b"\x00", trailing=1)
    # block and tx.
    case("block_core", "block", block)
    case("block_cut", "block", block[:-1])
    case("block_trailing", "block", block + b"\x00\x00", trailing=2)
    case("tx_core", "tx", tx)
    case("tx_cut", "tx", tx[:-1])
    case("tx_trailing", "tx", tx + b"\xff", trailing=1)
    # Small control messages.
    case("sendheaders2_payload", "sendheaders2", b"\x07\x07", reads=0)
    case("qwatch_payload", "qwatch", b"\x01", reads=0)
    case("ssc_ok", "ssc", struct.pack("<ii", 2, 7))
    case("ssc_cut", "ssc", struct.pack("<i", 2))
    case("ssc_trailing", "ssc", struct.pack("<ii", 2, 7) + b"\x01", trailing=1)
    case("getmnlistd_genesis_tip", "getmnlistd", gen + tip)
    case("getmnlistd_cut", "getmnlistd", gen + tip[:-1])
    case("senddsq_02", "senddsq", b"\x02")
    case("senddsq_empty", "senddsq", b"")
    case("qsendrecsigs_ff", "qsendrecsigs", b"\xff")
    case("qsendrecsigs_empty", "qsendrecsigs", b"")
    case("sendtxrcncl_off", "sendtxrcncl", u32(1) + u64(5), reads=0)
    # Inventory type 34 is known at v24 only; type 1 shows the request path.
    for t, v in ((34, 70240), (34, 70242), (1, 70240)):
        obj = hashlib.sha256(b"bs7 inv %d %d" % (t, v)).digest()
        case("inv_type%d_at_%d" % (t, v), "inv", compact(1) + u32(t) + obj, version=v, wait=6.0)
    # ssc once the masternode sync is reset, so Core reads it (last: the
    # reset changes the node's state for anything after it).
    reset = ("mnsync", "reset")
    case("ssc_unsynced_ok", "ssc", struct.pack("<ii", 2, 7), rpc=reset)
    case("ssc_unsynced_cut", "ssc", struct.pack("<i", 2), rpc=reset)
    case("ssc_unsynced_trailing", "ssc", struct.pack("<ii", 2, 7) + b"\x01", trailing=1, rpc=reset)
    return out


def framing_frames():
    """The framing cases: name -> the byte chunks sent, in order."""
    ping = frame("ping", u64(0xABCDEF))
    bad_sum = bytearray(ping)
    bad_sum[20] ^= 0xFF
    big = 3 * 1024 * 1024
    return {
        "wrong_magic": [b"\x00\x00\x00\x00" + ping[4:]],
        "length_3MiB_plus_1": [MAGIC + b"ping".ljust(12, b"\0") + u32(big + 1) + b"\0" * 4],
        "length_3MiB": [frame("bigunknown", b"\0" * big)],
        "bad_checksum": [bytes(bad_sum)],
        "invalid_command": [frame(b"pi\x1fng", u64(1))],
        "split_bytewise": [bytes([b]) for b in ping],
    }


def filter_expectations(node, start, height, tip):
    """The cfilter, cfheaders and cfcheckpt replies to requests from `start`
    to `tip`, from Core's index: one dict per reply, in order, in the
    corpus's serde form (hashes in RPC order)."""
    hashes = [nodes.cli(node, "getblockhash", str(i)) for i in range(start, height + 1)]
    filters = [nodes.cli(node, "getblockfilter", b) for b in hashes]
    prev = nodes.cli(node, "getblockfilter", nodes.cli(node, "getblockhash", str(start - 1)))["header"] if start \
        else "00" * 32
    filter_hashes = [sha256d(bytes.fromhex(f["filter"])) for f in filters]
    # BIP157's header chain from these hashes must reach Core's own header,
    # so the hashes and their byte order are the ones Core sends.
    header = h(prev)
    for fh in filter_hashes:
        header = sha256d(fh + header)
    assert header == h(filters[-1]["header"]), "filter header chain"
    checkpoints = [nodes.cli(node, "getblockfilter", nodes.cli(node, "getblockhash", str(k)))["header"]
                   for k in range(1000, height + 1, 1000)]
    return {
        "getcfilters": [{"filter_type": 0, "block_hash": b, "filter": f["filter"]} for b, f in zip(hashes, filters)],
        "getcfheaders": [{"filter_type": 0, "stop_hash": tip, "prev_header": prev,
                          "filter_hashes": [fh[::-1].hex() for fh in filter_hashes]}],
        "getcfcheckpt": [{"filter_type": 0, "stop_hash": tip, "filter_headers": checkpoints}],
    }


def framing(node, base):
    return [run_case(node, base, name, [], raw=chunks) for name, chunks in framing_frames().items()]


def main():
    out_path = sys.argv[1]
    # BS7_SKIP_BUILD=1 reuses a chain an earlier run built (before any probe
    # peer connected, so the captures are still the two nodes' alone).
    if not os.environ.get("BS7_SKIP_BUILD"):
        build_chain()
    base = base_version()
    # Before any probe peer connects, so only the two nodes' connections count.
    res = {"base_version": base.hex(), "captures": captures(), "nodes": {}}
    for node in nodes.UNDER_TEST:
        fresh_tx_block()
        facts = chain_facts(node)
        origs = originals(node, base, facts)
        res["nodes"][node] = {
            "facts": facts,
            "originals": origs,
            "cases": cases(node, base, facts, origs),
            "framing": framing(node, base),
            # The filtered block's merkleblock carries the requested block.
            "expect": {"filtered_block": [{"block_hash": facts["tx_block"]}]},
        }
        print(node, "done", file=sys.stderr)
    # BIP157 serving.
    for node in nodes.UNDER_TEST:
        restart(node, "-peerblockfilters=1", "-blockfilterindex=1")
    for node in nodes.UNDER_TEST:
        for _ in range(240):
            info = nodes.cli(node, "getindexinfo")
            if info.get("basic block filter index", {}).get("synced"):
                break
            time.sleep(0.5)
        facts = chain_facts(node)
        tip = h(facts["tip"])
        start = max(0, facts["height"] - 5)
        res["nodes"][node]["filters"] = {
            "getcfilters": request(node, base, [("getcfilters", b"\x00" + u32(start) + tip)], ["cfilter"]),
            "getcfheaders": request(node, base, [("getcfheaders", b"\x00" + u32(start) + tip)], ["cfheaders"]),
            "getcfcheckpt": request(node, base, [("getcfcheckpt", b"\x00" + tip)], ["cfcheckpt"]),
            "cases": [
                run_case(node, base, "getcfilters_type_1", [("getcfilters", b"\x01" + u32(start) + tip)]),
                run_case(node, base, "getcfilters_from_genesis", [("getcfilters", b"\x00" + u32(0) + tip)]),
                run_case(node, base, "getcfheaders_from_genesis", [("getcfheaders", b"\x00" + u32(0) + tip)]),
                run_case(node, base, "getcfilters_cut", [("getcfilters", b"\x00" + u32(start))]),
            ],
        }
        res["nodes"][node]["expect"].update(filter_expectations(node, start, facts["height"], facts["tip"]))
    json.dump(res, open(out_path, "w"), indent=1)


if __name__ == "__main__":
    main()
