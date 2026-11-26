#!/usr/bin/env python3
"""LK1b-s S1e main capture: Dash Core's InstantSend DB under direct ingestion, op by op.

Not a test of Core: a Dash functional test (DashTestFramework) used as a capture harness, copied into the
exported `test/functional/` of the O8-patched v24.0.0 tree and run there by run.sh.

Two roles:

- The producer is the framework's network: node0 (controller, miner) and four masternodes, 4-of-3
  `llmq_test` (ChainLocks) and `llmq_test_instantsend` (InstantSend, through
  `-llmqtestinstantsenddip0024`), spork 2 and spork 19 on. It mines blocks and signs locks as usual.
- The recorder is one more dashd, started after the producer's setup with `-statedigestlog` and
  `-recordstatedecisions`, and kept out of the framework's node list. It has no peers but the
  harness's own P2P connection, so nothing reaches it except what the harness delivers, one op at a
  time. It is Core's counterpart of NodeSut: every op the harness gives it is one op of the fixture.

Ops (README "Fixture"): BLOCK (submitblock of the block bytes), CL_RECORD (a `clsig` sent by inv/getdata),
ISLOCK (an `isdlock` sent the same way), SPORK (`sporkupdate` with the controller's key), RESTART
(clean stop or SIGKILL, then a start). After each op the harness waits until the recorder is quiet and
takes the digest and decision lines it wrote: one fixture step per op, numbered from 1. Payloads come
from the producer through a P2PInterface observer on node0 (getdata of a block, a clsig or an isdlock
by its inv hash), or from the harness itself for the blocks, ChainLocks and the one lock it builds.

Core writes a lock to its DB only when it has the transaction (`net_instantsend.cpp` v24:369-398).
The recorder runs with `-txindex` and `-persistmempool=0`; an ISLOCK op of an unmined transaction
first puts the transaction in the recorder's mempool (`core_tx`, not an op of the Rust side). A
restart then empties the mempool before every conflicting block, so that no mempool removal reaches
`CInstantSendManager::TransactionIsRemoved` (`instantsend.cpp` v24:132-147); the capture fails if one
does (its log line is checked).

Checkpoints are RESTART ops: the recorder is stopped (or killed), its `llmq/isdb` is copied and dumped
with isdump, and it is started again. One run writes one dump file per checkpoint, named by its step
number, and the fixture carries the same rows.

Scenarios (PLAN-LK1b §6 S1e): (a) lock, mine, ChainLock; (b) a conflicting block without a ChainLock;
(c) a ChainLocked conflicting block with a chained child; (c') the A/B overlapping index; (d) (c) with a
bad payee; (f) a reorg across a mined lock; (g) SIGKILL between ops; (e) a conflicting block in IBD;
(h) archive pruning at h-100. Scenario (i) (R18, MN4-08) is not captured; see the README.
"""
import hashlib
import json
import os
import shutil
import subprocess
import time
from datetime import datetime, timezone
from decimal import Decimal
from io import BytesIO
from pathlib import Path

from capture_lq3a import LLMQ_SIZE, LLMQ_THRESHOLD, raw_message_class
from test_framework import p2p
from test_framework.blocktools import create_block, create_coinbase
from test_framework.messages import (
    COIN,
    CBlock,
    CInv,
    CTxOut,
    hash256,
    msg_clsig,
    msg_getdata,
    msg_inv,
    msg_isdlock,
    ser_compact_size,
    ser_string,
    tx_from_hex,
    uint256_from_str,
)
from test_framework.authproxy import JSONRPCException
from test_framework.p2p import P2PInterface
from test_framework.script import OP_TRUE, CScript
from test_framework.test_framework import DashTestFramework
from test_framework.util import assert_equal, force_finish_mnsync, initialize_datadir

SCHEMA = "lk1b-is/1"
MSG_BLOCK = 2
MSG_CLSIG = 29
MSG_ISDLOCK = 31
DKG_INTERVAL = 24
# Heights (mod 24) at which some regtest LLMQ type may mine a commitment: llmq_test, llmq_test_instantsend
# and llmq_test_platform mine in 10..18, llmq_test_dip0024 in 12..20 (llmq/params.h v24:146-260).
DKG_MINING_WINDOW = range(10, 21)
IS_LLMQ_TYPE = 104
CL_LLMQ_TYPE = 100
SPORK_IDS = {"SPORK_2_INSTANTSEND_ENABLED": 10001, "SPORK_19_CHAINLOCKS_ENABLED": 10018}
SPORK_ON = 0
SPORK_OFF = 4070908800
IS_LOCKS_TABLES = ("is_i", "is_m", "is_a1", "is_a2")
# Log lines that mean Core changed the IS DB, or the chain, outside the ops (README "Core-only paths").
FORBIDDEN_LOG = (
    "was removed from mempool",       # TransactionIsRemoved -> RemoveConflictingLock (instantsend.cpp:144-146)
    "NetInstantSend::ResolveBlockConflicts -- invalidating block",  # net_instantsend.cpp:616
    "conflicts with islock",          # ResolveBlockConflicts / RemoveMempoolConflictsForLock found a conflict
)


def display(wire):
    return bytes(reversed(wire)).hex()


def is_locks_digest(rows):
    """O2b §2.6's `is_locks`: SHA-256 over the sorted `is_i`, `is_m`, `is_a1` and `is_a2` keys, each as a
    u32 LE length and the key bytes. Must match `is_locks_digest` in dashd-node/tests/lk1b_is/mod.rs."""
    h = hashlib.sha256()
    for key in sorted(bytes.fromhex(r[0]) for r in rows if r[2] in IS_LOCKS_TABLES):
        h.update(len(key).to_bytes(4, "little"))
        h.update(key)
    return h.hexdigest()


