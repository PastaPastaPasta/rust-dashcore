#!/usr/bin/env python3
"""BL-648: regtest chains with V19 above height 1 and evo content on both sides of the switch.

Not a test of Core: a DashTestFramework script used as a recorder. record.sh copies it into the
exported `test/functional/` of a Core tag (through ../lq3a-captures/run.sh) and runs it there
against that tag's release binaries.

--mode reorg (R1): V19 at N = 212 with DIP0003 active and ChainLocks off. A controller and 4
regular masternodes registered with legacy operator keys; DIP0024, V20 and MN_RR deferred, so every
CbTx is version 2. llmq_test and llmq_test_instantsend run 3-member DKGs at the same 24-block cycle
bases (168 and 192 legacy, 216 and 240 basic); llmq_test_platform does too while V19 is not active
after the base (regular members), and from then on, with no EvoNodes, gets null commitments only.
Around N: a ProUpServTx v1 at 210, a fifth masternode's ProRegTx v2 at N, a ProUpRegTx v2 moving a
legacy masternode's operator key to basic at 213, a ProUpRevTx (v2) revoking a legacy-key
masternode at 214, and the moved masternode's reviving ProUpServTx at 215. Then a side
branch S off 208 (209..214, plain blocks, crossing N at S's own B) and the reorg back to M.

--mode v20-first (R2): V20 and MN_RR at 100, V19 at N2 = 236, ChainLocks on. A controller and 4
legacy masternodes; the same types' cycles at 168, 192 and 216 (legacy) and 240 and 264 (basic).
DIP0024 is deferred here too, so no rotation DKG runs beside them. Core's
miner keeps ChainLocks below the V19 height out of CbTxs, so every CbTx up to N2 carries a null
one and N2 + 1's the basic ChainLock on N2. switch-refused.txt is N2 with its CbTx carrying the
ChainLock on N2 - 2 (legacy bytes); the probe gives dashd's answer. A raw P2P observer on node0
keeps every `clsig` wire payload.

--mode probe-reorg / probe-v20-first: one fresh node with the recording's chain args replays it from
--chain (record.sh runs this after a passing capture). See ProbeRun.
"""
import json
import os
import re
import struct
import time
from datetime import datetime, timezone

from io import BytesIO

from capture_lq3a import RawObserver
from test_framework.messages import CBlock, CCbTx
from test_framework.masternodes import check_banned, check_punished
from test_framework.test_framework import BitcoinTestFramework, DashTestFramework, MasternodeInfo
from test_framework.util import assert_equal, force_finish_mnsync, p2p_port

CYCLE = 24
PHASE_BLOCKS = 2
WINDOW_START, WINDOW_END = 10, 18
COMMIT_OFFSET = 11
MEMBERS = 3
MN_COUNT = 4
DEFERRED = 1000000
QC_TX = 6
ZERO_KEY = "0" * 96
ZERO_SIG = "0" * 192
SPORK_OFF = 4070908800
NEVER_ACTIVE = "testdummy"

MODES = {
    "reorg": {
        "v19": 212,
        "v20": DEFERRED,
        "types": {"llmq_test": 100, "llmq_test_instantsend": 104},
        "platform": {"llmq_test_platform": 106},
        "bases": (168, 192, 216, 240),
        "chainlocks": False,
    },
    "v20-first": {
        "v19": 236,
        "v20": 100,
        "types": {"llmq_test": 100, "llmq_test_instantsend": 104},
        "platform": {"llmq_test_platform": 106},
        "bases": (168, 192, 216, 240, 264),
        "chainlocks": True,
    },
}
# llmq_test_platform takes EvoNodes only where V19 is active after the base block
# (llmq/utils.cpp:294-295): before that its quorums are the regular masternodes', after it there
# are no EvoNodes and every window gets null commitments.


def cycle_types(mode, base):
    """The types with a non-null commitment in the cycle at `base`."""
    m = MODES[mode]
    return {**m["types"], **(m["platform"] if base + 1 < m["v19"] else {})}


# R1's side branch: off P = 208, six plain blocks 209..214, cut while M's tip is 215.
R1_P, R1_S_TIP, R1_M_AT_FORK = 208, 214, 215
# R2's tip: plain ChainLocked blocks after the last cycle.
R2_TIP = 290


def chain_args(mode):
    m = MODES[mode]
    return [
        "-dip3params=2:2",
        f"-testactivationheight=v20@{m['v20']}",
        f"-testactivationheight=mn_rr@{m['v20']}",
        f"-testactivationheight=dip0024@{DEFERRED}",
        "-vbparams=testdummy:-2:0",  # NEVER_ACTIVE
        f"-testactivationheight=v19@{m['v19']}",
    ]


def node_args(mode):
    """Command-line args: the chain args the framework's dash.conf does not already carry."""
    return chain_args(mode)[3:] + ["-deprecatedrpc=legacy_mn"]


def clsig_fields(payload_hex):
    raw = bytes.fromhex(payload_hex)
    assert len(raw) == 4 + 32 + 96, len(raw)
    height = struct.unpack("<i", raw[:4])[0]
    return height, raw[4:36][::-1].hex(), raw[36:].hex()


