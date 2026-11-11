#!/usr/bin/env python3
"""BL-646: the evo-deep-window guard's regtest chain, recorded from a Dash Core release.

Not a test of Core: a DashTestFramework script used as a recorder. record.sh copies it into the
exported `test/functional/` of a Core tag (through ../lq3a-captures/run.sh) and runs it there
against that tag's release binaries.

The main chain M: one controller and 4 EvoNodes; DIP0024, V20 and MN_RR deferred past the end, so
every block's CbTx is version 2 and the non-rotating llmq_test, llmq_test_instantsend and
llmq_test_platform run their DKGs at the same 24-block cycle bases. TESTDUMMY never activates
(regtest's would by signalling at 432 and enable llmq_test_v17, whose mining window then needs
null commitments of a fourth type). Every cycle has the same
shape: phases at base..base+9, null commitments at base+10, the three non-null commitments at
base+11 (mined only once all three are on the miner), then plain blocks to the next base.

The side branch S forks just below a cycle base about --fork-depth blocks from M's end and runs
its own quorums. Their base blocks are S's blocks, and node0 mines base+9..base+11 while isolated,
so S's commitments land at base+12. Neither key of Core's mined-commitment index (`q_mc`:
type and quorum hash; `q_mcih`: type and mined height) is then shared with M. M has more work,
so reconsidering M's block after the fork point reorgs every node back to it.

Output in --out: chain.txt (M, blocks 1..N) and fork.txt (S) as one block's hex per line after
`#` header lines, plus summary.json (chain args, every cycle's quorums and timing).
"""
import json
import os
import time
from datetime import datetime, timezone

from test_framework.masternodes import check_banned, check_punished
from test_framework.test_framework import DashTestFramework
from test_framework.util import assert_equal

TYPES = {"llmq_test": 100, "llmq_test_instantsend": 104, "llmq_test_platform": 106}
# dkgInterval, dkgPhaseBlocks, the mining window and the size of all three (llmq/params.h).
CYCLE = 24
PHASE_BLOCKS = 2
WINDOW_START, WINDOW_END = 10, 18
MEMBERS = 3
# Offsets of the commitment block: the stock flow's base+11 on M, one block later on S. Phase 6
# (finalize) covers base+10 and base+11 only, so base+12 is the latest the members still make it.
M_OFFSET, S_OFFSET = 11, 12
# The tip after a cycle: the 8 blocks mine_quorum mines to let a new quorum sign.
CYCLE_END = 19
EVO_COUNT = 4
DEFERRED = 1000000
CHAIN_ARGS = [
    "-dip3params=2:2",
    f"-testactivationheight=v20@{DEFERRED}",
    f"-testactivationheight=mn_rr@{DEFERRED}",
    f"-testactivationheight=dip0024@{DEFERRED}",
    "-vbparams=testdummy:-2:0",  # NEVER_ACTIVE
]
# getdeploymentinfo leaves out a NEVER_ACTIVE deployment (DeploymentEnabled), so TESTDUMMY must be
# absent; the EHF deployments stay inactive because no MNHF signal is ever mined.
NEVER_ACTIVE = "testdummy"
INACTIVE = ("v24", "evo_shares")
QC_TX = 6  # TRANSACTION_QUORUM_COMMITMENT
CBTX_VERSION = 2  # MERKLE_ROOT_QUORUMS, until V20's CLSIG_AND_BALANCE
SPORK_OFF = 4070908800
ZERO_KEY = "0" * 96


def offset_of(height):
    return height % CYCLE


def expected_qcs(offset, commit_offset):
    """(nulls, non-nulls) a block at `offset` must carry in a cycle mined at `commit_offset`."""
    if WINDOW_START <= offset < commit_offset:
        return len(TYPES), 0
    if offset == commit_offset:
        return 0, len(TYPES)
    return 0, 0


