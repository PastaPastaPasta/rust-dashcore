#!/usr/bin/env python3
"""SB11 testnet capture: a private Dash Core testnet node's own mnlistdiff / qrinfo replies and ChainLocks.

Not a test of Core: a capture harness run from the exported `test/functional/` directory of a Dash Core tag
(for its P2P framework) against a dashd that serves a copy of a synced testnet datadir with no outbound
networking (`-connect=0 -dnsseed=0 -listen=1 -bind=127.0.0.1`). Run by run-testnet.sh.

Every payload is the exact wire bytes Core sent to a raw P2P peer, or Core's RPC answer:

- `headers`: every header from `floor` to the tip (`getblockheader <hash> false`);
- `diffs.from_genesis`: `getmnlistd(null, start)`, the list the step diffs continue from;
- `diffs.step`: `getmnlistd(h - 1, h)` for every h in start+1..=tip;
- `qrinfos`: `getqrinfo(no base, cycle)` for the last rotation cycles of the DIP0024 InstantSend type, with
  `extraShare` unset (the engine's request) and set;
- `chainlocks`: the ChainLocks the tip's last blocks carry in their CbTx (`bestCLHeightDiff`,
  `bestCLSignature`, CbTx v3), each as (height, block hash, signature) with Core's `verifychainlock`;
- `quorums`: `quorum info` (members included) of every quorum of the used types active at the tip.

The datadir's ISLocks are gone (`getislocks` answers None once a block is ChainLocked), so this capture
carries none.
"""
import argparse
import gzip
import hashlib
import json
import time
from datetime import datetime, timezone

from capture_lq3a import raw_message_class
from test_framework import p2p
from test_framework.authproxy import AuthServiceProxy
from test_framework.messages import msg_getmnlistd, msg_getqrinfo
from test_framework.p2p import NetworkThread, P2PInterface

for _command in (b"mnlistdiff", b"qrinfo"):
    p2p.MESSAGEMAP[_command] = raw_message_class(_command)

WORK_DIFF_DEPTH = 8  # llmq/utils.h WORK_DIFF_DEPTH
# testnet's used roles (chainparams.cpp: CL llmq_50_60, IS llmq_60_75, Platform llmq_25_67)
USED = {"llmq_50_60": 1, "llmq_60_75": 5, "llmq_25_67": 6}
IS_TYPE = "llmq_60_75"


class RawFetcher(P2PInterface):
    """Asks one getmnlistd / getqrinfo at a time and keeps the reply's exact bytes."""

    def __init__(self):
        super().__init__()
        self.reply = None

    def on_mnlistdiff(self, message):
        self.reply = ("mnlistdiff", message.payload)

    def on_qrinfo(self, message):
        self.reply = ("qrinfo", message.payload)

    def ask(self, message, expect, timeout=120):
        with p2p.p2p_lock:
            self.reply = None
        self.send_message(message)
        self.wait_until(lambda: self.reply is not None, timeout=timeout)
        with p2p.p2p_lock:
            command, payload = self.reply
        assert command == expect, (command, expect)
        return payload.hex()