class V19WindowCapture(DashTestFramework):
    def add_options(self, parser):
        self.add_wallet_options(parser)
        parser.add_argument("--out", dest="out", required=True, help="output directory")
        parser.add_argument("--mode", dest="mode", required=True, choices=sorted(MODES))

    def set_test_params(self):
        self.mode = self.options.mode
        self.m = MODES[self.mode]
        self.set_dash_test_params(1, 0, [node_args(self.mode)])
        self.delay_v20_and_mn_rr(self.m["v20"])
        self.supports_cli = False

    # -- helpers -------------------------------------------------------------------------------

    def sync_tips(self):
        tip = self.nodes[0].getbestblockhash()
        self.wait_until(lambda: all(n.getbestblockhash() == tip for n in self.nodes), timeout=60)

    def active_mns(self):
        return [mn for mn in self.mninfo if mn.proTxHash not in self.revoked]

    def mine(self, count):
        for _ in range(count):
            self.mine_block()

    def mine_block(self, txs=None, wait_cl=False):
        """One block on node0: the mempool's, or with `txs` exactly the mempool's transactions, or
        with `txs` empty none at all (generateblock). generateblock's template is built before
        the transactions are added, so its masternode payment misses their fees: a block with
        fees comes from the mempool."""
        node0 = self.nodes[0]
        self.before_block()
        self.bump_mocktime(1)
        if txs == []:
            node0.generateblock(self.miner, [], invalid_call=False)
            self.sync_tips()
        else:
            if txs is not None:
                assert_equal(sorted(node0.getrawmempool()), sorted(txs))
            self.generate(node0, 1, sync_fun=self.sync_tips)
        self.after_block(wait_cl)
        return node0.getbestblockhash()

    def before_block(self):
        if self.cl is not None:
            self.cl.poll()

    def after_block(self, wait_cl):
        if self.cl is not None:
            self.cl.cbtx()
            if wait_cl:
                tip = self.nodes[0].getbestblockhash()
                self.wait_until(lambda: (best_or_none(self.nodes[0]) or {}).get("blockhash") == tip, timeout=60)

    def wait_phase(self, q, phase, received=None, count=0):
        """Every non-null type's 3 members are in `phase` of the session for base `q`."""
        types = cycle_types(self.mode, self.nodes[0].getblockheader(q)["height"])

        def ready():
            members = dict.fromkeys(types, 0)
            for mn in self.active_mns():
                for s in mn.get_node(self).quorum("dkgstatus")["session"]:
                    name, status = s["llmqType"], s["status"]
                    if name not in types or status["quorumHash"] != q:
                        continue
                    if status["phase"] != phase:
                        return False
                    if received is not None and status[received] < count:
                        return False
                    members[name] += 1
            return all(m >= MEMBERS for m in members.values())
        self.wait_until(ready, timeout=60)

    def wait_commitments(self, q):
        want = set(cycle_types(self.mode, self.nodes[0].getblockheader(q)["height"]).values())
        self.wait_until(lambda: self.node_quorum_commitment_types(self.nodes[0], q) >= want, timeout=60)

    def v19_at(self, height):
        """Whether V19 is active for the block at `height` (getdeploymentinfo at its parent)."""
        node0 = self.nodes[0]
        return node0.getdeploymentinfo(node0.getblockhash(height - 1))["deployments"]["v19"]["active"]

    def check_chain_args(self):
        v19, v20 = self.m["v19"], self.m["v20"]
        for node in self.nodes:
            d = node.getdeploymentinfo(node.getblockhash(1))["deployments"]
            assert_equal(d["dip0003"]["height"], 2)
            assert_equal(d["v19"]["height"], v19)
            assert_equal(d["dip0024"]["height"], DEFERRED)
            for name in ("v20", "mn_rr"):
                assert_equal(d[name]["height"], v20)
            assert NEVER_ACTIVE not in d, d.get(NEVER_ACTIVE)
            tip = node.getdeploymentinfo()["deployments"]
            for name in ("v24", "evo_shares", "dip0024"):
                assert not tip[name]["active"], name

    def check_masternodes(self):
        for mn in self.active_mns():
            assert not check_punished(self.nodes[0], mn), mn.proTxHash
            assert not check_banned(self.nodes[0], mn), mn.proTxHash

    def mined_tx(self, txid, block_hash):
        return self.nodes[0].getrawtransaction(txid, True, block_hash)

    def state_versions(self, height):
        node0 = self.nodes[0]
        block_hash = node0.getblockhash(height)
        out = {}
        for mn in self.mninfo:
            if mn.proTxHash in self.registered_after and height < self.registered_after[mn.proTxHash]:
                continue
            state = node0.protx("info", mn.proTxHash, block_hash)["state"]
            out[mn.proTxHash] = {"version": state["version"], "pubKeyOperator": state["pubKeyOperator"],
                                 "PoSeBanHeight": state["PoSeBanHeight"], "revocationReason": state["revocationReason"]}
        return out

    # -- one cycle -----------------------------------------------------------------------------

    def write_refused(self, n2, lock):
        """switch-refused.txt: N2 with its CbTx rewritten to carry `lock`, the ChainLock on N2 - 2
        as its `clsig` came (legacy bytes), at difference 1, re-solved as
        feature_llmq_chainlocks.py's test_bestCLHeightDiff shapes CbTxs. Core's miner never
        builds it (node/miner.cpp CalcCbTxBestChainlock keeps ChainLocks below the V19 height
        out); the probe gives dashd's answer."""
        node0 = self.nodes[0]
        mined = node0.getblockhash(n2)
        block = CBlock()
        block.deserialize(BytesIO(bytes.fromhex(node0.getblock(mined, 0))))
        cbtx = CCbTx()
        cbtx.deserialize(BytesIO(block.vtx[0].vExtraPayload))
        as_mined = {"hash": mined, "diff": cbtx.bestCLHeightDiff, "sig": cbtx.bestCLSignature.hex()}
        assert_equal((as_mined["diff"], as_mined["sig"]), (0, ZERO_SIG))
        _, _, sig = clsig_fields(lock["wire"])
        cbtx.bestCLHeightDiff = 1
        cbtx.bestCLSignature = bytes.fromhex(sig)
        block.vtx[0].vExtraPayload = cbtx.serialize()
        block.vtx[0].rehash()
        block.hashMerkleRoot = block.calc_merkle_root()
        block.solve()
        refused = {"height": n2, "hash": block.hash, "parent": node0.getblockhash(n2 - 1), "as_mined": as_mined,
                   "lock": [lock["height"], lock["block_hash"]], "lock_sig": sig}
        self.write_blocks(os.path.join(self.options.out, "switch-refused.txt"),
                          f"{os.path.basename(__file__)} --mode v20-first: block {n2} of chain.txt with its CbTx "
                          f"rewritten to carry the ChainLock on {n2 - 2} (difference 1, its clsig's legacy bytes) "
                          f"and re-solved; never mined by Core", [block.serialize().hex()])
        return refused

    def mine_cycle(self, base_expected):
        """Mines to `base_expected` and one DKG cycle of the non-null types, their commitments at
        base+11. Returns at base+11; the caller mines the rest of the cycle."""
        start = time.time()
        node0 = self.nodes[0]
        skip = base_expected - node0.getblockcount()
        assert skip >= 1, (base_expected, node0.getblockcount())
        self.mine(skip)
        base = node0.getblockcount()
        assert_equal(base % CYCLE, 0)
        q = node0.getbestblockhash()

        self.wait_phase(q, 1)
        types = cycle_types(self.mode, base)
        for name in types:
            self.wait_for_quorum_connections(q, MEMBERS - 1, self.active_mns(), llmq_type_name=name,
                                             wait_proc=lambda: self.bump_mocktime(1))
        for phase, received, count in ((2, "receivedContributions", MEMBERS), (3, "receivedComplaints", 0),
                                       (4, "receivedJustifications", 0), (5, "receivedPrematureCommitments", MEMBERS)):
            self.mine(PHASE_BLOCKS)
            self.wait_phase(q, phase, received, count)
        self.mine(COMMIT_OFFSET - 1 - (5 - 1) * PHASE_BLOCKS)
        assert_equal(node0.getblockcount(), base + COMMIT_OFFSET - 1)
        self.wait_phase(q, 6)
        self.wait_commitments(q)
        self.mine(1)
        listed = node0.quorum("list", 1)
        for name in types:
            assert_equal(listed[name], [q])
        return {"base": base, "quorum_hash": q, "mined_height": base + COMMIT_OFFSET, "types": sorted(types.values()),
                "version": 3 if self.v19_at(base + 1) else 1, "wall_s": round(time.time() - start, 2)}

    def check_blocks(self, first, last):
        """CbTx version and height, and the commitment pattern, of blocks first..last on node0."""
        node0 = self.nodes[0]
        v19, v20 = self.m["v19"], self.m["v20"]
        names = {t: n for n, t in {**self.m["types"], **self.m["platform"]}.items()}
        for h in range(first, last + 1):
            block = node0.getblock(node0.getblockhash(h), 2)
            assert_equal(block["cbTx"]["height"], h)
            assert_equal(block["cbTx"]["version"], 3 if h >= v20 else 2)
            for tx in block["tx"]:
                if tx.get("type") != QC_TX:
                    continue
                c = tx["qcTx"]["commitment"]
                assert c["llmqType"] in names, (h, c["llmqType"])
                base_h = node0.getblockheader(c["quorumHash"])["height"]
                assert_equal(base_h, h - h % CYCLE)
                null = c["signersCount"] == 0 and c["quorumPublicKey"] == ZERO_KEY
                if null:
                    assert WINDOW_START <= h % CYCLE <= WINDOW_END, h
                    assert_equal(c["version"], 3 if h >= v19 else 1)
                else:
                    assert names[c["llmqType"]] in cycle_types(self.mode, base_h), (h, c)
                    assert_equal(h % CYCLE, COMMIT_OFFSET)
                    assert_equal(c["version"], 3 if base_h + 1 >= v19 else 1)

    # -- output --------------------------------------------------------------------------------

    def dump(self, path, first, last, headline):
        node0 = self.nodes[0]
        self.write_blocks(path, headline, (node0.getblock(node0.getblockhash(h), 0) for h in range(first, last + 1)))

    def write_blocks(self, path, headline, blocks):
        version = os.environ.get("LQ3A_DASHD_VERSION", "")
        assert version, "run through record.sh (LQ3A_DASHD_VERSION)"
        with open(path, "w", encoding="utf8") as f:
            f.write(f"# {version}\n")
            f.write(f"# dashd -regtest {' '.join(chain_args(self.mode))}\n")
            f.write(f"# {headline}\n")
            for block in blocks:
                f.write(block + "\n")

    def scheme_lines(self, node_idx=0):
        path = os.path.join(self.nodes[node_idx].datadir, self.chain, "debug.log")
        pat = re.compile(r"(ScopedBLSLegacyScheme: .*|CSpecialTxProcessor::\w+ -- bls_legacy_scheme=\d|"
                         r"\w+: bls_legacy_scheme=\d)$")
        with open(path, encoding="utf8", errors="replace") as f:
            return [m.group(1) for line in f if (m := pat.search(line.rstrip("\n")))]

    # -- setup ---------------------------------------------------------------------------------

    def setup_masternodes(self):
        node0 = self.nodes[0]
        need = MN_COUNT * 1000 + 2000
        while node0.getbalance() < need:
            self.bump_mocktime(1)
            self.generate(node0, 10, sync_fun=self.no_op)
        for _ in range(MN_COUNT):
            mn = self.dynamically_add_masternode()
            assert mn.legacy and not mn.evo
        self.mine(8)
        assert_equal(len(self.mninfo), MN_COUNT)
        for mn in self.mninfo:
            assert_equal(node0.protx("info", mn.proTxHash)["state"]["version"], 1)
        node0.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        node0.sporkupdate("SPORK_2_INSTANTSEND_ENABLED", SPORK_OFF)
        node0.sporkupdate("SPORK_19_CHAINLOCKS_ENABLED", 0 if self.m["chainlocks"] else SPORK_OFF)
        self.wait_for_sporks_same()
        sporks = node0.spork("show")
        assert sporks["SPORK_23_QUORUM_POSE"] > 1, "PoSe probes would need wait_for_masternode_probes"
        self.check_chain_args()
        return sporks

    def start_mn_node(self, mn):
        """Starts a node for `mn`, registered on node0's chain, as dynamically_add_masternode does."""
        idx = len(self.nodes)
        assert_equal(mn.nodePort, p2p_port(idx))
        self.dynamically_initialize_datadir(idx)
        info = self.add_dynamically_node(self.extra_args[0])
        self.start_node(idx, ['-masternodeblsprivkey=%s' % mn.keyOperator] + info.extra_args)
        mn.set_node(idx)
        self.connect_nodes(idx, 0)
        self.wait_for_sporks_same()
        self.sync_blocks()
        force_finish_mnsync(self.nodes[idx])

    def restart_mn_node(self, mn, key=True):
        """Restarts `mn`'s node, with its (new) operator key or as a plain node. Peers drop a
        masternode connection whose operator key changed or whose masternode is gone, so a node
        whose masternode differs from its list's runs as a plain node."""
        self.stop_node(mn.nodeIdx)
        args = ['-masternodeblsprivkey=%s' % mn.keyOperator] if key else []
        self.start_node(mn.nodeIdx, args + self.extra_args[0])
        self.connect_nodes(mn.nodeIdx, 0)
        self.sync_blocks()
        force_finish_mnsync(mn.get_node(self))

    # -- the run -------------------------------------------------------------------------------

    def run_test(self):
        os.makedirs(self.options.out, exist_ok=True)
        self.started = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.revoked = set()
        self.registered_after = {}
        self.cl = None
        self.miner = self.nodes[0].getnewaddress()
        if self.mode == "reorg":
            self.run_reorg()
        else:
            self.run_v20_first()

    def write_summary(self, extra):
        node0 = self.nodes[0]
        summary = {
            "mode": self.mode,
            "core_tag": os.environ.get("LQ3A_CORE_TAG"),
            "dashd_version": os.environ.get("LQ3A_DASHD_VERSION"),
            "chain_args": chain_args(self.mode),
            "node_args": node_args(self.mode),
            "v19_height": self.m["v19"],
            "v20_height": self.m["v20"],
            "types": self.m["types"],
            "platform": self.m["platform"],
            "masternodes": [{"proTxHash": mn.proTxHash,
                             "legacy_at_registration": mn.proTxHash not in self.registered_after,
                             "node": mn.nodeIdx} for mn in self.mninfo],
            "started_utc": self.started,
            "finished_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "tip": node0.getblockcount(),
            "tip_hash": node0.getbestblockhash(),
            "scheme_lines_node0": self.scheme_lines(),
        }
        summary.update(extra)
        with open(os.path.join(self.options.out, "summary.json"), "w", encoding="utf8") as f:
            json.dump(summary, f, indent=1, sort_keys=True)
            f.write("\n")

    # -- R1 ------------------------------------------------------------------------------------

    def run_reorg(self):
        node0 = self.nodes[0]
        n = self.m["v19"]
        b = n - 1
        sporks = self.setup_masternodes()
        setup_tip = node0.getblockcount()
        assert setup_tip < self.m["bases"][0] - 1, setup_tip
        mn_a, mn_b, mn_c, mn_d = self.mninfo

        cycles = [self.mine_cycle(168)]
        self.mine(191 - node0.getblockcount())
        cycles.append(self.mine_cycle(192))
        assert_equal(node0.getblockcount(), 203)
        self.mine(1)

        # 205: the fifth masternode's collateral and every later ProTx's fee input, so that each
        # of them spends only outputs confirmed at or below P = 208.
        mn_e = MasternodeInfo(evo=False, legacy=False)
        mn_e.generate_addresses(node0)
        mn_e.set_params(nodePort=p2p_port(len(self.nodes)))
        fees = {k: node0.getnewaddress() for k in ("upserv_a", "reg_e", "upreg_b", "revoke_c", "upserv_b")}
        outputs = {mn_e.collateral_address: 1000, **{a: 1 for a in fees.values()}}
        funding = node0.sendmany("", outputs)
        e_vout = mn_e.get_collateral_vout(node0, funding)
        self.mine_block([funding])
        assert_equal(node0.getblockcount(), 205)
        assert_equal(node0.getrawmempool(), [])
        self.mine(3)
        assert_equal(node0.getblockcount(), R1_P)
        self.mine(1)

        events = {}
        # 210: a ProUpServTx v1 on a legacy masternode, below N.
        txid = mn_a.update_service(node0, submit=True, fundsAddr=fees["upserv_a"])
        at = self.mine_block([txid])
        events["upserv_a"] = {"height": 210, "txid": txid, "proTxHash": mn_a.proTxHash,
                              "version": self.mined_tx(txid, at)["proUpServTx"]["version"]}
        assert_equal(events["upserv_a"]["version"], 1)
        self.mine(1)
        assert_equal(node0.getblockcount(), b)
        assert not self.v19_at(b) and self.v19_at(n)

        # N: the fifth masternode's ProRegTx v2 (basic operator key).
        txid = mn_e.register(node0, submit=True, collateral_txid=funding, collateral_vout=e_vout,
                             operator_reward=0, fundsAddr=fees["reg_e"])
        reg_e_hex = node0.getrawtransaction(txid)
        at = self.mine_block([txid])
        assert_equal(node0.getblockcount(), n)
        mn_e.set_params(proTxHash=txid, operator_reward=0, collateral_txid=funding, collateral_vout=e_vout)
        self.registered_after[txid] = n
        tx = self.mined_tx(txid, at)
        events["reg_e"] = {"height": n, "txid": txid, "version": tx["proRegTx"]["version"], "hex": reg_e_hex,
                           "inputs": [i["txid"] for i in tx["vin"]]}
        assert_equal(events["reg_e"]["version"], 2)
        assert all(i == funding for i in events["reg_e"]["inputs"]), events["reg_e"]["inputs"]
        # E's node starts after the reorg back to M: S has no E.
        self.mninfo.append(mn_e)

        # 213: a ProUpRegTx v2 moving a legacy masternode's operator key to basic.
        mn_b.legacy = False
        new_key = node0.bls("generate")
        mn_b.pubKeyOperator, mn_b.keyOperator = new_key["public"], new_key["secret"]
        txid = mn_b.update_registrar(node0, submit=True, fundsAddr=fees["upreg_b"])
        at = self.mine_block([txid])
        events["upreg_b"] = {"height": 213, "txid": txid, "proTxHash": mn_b.proTxHash,
                             "version": self.mined_tx(txid, at)["proUpRegTx"]["version"]}
        assert_equal(events["upreg_b"]["version"], 2)
        # B's key differs on S: its node runs plain until the reorg back to M.
        self.restart_mn_node(mn_b, key=False)

        # 214: a ProUpRevTx revoking a legacy-key masternode after N (version 2: the RPC signs it with
        # the legacy-scheme state's operator secret under the basic scheme).
        txid = mn_c.revoke(node0, submit=True, reason=0, fundsAddr=fees["revoke_c"])
        at = self.mine_block([txid])
        events["revoke_c"] = {"height": 214, "txid": txid, "proTxHash": mn_c.proTxHash,
                              "version": self.mined_tx(txid, at)["proUpRevTx"]["version"]}
        assert_equal(events["revoke_c"]["version"], 2)
        self.revoked.add(mn_c.proTxHash)
        self.restart_mn_node(mn_c, key=False)

        # 215: the moved masternode's ProUpServTx (basic key) revives it.
        txid = mn_b.update_service(node0, submit=True, fundsAddr=fees["upserv_b"])
        at = self.mine_block([txid])
        events["upserv_b"] = {"height": R1_M_AT_FORK, "txid": txid, "proTxHash": mn_b.proTxHash,
                              "version": self.mined_tx(txid, at)["proUpServTx"]["version"]}
        assert_equal(events["upserv_b"]["version"], 2)
        assert_equal(node0.getrawmempool(), [])

        states = {h: self.state_versions(h) for h in (b, n, 214)}
        assert all(s["version"] == 1 for s in states[b].values())
        assert_equal(states[n][mn_e.proTxHash]["version"], 2)
        assert_equal(states[214][mn_b.proTxHash]["version"], 2)
        assert states[214][mn_b.proTxHash]["PoSeBanHeight"] == 213
        assert_equal(states[214][mn_a.proTxHash]["version"], 1)
        assert_equal(node0.protx("info", mn_b.proTxHash)["state"]["PoSeBanHeight"], -1)

        # S: off P, six plain blocks on every node's invalidation of M's P + 1.
        m_at_fork = node0.getbestblockhash()
        cut = node0.getblockhash(R1_P + 1)
        parent_hash = node0.getblockhash(R1_P)
        self.log.info("S: invalidateblock %s (height %d) on every node" % (cut, R1_P + 1))
        for node in self.nodes:
            node.invalidateblock(cut)
        self.wait_until(lambda: all(peer.getbestblockhash() == parent_hash for peer in self.nodes), timeout=60)
        n1 = None
        while node0.getblockcount() < R1_S_TIP:
            if node0.getblockcount() == b - 1:
                # N1: N's ProRegTx v2 one block early, at S's B, through dashd's own TestBlockValidity.
                try:
                    node0.generateblock(self.miner, [reg_e_hex], False, invalid_call=False)
                    n1 = "accepted"
                except Exception as e:  # JSONRPCException
                    n1 = str(e)
                self.log.info("N1 at S's %d: %s" % (b, n1))
            self.mine_block([])
        assert n1 is not None and "bad-protx-version" in n1, n1
        s_tip_hash = node0.getbestblockhash()
        self.check_blocks(R1_P + 1, R1_S_TIP)
        assert not self.v19_at(b) and self.v19_at(n)
        self.dump(os.path.join(self.options.out, "fork.txt"), R1_P + 1, R1_S_TIP,
                  f"fork off {R1_P} {parent_hash}: invalidateblock {cut} on every node at M's {R1_M_AT_FORK}, "
                  f"{R1_S_TIP - R1_P} plain blocks (generateblock, no transactions), crossing V19 at {n}; "
                  f"reconsiderblock {cut}")
        for node in self.nodes:
            node.reconsiderblock(cut)
        self.wait_until(lambda: all(peer.getbestblockhash() == m_at_fork for peer in self.nodes), timeout=60)
        assert_equal(node0.getrawmempool(), [])
        self.restart_mn_node(mn_b)
        self.start_mn_node(mn_e)

        cycles.append(self.mine_cycle(216))
        self.mine(239 - node0.getblockcount())
        cycles.append(self.mine_cycle(240))
        self.mine(240 + 19 - node0.getblockcount())
        tip = node0.getblockcount()
        self.check_blocks(2, tip)
        self.check_masternodes()
        self.check_chain_args()
        states[tip] = self.state_versions(tip)
        assert_equal([c["version"] for c in cycles], [1, 1, 3, 3])
        assert_equal(states[tip][mn_c.proTxHash]["PoSeBanHeight"], 214)
        assert_equal(node0.protx("info", mn_c.proTxHash)["state"]["revocationReason"], 0)

        self.dump(os.path.join(self.options.out, "chain.txt"), 1, tip,
                  f"{os.path.basename(__file__)} --mode reorg: blocks 1-{tip} of M; controller plus {MN_COUNT} "
                  f"legacy masternodes; V19 at {n}; DKG cycles of {', '.join(self.m['types'])} at "
                  f"{', '.join(str(c['base']) for c in cycles)} with commitments at base+{COMMIT_OFFSET}; "
                  f"ProUpServTx v1 at 210, ProRegTx v2 at {n}, ProUpRegTx v2 at 213, ProUpRevTx v2 at 214, "
                  f"ProUpServTx v2 at 215; sporks 17 on, 2 and 19 off; framework mocktime")
        self.write_summary({
            "setup_tip": setup_tip,
            "sporks": sporks,
            "cycles": cycles,
            "events": events,
            "revoked": sorted(self.revoked),
            "state_versions": {str(h): s for h, s in states.items()},
            "n1_dashd": n1,
            "m": {"blocks": tip, "tip_at_fork": R1_M_AT_FORK},
            "s": {"parent": R1_P, "parent_hash": parent_hash, "invalidated": cut, "blocks": R1_S_TIP - R1_P,
                  "tip_hash": s_tip_hash},
        })

    # -- R2 ------------------------------------------------------------------------------------

    def run_v20_first(self):
        node0 = self.nodes[0]
        n2 = self.m["v19"]
        sporks = self.setup_masternodes()
        setup_tip = node0.getblockcount()
        assert setup_tip < self.m["bases"][0] - 1, setup_tip
        self.cl = ClBook(self)
        self.cl.attach()

        cycles = []
        first_cl = None
        for base in self.m["bases"]:
            cycles.append(self.mine_cycle(base))
            while node0.getblockcount() < base + CYCLE - 1:
                # ChainLocks can sign once the first quorum is 8 blocks old (SIGN_HEIGHT_OFFSET).
                wait = node0.getblockcount() + 1 >= cycles[0]["mined_height"] + 8
                self.mine_block(wait_cl=wait)
                if wait and first_cl is None:
                    first_cl = node0.getblockcount()
        while node0.getblockcount() < R2_TIP:
            self.mine_block(wait_cl=True)
        self.cl.poll()
        tip = node0.getblockcount()
        assert first_cl is not None

        # The boundary: every CbTx from V20 to N2 carries a null ChainLock (Core's miner keeps
        # legacy ChainLocks out), and N2 + 1's carries the basic ChainLock on N2 at difference 0.
        for h in range(self.m["v20"], n2 + 1):
            assert_equal((h, self.cl.cbtxs[h]["diff"], self.cl.cbtxs[h]["sig"]), (h, 0, ZERO_SIG))
        first = self.cl.cbtxs[n2 + 1]
        assert_equal((first["diff"], first["lock"]), (0, [n2, node0.getblockhash(n2)]))
        locks = self.cl.locks()
        held = locks[(n2 - 2, node0.getblockhash(n2 - 2))]
        assert_equal((held["wire_source"], held["arrival_tip"]), ("p2p", n2 - 2))
        refused = self.write_refused(n2, held)
        assert not self.v19_at(n2 - 1) and self.v19_at(n2)
        self.check_blocks(2, tip)
        self.check_masternodes()
        self.check_chain_args()
        assert_equal([c["version"] for c in cycles], [1, 1, 1, 3, 3])

        with open(os.path.join(self.options.out, "clsigs.jsonl"), "w", encoding="utf8") as f:
            for lock in sorted(locks.values(), key=lambda x: (x["arrival_tip"], x["height"])):
                f.write(json.dumps(lock, sort_keys=True) + "\n")
        self.dump(os.path.join(self.options.out, "chain.txt"), 1, tip,
                  f"{os.path.basename(__file__)} --mode v20-first: blocks 1-{tip}; controller plus {MN_COUNT} "
                  f"legacy masternodes; V20 and MN_RR at {self.m['v20']}, V19 at {n2}; DKG cycles of "
                  f"{', '.join(self.m['types'])} at {', '.join(str(c['base']) for c in cycles)} with commitments "
                  f"at base+{COMMIT_OFFSET}; outside the DKG phases, from the first ChainLock, node0 waits "
                  f"for each block's ChainLock before the next; sporks 17 and 19 on, 2 off; framework mocktime")
        self.write_summary({
            "setup_tip": setup_tip,
            "sporks": sporks,
            "cycles": cycles,
            "first_chainlock_wait": first_cl,
            "cbtx": {str(h): c for h, c in sorted(self.cl.cbtxs.items())},
            "boundary": {"null_through": n2, "first": {"height": n2 + 1, **first}},
            "refused": refused,
            "chainlocks": len(locks),
            "observer": {k: sum(len(getattr(o, k)) for o in self.cl.observers)
                         for k in ("captured", "notfound", "unsolicited")},
            "state_versions": {str(tip): self.state_versions(tip)},
        })


