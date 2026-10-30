#!/usr/bin/env python3
"""SB11 regtest capture across the CbTx version 1 -> 2 quorum-root transition (DIP0008), then V19
(the basic BLS scheme).

Not a test of Core: a Dash functional test (DashTestFramework, after Core's own
`feature_dip4_coinbasemerkleroots.py`) used as a capture harness, copied into the exported
`test/functional/` directory of a Dash Core tag and run there by run.sh with SB11_SCRIPT set.

The chain: DIP3 at 2 (the framework's `-dip3params=2:2`), DIP0008 at DIP8_HEIGHT, V19 at V19_HEIGHT,
DIP0024 and V20 far above (no rotation, CbTx version 1 then 2):

- two llmq_test (100) quorums mined under CbTx version 1, with no quorum root;
- DIP0008 activates: the first version-2 CbTx commits to a quorum set that already holds them;
- a ChainLock signed while those two are the active set, so its quorum is one mined under version 1;
- a third quorum, mined under version 2 and the legacy BLS scheme, and a later ChainLock;
- V19 activates (every later block is under the basic scheme) and a ChainLock after it.

Every payload is the exact wire bytes Core sent to a raw P2P peer, or Core's RPC answer:

- `headers`: every header from genesis to the tip (`getblockheader <hash> false`);
- `diffs.step`: `getmnlistd(h - 1, h)` for every height h in 1..=tip;
- `diffs.from_genesis`: `getmnlistd(null, h)` for every quorum's work block, the blocks before
  DIP0008 and V19, DIP0008's first block and the tip;
- `chainlocks`: `getbestchainlock` at each capture point (its `hex` is the clsig payload), with Core's
  `verifychainlock`;
- `quorums`: `quorum info` of every listed quorum, members included.
"""
import gzip
import hashlib
import json
import os
from datetime import datetime, timezone

from capture_lq3a import raw_message_class
from test_framework import p2p
from test_framework.messages import msg_getmnlistd
from test_framework.p2p import P2PInterface
from test_framework.test_framework import DashTestFramework
from test_framework.util import assert_equal

DIP8_HEIGHT = 300
V19_HEIGHT = 340
LATE = 900  # DIP0024 and V20: above the whole chain
WORK_DIFF_DEPTH = 8
TYPES = {"llmq_test": 100, "llmq_test_instantsend": 104, "llmq_test_platform": 106}

for _command in (b"mnlistdiff",):
    p2p.MESSAGEMAP[_command] = raw_message_class(_command)


class RawFetcher(P2PInterface):
    def __init__(self):
        super().__init__()
        self.reply = None

    def on_mnlistdiff(self, message):
        self.reply = message.payload

    def ask(self, message, timeout=60):
        with p2p.p2p_lock:
            self.reply = None
        self.send_message(message)
        self.wait_until(lambda: self.reply is not None, timeout=timeout)
        with p2p.p2p_lock:
            return self.reply.hex()


def wire(display_hex):
    return int(display_hex, 16)


