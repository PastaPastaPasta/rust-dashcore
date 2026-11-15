#!/usr/bin/env python3
"""G4 transaction relay transcripts from SHA-pinned Dash Core v24.0.0 final.

Usage: P2A_DASHD=... P2A_CAPTURE_DIR=... python3 capture.py OUT_DIR
Pool admission is recorded as `txpool HEX TIME ANCESTORS FEE`, after the same
transaction passes dashd's sendrawtransaction RPC. The replay installs that entry
through SimTxPool and calls relay_transaction; admission belongs to G3/G5.
"""
import hashlib
import importlib.util
import pathlib
import struct
import sys

spec = importlib.util.spec_from_file_location(
    "p4b_capture", pathlib.Path(__file__).resolve().parents[1] / "p4b-transcripts/capture.py")
cap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cap)
p2a, p3 = cap.p2a, cap.p3
DASHD_SHA256 = "2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223"
# P4b's scenario checks and records this pin before starting its node.
p3.DASHD_SHA256 = DASHD_SHA256


def add(t, height, tag):
    block = t.node.rpc("getblock", t.node.rpc("getblockhash", height), 2)
    coinbase = block["tx"][0]
    value = coinbase["vout"][0]["valueSat"] - 10000
    raw = (struct.pack("<iB", 1, 1) + bytes.fromhex(coinbase["txid"])[::-1]
           + struct.pack("<IBIBqB", 0, 0, 0xffffffff, 1, value, 25) + b"\x76\xa9\x14" + bytes([1]) * 20 + b"\x88\xac"
           + struct.pack("<I", tag))
    txid = t.node.rpc("sendrawtransaction", raw.hex(), 0)
    entry = t.node.rpc("getmempoolentry", txid)
    t.lines.append(f"txpool {raw.hex()} {entry['time']} {entry['ancestorcount']} 10000")
    t.settle()
    return bytes.fromhex(txid)[::-1]


def trickle(out):
    with cap.scenario(out, "tx-trickle", 0, [
            "20 equal-size, equal-fee independent transactions: ordering is descending txid bytes.",
            "One inbound and one outbound peer; the mock-time jump releases both queues.",
            "Random inter-arrival times are measured separately, not compared byte-for-byte."], 130, extra=("-acceptnonstdtxn=1",)) as (t, _):
        cap.peer(t, 1)
        t.connect(2, "outbound-full-relay")
        t.handshake(2, cap.FULL | cap.BIT11)
        for i in range(20):
            add(t, i + 1, i)
        t.set_time(cap.SIM_START + 120)


def getdata(out):
    with cap.scenario(out, "tx-getdata", 1, [
            "A late peer cannot retrieve a fresh unannounced transaction before two minutes.",
            "An announced transaction remains retrievable from mapRelay after mining removes it."], 130, extra=("-acceptnonstdtxn=1",)) as (t, _):
        cap.peer(t, 1)
        txid = add(t, 1, 1)
        t.set_time(cap.SIM_START + 30)
        cap.peer(t, 2)
        t.send(2, "getdata", cap.getdata([(1, txid)]))
        t.set_time(cap.SIM_START + 119)
        t.send(2, "getdata", cap.getdata([(1, txid)]))
        t.set_time(cap.SIM_START + 120)
        t.send(2, "getdata", cap.getdata([(1, txid)]))
        blockhash = t.node.rpc("generatetodescriptor", 1, "raw(51)")[0]
        t.lines.append(f"txpoolremove {txid.hex()}")
        t.lines.append(f"submitblock {t.node.rpc('getblock', blockhash, 0)}")
        t.settle()
        t.send(1, "getdata", cap.getdata([(1, txid)]))


def mempool(out):
    with cap.scenario(out, "tx-mempool-bip35", 2, [
            "BIP35 without and with a bloom filter; both requests wait for trickle.",
            "Signed ISDLOCK/CLSIG additions are covered by focused seam simulations."], 130, extra=("-acceptnonstdtxn=1",)) as (t, _):
        hashes = [add(t, i + 1, i) for i in range(3)]
        cap.peer(t, 1)
        t.send(1, "mempool")
        t.set_time(cap.SIM_START + 30)
        cap.peer(t, 2)
        t.send(2, "filterload", cap.filterload(100, 10, [hashes[1]]))
        t.send(2, "mempool")
        t.set_time(cap.SIM_START + 60)
    with cap.scenario(out, "tx-mempool-nobloom", 3, [
            "Without NODE_BLOOM a plain peer is disconnected on its BIP35 request."],
            0, extra=("-peerbloomfilters=0",)) as (t, _):
        cap.peer(t, 1)
        t.send(1, "mempool")


def main():
    if p3.dashd_sha256() != DASHD_SHA256:
        sys.exit("requires the pinned Dash Core v24.0.0 final binary")
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for capture in [trickle, getdata, mempool]:
        capture(out)
    for path in sorted(out.glob("tx-*.txt")):
        print(path.name, hashlib.sha256(path.read_bytes()).hexdigest())


if __name__ == "__main__":
    main()