class ClBook:
    """R2's ChainLock record: every ChainLock node0 held before each block (getbestchainlock), each
    CbTx's (bestCLHeightDiff, bestCLSignature), and the observer's `clsig` wire payloads. A lock's
    arrival tip is the lowest of: the tip at which getbestchainlock first named it, one below the
    first block whose CbTx carries it, and the tip at which the observer's payload was drained."""

    def __init__(self, test):
        self.test = test
        self.observers = []  # the live one last; an isolation drops the one before
        self.seen = {}      # (height, hash) -> {"arrival_tip", "rpc_sig", "rpc_tip"}
        self.wire = {}      # (height, hash) -> {"wire", "drained_tip"}
        self.cbtxs = {}     # height -> {"diff", "sig", "lock"}

    def tip(self):
        return self.test.nodes[0].getblockcount()

    def attach(self):
        observer = self.test.nodes[0].add_p2p_connection(RawObserver("node0-%d" % len(self.observers)))
        observer.arm(True)
        self.observers.append(observer)

    def drain(self):
        tip = self.tip()
        self.test.wait_until(self.observers[-1].idle, timeout=30)
        for c in (c for o in self.observers for c in o.captured):
            if c["command"] != "clsig":
                continue
            height, block_hash, _ = clsig_fields(c["payload"])
            key = (height, block_hash)
            if key not in self.wire:
                self.wire[key] = {"wire": c["payload"], "drained_tip": tip}
            else:
                assert_equal(self.wire[key]["wire"], c["payload"])

    def poll(self):
        node0 = self.test.nodes[0]
        tip = self.tip()
        best = best_or_none(node0)
        if best is not None:
            key = (best["height"], best["blockhash"])
            if key not in self.seen:
                self.seen[key] = {"arrival_tip": tip, "rpc_sig": best["signature"], "rpc_tip": tip}
        self.drain()

    def cbtx(self):
        """Records the CbTx of every block up to the tip not yet recorded."""
        node0 = self.test.nodes[0]
        for h in range(max(self.cbtxs, default=99) + 1, self.tip() + 1):
            cb = node0.getblock(node0.getblockhash(h), 2)["cbTx"]
            if cb["version"] < 3:
                continue
            diff, sig = cb["bestCLHeightDiff"], cb["bestCLSignature"]
            lock = None
            if sig != ZERO_SIG:
                cl_h = h - 1 - diff
                lock = [cl_h, node0.getblockhash(cl_h)]
                key = tuple(lock)
                entry = self.seen.setdefault(key, {"arrival_tip": h - 1, "rpc_sig": None, "rpc_tip": None})
                entry["arrival_tip"] = min(entry["arrival_tip"], h - 1)
            self.cbtxs[h] = {"diff": diff, "sig": sig, "lock": lock}

    def locks(self):
        """Each ChainLock by (height, hash). A lock superseded before the observer's GETDATA was
        answered (NOTFOUND: node0 serves only its best) has no payload; its `wire` is then built
        from getbestchainlock's signature, else from the first CbTx carrying it, each serialized
        under node0's scheme at that tip (`wire_source` names which)."""
        self.drain()
        out = {}
        for key in set(self.seen) | set(self.wire):
            s, w = self.seen.get(key), self.wire.get(key)
            in_cbtx = sorted(h for h, c in self.cbtxs.items() if c["lock"] and tuple(c["lock"]) == key)
            if w is not None:
                wire, source = w["wire"], "p2p"
            else:
                if s["rpc_sig"]:
                    sig, source = s["rpc_sig"], "getbestchainlock"
                else:
                    sig, source = self.cbtxs[in_cbtx[0]]["sig"], "cbtx"
                wire = struct.pack("<i", key[0]).hex() + bytes.fromhex(key[1])[::-1].hex() + sig
            arrival = min(x for x in (s and s["arrival_tip"], w and w["drained_tip"]) if x is not None)
            out[key] = {"height": key[0], "block_hash": key[1], "wire": wire, "wire_source": source,
                        "arrival_tip": arrival, "rpc_sig": s and s["rpc_sig"], "rpc_tip": s and s["rpc_tip"],
                        "in_cbtx": in_cbtx}
        return out


