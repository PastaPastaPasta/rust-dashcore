#!/usr/bin/env python3
"""LQ3a regtest capture: Dash Core's own qsigrec / clsig / isdlock P2P payloads and inv hashes.

Not a test of Core: a Dash functional test (DashTestFramework) used as a capture harness. It is copied
into the exported `test/functional/` directory of a Dash Core tag and run there by run.sh.

Raw observers (P2PInterface peers that sent QSENDRECSIGS) are attached to every node. They record the
`inv` hashes of recovered sigs (28), ChainLocks (29) and InstantSend locks (31), then fetch each payload
with a single-entry GETDATA, one at a time, and store the exact wire bytes. Nothing here hashes a
payload: the pairing inv hash <-> payload comes from the GETDATA round trip alone.
"""
import hashlib
import json
import os
import time
from datetime import datetime, timezone
from io import BytesIO

from test_framework import p2p
from test_framework.messages import CInv, msg_getdata, msg_qsendrecsigs
from test_framework.p2p import P2PInterface
from test_framework.test_framework import DashTestFramework
from test_framework.util import assert_equal

MSG_QUORUM_RECOVERED_SIG = 28
MSG_CLSIG = 29
MSG_ISDLOCK = 31
INV_COMMAND = {MSG_QUORUM_RECOVERED_SIG: b"qsigrec", MSG_CLSIG: b"clsig", MSG_ISDLOCK: b"isdlock"}

LLMQ_SIZE = 4
LLMQ_THRESHOLD = 3
# llmq_test's and llmq_test_instantsend's keepOldConnections (llmq/params.h): IsQuorumActive's window.
KEEP_OLD_CONNECTIONS = 3
TYPES = {"llmq_test": 100, "llmq_test_instantsend": 104}


def raw_message_class(command):
    """A MESSAGEMAP entry that keeps every payload byte exactly as received."""
    class RawMessage:
        msgtype = command

        def __init__(self):
            self.payload = b""

        def deserialize(self, f):
            self.payload = f.read()

        def serialize(self):
            return self.payload

        def __repr__(self):
            return "RawMessage(%s, %d bytes)" % (command.decode(), len(self.payload))
    return RawMessage


for _command in INV_COMMAND.values():
    p2p.MESSAGEMAP[_command] = raw_message_class(_command)


class RawObserver(P2PInterface):
    """Records recsig / ChainLock / ISDLOCK invs and fetches each object with its own GETDATA.

    Invs are queued while `armed` is False; the harness arms an observer only once the network is
    quiet, so a proactively pushed QSIGREC (which has no inv) cannot be mistaken for the answer to a
    GETDATA. Whatever arrives without an outstanding request is kept in `unsolicited`.
    """

    def __init__(self, name):
        super().__init__()
        self.name = name
        self.armed = False
        self.queue = []            # CInv not yet requested
        self.seen = set()          # (type, hash) already queued
        self.outstanding = None    # CInv with a GETDATA in flight
        self.captured = []         # {"command", "inv_hash", "payload"} in fetch order
        self.unsolicited = []      # (command, payload hex) received without a request
        self.notfound = []         # (type, hash) answered with NOTFOUND
        self.invs_seen = 0

    def start(self):
        self.send_message(msg_qsendrecsigs(True))

    def arm(self, on):
        with p2p.p2p_lock:
            self.armed = on
            self._pump()

    def idle(self):
        with p2p.p2p_lock:
            return not self.queue and self.outstanding is None

    def _pump(self):
        if not self.armed or self.outstanding is not None or not self.queue:
            return
        self.outstanding = self.queue.pop(0)
        self.send_message(msg_getdata([CInv(self.outstanding.type, self.outstanding.hash)]))

    def on_inv(self, message):
        # Deliberately no auto-getdata of anything else (blocks, txs, sporks...).
        for inv in message.inv:
            if inv.type in INV_COMMAND and (inv.type, inv.hash) not in self.seen:
                self.seen.add((inv.type, inv.hash))
                self.queue.append(CInv(inv.type, inv.hash))
                self.invs_seen += 1
        self._pump()

    def on_notfound(self, message):
        for inv in message.vec:
            if self.outstanding is not None and (inv.type, inv.hash) == (self.outstanding.type, self.outstanding.hash):
                self.notfound.append((inv.type, inv.hash))
                self.outstanding = None
        self._pump()

    def _payload(self, message):
        cmd = message.msgtype
        o = self.outstanding
        if o is not None and INV_COMMAND[o.type] == cmd:
            self.captured.append({"command": cmd.decode(), "inv_hash": "%064x" % o.hash,
                                  "payload": message.payload.hex()})
            self.outstanding = None
        else:
            self.unsolicited.append((cmd.decode(), message.payload.hex()))
        self._pump()

    def on_qsigrec(self, message): self._payload(message)
    def on_clsig(self, message): self._payload(message)
    def on_isdlock(self, message): self._payload(message)