class DeepWindowCapture(DashTestFramework):
    def add_options(self, parser):
        self.add_wallet_options(parser)
        parser.add_argument("--out", dest="out", required=True, help="output directory")
        parser.add_argument("--blocks", dest="blocks", type=int, default=20100,
                            help="M's length N: cycles are mined until the tip reaches it")
        parser.add_argument("--cycles", dest="cycles", type=int, default=0,
                            help="mine exactly this many M cycles instead; N is then the last tip")
        parser.add_argument("--fork-depth", dest="fork_depth", type=int, default=1150,
                            help="S forks below the first cycle base at or above N - this")
        parser.add_argument("--side-blocks", dest="side_blocks", type=int, default=100,
                            help="S's length: as many cycles as fit, then plain blocks")

    def set_test_params(self):
        # A controller only; the 4 EvoNodes are added in run_test, as p2p_platform_ban.py does.
        self.set_dash_test_params(1, 0, [CHAIN_ARGS[3:]], evo_count=EVO_COUNT)
        self.delay_v20_and_mn_rr(DEFERRED)
        self.supports_cli = False

    # -- helpers -------------------------------------------------------------------------------

    def sync_tips(self):
        tip = self.nodes[0].getbestblockhash()
        self.wait_until(lambda: all(n.getbestblockhash() == tip for n in self.nodes), timeout=60)

    def mine(self, count):
        self.bump_mocktime(1)
        self.generate(self.nodes[0], count, sync_fun=self.sync_tips)

    def wait_phase(self, q, phase, received=None, count=0):
        """Every type's 3 members are in `phase` of the session for base `q` (`received` >= count)."""
        def ready():
            members = dict.fromkeys(TYPES, 0)
            for mn in self.mninfo:
                for s in mn.get_node(self).quorum("dkgstatus")["session"]:
                    name, status = s["llmqType"], s["status"]
                    if name not in TYPES or status["quorumHash"] != q:
                        continue
                    if status["phase"] != phase:
                        return False
                    if received is not None and status[received] < count:
                        return False
                    members[name] += 1
            return all(m >= MEMBERS for m in members.values())
        self.wait_until(ready, timeout=60)

    def commitments_on(self, node, q):
        return self.node_quorum_commitment_types(node, q)

    def wait_commitments(self, q):
        """All three final commitments for `q` on node0 (the miner), so none is mined as null."""
        want = set(TYPES.values())  # minableCommitments names types by number
        self.wait_until(lambda: self.commitments_on(self.nodes[0], q) >= want, timeout=60)

    def block_record(self, height, commit_offset, base_hash):
        """Asserts one block's CbTx version and commitments; returns its non-null commitments."""
        block = self.nodes[0].getblock(self.nodes[0].getblockhash(height), 2)
        assert_equal(block["height"], height)
        assert_equal(block["cbTx"]["version"], CBTX_VERSION)
        names = {t: n for n, t in TYPES.items()}
        nulls, mined = set(), {}
        for tx in block["tx"]:
            if tx.get("type") != QC_TX:
                continue
            assert_equal(tx["qcTx"]["height"], height)
            c = tx["qcTx"]["commitment"]
            name = names.get(c["llmqType"])
            assert name is not None, f"block {height}: commitment of type {c['llmqType']}"
            assert_equal(c["quorumHash"], base_hash)
            if c["signersCount"] == 0 and c["quorumPublicKey"] == ZERO_KEY:
                assert name not in nulls, f"block {height}: two null {name}"
                nulls.add(name)
            else:
                assert c["signersCount"] >= 2 and c["quorumPublicKey"] != ZERO_KEY, c
                assert name not in mined, f"block {height}: two {name}"
                mined[name] = c["quorumHash"]
        want_nulls, want_mined = expected_qcs(offset_of(height), commit_offset)
        assert_equal((len(nulls), len(mined)), (want_nulls, want_mined))
        return block["hash"], mined

    def check_masternodes(self):
        for mn in self.mninfo:
            assert not check_punished(self.nodes[0], mn), mn.proTxHash
            assert not check_banned(self.nodes[0], mn), mn.proTxHash

    def check_chain_args(self):
        for node in self.nodes:
            d = node.getdeploymentinfo()["deployments"]
            assert_equal(d["dip0003"]["height"], 2)
            assert d["v19"]["active"]
            for name in ("dip0024", "v20", "mn_rr"):
                assert_equal(d[name]["height"], DEFERRED)
                assert not d[name]["active"]
            assert NEVER_ACTIVE not in d, d.get(NEVER_ACTIVE)
            for name in INACTIVE:
                assert not d[name]["active"], name

    # -- one cycle -----------------------------------------------------------------------------

    def mine_cycle(self, commit_offset, check_from):
        """Mines to the next base and one DKG cycle of all three types, commitments at
        base+commit_offset, ending at base+CYCLE_END. Asserts every block from `check_from`."""
        assert commit_offset in (M_OFFSET, S_OFFSET)
        start = time.time()
        node0 = self.nodes[0]
        skip = -node0.getblockcount() % CYCLE
        if skip:
            self.mine(skip)
        base = node0.getblockcount()
        q = node0.getbestblockhash()
        assert_equal(offset_of(base), 0)

        self.wait_phase(q, 1)
        for name in TYPES:
            self.wait_for_quorum_connections(q, MEMBERS - 1, self.mninfo, llmq_type_name=name,
                                             wait_proc=lambda: self.bump_mocktime(1))
        for phase, received, count in ((2, "receivedContributions", MEMBERS), (3, "receivedComplaints", 0),
                                       (4, "receivedJustifications", 0), (5, "receivedPrematureCommitments", MEMBERS)):
            self.mine(PHASE_BLOCKS)
            self.wait_phase(q, phase, received, count)

        # Phase 6 starts at base+10. On S node0 mines up to base+commit_offset-1 alone, so no final
        # commitment can reach it before then and those blocks carry null ones.
        to_window = commit_offset - 1 - (5 - 1) * PHASE_BLOCKS
        if commit_offset == M_OFFSET:
            self.mine(to_window)
        else:
            self.isolate_node(0)
            self.bump_mocktime(1)
            self.generate(node0, to_window, sync_fun=self.no_op)
            for mn in self.mninfo:
                self.reconnect_isolated_node(0, mn.nodeIdx)
            self.sync_tips()
        assert_equal(node0.getblockcount(), base + commit_offset - 1)
        self.wait_phase(q, 6)
        self.wait_commitments(q)
        self.mine(1)
        listed = node0.quorum("list", 1)
        for name in TYPES:
            assert_equal(listed[name], [q])
        self.mine(CYCLE_END - commit_offset)
        mining = time.time() - start

        mined = {}
        for h in range(check_from, base + CYCLE_END + 1):
            block_hash, qcs = self.block_record(h, commit_offset, q if h >= base else None)
            if qcs:
                assert_equal(h, base + commit_offset)
                mined = qcs
            if h == base:
                assert_equal(block_hash, q)
        assert_equal(set(mined), set(TYPES))
        self.check_masternodes()
        return {"base": base, "quorum_hash": q, "mined_height": base + commit_offset,
                "types": sorted(TYPES.values()), "mine_s": round(mining, 2),
                "wall_s": round(time.time() - start, 2)}

    # -- output --------------------------------------------------------------------------------

    def dump(self, path, first, last, headline):
        node0 = self.nodes[0]
        version = os.environ.get("LQ3A_DASHD_VERSION", "")
        assert version, "run through record.sh (LQ3A_DASHD_VERSION)"
        with open(path, "w", encoding="utf8") as f:
            f.write(f"# {version}\n")
            f.write(f"# dashd -regtest {' '.join(CHAIN_ARGS)}\n")
            f.write(f"# {headline}\n")
            for h in range(first, last + 1):
                f.write(node0.getblock(node0.getblockhash(h), 0) + "\n")

    # -- the run -------------------------------------------------------------------------------

    def run_test(self):
        opts = self.options
        node0 = self.nodes[0]
        os.makedirs(opts.out, exist_ok=True)
        started = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

        for _ in range(EVO_COUNT):
            self.dynamically_add_masternode(evo=True)
        self.mine(8)
        assert_equal(len(self.mninfo), EVO_COUNT)
        assert all(mn.evo for mn in self.mninfo)
        # DKGs on; InstantSend and ChainLocks off: a ChainLock on M would refuse S, and neither
        # changes a block here (no transactions, CbTx v2 has no ChainLock field).
        node0.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        node0.sporkupdate("SPORK_2_INSTANTSEND_ENABLED", SPORK_OFF)
        node0.sporkupdate("SPORK_19_CHAINLOCKS_ENABLED", SPORK_OFF)
        self.wait_for_sporks_same()
        sporks = node0.spork("show")
        self.log.info("sporks: %s" % sporks)
        assert sporks["SPORK_23_QUORUM_POSE"] > 1, "PoSe probes would need wait_for_masternode_probes"
        self.check_chain_args()

        # M.
        cycles = []
        check_from = None
        while (len(cycles) < opts.cycles) if opts.cycles else (node0.getblockcount() < opts.blocks):
            tip = node0.getblockcount()
            first = check_from is None
            base = tip + (-tip % CYCLE)
            cycle = self.mine_cycle(M_OFFSET, base if first else tip + 1)
            check_from = check_from or base
            cycles.append(cycle)
            self.log.info("M cycle %d: base %d, %.1f s" % (len(cycles), cycle["base"], cycle["wall_s"]))
        n = node0.getblockcount() if opts.cycles else opts.blocks
        m_tip_hash = node0.getbestblockhash()
        m_tip_height = node0.getblockcount()
        first_base = cycles[0]["base"]

        fork_base = -(-(n - opts.fork_depth) // CYCLE) * CYCLE
        parent = fork_base - 1
        assert first_base + CYCLE < fork_base and parent + opts.side_blocks < n, (first_base, fork_base, n)
        s_cycles = (opts.side_blocks - 1 - CYCLE_END) // CYCLE + 1
        assert s_cycles >= 1
        pad = opts.side_blocks - ((s_cycles - 1) * CYCLE + CYCLE_END + 1)
        for h in range(parent + opts.side_blocks - pad + 1, parent + opts.side_blocks + 1):
            assert not WINDOW_START <= offset_of(h) <= WINDOW_END, "S's plain tail reaches a mining window"

        self.log.info("Dump M: blocks 1-%d (tip %d)" % (n, m_tip_height))
        self.dump(os.path.join(opts.out, "chain.txt"), 1, n,
                  f"{os.path.basename(__file__)}: blocks 1-{n} of M; controller plus {EVO_COUNT} EvoNodes "
                  f"from {self.mninfo[0].nodeIdx}; DKG cycles of {', '.join(TYPES)} from base {first_base} "
                  f"every {CYCLE} blocks, null commitments at base+{WINDOW_START}..base+{M_OFFSET - 1}, "
                  f"commitments at base+{M_OFFSET}; sporks 17 on, 2 and 19 off; framework mocktime")

        # S.
        cut = node0.getblockhash(parent + 1)
        parent_hash = node0.getblockhash(parent)
        self.log.info("S: invalidateblock %s (height %d) on every node" % (cut, parent + 1))
        for node in self.nodes:
            node.invalidateblock(cut)
        self.wait_until(lambda: all(peer.getbestblockhash() == parent_hash for peer in self.nodes), timeout=600)
        side = []
        for _ in range(s_cycles):
            tip = node0.getblockcount()
            side.append(self.mine_cycle(S_OFFSET, tip + 1))
            self.log.info("S cycle %d: base %d, %.1f s" % (len(side), side[-1]["base"], side[-1]["wall_s"]))
        if pad:
            self.mine(pad)
            for h in range(parent + opts.side_blocks - pad + 1, parent + opts.side_blocks + 1):
                self.block_record(h, S_OFFSET, None)
        s_tip = node0.getblockcount()
        assert_equal(s_tip, parent + opts.side_blocks)
        self.dump(os.path.join(opts.out, "fork.txt"), parent + 1, s_tip,
                  f"fork off {parent} {parent_hash}: invalidateblock {cut} on every node, "
                  f"{opts.side_blocks} blocks: {s_cycles} DKG cycles from base {fork_base} with commitments "
                  f"at base+{S_OFFSET} (node0 isolated for base+{(5 - 1) * PHASE_BLOCKS + 1}..base+{S_OFFSET - 1}), "
                  f"then {pad} plain blocks; reconsiderblock {cut}")
        for node in self.nodes:
            node.reconsiderblock(cut)
        self.wait_until(lambda: all(peer.getbestblockhash() == m_tip_hash for peer in self.nodes), timeout=600)
        self.check_chain_args()
        listed = node0.quorum("list", 2)
        for name in TYPES:
            assert not set(listed[name]) & {c["quorum_hash"] for c in side}

        # S's keys in Core's mined-commitment index never meet M's.
        m_hashes = {c["quorum_hash"] for c in cycles}
        m_heights = {c["mined_height"] for c in cycles}
        # (mined_height is base + offset by construction; block_record proved the commitments sit there.)
        for c in side:
            assert c["quorum_hash"] not in m_hashes
            assert c["mined_height"] not in m_heights
        bases = [c["base"] for c in cycles]
        assert_equal(bases, list(range(first_base, first_base + CYCLE * len(cycles), CYCLE)))

        summary = {
            "core_tag": os.environ.get("LQ3A_CORE_TAG"),
            "dashd_version": os.environ.get("LQ3A_DASHD_VERSION"),
            "chain_args": CHAIN_ARGS,
            "types": TYPES,
            "evonodes": [mn.proTxHash for mn in self.mninfo],
            "sporks": sporks,
            "started_utc": started,
            "finished_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "m": {"blocks": n, "tip_mined": m_tip_height, "first_base": first_base,
                  "commit_offset": M_OFFSET, "cycles": cycles},
            "s": {"parent": parent, "parent_hash": parent_hash, "invalidated": cut, "blocks": opts.side_blocks,
                  "fork_base": fork_base, "commit_offset": S_OFFSET, "plain_tail": pad, "cycles": side},
        }
        with open(os.path.join(opts.out, "summary.json"), "w", encoding="utf8") as f:
            json.dump(summary, f, indent=1, sort_keys=True)
            f.write("\n")
        self.log.info("M %d blocks, %d cycles; S %d blocks, %d cycles" % (n, len(cycles), opts.side_blocks, len(side)))


if __name__ == "__main__":
    DeepWindowCapture().main()