def block_hash(raw):
    block = CBlock()
    block.deserialize(BytesIO(raw))
    block.rehash()
    return block.hash, block


class Source(P2PInterface):
    """An observer on node0. It fetches every clsig and isdlock it is announced at once, because
    Core serves only its best ChainLock (chainlock/chainlock.cpp v24:140-149), and fetches a block
    by one GETDATA."""

    def __init__(self):
        super().__init__()
        self.seen = {}             # (inv type, hash int) -> payload, b"" for notfound
        self.request = None        # (inv type, hash int) in flight
        self.answer = None

    def on_inv(self, message):
        want = [CInv(inv.type, inv.hash) for inv in message.inv
                if inv.type in (MSG_CLSIG, MSG_ISDLOCK) and (inv.type, inv.hash) not in self.seen]
        if want:
            self.send_message(msg_getdata(want))

    def _answer(self, inv_type, h, payload):
        if inv_type in (MSG_CLSIG, MSG_ISDLOCK):
            self.seen[(inv_type, h)] = payload
        if self.request == (inv_type, h):
            self.answer = payload
            self.request = None

    def on_block(self, message):
        message.block.rehash()
        self._answer(MSG_BLOCK, message.block.sha256, message.block.serialize())

    def on_clsig(self, message):
        self._answer(MSG_CLSIG, uint256_from_str(hash256(message.payload)), message.payload)

    def on_isdlock(self, message):
        self._answer(MSG_ISDLOCK, uint256_from_str(hash256(message.payload)), message.payload)

    def on_notfound(self, message):
        for inv in message.vec:
            self._answer(inv.type, inv.hash, b"")

    def fetch(self, inv_type, h, timeout=60):
        with p2p.p2p_lock:
            self.request, self.answer = (inv_type, h), None
        self.send_message(msg_getdata([CInv(inv_type, h)]))
        self.wait_until(lambda: self.answer is not None, timeout=timeout)
        with p2p.p2p_lock:
            payload, self.answer = self.answer, None
        assert payload, "notfound for %d %064x" % (inv_type, h)
        return payload


class Feeder(P2PInterface):
    """The recorder's only peer: announces one clsig or isdlock by inv and answers the GETDATA."""

    def __init__(self):
        super().__init__()
        self.objects = {}

    def offer(self, command, inv_type, payload):
        h = uint256_from_str(hash256(payload))
        self.objects[h] = (command, payload)
        self.send_message(msg_inv([CInv(inv_type, h)]))
        return h

    def on_getdata(self, message):
        for inv in message.inv:
            if inv.hash in self.objects:
                command, payload = self.objects[inv.hash]
                msg = raw_message_class(command)()
                msg.payload = payload
                self.send_message(msg)


