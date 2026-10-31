#!/usr/bin/env python3
"""SB11 regtest capture: a masternode chain with Core's own mnlistdiff / qrinfo replies, locks and verdicts.

Not a test of Core: a Dash functional test (DashTestFramework) used as a capture harness, copied into the
exported `test/functional/` directory of a Dash Core tag next to `capture_lq3a.py` and run there by run.sh.

The chain (basic BLS scheme, V20 and MN_RR at height 100):

- two llmq_test (100) quorums, the ChainLock type, then one ChainLocked block;
- rotation cycle F of llmq_test_dip0024 (103), regtest's DIP0024 InstantSend type, and lock A;
- rotation cycle C = F + 24, with lock B signed at C's first block (F's quorum signs it) and lock D after
  C's quorums are eligible;
- three ChainLocked blocks.

Every payload below is the exact wire bytes Core sent, fetched by a raw P2P peer:

- `headers`: every block header from genesis to the tip (`getblockheader <hash> false`);
- `diffs.step`: `getmnlistd(h - 1, h)` for every height h in 1..=tip;
- `diffs.from_genesis`: `getmnlistd(null, h)` for the tip, every cycle base and every quorum's work block;
- `qrinfos`: `getqrinfo` at the tip and at each lock's signing tip, with no base and with a base hash;
- `messages`: the isdlock and clsig payloads, by inv hash (the LQ3a observers), each with Core's verdict
  (`verifyislock` / `verifychainlock`);
- `quorums`: `quorum info` of every listed quorum, members included, so a member computation can be
  compared with Core's.
"""
import gzip
import hashlib
import json
import os
from datetime import datetime, timezone

from capture_lq3a import LLMQ_SIZE, LLMQ_THRESHOLD, LQ3aCapture, display, raw_message_class
from test_framework import p2p
from test_framework.messages import msg_getmnlistd, msg_getqrinfo
from test_framework.p2p import P2PInterface
from test_framework.util import assert_equal, softfork_active

CYCLE = 24  # llmq_test_dip0024's dkgInterval
WORK_DIFF_DEPTH = 8  # llmq/utils.h WORK_DIFF_DEPTH
TYPES = {"llmq_test": 100, "llmq_test_instantsend": 104, "llmq_test_dip0024": 103, "llmq_test_platform": 106}

for _command in (b"mnlistdiff", b"qrinfo"):
    p2p.MESSAGEMAP[_command] = raw_message_class(_command)


class RawFetcher(P2PInterface):
    """Asks one getmnlistd / getqrinfo at a time and keeps the reply's exact bytes."""

    def __init__(self):
        super().__init__()
        self.reply = None

    def on_mnlistdiff(self, message):
        self.reply = ("mnlistdiff", message.payload)

    def on_qrinfo(self, message):
        self.reply = ("qrinfo", message.payload)

    def ask(self, message, expect, timeout=60):
        with p2p.p2p_lock:
            self.reply = None
        self.send_message(message)
        self.wait_until(lambda: self.reply is not None, timeout=timeout)
        with p2p.p2p_lock:
            command, payload = self.reply
        assert_equal(command, expect)
        return payload.hex()


def wire(display_hex):
    """Display hex -> the uint256 integer the framework's messages serialize."""
    return int(display_hex, 16)