class SB11TransitionsCapture(DashTestFramework):
    def add_options(self, parser):
        self.add_wallet_options(parser)
        parser.add_argument("--out", dest="out", required=True)

    def set_test_params(self):
        self.chain_args = [f"-testactivationheight=v19@{V19_HEIGHT}",
                           f"-testactivationheight=dip0008@{DIP8_HEIGHT}",
                           f"-testactivationheight=dip0024@{LATE}",
                           "-vbparams=testdummy:999999999999:999999999999"]
        # Masternodes registered before V19 carry legacy BLS keys (`bls generate true`).
        node_args = self.chain_args + ["-deprecatedrpc=legacy_mn"]
        self.set_dash_test_params(4, 3, extra_args=[list(node_args) for _ in range(4)])
        self.delay_v20_and_mn_rr(height=LATE)

    def cbtx_version(self):
        node = self.nodes[0]
        return node.getblock(node.getbestblockhash(), 2)["tx"][0]["cbTx"]["version"]

    def chainlock(self, name):
        node = self.nodes[0]
        self.bump_mocktime(1)
        blk = self.generate(node, 1, sync_fun=self.sync_blocks)[0]
        self.wait_for_chainlocked_block_all_nodes(blk, timeout=60)
        cl = node.getbestchainlock()
        assert_equal(cl["blockhash"], blk)
        cl["core_verify"] = node.verifychainlock(cl["blockhash"], cl["signature"], cl["height"])
        assert cl["core_verify"] is True, cl
        cl["name"] = name
        cl["active"] = node.quorum("list", 2)["llmq_test"]
        self.chainlocks.append(cl)

    def run_test(self):
        node = self.nodes[0]
        self.chainlocks = []
        node.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        self.wait_for_sporks_same()

        self.log.info("Two llmq_test quorums under CbTx version 1")
        early = [self.mine_quorum(), self.mine_quorum()]
        assert_equal(self.cbtx_version(), 1)
        assert node.getblockcount() < DIP8_HEIGHT - 2, node.getblockcount()

        self.log.info("DIP0008 at %d" % DIP8_HEIGHT)
        node.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 4070908800)
        self.wait_for_sporks_same()
        self.generate(node, DIP8_HEIGHT - node.getblockcount() - 2, sync_fun=self.sync_blocks)
        self.chainlock("before")
        assert_equal(node.getblockcount(), DIP8_HEIGHT - 1)
        assert_equal(self.cbtx_version(), 1)
        self.chainlock("first rooted")
        assert_equal(node.getblockcount(), DIP8_HEIGHT)
        assert_equal(self.cbtx_version(), 2)
        self.chainlock("later rooted")
        assert_equal(sorted(self.chainlocks[-1]["active"]), sorted(early))

        self.log.info("A quorum under version 2, then a later ChainLock")
        node.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        self.wait_for_sporks_same()
        late = self.mine_quorum()
        self.chainlock("after")
        assert late in self.chainlocks[-1]["active"]
        mined = node.getblockheader(node.quorum("info", 100, late)["minedBlock"])["height"]
        assert mined < V19_HEIGHT, mined

        self.log.info("V19 at %d" % V19_HEIGHT)
        node.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 4070908800)
        self.wait_for_sporks_same()
        self.generate(node, V19_HEIGHT - node.getblockcount() - 1, sync_fun=self.sync_blocks)
        self.chainlock("after v19")
        assert_equal(node.getblockcount(), V19_HEIGHT)
        assert late in self.chainlocks[-1]["active"]

        self.write_fixture(early, late)

    def write_fixture(self, early, late):
        node = self.nodes[0]
        tip = node.getblockcount()
        hashes = [node.getblockhash(h) for h in range(tip + 1)]
        headers = [node.getblockheader(h, False) for h in hashes]
        listing = node.quorum("list", 10)
        quorums = {}
        for tname, ltype in TYPES.items():
            quorums[str(ltype)] = []
            for qhash in listing.get(tname, []):
                info = node.quorum("info", ltype, qhash)
                info["minedHeight"] = node.getblockheader(info["minedBlock"])["height"]
                quorums[str(ltype)].append(info)
        fetcher = node.add_p2p_connection(RawFetcher())
        step = [{"base_height": h - 1, "height": h,
                 "payload": fetcher.ask(msg_getmnlistd(wire(hashes[h - 1]), wire(hashes[h])))}
                for h in range(1, tip + 1)]
        targets = {tip, V19_HEIGHT - 1, DIP8_HEIGHT - 1, DIP8_HEIGHT}
        for qs in quorums.values():
            for q in qs:
                targets.add(q["height"] - WORK_DIFF_DEPTH)
        from_genesis = [{"base_height": None, "height": h,
                         "payload": fetcher.ask(msg_getmnlistd(0, wire(hashes[h])))} for h in sorted(targets)]
        node.disconnect_p2ps()
        with open(os.environ["DASHD"], "rb") as f:
            dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
        fixture = {
            "provenance": {
                "dashd_version": os.environ.get("LQ3A_DASHD_VERSION", ""),
                "dashd_sha256": dashd_sha256,
                "core_tag": os.environ.get("LQ3A_CORE_TAG", ""),
                "tool": "contrib/node/sb11-captures (capture_sb11_transitions.py)",
                "chain_args": self.chain_args + ["-dip3params=2:2", f"-testactivationheight=v20@{LATE}",
                                                 f"-testactivationheight=mn_rr@{LATE}"],
                "deployments": node.getdeploymentinfo()["deployments"],
                "captured_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "v19_height": V19_HEIGHT,
                "dip8_height": DIP8_HEIGHT,
                "early_quorums": early,
                "late_quorum": late,
                "llmq_size": self.llmq_size,
                "llmq_threshold": self.llmq_threshold,
            },
            "network": "regtest",
            "tip_height": tip,
            "block_hashes": hashes,
            "headers": headers,
            "quorum_list": listing,
            "quorums": quorums,
            "diffs": {"step": step, "from_genesis": from_genesis},
            "chainlocks": self.chainlocks,
        }
        with gzip.open(self.options.out, "wt") as f:
            json.dump(fixture, f, indent=1, sort_keys=True)
            f.write("\n")
        self.log.info("fixture written to %s" % self.options.out)


if __name__ == "__main__":
    SB11TransitionsCapture().main()