class Recorder:
    """The recorder dashd, its feeder and the steps it produced."""

    def __init__(self, test, node, args, isdump, out_dir):
        self.test = test
        self.node = node
        self.args = list(args)
        self.isdump = isdump
        self.ckpt_dir = Path(out_dir)
        self.ckpt_dir.mkdir(parents=True, exist_ok=True)
        self.steps = []
        self.offsets = {"digest": 0, "decisions": 0, "log": 0}
        self.run_args = []              # the extra args of the current start
        self.feeder = None
        # A ChainLocked block that fails validation: the ChainLock scheduler retries it every 5 s
        # of real time (chainlock/handler.cpp v24:63-71, 296), so its attempts after the first are
        # timer output, kept apart in the step's `periodic` lines.
        self.periodic = None
        self.periodic_first = set()

    # -- process ---------------------------------------------------------------------------------

    def start(self, extra=()):
        self.run_args = list(extra)
        self.node.mocktime = self.test.mocktime
        self.node.start(self.args + self.run_args)
        self.node.wait_for_rpc_connection()
        force_finish_mnsync(self.node)
        self.feeder = self.node.add_p2p_connection(Feeder())

    def files(self):
        chain = self.node.chain_path
        return {"digest": chain / "digest.jsonl", "decisions": chain / "decisions.jsonl",
                "log": chain / "debug.log"}

    def read_new(self, what):
        path = self.files()[what]
        if not path.exists():
            return []
        data = path.read_bytes()
        if len(data) < self.offsets[what]:
            # A reopened file that started over: read it from its start.
            self.offsets[what] = 0
        end = data.rfind(b"\n", self.offsets[what]) + 1
        if end <= self.offsets[what]:
            return []
        chunk = data[self.offsets[what]:end]
        self.offsets[what] = end
        return chunk.decode().splitlines()

    def check_log(self):
        for line in self.read_new("log"):
            for bad in FORBIDDEN_LOG:
                assert bad not in line, "Core-only IS path on the recorder: %s" % line

    def wait_log(self, needles, timeout=60):
        """Wait until the log has a line holding every string of one entry of `needles`; return it."""
        path = self.files()["log"]
        start = self.offsets["log"]
        deadline = time.time() + timeout * self.test.options.timeout_factor
        while time.time() < deadline:
            text = path.read_bytes()[start:].decode(errors="replace")
            for line in text.splitlines():
                for i, group in enumerate(needles):
                    if all(n in line for n in group):
                        return i, line
            time.sleep(0.1)
        raise AssertionError("recorder log never showed any of %r" % (needles,))

    def settle(self, rounds=4, timeout=60):
        """Quiet: the validation queue drained and tip, ChainLock and record files unchanged for a while."""
        files = self.files()
        last, calm = None, 0
        deadline = time.time() + timeout * self.test.options.timeout_factor
        while calm < rounds:
            assert time.time() < deadline, "recorder never went quiet"
            self.node.syncwithvalidationinterfacequeue()
            try:
                best_cl = self.node.getbestchainlock()["blockhash"]
            except Exception:
                best_cl = None
            state = (self.node.getbestblockhash(), best_cl, json.dumps(self.node.getchaintips(), default=str, sort_keys=True),
                     files["digest"].stat().st_size if files["digest"].exists() else 0,
                     files["decisions"].stat().st_size if files["decisions"].exists() else 0)
            calm = calm + 1 if state == last else 0
            last = state
            time.sleep(0.25)

    # -- steps -----------------------------------------------------------------------------------

    def split_periodic(self, lines, kind):
        """`lines` without the timer's retries of `self.periodic` (all but its first `kind` record), and
        those. Lines after a new `H` come from a restarted recorder, which holds no ChainLock, so none
        of them is a timer retry."""
        keep, periodic = [], []
        restarted = False
        for line in lines:
            r = json.loads(line)
            restarted = restarted or r["k"] == "H"
            if (not restarted and self.periodic is not None and r.get("b") == self.periodic
                    and r["k"] == kind):
                if kind in self.periodic_first:
                    periodic.append(line)
                    continue
                self.periodic_first.add(kind)
            keep.append(line)
        return keep, periodic

    def take(self, scenario, label, op, started, checkpoint=None, conflict=None):
        """Records the op as a step; `started` is the harness's mocktime when the op began."""
        self.check_log()
        digest, p_digest = self.split_periodic(self.read_new("digest"), "R")
        decisions, p_decisions = self.split_periodic(self.read_new("decisions"), "A")
        step = {
            "step": len(self.steps) + 1,
            "scenario": scenario,
            "label": label,
            "mocktime": started,
            "op": op,
            "digest": digest,
            "decisions": decisions,
            "conflict": conflict,
            "checkpoint": checkpoint,
        }
        if p_digest or p_decisions:
            step["periodic"] = p_digest + p_decisions
        self.steps.append(step)
        self.test.log.info("step %d [%s] %s: %s, %d records" % (
            step["step"], scenario, label, op["op"], len(step["digest"])))
        return step

    def block(self, scenario, label, raw, source, conflict=False):
        started = self.test.mocktime
        h, _ = block_hash(raw)
        reply = self.node.submitblock(raw.hex())
        self.settle()
        op = {"op": "BLOCK", "hash": h, "bytes": raw.hex(), "source": source, "core_reply": reply}
        return self.take(scenario, label, op, started, conflict=[h] if conflict else None)

    def clsig(self, scenario, label, payload, source, conflict=None):
        started = self.test.mocktime
        cl = msg_clsig()
        cl.deserialize(BytesIO(payload))
        h = "%064x" % cl.blockHash
        self.feeder.offer(b"clsig", MSG_CLSIG, payload)
        self.test.wait_until(lambda: self._best_cl() == (cl.height, h), timeout=60)
        self.settle()
        op = {"op": "CL_RECORD", "height": cl.height, "hash": h, "sig": cl.sig.hex(),
              "inv_hash": display(hash256(payload)), "source": source}
        return self.take(scenario, label, op, started, conflict=conflict)

    def _best_cl(self):
        try:
            cl = self.node.getbestchainlock()
        except Exception:
            return None
        return cl["height"], cl["blockhash"]

    def islock(self, scenario, label, payload, source, core_tx=None):
        started = self.test.mocktime
        lock = msg_isdlock()
        lock.deserialize(BytesIO(payload))
        txid = "%064x" % lock.txid
        inv_hash = display(hash256(payload))
        if core_tx is not None:
            self.node.sendrawtransaction(core_tx)
        mined = self.mined_height(txid)
        self.check_log()
        self.feeder.offer(b"isdlock", MSG_ISDLOCK, payload)
        which, line = self.wait_log([("islock=%s: processing islock" % inv_hash,),
                                     ("islock=%s: verification failed" % inv_hash,)])
        assert which == 0, "the recorder refused lock %s: %s" % (inv_hash, line)
        self.settle()
        stored = self.node.getislocks([txid])[0]
        assert isinstance(stored, dict), "the recorder did not store lock %s (txid %s)" % (inv_hash, txid)
        assert_equal(stored["hex"], payload.hex())
        op = {"op": "ISLOCK", "bytes": payload.hex(), "inv_hash": inv_hash, "txid": txid,
              "mined_height": mined, "core_tx": core_tx, "source": source}
        return self.take(scenario, label, op, started)

    def has_block(self, bh):
        try:
            self.node.getblockheader(bh)
            return True
        except JSONRPCException:
            return False

    def mined_height(self, txid):
        try:
            info = self.node.getrawtransaction(txid, True)
        except Exception:
            return None
        bh = info.get("blockhash")
        if bh is None:
            return None
        header = self.node.getblockheader(bh)
        return header["height"] if header["confirmations"] > 0 else None

    def spork(self, scenario, label, name, value):
        started = self.test.mocktime
        self.node.sporkupdate(name, value)
        self.settle()
        op = {"op": "SPORK", "id": SPORK_IDS[name], "name": name, "value": value}
        return self.take(scenario, label, op, started)

    def restart(self, scenario, label, kind="CLEAN", extra=(), checkpoint=True):
        assert kind in ("CLEAN", "KILL")
        started = self.test.mocktime
        self.check_log()
        self.node.disconnect_p2ps()
        if kind == "KILL":
            self.node.kill_process()
        else:
            self.node.stop_node()
        number = len(self.steps) + 1
        ckpt = self.dump(number) if checkpoint else None
        # The stop's own lines (none expected) go with this step; the next start begins a new H.
        self.start(extra)
        self.settle()
        op = {"op": "RESTART", "kind": kind, "args": list(extra)}
        return self.take(scenario, label, op, started, checkpoint=ckpt)

    def dump(self, number):
        src = self.node.chain_path / "llmq" / "isdb"
        copy = self.ckpt_dir / ("step-%04d.isdb" % number)
        if copy.exists():
            shutil.rmtree(copy)
        shutil.copytree(src, copy)
        text = subprocess.run([self.isdump, str(copy)], check=True, capture_output=True, text=True).stdout
        (self.ckpt_dir / ("step-%04d.isdump.json" % number)).write_text(text)
        dumped = json.loads(text)
        assert not dumped["obfuscated"], "Core's IS DB is not obfuscated"
        rows = [[r["k"], r["v"], r["t"]] for r in dumped["rows"]]
        return {"rows": rows, "is_locks": is_locks_digest(rows), "counts": dumped["counts"]}

    def stop(self):
        if self.node.running:
            self.node.stop_node()