class ProbeRun(BitcoinTestFramework):
    """One fresh node with a recording's chain args (node args too), mocktime at the recording's
    tip, spork 19 on. Replays --chain's blocks with submitblock and writes probe.json.

    probe-reorg: M to 215, invalidateblock 209, S, a restart on S's tip, reconsiderblock 209, M
    to its tip; the node's scheme log lines in each window.
    probe-v20-first: (a) every block, no ChainLock; (b) after a restart on a new datadir, every
    block with each recorded ChainLock submitted (submitchainlock) at its arrival tip."""

    def add_options(self, parser):
        parser.add_argument("--out", dest="out", required=True)
        parser.add_argument("--mode", dest="mode", required=True, choices=["probe-" + m for m in MODES])
        parser.add_argument("--chain", dest="chain", required=True, help="the recording's out dir")

    def set_test_params(self):
        self.recorded = self.options.mode[len("probe-"):]
        self.num_nodes = 2
        self.setup_clean_chain = True
        self.blocks = read_hex(os.path.join(self.options.chain, "chain.txt"))
        last_time = struct.unpack("<I", bytes.fromhex(self.blocks[-1])[68:72])[0]
        self.disable_mocktime = True
        args = chain_args(self.recorded) + [f"-mocktime={last_time + 60}", "-deprecatedrpc=legacy_mn",
                                            "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK"]
        self.extra_args = [args, args]
        self.supports_cli = False

    def setup_network(self):
        self.setup_nodes()

    def submit(self, node, first, last, blocks=None, base=1):
        verdicts = {}
        for h in range(first, last + 1):
            block = (blocks or self.blocks)[h - base]
            r = node.submitblock(block)
            if r is not None:
                verdicts[h] = r
                break
        return verdicts

    def lines(self, node):
        path = os.path.join(node.datadir, self.chain, "debug.log")
        pat = re.compile(r"(ScopedBLSLegacyScheme: .*|CSpecialTxProcessor::\w+ -- bls_legacy_scheme=\d|"
                         r"\w+: bls_legacy_scheme=\d|Verifying last \d+ blocks at level \d)$")
        with open(path, encoding="utf8", errors="replace") as f:
            return [m.group(1) for line in f if (m := pat.search(line.rstrip("\n")))]

    def run_test(self):
        os.makedirs(self.options.out, exist_ok=True)
        for node in self.nodes:
            node.sporkupdate("SPORK_19_CHAINLOCKS_ENABLED", 0)
        result = self.probe_reorg() if self.recorded == "reorg" else self.probe_v20_first()
        result.update({"mode": self.options.mode, "chain_args": chain_args(self.recorded),
                       "dashd_version": os.environ.get("LQ3A_DASHD_VERSION")})
        with open(os.path.join(self.options.out, "probe.json"), "w", encoding="utf8") as f:
            json.dump(result, f, indent=1, sort_keys=True)
            f.write("\n")

    def probe_reorg(self):
        node = self.nodes[0]
        fork = read_hex(os.path.join(self.options.chain, "fork.txt"))
        windows = []
        seen = 0

        def step(name, verdicts=None):
            nonlocal seen
            all_lines = self.lines(node)
            windows.append({"step": name, "tip": node.getblockcount(), "lines": all_lines[seen:],
                            "verdicts": verdicts or {}})
            seen = len(all_lines)

        step("start")
        step("connect M 1-%d" % R1_M_AT_FORK, self.submit(node, 1, R1_M_AT_FORK))
        cut = node.getblockhash(R1_P + 1)
        node.invalidateblock(cut)
        step("invalidateblock %d" % (R1_P + 1))
        step("connect S %d-%d" % (R1_P + 1, R1_S_TIP), self.submit(node, R1_P + 1, R1_S_TIP, fork, R1_P + 1))
        assert_equal(node.getblockcount(), R1_S_TIP)
        self.restart_node(0)
        step("restart on S's %d" % R1_S_TIP)
        node = self.nodes[0]
        node.reconsiderblock(cut)
        step("reconsiderblock %d" % (R1_P + 1))
        assert_equal(node.getblockcount(), R1_M_AT_FORK)
        step("connect M %d-%d" % (R1_M_AT_FORK + 1, len(self.blocks)),
             self.submit(node, R1_M_AT_FORK + 1, len(self.blocks)))
        assert_equal(node.getblockcount(), len(self.blocks))
        return {"windows": windows, "tip": node.getblockcount()}

    def probe_v20_first(self):
        locks = []
        with open(os.path.join(self.options.chain, "clsigs.jsonl"), encoding="utf8") as f:
            locks = [json.loads(line) for line in f]
        by_tip = {}
        for lock in locks:
            by_tip.setdefault(lock["arrival_tip"], []).append(lock)
        with open(os.path.join(self.options.chain, "summary.json"), encoding="utf8") as f:
            n2 = json.load(f)["refused"]["height"]
        [refused] = read_hex(os.path.join(self.options.chain, "switch-refused.txt"))

        def refuse(node):
            """switch-refused.txt at tip N2 - 1, with the best ChainLock the node holds then."""
            assert_equal(node.getblockcount(), n2 - 1)
            held = best_or_none(node)
            return {"tip": n2 - 1, "verdict": node.submitblock(refused),
                    "best_chainlock": held and held["height"], "tip_after": node.getblockcount()}

        # (a) no ChainLock: the first refusal, if any.
        a_node = self.nodes[0]
        a = self.submit(a_node, 1, n2 - 1)
        a_refused = refuse(a_node) if not a else None
        a = a or self.submit(a_node, n2, len(self.blocks))
        a_tip = a_node.getblockcount()

        # (b) each recorded ChainLock at its arrival tip, on the second node.
        node = self.nodes[1]
        b_verdicts, b_refusal, b_refused = [], {}, None
        for h in range(1, len(self.blocks) + 1):
            r = node.submitblock(self.blocks[h - 1])
            if r is not None:
                b_refusal = {h: r}
                break
            if h == n2 - 1:
                # Before the ChainLocks first seen at N2 - 1: the best is the one on N2 - 2.
                b_refused = refuse(node)
            for lock in by_tip.get(h, []):
                _, _, sig = clsig_fields(lock["wire"])
                try:
                    best = node.submitchainlock(lock["block_hash"], sig, lock["height"])
                    verdict = {"best_height": best}
                except Exception as e:
                    verdict = {"error": str(e)}
                b_verdicts.append({"tip": h, "height": lock["height"], **verdict})
        return {
            "a": {"refusal": a, "tip": a_tip, "best_chainlock": best_or_none(a_node), "switch_refused": a_refused},
            "b": {"refusal": b_refusal, "tip": node.getblockcount(), "submitchainlock": b_verdicts,
                  "best_chainlock": best_or_none(node), "switch_refused": b_refused},
            "a_lines": self.lines(a_node),
            "b_lines": self.lines(node),
        }


def best_or_none(node):
    try:
        return node.getbestchainlock()
    except Exception:
        return None


def read_hex(path):
    with open(path, encoding="utf8") as f:
        return [line.strip() for line in f if line.strip() and not line.startswith("#")]


if __name__ == "__main__":
    import sys
    probe = any(a.startswith("--mode=probe-") or a.startswith("probe-") for a in sys.argv)
    (ProbeRun if probe else V19WindowCapture)().main()
