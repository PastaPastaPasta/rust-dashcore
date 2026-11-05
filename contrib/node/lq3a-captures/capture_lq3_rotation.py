#!/usr/bin/env python3
"""O2a-LQ3 follow-up capture (BL-218): rotated InstantSend locks and legacy-scheme signatures.

The same harness as capture_lq3a.py (raw observers, one GETDATA per inv), on another chain:

- V19 is deferred past the end of the run (`-testactivationheight=v19@2000`, with V20 and MN_RR at the
  same height), so every masternode key, quorum and signature is in the legacy BLS scheme, as on
  testnet and mainnet between DIP0024 and V19.
- InstantSend uses the rotated `llmq_test_dip0024` (103), regtest's default
  `llmqTypeDIP0024InstantSend` (no `-llmqtestinstantsenddip0024` override).
- Two rotation cycles are mined, at F and C = F + 24. Three locks are signed:
  - A at F + 20, after F's quorums are eligible;
  - B at C, while C's DKG runs: the signer selects 8 blocks below its tip, where C's quorums are
    not mined yet, so F's quorum signs it;
  - D at C + 20, after C's quorums are eligible.
  A lock's cycle hash is the cycle of the quorum that signed it (`instantsend/signing.cpp` v24:460-462),
  so A and B carry F and D carries C; `provenance.locks` records each lock's cycle as Core reports it
  (`getislocks`). The run ends at C + 23 (three ChainLocked blocks after C + 20), inside C's cycle.
"""
import hashlib
import json
import os
from datetime import datetime, timezone

from capture_lq3a import KEEP_OLD_CONNECTIONS, LLMQ_SIZE, LLMQ_THRESHOLD, LQ3aCapture
from test_framework.util import assert_equal, softfork_active

V19_HEIGHT = 2000
CYCLE = 24  # llmq_test_dip0024's dkgInterval
ROTATED_IS = 103


