#!/usr/bin/env python3
"""Capture G3 transaction intake against the SHA-pinned Dash Core v24.0.0 final.

Built on P4b's node/peer/transcript tooling. Admission records retain Core's
validation decisions for the simulated mempool; utxo records seed the replay's
input-availability checks. Transaction inventory emission is G4's responsibility.

P2A_CAPTURE_DIR, P2A_PORT_BASE, and P2A_DASHD select private capture resources.
Usage: capture.py OUT_DIR [SCENARIO ...]
"""

import hashlib
import importlib.util
import os
import pathlib
import struct
import sys
import time

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p3/laneg/tmp-g3/capture")
os.environ.setdefault("P2A_PORT_BASE", "35720")
os.environ.setdefault("P2A_DASHD", str(pathlib.Path.home() / ".rust-dashcore-test/dashcore-24.0.0/bin/dashd"))
spec = importlib.util.spec_from_file_location(
    "p4b", pathlib.Path(__file__).resolve().parent.parent / "p4b-transcripts/capture.py")
p4b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p4b)
p2a = p4b.p2a
DASHD_SHA256 = "2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223"
# P4b checks this exact binary hash and records it in every fixture.
p4b.p3.DASHD_SHA256 = DASHD_SHA256


def scenario(out, name, index, notes, mine=0, extra=()):
    return p4b.scenario(out, name, index, notes, mine,
                        extra=("-acceptnonstdtxn=1",) + extra)


def spend(parent, value, tag, script=b"\x61" * 4 + b"\x51", input_script=b""):
    return (struct.pack("<I", 1) + b"\x01" + parent + struct.pack("<I", 0)
            + p2a.compact(len(input_script)) + input_script + b"\xff" * 4 + b"\x01"
            + struct.pack("<q", value) + p2a.compact(len(script)) + script + struct.pack("<I", tag))


def txid(raw):
    return hashlib.sha256(hashlib.sha256(raw).digest()).digest()


def coin(t, c, height):
    block = t.node.rpc("getblock", c.hashes[height][::-1].hex(), 2)
    cb = block["tx"][0]
    output = cb["vout"][0]
    amount = round(output["value"] * 100_000_000)
    wire = bytes.fromhex(cb["txid"])[::-1]
    t.lines.append(f"utxo {wire.hex()} 0 {amount} {output['scriptPubKey']['hex']}")
    return wire, amount


def admission(t, raw, outcome, reason="", debug=""):
    verdict = t.node.rpc("testmempoolaccept", [raw.hex()], 0)[0]
    assert verdict["allowed"] == (outcome == "valid"), (t.name, outcome, verdict)
    if not verdict["allowed"]:
        reason = verdict.get("reject-reason", reason)
    t.lines.append(f"admission {raw.hex()} {outcome} {reason.encode().hex() or '-'} {debug.encode().hex() or '-'}")


def accepted(t, raw):
    hashes = t.node.rpc("getrawmempool")
    assert txid(raw)[::-1].hex() in hashes, (t.name, hashes)
    t.lines.append(f"mempool-has {txid(raw).hex()}")


def peer(t, n, port=None, perm=None):
    if port is None:
        t.accept(n)
    else:
        import socket
        sock = socket.create_connection(("127.0.0.1", port), timeout=30)
        t.lines.append(f"accept {n} from 127.0.0.1 perm {perm}")
        t.peers[n] = p2a.Peer(n, sock)
        t.settle()
    t.handshake(n, p4b.FULL | p4b.BIT11)


def tx_accept_relay(out):
    with scenario(out, "tx-accept-relay", 0,
                      ["A valid transaction is relayed to two peers; a duplicate is ignored."], 110) as (t, c):
        for n in (1, 2, 3):
            peer(t, n)
        parent, value = coin(t, c, 1)
        raw = spend(parent, value - 10000, 0)
        admission(t, raw, "valid")
        t.send(1, "tx", raw)
        accepted(t, raw)
        t.set_time(p2a.SIM_START + 60)
        t.send(1, "tx", raw)
        relayed = {int(line.split()[1]) for line in t.lines if line.startswith("< ")
                   and " inv " in line and txid(raw).hex() in line}
        assert {2, 3} <= relayed, relayed


