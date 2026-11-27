#!/usr/bin/env python3
"""LK1b-s S1e rotation capture: capture_lq3_rotation.py's chain, with every block given to a recorder.

The producer is LQ3RotationCapture unchanged (9 nodes, 8 masternodes, V19 deferred, rotated
`llmq_test_dip0024` InstantSend, locks A, B and D signed at chosen tips; see its docstring). The
recorder of capture_lk1b.py follows it from genesis: every producer block is a BLOCK op as soon as it
is mined, each of the three locks is an ISLOCK op at the tip it was signed at, and the last block's
ChainLock is a CL_RECORD op. A clean restart at the end is the one checkpoint.

The fixture has capture_lk1b.py's layout with `run` "rot", plus `lq3r`: the quorum context, the
cycles and each lock's signing tip as capture_lq3_rotation.py records them. It is the block dump
PLAN-LK1b §6 S1c's real-V test needs: NodeQuorumView on a V that connected these blocks, against a
view built from `lq3r`, at the recorded tips.
"""
from capture_lk1b import SPORK_ON, RecorderMixin
from capture_lq3_rotation import LQ3RotationCapture


class LK1bRotationCapture(RecorderMixin, LQ3RotationCapture):
    RUN = "rot"

    def add_options(self, parser):
        super().add_options(parser)
        self.add_recorder_options(parser)

    def set_test_params(self):
        super().set_test_params()
        self.rec = None
        self.scenario = "rot"

    def run_test(self):
        self.start_recorder()
        try:
            self.rec.spork("rot", "IS on", "SPORK_2_INSTANTSEND_ENABLED", SPORK_ON)
            self.rec.spork("rot", "ChainLocks on", "SPORK_19_CHAINLOCKS_ENABLED", SPORK_ON)
            self.catch_up("setup block")
            super().run_test()
        finally:
            self.rec.stop()

    def islock(self, name):
        super().islock(name)
        txid = self.locks[-1]["txid"]
        self.catch_up()
        payload = self.producer_islock(txid, wait=False)
        core_tx = self.nodes[0].getrawtransaction(txid)
        step = self.rec.islock("rot", "lock %s" % name, payload, "observer", core_tx=core_tx)
        self.locks[-1]["step"] = step["step"]

    def write_rotation_fixture(self, ctx):
        node0 = self.nodes[0]
        self.catch_up()
        tip = node0.getbestblockhash()
        self.rec.clsig("rot", "tip ChainLock", self.producer_clsig(tip), "observer")
        self.rec.restart("rot", "checkpoint: end of the run")
        self.write_fixture({"lq3r": {
            "tip_height": ctx["height"],
            "bls_scheme": "legacy",
            "quorum_list": ctx["list"],
            "quorums": ctx["quorums"],
            "cycles": self.cycles,
            "locks": self.locks,
        }})


if __name__ == "__main__":
    LK1bRotationCapture().main()