def wire(display_hex):
    """Display hex -> the uint256 integer the framework's messages serialize."""
    return int(display_hex, 16)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", required=True, help="http://user:pass@127.0.0.1:port")
    ap.add_argument("--p2p-port", type=int, required=True)
    ap.add_argument("--headers", type=int, default=5000, help="headers below the tip")
    ap.add_argument("--steps", type=int, default=1300, help="step diffs below the tip")
    ap.add_argument("--cycles", type=int, default=3, help="qrinfo cycles")
    ap.add_argument("--chainlocks", type=int, default=200, help="blocks whose CbTx ChainLock is read")
    ap.add_argument("--dashd", required=True, help="the serving dashd binary, for its sha256")
    ap.add_argument("--core-tag", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rpc = AuthServiceProxy(args.rpc, timeout=600)
    version = rpc.getnetworkinfo()["subversion"]
    chain = rpc.getblockchaininfo()
    assert chain["chain"] == "test", chain["chain"]
    tip = chain["blocks"]
    floor = tip - args.headers
    start = tip - args.steps
    hashes = {h: rpc.getblockhash(h) for h in range(floor, tip + 1)}
    headers = [rpc.getblockheader(hashes[h], False) for h in range(floor, tip + 1)]

    thread = NetworkThread()
    thread.start()
    fetcher = RawFetcher()
    fetcher.p2p_connected_to_node = True
    fetcher.peer_connect(dstaddr="127.0.0.1", dstport=args.p2p_port, net="testnet3", timeout_factor=1,
                         supports_v2_p2p=False, send_version=True)()
    # As `add_p2p_connection`: the connection is made on the network thread, after this returns.
    fetcher.wait_for_connect()
    fetcher.wait_for_verack(timeout=300)

    from_genesis = [{"base_height": None, "height": start,
                     "payload": fetcher.ask(msg_getmnlistd(0, wire(hashes[start])), "mnlistdiff")}]
    step = []
    for h in range(start + 1, tip + 1):
        step.append({"base_height": h - 1, "height": h,
                     "payload": fetcher.ask(msg_getmnlistd(wire(hashes[h - 1]), wire(hashes[h])), "mnlistdiff")})

    cycle = 288  # llmq_60_75's dkgInterval (llmq/params.h)
    last_cycle = tip - tip % cycle
    qrinfos = []
    for k in range(args.cycles):
        at = last_cycle - k * cycle
        for extra in (False, True):
            qrinfos.append({"request_height": at, "base_heights": [], "extra_share": extra,
                            "payload": fetcher.ask(msg_getqrinfo([], wire(hashes[at]), extra), "qrinfo")})
    fetcher.peer_disconnect()
    # dashd closes an idle RPC connection (`-rpcservertimeout`) while the P2P fetches run.
    rpc = AuthServiceProxy(args.rpc, timeout=600)

    seen = set()
    chainlocks = []
    for h in range(tip, tip - args.chainlocks, -1):
        cb = rpc.getblock(hashes[h], 2)["tx"][0]
        cbtx = cb.get("cbTx", {})
        if cbtx.get("version", 0) < 3:
            continue
        height = h - cbtx["bestCLHeightDiff"] - 1
        if height in seen:
            continue
        seen.add(height)
        sig = cbtx["bestCLSignature"]
        block = rpc.getblockhash(height)
        verdict = rpc.verifychainlock(block, sig, height)
        chainlocks.append({"carried_by": h, "height": height, "block_hash": block, "signature": sig,
                           "core_verify": verdict})
    assert chainlocks and all(c["core_verify"] is True for c in chainlocks), chainlocks

    listing = rpc.quorum("list")
    quorums = {}
    for tname, ltype in USED.items():
        quorums[str(ltype)] = []
        for qhash in listing.get(tname, []):
            info = rpc.quorum("info", ltype, qhash)
            assert info["quorumHash"] == qhash
            info["minedHeight"] = rpc.getblockheader(info["minedBlock"])["height"]
            quorums[str(ltype)].append(info)

    assert rpc.getblockcount() == tip
    with open(args.dashd, "rb") as f:
        dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
    fixture = {
        "provenance": {
            "dashd_subversion": version,
            "dashd_sha256": dashd_sha256,
            "core_tag": args.core_tag,
            "tool": "contrib/node/sb11-captures (capture_sb11_testnet.py)",
            "captured_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "deployments": rpc.getdeploymentinfo()["deployments"],
            "connections": rpc.getnetworkinfo()["connections"],
        },
        "network": "testnet",
        "tip_height": tip,
        "floor": floor,
        "block_hashes": [hashes[h] for h in range(floor, tip + 1)],
        "headers": headers,
        "quorum_list": listing,
        "quorums": quorums,
        "diffs": {"step": step, "from_genesis": from_genesis},
        "qrinfos": qrinfos,
        "chainlocks": chainlocks,
    }
    with gzip.open(args.out, "wt") as f:
        json.dump(fixture, f, indent=1, sort_keys=True)
        f.write("\n")
    thread.close()
    print("fixture written to %s: %d headers, %d step diffs, %d qrinfos, %d chainlocks" % (
        args.out, len(headers), len(step), len(qrinfos), len(chainlocks)))


if __name__ == "__main__":
    t0 = time.time()
    try:
        main()
    finally:
        # The network thread is not a daemon: close it, or a failed capture never exits.
        if NetworkThread.network_event_loop is not None and NetworkThread.network_event_loop.is_running():
            NetworkThread.network_event_loop.call_soon_threadsafe(NetworkThread.network_event_loop.stop)
    print("%.0f s" % (time.time() - t0))
