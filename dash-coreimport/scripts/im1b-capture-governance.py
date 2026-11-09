#!/usr/bin/env python3
"""IM1b vector capture: drive a real Dash Core v24.0.0-rc.3 regtest dashd with Core's own
functional-test framework (DashTestFramework), stop the controller node cleanly and keep the
governance / masternode-meta / net-fulfilled / spork cache files it wrote.

Not part of Core. Run from <core>/test/functional (see the PROVENANCE.md next to the vectors):

  populated (default):
    python3 im1b-capture-governance.py --configfile=<core>/test/config.ini --tmpdir=<dir> \
        --nocleanup --outdir=<out> --timeout-factor=6
  empty (a plain 1-node regtest node started and stopped at once):
    python3 im1b-capture-governance.py --empty ... same options

Output layout (--outdir):
  empty/      governance.dat mncache.dat netfulfilled.dat sporks.dat of a node stopped at once
  populated/  the same four files of node 0 (the controller: it holds the wallet with the voting
              keys, sees every governance object and vote) after the state built below, plus
              expected.json: ground truth read over RPC (dash-cli, raw text kept as Core printed
              it) from the same node just before the stop, plus the sha256 of each file.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys

from test_framework.governance import prepare_object
from test_framework.messages import uint256_to_string
from test_framework.test_framework import BitcoinTestFramework, DashTestFramework
from test_framework.util import assert_equal

FILES = ["governance.dat", "mncache.dat", "netfulfilled.dat", "sporks.dat"]
EMPTY_MODE = "--empty" in sys.argv


def copy_files(node, outdir, name):
    dest = os.path.join(outdir, name)
    os.makedirs(dest, exist_ok=True)
    for f in FILES:
        src = os.path.join(node.datadir, "regtest", f)
        if os.path.exists(src):
            shutil.copy(src, os.path.join(dest, f))
    return dest


class Im1bEmptyCapture(BitcoinTestFramework):
    def add_options(self, parser):
        parser.add_argument("--outdir", dest="outdir", required=True)
        parser.add_argument("--empty", dest="empty", action="store_true")
        self.add_wallet_options(parser, legacy=False)

    def set_test_params(self):
        self.num_nodes = 1
        self.setup_clean_chain = True
        self.disable_autoconnect = False

    def run_test(self):
        self.log.info("empty: start + stop")
        self.stop_node(0)
        copy_files(self.nodes[0], self.options.outdir, "empty")


class Im1bGovernanceCapture(DashTestFramework):
    def add_options(self, parser):
        parser.add_argument("--outdir", dest="outdir", required=True)
        parser.add_argument("--empty", dest="empty", action="store_true")
        self.add_wallet_options(parser)

    def set_test_params(self):
        # node 0 = controller (wallet with all voting keys), nodes 1..4 = regular masternodes,
        # nodes 5, 6 = evonodes added dynamically in run_test
        self.set_dash_test_params(5, 4, [["-budgetparams=10:10:10"]] * 5, evo_count=2)

    # --- helpers -------------------------------------------------------------------------
    def raw_cli(self, *args):
        """Run dash-cli against node 0 and return stdout exactly as Core printed it."""
        node = self.nodes[0]
        p = subprocess.run([os.environ["DASHCLI"], "-datadir=" + node.datadir, *map(str, args)],
                           capture_output=True, text=True)
        if p.returncode:
            return {"__error__": p.stderr.strip()}
        return p.stdout.rstrip("\n")

    def vote(self, h, signal, outcome, mn):
        """Funding votes on proposals are signed with the voting key (controller wallet, `gobject
        vote-alias`). Every other signal needs the masternode's BLS operator key, which the wallet
        does not have: sign the vote hash with it here and submit through `voteraw`."""
        node = self.nodes[0]
        if signal == "funding":
            res = node.gobject("vote-alias", h, signal, outcome, mn.proTxHash)
            assert res["overall"].startswith("Voted successfully 1"), res
        else:
            info = node.protx("info", mn.proTxHash)  # collateral of a register_fund MN is in its ProRegTx
            res = node.voteraw(info["collateralHash"], info["collateralIndex"], h, signal, outcome,
                               int(self.mocktime), self.operator_signature(h, signal, outcome, mn, info))
            assert_equal(res, "Voted successfully")
        self.votes_cast.append((h, signal, outcome, mn.proTxHash))

    def operator_signature(self, h, signal, outcome, mn, info):
        import base64
        import struct
        from py_ecc.bls import G2Basic
        from test_framework.messages import hash256
        sigs = {"funding": 1, "valid": 2, "delete": 3, "endorsed": 4}
        outs = {"yes": 1, "no": 2, "abstain": 3}
        sk_raw = bytes.fromhex(mn.keyOperator)
        for sk_bytes in (sk_raw, sk_raw[::-1]):
            sk = int.from_bytes(sk_bytes, "big")
            if G2Basic.SkToPk(sk).hex() == mn.pubKeyOperator:
                break
        else:
            raise AssertionError("cannot derive the operator public key from keyOperator")
        msg = (bytes.fromhex(info["collateralHash"])[::-1] + struct.pack("<I", info["collateralIndex"])
               + bytes.fromhex(h)[::-1] + struct.pack("<i", outs[outcome]) + struct.pack("<i", sigs[signal])
               + struct.pack("<q", int(self.mocktime)))
        return base64.b64encode(G2Basic.Sign(sk, hash256(msg))).decode()

    def gov_count(self):
        return self.nodes[0].gobject("count")

    def run_test(self):
        node = self.nodes[0]
        self.votes_cast = []
        self.log.info("Start: height %d, %d masternodes", node.getblockcount(), len(self.mninfo))

        # --- 1. two evonodes on top of the four regular masternodes ---------------------
        self.log.info("Adding two evonodes")
        for _ in range(2):
            self.dynamically_add_masternode(evo=True)
            self.bump_mocktime(1)
            self.generate(node, 8, sync_fun=lambda: self.sync_blocks())
        self.wait_until(lambda: all(s == "ENABLED" for s in node.masternodelist("status").values())
                        and len(node.masternodelist("status")) == 6, timeout=60)
        assert_equal(len(self.mninfo), 6)

        # --- 2. sporks: distinct values ------------------------------------------------
        self.log.info("Sporks")
        spork_values = {
            "SPORK_2_INSTANTSEND_ENABLED": 4070908800,
            "SPORK_17_QUORUM_DKG_ENABLED": 4070908801,
            "SPORK_19_CHAINLOCKS_ENABLED": 19,
            "SPORK_21_QUORUM_ALL_CONNECTED": 21,
            "SPORK_23_QUORUM_POSE": 4070908802,
        }
        for name, value in spork_values.items():
            node.sporkupdate(name, value)
        self.wait_for_sporks_same()
        shown = node.spork("show")
        for name, value in spork_values.items():
            assert_equal(shown[name], value)

        # --- 3. proposals --------------------------------------------------------------
        self.log.info("Prepare + submit four proposals")
        proposal_time = self.mocktime
        specs = [("Proposal_0", "1.1"), ("Proposal_1", "3.3"), ("Proposal_2", "2.2"), ("Proposal_3", "4.4")]
        prepared = []
        for name, amount in specs:
            addr = node.getnewaddress()
            prepared.append(prepare_object(node, 1, uint256_to_string(0), proposal_time, 1, name, amount, addr))
        self.bump_mocktime(6)
        self.generate(node, 6, sync_fun=lambda: self.sync_blocks())
        assert_equal(len(node.gobject("list-prepared")), 4)
        hashes = [node.gobject("submit", "0", 1, proposal_time, p["hex"], p["collateralHash"]) for p in prepared]
        p0, p1, p2, p3 = hashes
        assert_equal(len(node.gobject("list")), 4)
        self.wait_until(lambda: len(self.nodes[1].gobject("list")) == 4, timeout=10)

        # --- 4. votes with differing outcomes and signals ------------------------------
        self.log.info("Votes")
        mns = self.mninfo
        y, n, a = "yes", "no", "abstain"
        for mn, o in zip(mns, [y, y, y, y, n, a]):
            self.vote(p0, "funding", o, mn)
        for mn in mns[:3]:
            self.vote(p0, "valid", y, mn)
        self.vote(p0, "endorsed", y, mns[0])
        for mn, o in zip(mns, [n, n, y, a, y, n]):
            self.vote(p1, "funding", o, mn)
        self.vote(p1, "valid", n, mns[0])
        for mn in mns:
            self.vote(p2, "funding", y, mn)
        for mn in mns[:2]:
            self.vote(p2, "valid", y, mn)
        for mn in mns[:2]:
            self.vote(p3, "funding", y, mn)
        self.wait_until(lambda: self.nodes[1].gobject("count")["votes"] == node.gobject("count")["votes"], timeout=20)

        # --- 5. a deleted object: delete votes, then wait for CheckAndRemove ----------
        self.log.info("Delete Proposal_3 (delete-signal votes from every masternode)")
        for mn in mns:
            self.vote(p3, "delete", y, mn)
        erased = 0
        for _ in range(16):
            self.bump_mocktime(90)
            self.generate(node, 1, sync_fun=lambda: self.sync_blocks())
            erased = self.gov_count()["erased"]
            if erased:
                break
        assert_equal(erased, 1)
        self.log.info("mapErasedGovernanceObjects non-empty: %s", self.gov_count())

        # --- 6. a trigger (superblock) created by the masternodes themselves -----------
        self.log.info("Move into the next superblock maturity window")
        gi = node.getgovernanceinfo()
        window = gi["superblockmaturitywindow"]
        trigger_found = False
        for _ in range(60):
            gi = node.getgovernanceinfo()
            height = node.getblockcount()
            if height + 1 >= gi["nextsuperblock"] - window:
                trigger_found = self.wait_until(lambda: len(node.gobject("list", "valid", "triggers")) >= 1,
                                                timeout=3, do_assert=False)
                if trigger_found:
                    break
            self.bump_mocktime(1)
            self.generate(node, 1, sync_fun=lambda: self.sync_blocks())
        assert trigger_found, "no trigger created"
        # let every masternode vote on the trigger
        self.wait_until(lambda: max(t["YesCount"] for t in node.gobject("list", "valid", "triggers").values())
                        >= len(mns) - 1, timeout=60, do_assert=False)
        self.bump_mocktime(1)
        self.generate(node, 1, sync_fun=lambda: self.sync_blocks())

        # --- 7. net-fulfilled requests: let old ones expire, then re-sync node 0 ---------
        self.log.info("Re-sync node 0 so that fresh fulfilled requests exist at shutdown")
        self.bump_mocktime(5 * 60)

        def sync_gov():
            self.bump_mocktime(1)
            return node.mnsync("status")["IsSynced"]

        node.mnsync("reset")
        node.mnsync("next")
        self.wait_until(sync_gov, timeout=120)
        self.wait_until(lambda: self.nodes[1].gobject("count")["votes"] == node.gobject("count")["votes"], timeout=20)

        # --- 8. ground truth from RPC, same node, right before the stop ---------------
        self.log.info("Reading expected values")
        expected = {}
        expected["captured_node_index"] = 0
        expected["getblockcount"] = self.raw_cli("getblockcount")
        expected["getbestblockhash"] = self.raw_cli("getbestblockhash")  # bare hash: quoted below
        for key, args in [
            ("spork_show", ("spork", "show")),
            ("spork_active", ("spork", "active")),
            ("gobject_count", ("gobject", "count")),
            ("gobject_list_all", ("gobject", "list", "all")),
            ("gobject_list_prepared", ("gobject", "list-prepared")),
            ("masternodelist", ("masternodelist",)),
            ("masternodelist_status", ("masternodelist", "status")),
            ("protx_list_valid", ("protx", "list", "valid")),
            ("protx_list_valid_detailed", ("protx", "list", "valid", "true")),
            ("masternode_count", ("masternode", "count")),
            ("getgovernanceinfo", ("getgovernanceinfo",)),
            ("mnsync_status", ("mnsync", "status")),
            ("getpeerinfo_addr", ("getpeerinfo",)),
        ]:
            expected[key] = self.raw_cli(*args)
        all_objs = json.loads(expected["gobject_list_all"])
        expected["gobject_getcurrentvotes"] = {
            h: self.raw_cli("gobject", "getcurrentvotes", h) for h in all_objs
        }
        expected["gobject_get"] = {h: self.raw_cli("gobject", "get", h) for h in all_objs}
        expected["proposal_hashes_submitted"] = {"Proposal_0": p0, "Proposal_1": p1, "Proposal_2": p2,
                                                 "Proposal_3_deleted": p3}
        expected["votes_cast_by_script"] = [
            {"object": h, "signal": s, "outcome": o, "proTxHash": m} for (h, s, o, m) in self.votes_cast]
        expected["masternodes"] = [
            {"index": i, "evo": mn.evo, "proTxHash": mn.proTxHash, "nodeIdx": mn.nodeIdx}
            for i, mn in enumerate(self.mninfo)]
        expected["spork_values_set_by_script"] = spork_values

        # --- 9. clean stop of node 0, copy files byte for byte -------------------------
        self.log.info("Stopping node 0")
        self.stop_node(0)
        dest = copy_files(node, self.options.outdir, "populated")
        expected["sha256"] = {
            f: hashlib.sha256(open(os.path.join(dest, f), "rb").read()).hexdigest()
            for f in FILES if os.path.exists(os.path.join(dest, f))}
        # Raw Core text is spliced in unmodified: every value is already JSON (RPC results);
        # strings printed bare by dash-cli were wrapped above.
        parts = []
        for k, v in expected.items():
            if k in ("gobject_getcurrentvotes", "gobject_get"):
                inner = ",\n  ".join(
                    "%s: %s" % (json.dumps(h), json.dumps(raw) if isinstance(raw, dict) else raw)
                    for h, raw in v.items())
                parts.append(" %s: {\n  %s\n }" % (json.dumps(k), inner))
            elif isinstance(v, str) and k not in ("getbestblockhash",) and k != "captured_node_index" \
                    and not (k in ("proposal_hashes_submitted",)):
                parts.append(" %s: %s" % (json.dumps(k), v))
            else:
                parts.append(" %s: %s" % (json.dumps(k), json.dumps(v, default=str)))
        with open(os.path.join(dest, "expected.json"), "w") as fh:
            fh.write("{\n" + ",\n".join(parts) + "\n}\n")
        json.load(open(os.path.join(dest, "expected.json")))  # must parse


if __name__ == "__main__":
    (Im1bEmptyCapture if EMPTY_MODE else Im1bGovernanceCapture)().main()
