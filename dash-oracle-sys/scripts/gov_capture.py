#!/usr/bin/env python3
"""O2a-GOV capture: a regtest governance state built by Dash Core's own functional-test framework
(DashTestFramework, tag v24.0.0-rc.3) on a release dashd, and Core's verdicts on governance votes
delivered over P2P (node-program O2a-GOV, tests/data/gov-regtest-v24.0.json.gz).

Not part of Core. Run from <core>/test/functional of the tag (see PROVENANCE.md, "Governance
regtest network"):

    python3 gov_capture.py --configfile=<core>/test/config.ini --tmpdir=<dir> --nocleanup \\
        --outdir=<out> --timeout-factor=8 --portseed=733

Node 0 is the controller (it holds the voting keys and sees every object and vote) and runs with
-debug=gobject. The script builds four proposals, masternode votes on them with every signal, and
a superblock trigger the masternodes create and vote on themselves, then advances to the block
before the superblock. There it reads, over RPC from node 0: the headers, the masternode list
(protx listdiff 1 <tip>), the proposals' collateral transactions, `gobject get` and
`getcurrentvotes` of every object, `getblocktemplate`, `getdeploymentinfo` and the mock time.

Then it sends crafted votes to node 0 over P2P, one fresh inbound peer each (inv, node 0's
getdata, then the vote), and keeps the lines node 0's debug.log printed for each one: Core's own
verdict (`MNGOVERNANCEOBJECTVOTE -- <hash> new`, or `Rejected vote, error = <message>`). Finally
it mines the superblock, keeps the raw block, stops node 0 and copies its governance.dat, which
holds every object and signed vote byte for byte.

<out>/capture.json holds all of it; <out>/governance.dat is node 0's file.
"""
import base64
import hashlib
import json
import os
import random
import shutil
import struct
import time

from test_framework.governance import prepare_object
from test_framework.messages import CInv, MSG_GOVERNANCE_OBJECT_VOTE, hash256, msg_inv, uint256_to_string
from test_framework.p2p import P2PInterface
from test_framework.test_framework import DashTestFramework
from test_framework.util import assert_equal

SIGNALS = {"funding": 1, "valid": 2, "delete": 3, "endorsed": 4}
OUTCOMES = {"yes": 1, "no": 2, "abstain": 3}


class msg_govobjvote:
    """A raw MNGOVERNANCEOBJECTVOTE payload."""
    __slots__ = ("raw",)
    msgtype = b"govobjvote"

    def __init__(self, raw):
        self.raw = raw

    def serialize(self):
        return self.raw


class VotePeer(P2PInterface):
    def __init__(self):
        super().__init__()
        self.requested = set()

    def on_getdata(self, message):
        for inv in message.inv:
            self.requested.add(inv.hash)


def outpoint_bytes(txid_hex, n):
    return bytes.fromhex(txid_hex)[::-1] + struct.pack("<I", n)


def vote_bytes(outpoint, parent_hex, signal, outcome, t, sig):
    """CGovernanceVote in network serialization (vote.h SERIALIZE_METHODS)."""
    return (outpoint + bytes.fromhex(parent_hex)[::-1] + struct.pack("<i", outcome) + struct.pack("<i", signal)
            + struct.pack("<q", t) + bytes([len(sig)]) + sig)


def vote_hash(outpoint, parent_hex, signal, outcome, t):
    """CGovernanceVote::UpdateHash() (vote.cpp:145-156), as a hex string."""
    pre = (outpoint + b"\x00" + struct.pack("<I", 0xFFFFFFFF) + bytes.fromhex(parent_hex)[::-1]
           + struct.pack("<i", signal) + struct.pack("<i", outcome) + struct.pack("<q", t))
    return hash256(pre)[::-1].hex()