def tx_orphans(out):
    with scenario(out, "tx-orphans", 1,
                      ["Child before parent, sender-owned reconsideration, disconnect, and zero-byte eviction."], 110,
                      extra=("-maxorphantxsize=0",)) as (t, c):
        # With no orphan storage the child still requests its missing parent, then is evicted.
        peer(t, 1)
        parent_hash, value = coin(t, c, 1)
        parent = spend(parent_hash, value - 10000, 0)
        child = spend(txid(parent), value - 20000, 0)
        admission(t, child, "missing", "bad-txns-inputs-missingorspent")
        t.send(1, "tx", child)
        t.lines.append("orphans 0")
        t.set_time(p2a.SIM_START + 3)
        admission(t, parent, "valid")
        t.send(1, "tx", parent)
        accepted(t, parent)
        # The evicted child must be sent again to be admitted.
        admission(t, child, "valid")
        t.send(1, "tx", child)
        accepted(t, child)
    with scenario(out, "tx-orphans-retry", 4,
                      ["The retained child is accepted after the parent; peer disconnect erases its other orphan."], 110) as (t, c):
        peer(t, 1)
        peer(t, 2)
        parent_hash, value = coin(t, c, 1)
        parent = spend(parent_hash, value - 10000, 0)
        child = spend(txid(parent), value - 20000, 0)
        admission(t, child, "missing", "bad-txns-inputs-missingorspent")
        t.send(1, "tx", child)
        t.lines.append("orphans 1")
        t.set_time(p2a.SIM_START + 3)
        admission(t, parent, "valid")
        t.send(2, "tx", parent)
        accepted(t, parent)
        accepted(t, child)
        t.lines.append("orphans 0")
        orphan = spend(bytes([19]) * 32, 10000, 0)
        admission(t, orphan, "missing", "bad-txns-inputs-missingorspent")
        t.send(1, "tx", orphan)
        t.lines.append("orphans 1")
        t.lines.append("close 1")
        t.peers[1].sock.close()
        t.peers[1].closed = True
        end = time.monotonic() + 30
        while any(p["id"] == 0 for p in t.node.rpc("getpeerinfo")):
            if time.monotonic() > end:
                raise RuntimeError("peer was not finalized")
            time.sleep(0.1)
        t.lines.append("orphans 0")
        t.settle()


def tx_reject(out):
    force_port = 35759
    with scenario(out, "tx-reject", 2,
                      ["Script rejection scores 100, policy rejection scores zero, recent-reject inv is ignored, forcerelay only relays mempool entries."],
                      110, extra=(f"-whitebind=forcerelay@127.0.0.1:{force_port}", "-debug=mempoolrej")) as (t, c):
        peer(t, 1)
        parent, value = coin(t, c, 1)
        invalid = spend(parent, value - 10000, 0, input_script=b"\x6a")
        verdict = t.node.rpc("testmempoolaccept", [invalid.hex()], 0)[0]
        assert not verdict["allowed"], verdict
        reason = verdict.get("reject-reason", "mandatory-script-verify-flag-failed")
        admission(t, invalid, "consensus", reason)
        t.send(1, "tx", invalid)
        assert t.peers[1].closed, "invalid script did not disconnect local peer"
        peer(t, 2)
        nonstandard = spend(parent, value - 10000, 0, script=b"\x51")
        admission(t, nonstandard, "nonstandard")
        t.send(2, "tx", nonstandard)
        assert not t.peers[2].closed
        # A zero-fee transaction fails min-relay-fee policy even with nonstandard scripts allowed.
        policy = spend(parent, value, 0)
        verdict = t.node.rpc("testmempoolaccept", [policy.hex()], 0)[0]
        assert not verdict["allowed"], verdict
        admission(t, policy, "policy", verdict.get("reject-reason", "min relay fee not met"))
        t.send(2, "tx", policy)
        assert not t.peers[2].closed
        t.send(2, "inv", p4b.getdata([(1, txid(policy))]))
        t.set_time(p2a.SIM_START + 3)
        peer(t, 3, force_port, "forcerelay,relay")
        t.send(3, "tx", policy)
        valid = spend(parent, value - 10000, 0)
        admission(t, valid, "valid")
        t.send(3, "tx", valid)
        accepted(t, valid)
        t.set_time(p2a.SIM_START + 60)
        t.send(3, "tx", valid)


def tx_blocks_only(out):
    relay_port = 35760
    with scenario(out, "tx-blocksonly-relay-perm", 3,
                      ["Blocks-only disconnects a plain sender and admits a relay-permission sender."], 110,
                      extra=("-blocksonly=1", f"-whitebind=relay@127.0.0.1:{relay_port}")) as (t, c):
        peer(t, 1)
        peer(t, 2, relay_port, "relay")
        parent, value = coin(t, c, 1)
        raw = spend(parent, value - 10000, 0)
        t.send(1, "tx", raw)
        assert t.peers[1].closed
        admission(t, raw, "valid")
        t.send(2, "tx", raw)
        accepted(t, raw)


SCENARIOS = {"tx-accept-relay": tx_accept_relay, "tx-orphans": tx_orphans,
             "tx-reject": tx_reject, "tx-blocksonly-relay-perm": tx_blocks_only}


def main():
    if hashlib.sha256(pathlib.Path(p2a.DASHD).read_bytes()).hexdigest() != DASHD_SHA256:
        sys.exit("wrong dashd binary; require SHA-pinned v24.0.0 final")
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for name in sys.argv[2:] or SCENARIOS:
        SCENARIOS[name](out)


if __name__ == "__main__":
    main()