class LQ3RotationCapture(LQ3aCapture):
    TYPES = {"llmq_test": 100, "llmq_test_dip0024": ROTATED_IS}

    def set_test_params(self):
        args = [f"-testactivationheight=v19@{V19_HEIGHT}", "-deprecatedrpc=legacy_mn",
                "-vbparams=testdummy:999999999999:999999999999"]
        self.set_dash_test_params(9, 8, [list(args) for _ in range(9)])
        self.set_dash_llmq_test_params(LLMQ_SIZE, LLMQ_THRESHOLD)
        self.delay_v20_and_mn_rr(height=V19_HEIGHT)
        self.extra_args[0].append("-watchquorums=1")
        self.supports_cli = False
        self.phase_hook = None

    def wait_for_quorum_phase(self, quorum_hash, phase, *args, **kwargs):
        # A one-shot hook at the first DKG phase wait of mine_cycle_quorum: the tip is the cycle's
        # first block, and the cycle's quorums are not mined yet.
        hook, self.phase_hook = self.phase_hook, None
        if hook is not None:
            hook()
        return super().wait_for_quorum_phase(quorum_hash, phase, *args, **kwargs)

    def islock(self, name):
        node0 = self.nodes[0]
        tip = node0.getblockcount()
        base = self.mark()
        txid = node0.sendtoaddress(node0.getnewaddress(), 1)
        self.wait_for_instantlock(txid)
        self.drain("islock %s" % name, base)
        cycle_hash = node0.getislocks([txid])[0]["cycleHash"]
        self.locks.append({"name": name, "txid": txid, "signed_at_tip": tip, "cycle_hash": cycle_hash,
                           "cycle_height": node0.getblockheader(cycle_hash)["height"]})

    def run_test(self):
        node0 = self.nodes[0]
        node0.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        self.wait_for_sporks_same()
        assert not softfork_active(node0, "v19")
        self.locks = []

        self.log.info("Two llmq_test quorums for ChainLocks and recovered sigs")
        self.mine_quorum()
        self.mine_quorum()
        self.generate(node0, 1)
        self.wait_for_chainlocked_block_all_nodes(node0.getbestblockhash())

        self.log.info("Rotation cycle F")
        q_f = self.mine_cycle_quorum()
        f = q_f[0]["height"]
        assert_equal(f % CYCLE, 0)
        assert_equal(node0.getblockcount(), f + 20)

        self.attach_observers()
        self.bump_mocktime(5)

        self.log.info("Recovered signatures on llmq_test, legacy scheme")
        for n in range(1, 5):
            base = self.mark()
            self.get_recovered_sig("%064x" % (0x3000 + n), "%064x" % (0x4000 + n), llmq_type=100)
            self.drain("recsig %d" % n, base)

        self.islock("A")
        self.log.info("Rotation cycle C, with lock B signed at its first block")
        self.phase_hook = lambda: self.islock("B")
        q_c = self.mine_cycle_quorum()
        assert self.phase_hook is None, "the lock-B hook did not run"
        c = q_c[0]["height"]
        assert_equal(c, f + CYCLE)
        assert_equal(self.locks[1]["signed_at_tip"], c)
        assert_equal([lock["cycle_height"] for lock in self.locks[:2]], [f, f])
        assert_equal(node0.getblockcount(), c + 20)
        self.islock("D")
        assert_equal(self.locks[2]["cycle_height"], c)

        self.log.info("ChainLocks")
        for n in range(1, 4):
            base = self.mark()
            self.bump_mocktime(1)
            blk = self.generate(node0, 1, sync_fun=self.sync_blocks)[0]
            self.wait_for_chainlocked_block_all_nodes(blk, timeout=60)
            self.drain("chainlock %d" % n, base)
        assert_equal(node0.getblockcount(), c + 23)
        assert not softfork_active(node0, "v19")
        self.cycles = {"F": f, "C": c}

        ctx = self.quorum_context()
        self.write_rotation_fixture(ctx)

    def write_rotation_fixture(self, ctx):
        msgs = self.merge()
        for m in msgs:
            m["core_verify"] = self.core_verdict(m)
        counts = {c: sum(1 for m in msgs if m["command"] == c) for c in ("qsigrec", "isdlock", "clsig")}
        self.log.info("merged message counts: %s" % counts)
        assert counts["qsigrec"] >= 4 and counts["isdlock"] >= 3 and counts["clsig"] >= 3, counts
        with open(os.environ["DASHD"], "rb") as f:
            dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
        fixture = {
            "provenance": {
                "dashd_version": os.environ.get("LQ3A_DASHD_VERSION", ""),
                "dashd_sha256": dashd_sha256,
                "v19_deployment": self.nodes[0].getdeploymentinfo()["deployments"]["v19"],
                "core_tag": os.environ.get("LQ3A_CORE_TAG", ""),
                "tool": "contrib/node/lq3a-captures (capture_lq3_rotation.py)",
                "extra_args": [list(a) for a in self.extra_args],
                "captured_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "observers": [o.name for o in self.observers],
                "llmq_size": LLMQ_SIZE,
                "llmq_threshold": LLMQ_THRESHOLD,
                "keep_old_connections": KEEP_OLD_CONNECTIONS,
                "cycles": self.cycles,
                "locks": self.locks,
                "unsolicited_payloads_not_in_fixture": {o.name: len(o.unsolicited) for o in self.observers},
            },
            "network": "regtest",
            "tip_height": ctx["height"],
            "bls_scheme": "legacy",
            "quorum_list": ctx["list"],
            "quorums": ctx["quorums"],
            "messages": msgs,
        }
        with open(self.options.out, "w") as f:
            json.dump(fixture, f, indent=2, sort_keys=True)
            f.write("\n")
        self.log.info("fixture written to %s" % self.options.out)


if __name__ == "__main__":
    LQ3RotationCapture().main()