class RecorderMixin:
    """The recorder's side of a capture, for a DashTestFramework subclass that sets `RUN` and has
    `--isdump` and `--checkpoints` options."""

    # -- the recorder ----------------------------------------------------------------------------

    @staticmethod
    def add_recorder_options(parser):
        parser.add_argument("--isdump", dest="isdump", required=True, help="the isdump binary")
        parser.add_argument("--checkpoints", dest="checkpoints", required=True,
                            help="directory for the per-checkpoint isdb copies and dumps")

    def recorder_args(self):
        """The producer's chain arguments, without node0's own roles, plus the recorder's."""
        chain_args = [a for a in self.extra_args[0] if not a.startswith("-watchquorums")]
        with open(os.path.join(self.nodes[0].datadir, "dash.conf"), encoding="utf8") as f:
            key = next(line.split("=", 1)[1].strip() for line in f if line.startswith("sporkkey="))
        return chain_args + [
            "-connect=0", "-listen=1", "-txindex=1", "-persistmempool=0", "-disablewallet=1",
            "-debug=chainlocks", "-debug=instantsend", "-sporkkey=%s" % key,
            "-statedigestlog=digest.jsonl", "-recordstatedecisions=decisions.jsonl",
        ]

    def start_recorder(self):
        idx = len(self.nodes)
        initialize_datadir(self.options.tmpdir, idx, self.chain)
        args = self.recorder_args()
        self.add_nodes(1, extra_args=[args])
        node = self.nodes.pop()
        self.rec = Recorder(self, node, args, self.options.isdump, self.options.checkpoints)
        self.rec.start()
        self.source = self.nodes[0].add_p2p_connection(Source())
        self.node0_feeder = self.nodes[0].add_p2p_connection(Feeder())
        self.rec_args = args

    def bump_mocktime(self, t, update_nodes=True, nodes=None, update_schedulers=True):
        super().bump_mocktime(t, update_nodes=update_nodes, nodes=nodes, update_schedulers=update_schedulers)
        rec = getattr(self, "rec", None)
        if rec is not None and update_nodes and rec.node.running:
            rec.node.setmocktime(self.mocktime)
            if update_schedulers:
                rec.node.mockscheduler(t)

    def generate(self, generator, *args, sync_fun=None, **kwargs):
        blocks = super().generate(generator, *args, sync_fun=sync_fun, **kwargs)
        if getattr(self, "rec", None) is not None:
            self.catch_up()
        return blocks

    def catch_up(self, label="producer block"):
        """Deliver node0's active chain blocks the recorder does not have, from the fork point up. What
        the recorder has is asked, not remembered: a SIGKILL loses blocks it had not flushed."""
        node0, rec = self.nodes[0], self.rec.node
        tip = node0.getblockcount()
        h = min(rec.getblockcount(), tip)
        while h > 0 and rec.getblockhash(h) != node0.getblockhash(h):
            h -= 1
        for height in range(h + 1, tip + 1):
            bh = node0.getblockhash(height)
            if self.rec.has_block(bh):
                continue
            raw = self.source.fetch(MSG_BLOCK, int(bh, 16))
            assert_equal(raw.hex(), node0.getblock(bh, 0))
            self.rec.block(self.scenario, "%s %d" % (label, height), raw, "observer")
        assert_equal(rec.getbestblockhash(), node0.getbestblockhash())

    # -- producer payloads -----------------------------------------------------------------------

    def observed(self, inv_type, payload):
        """Cross-check an RPC payload against the bytes node0 sent the observer, when it was in time."""
        seen = self.source.seen.get((inv_type, uint256_from_str(hash256(payload))))
        if seen:
            assert_equal(seen.hex(), payload.hex())

    def producer_islock(self, txid, wait=True):
        """node0's lock of `txid`, as `getislocks` serializes it (the isdlock payload)."""
        if wait:
            self.wait_for_instantlock(txid)
        payload = bytes.fromhex(self.nodes[0].getislocks([txid])[0]["hex"])
        self.observed(MSG_ISDLOCK, payload)
        return payload

    def producer_clsig(self, bh):
        """node0's ChainLock of `bh`, which must be its best one."""
        node0 = self.nodes[0]
        self.wait_for_chainlocked_block(node0, bh, timeout=60)
        best = node0.getbestchainlock()
        assert_equal(best["blockhash"], bh)
        payload = bytes.fromhex(best["hex"])
        self.observed(MSG_CLSIG, payload)
        return payload

    def build_clsig(self, height, block):
        """A ChainLock for a block the network has not ChainLocked, signed through `quorum sign`
        (feature_llmq_is_cl_conflicts.py v24:286-293)."""
        request_id = hash256(ser_string(b"clsig") + height.to_bytes(4, "little"))[::-1].hex()
        rec_sig = self.get_recovered_sig(request_id, block.hash, llmq_type=CL_LLMQ_TYPE)
        return msg_clsig(height, block.sha256, bytes.fromhex(rec_sig["sig"])).serialize()

    def build_islock(self, tx):
        """An isdlock for `tx` signed through `quorum sign` by the network's IS quorum. The type is not
        rotated, so the cycle hash is the signing quorum's hash (net_instantsend.cpp v24:143-146):
        the one `selectquorum` names for the request id, which `get_recovered_sig` signs with."""
        tx.rehash()
        request = ser_string(b"islock") + ser_compact_size(len(tx.vin))
        inputs = []
        for txin in tx.vin:
            request += txin.prevout.serialize()
            inputs.append(txin.prevout)
        request_id = hash256(request)[::-1].hex()
        quorum = self.mninfo[0].get_node(self).quorum("selectquorum", IS_LLMQ_TYPE, request_id)["quorumHash"]
        rec_sig = self.get_recovered_sig(request_id, tx.hash, llmq_type=IS_LLMQ_TYPE)
        return msg_isdlock(1, inputs, tx.sha256, int(quorum, 16), bytes.fromhex(rec_sig["sig"])).serialize()

    # -- fixture ---------------------------------------------------------------------------------

    def provenance(self):
        with open(os.environ["DASHD"], "rb") as f:
            dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
        return {
            "dashd_version": os.environ.get("LK1B_DASHD_VERSION", ""),
            "dashd_sha256": dashd_sha256,
            "build_describe": os.environ.get("LK1B_BUILD_DESCRIBE", ""),
            "build_commit": os.environ.get("LK1B_BUILD_COMMIT", ""),
            "tool": "contrib/node/lk1b-captures (%s)" % os.path.basename(__file__),
            "producer_args": [list(a) for a in self.extra_args],
            "recorder_args": [a for a in self.rec_args if not a.startswith("-sporkkey=")],
            "captured_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "llmq_size": LLMQ_SIZE,
            "llmq_threshold": LLMQ_THRESHOLD,
        }

    def write_fixture(self, extra=None):
        fixture = {
            "schema": SCHEMA,
            "run": self.RUN,
            "network": "regtest",
            "provenance": self.provenance(),
            "steps": self.rec.steps,
        }
        fixture.update(extra or {})
        with open(self.options.out, "w") as f:
            json.dump(fixture, f, indent=1, sort_keys=True)
            f.write("\n")
        self.log.info("fixture written to %s (%d steps)" % (self.options.out, len(self.rec.steps)))