def display(wire):
    """Wire (little-endian) uint256 bytes -> display hex."""
    return bytes(reversed(wire)).hex()


class LQ3aCapture(DashTestFramework):
    TYPES = TYPES

    def add_options(self, parser):
        self.add_wallet_options(parser)
        parser.add_argument("--out", dest="out", required=True, help="fixture JSON path")

    def set_test_params(self):
        self.set_dash_test_params(5, 4, [["-llmqtestinstantsenddip0024=llmq_test_instantsend"]] * 5)
        self.set_dash_llmq_test_params(LLMQ_SIZE, LLMQ_THRESHOLD)
        # node0 is a non-masternode that watches the quorums. Core then asks the masternodes it is
        # connected to for recovered sigs (QSENDRECSIGS). Because node0 never recovers a sig itself,
        # it learns each one from a peer and announces it by `inv`; a masternode that recovered a sig
        # on its own pushes an unsolicited QSIGREC to its peers instead, with no inv at all.
        self.extra_args[0].append("-watchquorums=1")
        self.supports_cli = False

    # -- observers -------------------------------------------------------------------------------

    def attach_observers(self):
        self.observers = []
        targets = [("node0", self.nodes[0])] + [("mn%d" % i, mn.get_node(self)) for i, mn in enumerate(self.mninfo)]
        for name, node in targets:
            obs = node.add_p2p_connection(RawObserver(name))
            obs.start()
            self.observers.append(obs)

    def mark(self):
        return self.observers[0].invs_seen

    def drain(self, what, base, quiet_rounds=8, timeout=240):
        """Wait until the network is quiet, then fetch every queued inv one GETDATA at a time."""
        start = time.time()
        last, calm = None, 0
        while True:
            self.bump_mocktime(2)
            time.sleep(1)
            total = sum(o.invs_seen for o in self.observers)
            calm = calm + 1 if total == last else 0
            last = total
            if calm >= quiet_rounds and self.observers[0].invs_seen > base:
                break
            assert time.time() - start < timeout, "network never went quiet after " + what
        for o in self.observers:
            o.arm(True)
        deadline = time.time() + timeout
        while not all(o.idle() for o in self.observers):
            self.bump_mocktime(1)
            time.sleep(0.5)
            assert time.time() < deadline, "getdata queue did not drain after " + what
        for o in self.observers:
            o.arm(False)
        self.log.info("%s: captured per observer %s" % (what, {o.name: len(o.captured) for o in self.observers}))

    # -- quorum context --------------------------------------------------------------------------

    def quorum_context(self):
        node = self.nodes[0]
        listing = node.quorum("list", 10)
        infos = {}
        for tname, ltype in self.TYPES.items():
            infos[str(ltype)] = []
            for qhash in listing.get(tname, []):
                info = node.quorum("info", ltype, qhash)
                assert_equal(info["quorumHash"], qhash)
                infos[str(ltype)].append({
                    "hash": qhash,
                    "public_key": info["quorumPublicKey"],
                    "height": info["height"],
                    "mined_block": info.get("minedBlock"),
                    "index": info.get("quorumIndex", 0),
                })
        return {"height": node.getblockcount(), "list": listing, "quorums": infos}

    # -- the run ---------------------------------------------------------------------------------

    def run_test(self):
        node0 = self.nodes[0]
        node0.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        self.wait_for_sporks_same()
        sporks = node0.spork("show")
        self.log.info("sporks: %s" % sporks)

        # Three DKG cycles: each type then holds keepOldConnections (3) quorums, one more than
        # signingActiveQuorumCount (2), so the oldest is active but never selected for signing.
        self.log.info("Mine three cycles of llmq_test (100) and llmq_test_instantsend (104) quorums")
        self.mine_quorum()
        self.mine_quorum(llmq_type_name="llmq_test_instantsend", llmq_type=104)
        self.mine_quorum()
        listing = node0.quorum("list", 10)
        for tname in TYPES:
            assert_equal(len(listing[tname]), KEEP_OLD_CONNECTIONS)

        self.attach_observers()
        # let QSENDRECSIGS land and the connections settle
        time.sleep(2)
        self.bump_mocktime(5)

        before = self.quorum_context()
        self.log.info("Context before generation: height=%d list=%s" % (before["height"], before["list"]))

        # The third-newest llmq_test quorum signs one recovered sig, named explicitly: Core relays it
        # because IsQuorumActive scans keepOldConnections quorums, not signingActiveQuorumCount.
        oldest = before["list"]["llmq_test"][KEEP_OLD_CONNECTIONS - 1]
        rid, mhash = "%064x" % 0x1100, "%064x" % 0x2100
        self.log.info("Recovered signature by the third-newest llmq_test quorum %s" % oldest)
        base = self.mark()
        for mn in self.get_quorum_masternodes(oldest, llmq_type=100):
            mn.get_node(self).quorum("sign", 100, rid, mhash, oldest)
        self.wait_for_recovered_sig(rid, mhash, 100, 10)
        assert_equal(self.mninfo[0].get_node(self).quorum("getrecsig", 100, rid, mhash)["quorumHash"], oldest)
        self.drain("recsig by the third-newest quorum", base)
        self.oldest_signed = {"llmq_type": 100, "quorum_hash": oldest, "id": rid, "msg_hash": mhash}

        self.log.info("Recovered signatures on llmq_test")
        for n in range(1, 7):
            rid = "%064x" % (0x1000 + n)
            mhash = "%064x" % (0x2000 + n)
            base = self.mark()
            self.get_recovered_sig(rid, mhash, llmq_type=100)
            self.drain("recsig %d" % n, base)

        self.log.info("InstantSend locks")
        for n in range(1, 4):
            addr = node0.getnewaddress()
            base = self.mark()
            txid = node0.sendtoaddress(addr, 1)
            self.wait_for_instantlock(txid)
            self.drain("islock %d" % n, base)

        self.log.info("ChainLocks")
        for n in range(1, 4):
            base = self.mark()
            self.bump_mocktime(1)
            blk = self.generate(node0, 1, sync_fun=self.sync_blocks)[0]
            self.wait_for_chainlocked_block_all_nodes(blk, timeout=60)
            assert_equal(node0.getbestchainlock()["blockhash"], blk)
            self.drain("chainlock %d" % n, base)

        after = self.quorum_context()
        self.log.info("Context after generation: height=%d list=%s" % (after["height"], after["list"]))
        assert_equal(before["list"], after["list"])
        assert_equal(before["quorums"], after["quorums"])

        self.write_fixture(before, after)

    # -- fixture ---------------------------------------------------------------------------------

    def merge(self):
        """Merge observers' captures by payload; every payload must map to a single inv hash."""
        by_payload = {}
        by_inv = {}
        for o in self.observers:
            for c in o.captured:
                key = (c["command"], c["payload"])
                prev = by_payload.setdefault(key, c["inv_hash"])
                assert prev == c["inv_hash"], "payload seen under two inv hashes: %s" % (key,)
                invkey = (c["command"], c["inv_hash"])
                prevp = by_inv.setdefault(invkey, c["payload"])
                assert prevp == c["payload"], "inv hash answered with two payloads: %s" % (invkey,)
        msgs = []
        order = {"qsigrec": 0, "isdlock": 1, "clsig": 2}
        for (cmd, payload), inv_hash in by_payload.items():
            msgs.append({"command": cmd, "inv_hash": inv_hash, "payload": payload})
        msgs.sort(key=lambda m: (order[m["command"]], m["inv_hash"]))
        return msgs

    def core_verdict(self, m):
        raw = bytes.fromhex(m["payload"])
        node = self.nodes[0]
        if m["command"] == "qsigrec":
            assert_equal(len(raw), 1 + 32 * 3 + 96)
            ltype = raw[0]
            qhash, rid, mhash = (display(raw[1 + 32 * i: 33 + 32 * i]) for i in range(3))
            sig = raw[97:].hex()
            m["fields"] = {"llmq_type": ltype, "quorum_hash": qhash, "id": rid, "msg_hash": mhash, "sig": sig}
            verdict = node.quorum("verify", ltype, rid, mhash, sig, qhash)
            assert verdict is True, ("quorum verify said %r for %s" % (verdict, m))
            return True
        if m["command"] == "clsig":
            assert_equal(len(raw), 4 + 32 + 96)
            height = int.from_bytes(raw[:4], "little", signed=True)
            bhash, sig = display(raw[4:36]), raw[36:].hex()
            m["fields"] = {"height": height, "block_hash": bhash, "sig": sig}
            verdict = node.verifychainlock(bhash, sig, height)
            assert verdict is True, ("verifychainlock said %r for %s" % (verdict, m))
            return True
        return None

    def write_fixture(self, before, after):
        msgs = self.merge()
        for m in msgs:
            m["core_verify"] = self.core_verdict(m)
        counts = {c: sum(1 for m in msgs if m["command"] == c) for c in ("qsigrec", "isdlock", "clsig")}
        self.log.info("merged message counts: %s" % counts)
        assert counts["qsigrec"] >= 6 and counts["isdlock"] >= 2 and counts["clsig"] >= 2, counts
        o = self.oldest_signed
        assert any(m["command"] == "qsigrec" and m["fields"]["llmq_type"] == o["llmq_type"]
                   and m["fields"]["quorum_hash"] == o["quorum_hash"] and m["fields"]["id"] == o["id"]
                   for m in msgs), "the third-newest quorum's recovered sig was not captured"
        with open(os.environ["DASHD"], "rb") as f:
            dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
        v19 = self.nodes[0].getdeploymentinfo()["deployments"]["v19"]
        quorum_count = {t: len(v) for t, v in before["quorums"].items()}
        self.log.info("quorums: %s; unsolicited payloads per observer: %s; notfound: %s" % (
            quorum_count,
            {o.name: len(o.unsolicited) for o in self.observers},
            {o.name: len(o.notfound) for o in self.observers}))
        fixture = {
            "provenance": {
                "dashd_version": os.environ.get("LQ3A_DASHD_VERSION", ""),
                "dashd_sha256": dashd_sha256,
                "v19_deployment": v19,
                "keep_old_connections_recsig": o,
                "core_tag": os.environ.get("LQ3A_CORE_TAG", ""),
                "tool": "contrib/node/lq3a-captures",
                "extra_args": [list(a) for a in self.extra_args],
                "captured_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "observers": [o.name for o in self.observers],
                "llmq_size": LLMQ_SIZE,
                "llmq_threshold": LLMQ_THRESHOLD,
                "quorum_height_before": before["height"],
                "quorum_height_after": after["height"],
                "unsolicited_payloads_not_in_fixture": {o.name: len(o.unsolicited) for o in self.observers},
            },
            "network": "regtest",
            "tip_height": after["height"],
            "bls_scheme": "basic",
            "quorum_list": before["list"],
            "quorums": before["quorums"],
            "messages": msgs,
        }
        with open(self.options.out, "w") as f:
            json.dump(fixture, f, indent=2, sort_keys=True)
            f.write("\n")
        self.log.info("fixture written to %s" % self.options.out)


if __name__ == "__main__":
    LQ3aCapture().main()