class GovCapture(DashTestFramework):
    def add_options(self, parser):
        parser.add_argument("--outdir", dest="outdir", required=True)
        self.add_wallet_options(parser)

    def set_test_params(self):
        # node 0 = controller, nodes 1..4 regular masternodes, two evonodes added in run_test
        self.set_dash_test_params(5, 4, [["-budgetparams=10:10:10", "-debug=gobject"]] * 5, evo_count=2)

    # --- signing -------------------------------------------------------------------------
    def operator_key(self, mn):
        from py_ecc.bls import G2Basic
        raw = bytes.fromhex(mn.keyOperator)
        for sk_bytes in (raw, raw[::-1]):
            sk = int.from_bytes(sk_bytes, "big")
            if G2Basic.SkToPk(sk).hex() == mn.pubKeyOperator:
                return sk
        raise AssertionError("cannot derive the operator public key from keyOperator")

    def operator_sign(self, mn, outpoint, parent_hex, signal, outcome, t):
        """The BLS signature CheckSignature(pubKeyOperator) verifies: over the signature hash, the
        vote's serialization without vchSig."""
        from py_ecc.bls import G2Basic
        msg = (outpoint + bytes.fromhex(parent_hex)[::-1] + struct.pack("<i", outcome) + struct.pack("<i", signal)
               + struct.pack("<q", t))
        return G2Basic.Sign(self.operator_key(mn), hash256(msg))

    def collateral(self, mn):
        info = self.nodes[0].protx("info", mn.proTxHash)
        return info["collateralHash"], info["collateralIndex"]

    def vote(self, h, signal, outcome, mn):
        node = self.nodes[0]
        if signal == "funding":
            res = node.gobject("vote-alias", h, signal, outcome, mn.proTxHash)
            assert res["overall"].startswith("Voted successfully 1"), res
        else:
            txid, n = self.collateral(mn)
            sig = self.operator_sign(mn, outpoint_bytes(txid, n), h, SIGNALS[signal], OUTCOMES[outcome],
                                     int(self.mocktime))
            res = node.voteraw(txid, n, h, signal, outcome, int(self.mocktime), base64.b64encode(sig).decode())
            assert_equal(res, "Voted successfully")

    # --- P2P probes ----------------------------------------------------------------------
    def probe(self, name, raw, hash_hex):
        """Deliver raw to node 0 from a fresh inbound peer after node 0 asked for it; return what
        node 0 logged for it."""
        node = self.nodes[0]
        log = node.debug_log_path
        start = os.path.getsize(log)
        peer = node.add_p2p_connection(VotePeer())
        peer.send_message(msg_inv([CInv(MSG_GOVERNANCE_OBJECT_VOTE, int(hash_hex, 16))]))
        asked = False
        for _ in range(60):
            time.sleep(0.5)
            if int(hash_hex, 16) in peer.requested:
                asked = True
                break
            self.bump_mocktime(1, nodes=[node])
        t = int(self.mocktime)
        verdict = None
        if asked:
            peer.send_message(msg_govobjvote(raw))
            for _ in range(120):
                with open(log, "rb") as f:
                    f.seek(start)
                    text = f.read().decode(errors="replace")
                lines = [line.split(" ", 1)[1] if " " in line else line for line in text.splitlines()]
                for line in lines:
                    if f"MNGOVERNANCEOBJECTVOTE -- {hash_hex} new" in line:
                        verdict = {"accepted": True, "error": None}
                    elif "MNGOVERNANCEOBJECTVOTE -- Rejected vote, error = " in line:
                        verdict = {"accepted": False,
                                   "error": line.split("MNGOVERNANCEOBJECTVOTE -- Rejected vote, error = ", 1)[1]}
                    elif "malformed governance vote" in line:
                        verdict = {"accepted": False, "error": None, "malformed": True}
                if verdict is not None:
                    break
                time.sleep(0.25)
        with open(log, "rb") as f:
            f.seek(start)
            text = f.read().decode(errors="replace")
        gov_lines = [line.split(" ", 1)[1] for line in text.splitlines()
                     if ("GOVERNANCE" in line or "Governance" in line or "ProcessVote" in line
                         or "Misbehaving" in line or "malformed" in line) and " " in line]
        connected = peer.is_connected
        node.disconnect_p2ps()
        self.log.info("probe %s: asked %s, verdict %s", name, asked, verdict)
        return {"name": name, "vote": raw.hex(), "hash": hash_hex, "time": t, "requested": asked,
                "verdict": verdict, "connected_after": connected, "log": gov_lines}

    def run_test(self):
        node = self.nodes[0]
        rnd = random.Random(733)
        self.log.info("Start: height %d, %d masternodes", node.getblockcount(), len(self.mninfo))
        # As feature_governance.py: with InstantSend on, miners leave the collaterals unlocked
        # transactions out of their blocks (IsTxSafeForMining).
        node.sporkupdate("SPORK_2_INSTANTSEND_ENABLED", 4070908800)
        self.wait_for_sporks_same()

        # --- 1. two evonodes on top of the four regular masternodes ------------------------
        for _ in range(2):
            self.dynamically_add_masternode(evo=True)
            self.bump_mocktime(1)
            self.generate(node, 8, sync_fun=lambda: self.sync_blocks())
        self.wait_until(lambda: all(s == "ENABLED" for s in node.masternodelist("status").values())
                        and len(node.masternodelist("status")) == 6, timeout=60)
        mns = self.mninfo

        # --- 2. proposals: P3 gets no votes before the probes ------------------------------
        proposal_time = self.mocktime
        specs = [("Proposal_0", "1.1"), ("Proposal_1", "3.3"), ("Proposal_2", "2.2"), ("Proposal_3", "4.4")]
        prepared = [prepare_object(node, 1, uint256_to_string(0), proposal_time, 1, name, amount, node.getnewaddress())
                    for name, amount in specs]
        self.bump_mocktime(6)
        self.generate(node, 6, sync_fun=lambda: self.sync_blocks())
        hashes = [node.gobject("submit", "0", 1, proposal_time, p["hex"], p["collateralHash"]) for p in prepared]
        p0, p1, p2, p3 = hashes
        self.wait_until(lambda: len(self.nodes[1].gobject("list")) == 4, timeout=10)

        # --- 3. votes ------------------------------------------------------------------------
        y, n, a = "yes", "no", "abstain"
        for mn, o in zip(mns, [y, y, y, y, n, a]):
            self.vote(p0, "funding", o, mn)
        for mn in mns[:3]:
            self.vote(p0, "valid", y, mn)
        self.vote(p0, "endorsed", y, mns[0])
        self.vote(p0, "delete", n, mns[1])
        for mn, o in zip(mns, [n, n, y, a, y, n]):
            self.vote(p1, "funding", o, mn)
        for mn in mns:
            self.vote(p2, "funding", y, mn)
        self.wait_until(lambda: self.nodes[1].gobject("count")["votes"] == node.gobject("count")["votes"], timeout=20)

        # --- 4. a trigger the masternodes create and vote on --------------------------------
        trigger_found = False
        for _ in range(80):
            gi = node.getgovernanceinfo()
            if node.getblockcount() + 1 >= gi["nextsuperblock"] - gi["superblockmaturitywindow"]:
                trigger_found = self.wait_until(lambda: len(node.gobject("list", "valid", "triggers")) >= 1,
                                                timeout=3, do_assert=False)
                if trigger_found:
                    break
            self.bump_mocktime(1)
            self.generate(node, 1, sync_fun=lambda: self.sync_blocks())
        assert trigger_found, "no trigger created"
        self.wait_until(lambda: max(t["YesCount"] for t in node.gobject("list", "valid", "triggers").values())
                        >= len(mns) - 1, timeout=60, do_assert=False)

        # --- 5. the block before the superblock ---------------------------------------------
        gi = node.getgovernanceinfo()
        while node.getblockcount() + 1 < gi["nextsuperblock"]:
            self.bump_mocktime(1)
            self.generate(node, 1, sync_fun=lambda: self.sync_blocks())
        tip = node.getblockcount()
        assert_equal(tip + 1, gi["nextsuperblock"])
        self.wait_until(lambda: node.mnsync("status")["IsSynced"], timeout=60)

        out = {"tip": tip, "mocktime_at_tip": int(self.mocktime)}
        out["headers"] = [node.getblockheader(node.getblockhash(h), False) for h in range(tip + 1)]
        out["block_hashes"] = [node.getblockhash(h) for h in range(tip + 1)]
        out["listdiff"] = node.protx("listdiff", 1, tip)
        out["dip3_height"] = next(h for h in range(tip + 1)
                                  if node.getblock(node.getblockhash(h), 2)["tx"][0].get("type") == 5)
        out["collaterals"] = {}
        for p in prepared:
            tx = node.getrawtransaction(p["collateralHash"], True)
            out["collaterals"][p["collateralHash"]] = {"hex": tx["hex"], "blockhash": tx["blockhash"],
                                                      "height": tx["height"]}
        objs = node.gobject("list", "all")
        out["gobject_list_all"] = objs
        out["gobject_get"] = {h: node.gobject("get", h) for h in objs}
        out["gobject_getcurrentvotes"] = {h: node.gobject("getcurrentvotes", h) for h in objs}
        out["getgovernanceinfo"] = node.getgovernanceinfo()
        out["getdeploymentinfo"] = node.getdeploymentinfo()
        out["getblocktemplate"] = node.getblocktemplate()
        out["mnsync_status"] = node.mnsync("status")
        try:
            out["getbestchainlock"] = node.getbestchainlock()
        except Exception as e:  # no ChainLock yet
            out["getbestchainlock"] = {"error": str(e)}
        out["proposals"] = {"Proposal_0": p0, "Proposal_1": p1, "Proposal_2": p2, "Proposal_3": p3}
        out["masternodes"] = [{"proTxHash": mn.proTxHash, "evo": mn.evo, "collateral": list(self.collateral(mn)),
                               "pubKeyOperator": mn.pubKeyOperator} for mn in mns]

        # --- 6. probes ----------------------------------------------------------------------
        def op(mn):
            txid, n = self.collateral(mn)
            return outpoint_bytes(txid, n), txid, n

        def signed(mn, parent, signal, outcome, t):
            outpoint, _, _ = op(mn)
            sig = self.operator_sign(mn, outpoint, parent, signal, outcome, t)
            return vote_bytes(outpoint, parent, signal, outcome, t, sig), vote_hash(outpoint, parent, signal, outcome, t)

        def unknown(parent, signal, outcome, t):
            outpoint = rnd.randbytes(32) + struct.pack("<I", 0)
            sig = rnd.randbytes(96)
            return vote_bytes(outpoint, parent, signal, outcome, t, sig), vote_hash(outpoint, parent, signal, outcome, t)

        probes = []
        t = lambda: int(self.mocktime)  # noqa: E731
        random_parent = rnd.randbytes(32).hex()
        probes.append(self.probe("unknown masternode, known parent", *unknown(p0, 1, 1, t())))
        probes.append(self.probe("unknown masternode, unknown parent", *unknown(random_parent, 1, 1, t())))
        probes.append(self.probe("known masternode, unknown parent", *signed(mns[0], random_parent, 2, 1, t())))
        probes.append(self.probe("operator-signed funding, unknown parent",
                                 *signed(mns[1], rnd.randbytes(32).hex(), 1, 1, t())))
        probes.append(self.probe("operator-signed funding on a proposal", *signed(mns[0], p3, 1, 1, t())))
        probes.append(self.probe("endorsed on a proposal", *signed(mns[0], p3, 4, 1, t())))
        probes.append(self.probe("the same signal again", *signed(mns[0], p3, 4, 2, t() + 1)))
        probes.append(self.probe("an obsolete vote", *signed(mns[0], p3, 4, 2, t() - 600)))
        probes.append(self.probe("a vote from the future", *signed(mns[1], p3, 2, 1, t() + 3601)))
        probes.append(self.probe("an unsupported signal", *signed(mns[2], p3, 7, 1, t())))
        probes.append(self.probe("a vote of the trigger by an evonode", *signed(mns[5], next(
            h for h, o in objs.items() if o["ObjectType"] == 2), 2, 1, t())))
        raw, h = signed(mns[3], p3, 2, 1, t())
        short = raw[:-97] + bytes([10]) + rnd.randbytes(10)
        probes.append(self.probe("a 10-byte signature", short, h))
        out["probes"] = probes
        out["gobject_get_after_probes"] = {h: node.gobject("get", h) for h in node.gobject("list", "all")}
        out["gobject_getcurrentvotes_after_probes"] = {
            h: node.gobject("getcurrentvotes", h) for h in node.gobject("list", "all")}

        # --- 7. the superblock --------------------------------------------------------------
        self.bump_mocktime(1)
        block_hash = self.generate(node, 1, sync_fun=lambda: self.sync_blocks())[0]
        out["superblock"] = {"height": tip + 1, "hash": block_hash, "hex": node.getblock(block_hash, 0)}

        # --- 8. stop node 0, keep governance.dat ---------------------------------------------
        self.stop_node(0)
        os.makedirs(self.options.outdir, exist_ok=True)
        dat = os.path.join(self.options.outdir, "governance.dat")
        shutil.copy(os.path.join(node.datadir, "regtest", "governance.dat"), dat)
        out["governance_dat_sha256"] = hashlib.sha256(open(dat, "rb").read()).hexdigest()
        with open(os.path.join(self.options.outdir, "capture.json"), "w") as f:
            json.dump(out, f, indent=1, sort_keys=True, default=str)


if __name__ == "__main__":
    GovCapture().main()