class LK1bCapture(RecorderMixin, DashTestFramework):
    RUN = "main"

    def add_options(self, parser):
        self.add_wallet_options(parser)
        parser.add_argument("--out", dest="out", required=True, help="fixture JSON path")
        self.add_recorder_options(parser)

    def set_test_params(self):
        self.set_dash_test_params(5, 4, [["-llmqtestinstantsenddip0024=llmq_test_instantsend"]] * 5)
        self.set_dash_llmq_test_params(LLMQ_SIZE, LLMQ_THRESHOLD)
        self.supports_cli = False
        self.rec = None
        self.scenario = "setup"

    # -- producer transactions -------------------------------------------------------------------

    def utxos(self, n):
        node0 = self.nodes[0]
        out = []
        for u in sorted(node0.listunspent(), key=lambda u: (u["txid"], u["vout"])):
            if u["amount"] >= 2 and (u["txid"], u["vout"]) not in self.used:
                out.append(u)
                self.used.add((u["txid"], u["vout"]))
                if len(out) == n:
                    return out
        raise AssertionError("not enough spendable outputs")

    def spend(self, inputs, fee_units=1, nonstandard=False):
        """A signed transaction spending `inputs` (listunspent entries or {txid, vout, amount}) to one
        fresh address of node0, or with `nonstandard` to a bare OP_TRUE output, which blocks accept
        and the mempool refuses (policy/policy.cpp v24:57, 130); returns (hex, txid, CTransaction)."""
        node0 = self.nodes[0]
        total = sum(Decimal(str(i["amount"])) for i in inputs)
        outputs = {node0.getnewaddress(): str(total - fee_units * Decimal("0.0001"))}
        raw = node0.createrawtransaction([{"txid": i["txid"], "vout": i["vout"]} for i in inputs], outputs)
        if nonstandard:
            unsigned = tx_from_hex(raw)
            unsigned.vout[0].scriptPubKey = CScript([OP_TRUE])
            raw = unsigned.serialize().hex()
        signed = node0.signrawtransactionwithwallet(raw)
        assert signed["complete"], signed
        tx = tx_from_hex(signed["hex"])
        tx.rehash()
        return signed["hex"], tx.hash, tx

    def lock_tx(self, scenario, label, tx_hex, txid):
        """Send a transaction to node0, wait for the network's lock, and deliver it to the recorder."""
        self.nodes[0].sendrawtransaction(tx_hex)
        payload = self.producer_islock(txid)
        self.rec.islock(scenario, label, payload, "observer", core_tx=tx_hex)
        return payload

    def advance_to(self, ok):
        """Mine producer blocks (delivered as they come) until `ok(tip height)`."""
        node0 = self.nodes[0]
        while not ok(node0.getblockcount()):
            self.bump_mocktime(1)
            self.generate(node0, 1, sync_fun=self.sync_blocks)

    def advance_to_safe(self):
        """A tip whose next block is outside every DKG mining window."""
        self.advance_to(lambda tip: (tip + 1) % DKG_INTERVAL not in DKG_MINING_WINDOW)

    def mn_payouts(self, bt, fees):
        """GBT's masternode payouts for a block whose transactions pay `fees`.

        v24 pays the platform share of the subsidy to OP_RETURN, then the payee's owner and, with an
        operator reward and payout script, its operator, out of 3/4 of subsidy plus fees
        (masternode/payments.cpp:63-100, 132-199 at a81a871). The platform share does not depend on
        fees; owner and operator do. The model must give GBT's own amounts for GBT's own fees first.
        """
        payees = bt["masternode"]
        assert payees[0]["script"] == "6a" and 2 <= len(payees) <= 3, payees
        template_fees = sum(tx["fee"] for tx in bt["transactions"])
        subsidy = bt["coinbasevalue"] - template_fees
        owner = payees[1]
        operator_bp = 0
        if len(payees) == 3:
            mn = next(m for m in self.mninfo if m.rewards_address == owner["payee"])
            operator_bp = mn.operator_reward * 100

        def amounts(f):
            reward = (subsidy + f) * 3 // 4 - payees[0]["amount"]
            operator = reward * operator_bp // 10000
            return [payees[0]["amount"], reward - operator] + ([operator] if len(payees) == 3 else [])

        assert_equal(amounts(template_fees), [p["amount"] for p in payees])
        return [(a, p["script"]) for a, p in zip(amounts(fees), payees)]

    def conflict_block(self, txs, mn_payee=None):
        """A block on node0's tip with `txs`, paying GBT's masternode payees (or `mn_payee` in the
        owner's place), the template's quorum commitments and its CbTx payload verbatim: the way
        Core's v24 tests build one (feature_masternode_payout_shares.py:36-70 at a81a871). The
        harness keeps MN list changes out of these heights, so the payload's roots still hold."""
        node0 = self.nodes[0]
        bt = node0.getblocktemplate()
        assert (bt["height"]) % DKG_INTERVAL not in DKG_MINING_WINDOW
        assert not bt.get("superblock"), "superblock payments are not built"
        fees = 0
        for tx in txs:
            for txin in tx.vin:
                prev = node0.gettxout("%064x" % txin.prevout.hash, txin.prevout.n, False)
                fees += int(Decimal(str(prev["value"])) * COIN)
            fees -= sum(out.nValue for out in tx.vout)
        payouts = self.mn_payouts(bt, fees)
        if mn_payee is not None:
            script = node0.getaddressinfo(mn_payee)["scriptPubKey"]
            payouts[1] = (payouts[1][0], script)
        coinbase = create_coinbase(bt["height"])
        coinbase.vout = [CTxOut(a, CScript(bytes.fromhex(script))) for a, script in payouts]
        miner = node0.getaddressinfo(node0.get_deterministic_priv_key().address)["scriptPubKey"]
        total = bt["coinbasevalue"] - sum(tx["fee"] for tx in bt["transactions"]) + fees
        coinbase.vout.append(CTxOut(total - sum(a for a, _ in payouts), CScript(bytes.fromhex(miner))))
        coinbase.nVersion = 3
        coinbase.nType = 5
        coinbase.vExtraPayload = bytes.fromhex(bt["coinbase_payload"])
        coinbase.rehash()
        block = create_block(int(bt["previousblockhash"], 16), coinbase, ntime=bt["curtime"], version=bt["version"])
        block.vtx += txs
        block.vtx += [t for t in (tx_from_hex(x["data"]) for x in bt["transactions"]) if t.nType == 6]
        block.hashMerkleRoot = block.calc_merkle_root()
        block.solve()
        block.rehash()
        return block

    def producer_adopt(self, block, clsig_payload):
        """Make the producer follow a ChainLocked block the harness built (it rejects it first)."""
        self.nodes[0].submitblock(block.serialize().hex())
        self.node0_feeder.offer(b"clsig", MSG_CLSIG, clsig_payload)
        for node in self.nodes:
            self.wait_for_best_chainlock(node, block.hash, timeout=60)
        self.sync_blocks()
        assert_equal(self.nodes[0].getbestblockhash(), block.hash)

    def check_conflict(self, step, block_hash_, kind, why, removed):
        """The Core record of a conflict attempt: its kind, reject reason and `fx`."""
        records = [json.loads(line) for line in step["digest"]]
        mine = [r for r in records if r.get("b") == block_hash_ and r["k"] in ("C", "R")]
        assert mine, "no attempt record for %s in step %d" % (block_hash_, step["step"])
        rec = mine[-1]
        assert_equal(rec["k"], kind)
        if why is not None:
            assert_equal(rec["s"].get("why"), why)
        fx = rec["o"].get("fx")
        assert fx is not None, "Core wrote no fx on conflict attempt %s" % block_hash_
        rm = [e for e in fx if e[0] == "is_rm"]
        assert_equal(rm, [["is_rm", lock, txid] for lock, txid in removed])
        step["conflict_fx_kinds"] = sorted({e[0] for e in fx})

    # -- the run ---------------------------------------------------------------------------------

    def run_test(self):
        node0 = self.nodes[0]
        self.used = set()
        node0.sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
        self.wait_for_sporks_same()
        self.log.info("Quorums: llmq_test (ChainLocks) and llmq_test_instantsend (InstantSend)")
        self.mine_quorum()
        self.mine_quorum(llmq_type_name="llmq_test_instantsend", llmq_type=IS_LLMQ_TYPE)
        self.generate(node0, 1)
        self.wait_for_chainlocked_block_all_nodes(node0.getbestblockhash())

        self.start_recorder()
        try:
            self.scenarios()
        finally:
            self.rec.stop()
        self.write_fixture()

    def scenarios(self):
        node0, rec = self.nodes[0], self.rec
        self.scenario = "setup"
        rec.spork("setup", "IS on", "SPORK_2_INSTANTSEND_ENABLED", SPORK_ON)
        rec.spork("setup", "ChainLocks on", "SPORK_19_CHAINLOCKS_ENABLED", SPORK_ON)
        self.catch_up("setup block")
        tip = node0.getbestblockhash()
        rec.clsig("setup", "tip ChainLock", self.producer_clsig(tip), "observer")
        rec.restart("setup", "checkpoint: after setup")
        self.archived_a = self.scenario_a()
        self.scenario_b()
        self.scenario_c()
        self.scenario_c_prime()
        self.scenario_d()
        self.scenario_f()
        self.scenario_g()
        self.scenario_e()
        self.scenario_h()

    def scenario_a(self):
        """(a) lock, then mine (mined mark), then ChainLock (confirm, archive)."""
        node0, rec = self.nodes[0], self.rec
        self.scenario = "a"
        tx_hex, txid, _ = self.spend(self.utxos(1))
        self.lock_tx("a", "lock", tx_hex, txid)
        self.bump_mocktime(1)
        bh = self.generate(node0, 1, sync_fun=self.sync_blocks)[0]
        assert txid in node0.getblock(bh)["tx"]
        rec.clsig("a", "ChainLock of the mining block", self.producer_clsig(bh), "observer")
        height = node0.getblockheader(bh)["height"]
        rec.restart("a", "checkpoint: lock confirmed and archived")
        return height

    def scenario_b(self):
        """(b) a conflicting block, not ChainLocked: conflict-tx-lock."""
        rec = self.rec
        self.scenario = "b"
        self.advance_to_safe()
        u = self.utxos(1)
        tx1_hex, tx1, _ = self.spend(u)
        _, _, tx2 = self.spend(u, fee_units=2)
        self.lock_tx("b", "lock", tx1_hex, tx1)
        rec.restart("b", "checkpoint: before the conflict (empties the mempool)")
        block = self.conflict_block([tx2])
        step = rec.block("b", "conflicting block", block.serialize(), "built", conflict=True)
        assert_equal(step["op"]["core_reply"], "conflict-tx-lock")
        self.check_conflict(step, block.hash, "R", "conflict-tx-lock", [])
        # A restart reconsiders a best invalid block that is ahead of the tip
        # (node/chainstate.cpp v24:463-465, validation.cpp v24:4166-4170) and connects it while
        # still importing, so in IBD (node/blockstorage.cpp v24:898, validation.cpp v24:1990),
        # before the IS check (validation.cpp v24:2812). A producer block moves the tip past it,
        # so the next restart leaves it invalid.
        self.bump_mocktime(1)
        self.generate(self.nodes[0], 1, sync_fun=self.sync_blocks)

    def scenario_c(self):
        """(c) a ChainLocked conflicting block: is_rm, chained child removed."""
        rec = self.rec
        self.scenario = "c"
        self.advance_to_safe()
        u = self.utxos(1)
        tx1_hex, tx1, tx1_obj = self.spend(u)
        _, _, tx2 = self.spend(u, fee_units=2)
        l1 = self.lock_tx("c", "lock", tx1_hex, tx1)
        # The wallet finds tx1's output only once tx1 is in node0's mempool.
        child = [{"txid": tx1, "vout": 0, "amount": Decimal(tx1_obj.vout[0].nValue) / COIN}]
        tx4_hex, tx4, _ = self.spend(child)
        self.lock_tx("c", "chained child lock", tx4_hex, tx4)
        rec.restart("c", "checkpoint: before the conflict")
        block = self.conflict_block([tx2])
        step = rec.block("c", "conflicting block", block.serialize(), "built", conflict=True)
        assert_equal(step["op"]["core_reply"], "conflict-tx-lock")
        cl = self.build_clsig(self.nodes[0].getblockcount() + 1, block)
        step = rec.clsig("c", "ChainLock of the conflicting block", cl, "built", conflict=[block.hash])
        self.check_conflict(step, block.hash, "C", None, [(display(hash256(l1)), tx2.hash)])
        assert_equal(rec.node.getbestblockhash(), block.hash)
        self.producer_adopt(block, cl)
        rec.restart("c", "checkpoint: lock and child removed")

    def scenario_c_prime(self):
        """(c') the A/B overlapping index (PLAN-LK1b §5.3 counterexample A/B)."""
        node0, rec = self.nodes[0], self.rec
        self.scenario = "c'"
        # Bblk and X sit at a height outside the mining windows.
        self.advance_to(lambda tip: tip % DKG_INTERVAL == 21)
        x, y = self.utxos(2)
        txa_hex, txa, _ = self.spend([x, y])
        # B's tx is non-standard: when CL(X) disconnects Bblk, the reorg cannot put it back in the
        # mempool (validation.cpp v24:450-456, 877), so U's connect does not evict it and remove
        # lock B through the mempool (instantsend/instantsend.cpp v24:132-147, README Core-only).
        _, _, txb = self.spend([x], fee_units=2, nonstandard=True)
        _, _, t = self.spend([y], fee_units=3)
        _, _, u = self.spend([x], fee_units=4)
        lock_a = self.lock_tx("c'", "lock A on x and y", txa_hex, txa)
        lock_b = self.build_islock(txb)
        rec.restart("c'", "checkpoint: lock A (empties the mempool)")
        rec.spork("c'", "IS off for B's block", "SPORK_2_INSTANTSEND_ENABLED", SPORK_OFF)
        bblk = self.conflict_block([txb])
        step = rec.block("c'", "B's block (no IS check)", bblk.serialize(), "built")
        assert_equal(step["op"]["core_reply"], None)
        rec.spork("c'", "IS on", "SPORK_2_INSTANTSEND_ENABLED", SPORK_ON)
        step = rec.islock("c'", "lock B on x (mined)", lock_b, "built")
        assert_equal(step["op"]["mined_height"], node0.getblockcount() + 1)
        block = self.conflict_block([t, u])
        rec.block("c'", "X: T spends y, U spends x (stored, not connected)", block.serialize(), "built")
        cl = self.build_clsig(node0.getblockcount() + 1, block)
        step = rec.clsig("c'", "ChainLock of X", cl, "built", conflict=[block.hash])
        self.check_conflict(step, block.hash, "C", None, [(display(hash256(lock_a)), t.hash)])
        assert isinstance(rec.node.getislocks([txb.hash])[0], dict), "lock B must stay"
        self.producer_adopt(block, cl)
        rec.restart("c'", "checkpoint: A removed, B kept without its is_in row")

    def scenario_d(self):
        """(d) (c) with a bad payee: the block is rejected and the lock stays removed."""
        node0, rec = self.nodes[0], self.rec
        self.scenario = "d"
        self.advance_to_safe()
        u = self.utxos(1)
        tx1_hex, tx1, _ = self.spend(u)
        _, _, tx2 = self.spend(u, fee_units=2)
        l1 = self.lock_tx("d", "lock", tx1_hex, tx1)
        rec.restart("d", "checkpoint: before the conflict")
        block = self.conflict_block([tx2], mn_payee=node0.getnewaddress())
        step = rec.block("d", "conflicting block, bad payee", block.serialize(), "built", conflict=True)
        assert_equal(step["op"]["core_reply"], "conflict-tx-lock")
        cl = self.build_clsig(node0.getblockcount() + 1, block)
        rec.periodic = block.hash
        step = rec.clsig("d", "ChainLock of the bad block", cl, "built", conflict=[block.hash])
        self.check_conflict(step, block.hash, "R", "bad-cb-payee", [(display(hash256(l1)), tx2.hash)])
        # The best ChainLock is held in memory only, so the restart ends the retries and the
        # producer's blocks connect.
        rec.restart("d", "checkpoint: lock removed, block rejected")
        rec.periodic = None
        rec.periodic_first.clear()
        self.bump_mocktime(1)
        bh = self.generate(node0, 3, sync_fun=self.sync_blocks)[-1]
        rec.clsig("d", "ChainLock after the reset", self.producer_clsig(bh), "observer")

    def scenario_f(self):
        """(f) a reorg across a mined lock."""
        node0, rec = self.nodes[0], self.rec
        self.scenario = "f"
        self.advance_to_safe()
        tx_hex, txid, tx = self.spend(self.utxos(1))
        self.lock_tx("f", "lock", tx_hex, txid)
        node0.prioritisetransaction(txid, -COIN)
        fork = self.conflict_block([tx])
        step = rec.block("f", "recorder-only block mining the locked tx", fork.serialize(), "built")
        assert_equal(step["op"]["core_reply"], None)
        self.bump_mocktime(1)
        self.generate(node0, 2, sync_fun=self.sync_blocks)
        assert_equal(rec.node.getbestblockhash(), node0.getbestblockhash())
        node0.prioritisetransaction(txid, COIN)
        self.bump_mocktime(1)
        bh = self.generate(node0, 1, sync_fun=self.sync_blocks)[0]
        assert txid in node0.getblock(bh)["tx"]
        rec.clsig("f", "ChainLock of the re-mining block", self.producer_clsig(bh), "observer")
        rec.restart("f", "checkpoint: after the reorg")

    def scenario_g(self):
        """(g) SIGKILL between ops, then restart and continue."""
        node0, rec = self.nodes[0], self.rec
        self.scenario = "g"
        tx_hex, txid, _ = self.spend(self.utxos(1))
        self.lock_tx("g", "lock", tx_hex, txid)
        self.bump_mocktime(1)
        bh = self.generate(node0, 1, sync_fun=self.sync_blocks)[0]
        rec.restart("g", "SIGKILL: every committed batch survives", kind="KILL")
        # Core flushes the block index and chainstate lazily, so the kill can lose the newest block
        # while the IS DB keeps its rows; delivering it again rewrites the same is_m keys
        # (PLAN-LK1b §2.4 "Process crash").
        self.catch_up("re-delivered after the kill")
        rec.clsig("g", "ChainLock after the kill", self.producer_clsig(bh), "observer")
        rec.restart("g", "checkpoint: confirmed after the kill")

    def scenario_e(self):
        """(e) a conflicting block inside IBD: no IS check."""
        node0, rec = self.nodes[0], self.rec
        self.scenario = "e"
        self.advance_to_safe()
        u = self.utxos(1)
        tx1_hex, tx1, _ = self.spend(u)
        _, _, tx2 = self.spend(u, fee_units=2)
        self.lock_tx("e", "lock", tx1_hex, tx1)
        self.bump_mocktime(10)
        rec.restart("e", "into IBD: tip older than -maxtipage", extra=["-maxtipage=1"])
        assert rec.node.getblockchaininfo()["initialblockdownload"]
        block = self.conflict_block([tx2])
        step = rec.block("e", "conflicting block in IBD", block.serialize(), "built")
        assert_equal(step["op"]["core_reply"], None)
        rec.restart("e", "checkpoint: out of IBD, default -maxtipage")
        assert not rec.node.getblockchaininfo()["initialblockdownload"], \
            "(e)'s restart with the default -maxtipage did not leave IBD"
        self.bump_mocktime(1)
        self.generate(node0, 2, sync_fun=self.sync_blocks)
        assert_equal(rec.node.getbestblockhash(), node0.getbestblockhash())

    def scenario_h(self):
        """(h) archive pruning at h-100: (a)'s archive goes once a ChainLock reaches its height + 100."""
        node0, rec = self.nodes[0], self.rec
        self.scenario = "h"
        rec.restart("h", "checkpoint: before pruning")
        self.advance_to(lambda tip: tip >= self.archived_a + 101)
        bh = node0.getbestblockhash()
        rec.clsig("h", "ChainLock past (a)'s archive height + 100", self.producer_clsig(bh), "observer")
        rec.restart("h", "checkpoint: (a)'s archive pruned")

if __name__ == "__main__":
    LK1bCapture().main()
