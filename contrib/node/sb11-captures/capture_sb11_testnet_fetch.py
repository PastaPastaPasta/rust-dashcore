#!/usr/bin/env python3
"""SB11 testnet fetch capture: Core's one `mnlistdiff` per (base, target) the engine asks for.

Not a test of Core: a capture harness run from the exported `test/functional/` directory of a Dash Core tag
(for its P2P framework) against the same kind of private testnet dashd as capture_sb11_testnet.py (a copy of
a synced testnet datadir, no outbound networking). Run by run-testnet.sh with SB11_CAPTURE set to this file.

The testnet capture's step diffs answer `getmnlistd(h - 1, h)` only; a fetch after a restart asks Core for one
diff from an arbitrary held base to its target, which holds the target's list only (`evo/smldiff.cpp`
rc.3:154-209). Each `--pair BASE:TARGET` (heights; BASE `null` for a null base) is asked once and kept as the
exact wire bytes Core sent, with both blocks' hashes and the payload's sha256 (DEC-481, SB11 notes D55).
"""
import argparse
import gzip
import hashlib
import json
import time
from datetime import datetime, timezone

# Importing it registers the raw `mnlistdiff` reply class.
from capture_sb11_testnet import RawFetcher, wire
from test_framework.authproxy import AuthServiceProxy
from test_framework.messages import msg_getmnlistd
from test_framework.p2p import NetworkThread


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", required=True, help="http://user:pass@127.0.0.1:port")
    ap.add_argument("--p2p-port", type=int, required=True)
    ap.add_argument("--pair", action="append", required=True, help="BASE:TARGET heights, BASE may be null")
    ap.add_argument("--expect-tip", type=int, required=True, help="the testnet capture's tip")
    ap.add_argument("--dashd", required=True, help="the serving dashd binary, for its sha256")
    ap.add_argument("--core-tag", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    pairs = []
    for p in args.pair:
        base, target = p.split(":")
        pairs.append((None if base == "null" else int(base), int(target)))
    rpc = AuthServiceProxy(args.rpc, timeout=600)
    version = rpc.getnetworkinfo()["subversion"]
    chain = rpc.getblockchaininfo()
    assert chain["chain"] == "test", chain["chain"]
    assert chain["blocks"] == args.expect_tip, (chain["blocks"], args.expect_tip)
    heights = {h for pair in pairs for h in pair if h is not None}
    hashes = {h: rpc.getblockhash(h) for h in sorted(heights)}

    thread = NetworkThread()
    thread.start()
    fetcher = RawFetcher()
    fetcher.p2p_connected_to_node = True
    fetcher.peer_connect(dstaddr="127.0.0.1", dstport=args.p2p_port, net="testnet3", timeout_factor=1,
                         supports_v2_p2p=False, send_version=True)()
    fetcher.wait_for_connect()
    fetcher.wait_for_verack(timeout=300)
    diffs = []
    for base, target in pairs:
        payload = fetcher.ask(msg_getmnlistd(0 if base is None else wire(hashes[base]), wire(hashes[target])),
                              "mnlistdiff")
        diffs.append({"base_height": base, "base_hash": None if base is None else hashes[base],
                      "height": target, "block_hash": hashes[target], "payload": payload,
                      "payload_sha256": hashlib.sha256(bytes.fromhex(payload)).hexdigest()})
    fetcher.peer_disconnect()
    # dashd closes an idle RPC connection (`-rpcservertimeout`) while the P2P fetches run.
    rpc = AuthServiceProxy(args.rpc, timeout=600)
    assert rpc.getblockcount() == args.expect_tip
    with open(args.dashd, "rb") as f:
        dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
    fixture = {
        "provenance": {
            "dashd_subversion": version,
            "dashd_sha256": dashd_sha256,
            "core_tag": args.core_tag,
            "tool": "contrib/node/sb11-captures (capture_sb11_testnet_fetch.py)",
            "request": "P2P getmnlistd(baseBlockHash, blockHash); a null base is the zero hash",
            "captured_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "connections": rpc.getnetworkinfo()["connections"],
        },
        "network": "testnet",
        "tip_height": args.expect_tip,
        "diffs": diffs,
    }
    with gzip.open(args.out, "wt") as f:
        json.dump(fixture, f, indent=1, sort_keys=True)
        f.write("\n")
    thread.close()
    print("fixture written to %s: %s" % (args.out, ", ".join(
        "%s->%d" % (d["base_height"], d["height"]) for d in diffs)))


if __name__ == "__main__":
    t0 = time.time()
    try:
        main()
    finally:
        # The network thread is not a daemon: close it, or a failed capture never exits.
        if NetworkThread.network_event_loop is not None and NetworkThread.network_event_loop.is_running():
            NetworkThread.network_event_loop.call_soon_threadsafe(NetworkThread.network_event_loop.stop)
    print("%.0f s" % (time.time() - t0))