class SB11Capture(LQ3aCapture):
    TYPES = TYPES

    def set_test_params(self):
        args = ["-vbparams=testdummy:999999999999:999999999999"]
        self.set_dash_test_params(9, 8, [list(args) for _ in range(9)])
        self.set_dash_llmq_test_params(LLMQ_SIZE, LLMQ_THRESHOLD)
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
        info = node0.getislocks([txid])[0]
        self.locks.append({"name": name, "txid": txid, "signed_at_tip": tip,
                           "signed_at_tip_hash": node0.getblockhash(tip),
                           "id": info["id"], "cycle_hash": info["cycleHash"], "signature": info["signature"],
                           "cycle_height": node0.getblockheader(info["cycleHash"])["height"]})

    def run_test(self):
        node0 = self.nodes[0]
        node0.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        self.wait_for_sporks_same()
        assert softfork_active(node0, "v19")
        self.locks = []

        self.log.info("Two llmq_test quorums for ChainLocks")
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

        self.islock("A")
        self.log.info("Rotation cycle C, with lock B signed at its first block")
        self.phase_hook = lambda: self.islock("B")
        q_c = self.mine_cycle_quorum()
        assert self.phase_hook is None, "the lock-B hook did not run"
        c = q_c[0]["height"]
        assert_equal(c, f + CYCLE)
        assert_equal(self.locks[1]["signed_at_tip"], c)
        assert_equal([lock["cycle_height"] for lock in self.locks[:2]], [f, f])
        self.islock("D")
        assert_equal(self.locks[2]["cycle_height"], c)

        self.log.info("ChainLocks")
        for n in range(1, 4):
            base = self.mark()
            self.bump_mocktime(1)
            blk = self.generate(node0, 1, sync_fun=self.sync_blocks)[0]
            self.wait_for_chainlocked_block_all_nodes(blk, timeout=60)
            self.drain("chainlock %d" % n, base)
        self.cycles = {"F": f, "C": c}

        ctx = self.quorum_context()
        self.write_sb11_fixture(ctx)

    # -- context ---------------------------------------------------------------------------------

    def quorum_context(self):
        node = self.nodes[0]
        listing = node.quorum("list", 20)
        infos = {}
        for tname, ltype in self.TYPES.items():
            infos[str(ltype)] = []
            for qhash in listing.get(tname, []):
                info = node.quorum("info", ltype, qhash)
                assert_equal(info["quorumHash"], qhash)
                infos[str(ltype)].append(info)
        return {"height": node.getblockcount(), "list": listing, "quorums": infos}

    # -- captures --------------------------------------------------------------------------------

    def capture_chain(self, ctx):
        node = self.nodes[0]
        tip = node.getblockcount()
        hashes = [node.getblockhash(h) for h in range(tip + 1)]
        headers = [node.getblockheader(h, False) for h in hashes]
        fetcher = node.add_p2p_connection(RawFetcher())

        step = []
        for h in range(1, tip + 1):
            step.append({"base_height": h - 1, "height": h,
                         "payload": fetcher.ask(msg_getmnlistd(wire(hashes[h - 1]), wire(hashes[h])), "mnlistdiff")})

        targets = {tip}
        targets.update(h for h in range(0, tip + 1, CYCLE) if h > 0)
        for quorums in ctx["quorums"].values():
            for q in quorums:
                if q["height"] >= WORK_DIFF_DEPTH:
                    targets.add(q["height"] - WORK_DIFF_DEPTH)
                targets.add(q["height"])
        from_genesis = []
        for h in sorted(targets):
            from_genesis.append({"base_height": None, "height": h,
                                 "payload": fetcher.ask(msg_getmnlistd(0, wire(hashes[h])), "mnlistdiff")})

        requests = [("tip", tip)] + [("lock %s" % lock["name"], lock["signed_at_tip"]) for lock in self.locks]
        qrinfos = []
        for name, at in requests:
            for bases in ([], [self.cycles["F"] - CYCLE]):
                msg = msg_getqrinfo([wire(hashes[b]) for b in bases], wire(hashes[at]), True)
                qrinfos.append({"name": name, "request_height": at, "base_heights": bases, "extra_share": True,
                                "payload": fetcher.ask(msg, "qrinfo")})
        node.disconnect_p2ps()
        return hashes, headers, {"step": step, "from_genesis": from_genesis}, qrinfos

    def core_verdict(self, m):
        if m["command"] != "isdlock":
            return super().core_verdict(m)
        raw = bytes.fromhex(m["payload"])
        # instantsend/lock.h: nVersion, inputs, txid, cycleHash, sig
        assert_equal(raw[0], 1)
        n = raw[1]
        assert n < 0xfd
        at = 2 + 36 * n
        txid, cycle_hash, sig = display(raw[at:at + 32]), display(raw[at + 32:at + 64]), raw[at + 64:].hex()
        assert_equal(len(raw), at + 64 + 96)
        lock = next(lock for lock in self.locks if lock["txid"] == txid)
        assert_equal(lock["cycle_hash"], cycle_hash)
        assert_equal(lock["signature"], sig)
        m["fields"] = {"txid": txid, "cycle_hash": cycle_hash, "id": lock["id"], "sig": sig, "lock": lock["name"]}
        verdict = self.nodes[0].verifyislock(lock["id"], txid, sig)
        assert verdict is True, ("verifyislock said %r for %s" % (verdict, m))
        return True

    def write_sb11_fixture(self, ctx):
        msgs = [m for m in self.merge() if m["command"] in ("isdlock", "clsig")]
        for m in msgs:
            m["core_verify"] = self.core_verdict(m)
        counts = {c: sum(1 for m in msgs if m["command"] == c) for c in ("isdlock", "clsig")}
        self.log.info("merged message counts: %s" % counts)
        assert counts["isdlock"] >= 3 and counts["clsig"] >= 3, counts
        hashes, headers, diffs, qrinfos = self.capture_chain(ctx)
        assert_equal(self.nodes[0].getblockcount(), ctx["height"])
        with open(os.environ["DASHD"], "rb") as f:
            dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
        chain_args = list(self.extra_args[0]) + [
            "-dip3params=2:2",
            f"-testactivationheight=v20@{self.v20_height}",
            f"-testactivationheight=mn_rr@{self.mn_rr_height}",
        ]
        fixture = {
            "provenance": {
                "dashd_version": os.environ.get("LQ3A_DASHD_VERSION", ""),
                "dashd_sha256": dashd_sha256,
                "core_tag": os.environ.get("LQ3A_CORE_TAG", ""),
                "tool": "contrib/node/sb11-captures (capture_sb11.py)",
                "chain_args": chain_args,
                "deployments": self.nodes[0].getdeploymentinfo()["deployments"],
                "captured_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "observers": [o.name for o in self.observers],
                "llmq_size": LLMQ_SIZE,
                "llmq_threshold": LLMQ_THRESHOLD,
                "cycles": self.cycles,
                "locks": self.locks,
                "best_chainlock": self.nodes[0].getbestchainlock(),
            },
            "network": "regtest",
            "tip_height": ctx["height"],
            "block_hashes": hashes,
            "headers": headers,
            "quorum_list": ctx["list"],
            "quorums": ctx["quorums"],
            "diffs": diffs,
            "qrinfos": qrinfos,
            "messages": msgs,
        }
        with gzip.open(self.options.out, "wt") as f:
            json.dump(fixture, f, indent=1, sort_keys=True)
            f.write("\n")
        self.log.info("fixture written to %s" % self.options.out)


if __name__ == "__main__":
    SB11Capture().main()
