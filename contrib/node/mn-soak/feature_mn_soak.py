#!/usr/bin/env python3
# Copyright (c) 2026 The Dash Core developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or http://www.opensource.org/licenses/mit-license.php.
'''
feature_mn_soak.py (rust-dashcore's contrib/node/mn-soak, DEC-1893 (A); not a Core test)

Soaks one rdashd masternode among Core masternodes on regtest. mn-soak.py copies this file into
func-compat's G2 test tree and runs it with that tree's mask "mn:K" (rust_nodes.py), so the K-th
regular masternode runs rdashd and every other node runs dashd.

Each 24-block DKG cycle is stepped block by block as DashTestFramework's mine_quorum and
mine_cycle_quorum step it, but nothing is asserted: every wait has a timeout and its outcome is
recorded, so one bad cycle is evidence and the soak goes on. Per cycle it writes one JSON line
to --soak-out/cycles.jsonl: each DKG session (llmq_test, llmq_test_v17, llmq_test_platform,
both llmq_test_dip0024 indexes) with rdashd's selection, contribution, premature commitment, bad
flag and timing as the Core members saw them, the mined commitment's valid and signer bits,
PoSe, recovered signatures, ChainLock and InstantSend agreement, rdashd's RSS and fd count, and
rdashd's warning and error lines (debug.log) and panics (stderr).

LLMQ types: regtest v24 defines llmq_test (100), llmq_test_instantsend (104), llmq_test_v17
(102), llmq_test_dip0024 (103) and llmq_test_platform (106). 104 is not tracked: Core enables it
only while DIP0024 or its quorums are not active, or when it is the DIP0024 InstantSend type
(v24.0.0 validation.cpp:6702-6704), and regtest's DIP0024 InstantSend type is 103, which these
cycles run after. 102 is enabled by the TESTDUMMY deployment (validation.cpp:6712-6713), so it
joins mid-run; each cycle records node0's `quorum list` types, a session of a type not enabled
at its base's predecessor is recorded as not enabled, and a type the harness doesn't know stops
the soak.

llmq_test_dip0024 forms no quorum until the chain has the quorum snapshots of three earlier
cycles; mn-soak.py's summary treats the cycles up to its first non-null commitment, at most 4,
as warm-up. rdashd's own `quorum dkgstatus` leaves Core's sent*/aborted/bad fields out (phase
H), so rdashd's participation is judged from what each Core member received from it.

Signing: each cycle asks `quorum sign` for llmq_test, llmq_test_v17 and llmq_test_dip0024 ids
twice over. Control requests go to every masternode. rdashd-dependent requests pick an id whose
selected quorum (`quorum selectquorum`) has rdashd in it and ask only rdashd and threshold - 1
of that quorum's Core members, so the signature recovers only with rdashd's share; a relayed
recovered signature can't stand in for it.
'''

import json
import os
import re
import struct
import time

from test_framework.messages import hash256, ser_string
from test_framework.test_framework import DashTestFramework

CYCLE = 24
TYPES = {100: "llmq_test", 102: "llmq_test_v17", 103: "llmq_test_dip0024", 106: "llmq_test_platform"}
ROTATION = 103
PLATFORM = 106
CHAINLOCKS = 100
V17 = 102
# Signing thresholds, v24.0.0 llmq/params.h: llmq_test 2 of 3, llmq_test_v17 2 of 3,
# llmq_test_dip0024 3 of 4. llmq_test_platform is evo-only, so rdashd is never in it.
THRESHOLD = {CHAINLOCKS: 2, V17: 2, ROTATION: 3}
DEPENDENT_TRIES = 32
# The phase a session based at height b is in at tip t (dkgPhaseBlocks 2), capped at finalize.
PHASES = {2: "receivedContributions", 3: "receivedComplaints", 4: "receivedJustifications",
          5: "receivedPrematureCommitments"}
PHASE_TIMEOUT = 40
POLL = 0.5
# rdashd's GetLogPrefix forms ("[warning] ", "[llmq:error] ") and Core's ported text forms.
# mn-soak.py holds the same pattern for its rescan; the selftest checks they match.
LOG_PROBLEM = re.compile(r"\[(?:[a-z0-9_.-]+:)?(?:warning|error)\] |\bERROR\b|\bWarning:|\bError:|\bwarning:|\berror:")
PANIC = re.compile(r"panicked at")


def expected_phase(base, tip):
    return min(6, (tip - base) // 2 + 1)


def bit(hexbits, i):
    """Bit i of Core's BitsVectorToHexStr (byte i/8, bit i%8)."""
    data = bytes.fromhex(hexbits)
    return i // 8 < len(data) and bool(data[i // 8] >> (i % 8) & 1)


def clsig_request_id(height):
    """chainlock::GenSigRequestId: SerializeHash(("clsig", int32 height)), in RPC (display) order."""
    return hash256(ser_string(b"clsig") + struct.pack("<i", height))[::-1].hex()


def session_connections(status, type_name, quorum_hash):
    """The `quorumConnections` entry of a `quorum dkgstatus` reply for one session, or None."""
    for qc in status.get("quorumConnections", []):
        if qc.get("llmqType") == type_name and qc.get("quorumHash") == quorum_hash and "quorumConnections" in qc:
            return qc["quorumConnections"]
    return None


def connected(conns):
    return sum(1 for c in conns or [] if c.get("connected"))


def proc_sample(pid):
    """rdashd's VmRSS (kB) and open fd count, or None once it has exited."""
    try:
        with open(f"/proc/{pid}/status", encoding="utf8") as f:
            rss = next(int(line.split()[1]) for line in f if line.startswith("VmRSS:"))
        return {"rss_kb": rss, "fds": len(os.listdir(f"/proc/{pid}/fd"))}
    except (OSError, StopIteration):
        return None


class MnSoak(DashTestFramework):
    def add_options(self, parser):
        self.add_wallet_options(parser)
        parser.add_argument("--soak-seconds", type=int, default=3600, help="wall time for the cycles")
        parser.add_argument("--soak-out", required=True, help="cycles.jsonl and meta.json go here")
        parser.add_argument("--soak-regular", type=int, default=4, help="regular masternodes, rdashd's included")
        parser.add_argument("--soak-evo", type=int, default=3, help="Core evo masternodes (llmq_test_platform)")
        parser.add_argument("--soak-txs", type=int, default=3, help="InstantSend transactions per cycle")
        parser.add_argument("--soak-signs", type=int, default=2, help="quorum sign requests per cycle")
        parser.add_argument("--soak-max-cycles", type=int, default=0, help="stop after this many cycles (0: none)")

    def set_test_params(self):
        regular = self.options.soak_regular
        self.set_dash_test_params(regular + 1, regular, evo_count=self.options.soak_evo)

    def setup_soak(self):
        """Sporks, evo masternodes and rdashd's handles, mined until the masternodes are confirmed
        for the next DKG; writes meta.json and drains setup's rdashd lines."""
        out = self.options.soak_out
        os.makedirs(out, mode=0o700, exist_ok=True)
        self.jsonl = open(os.path.join(out, "cycles.jsonl"), "a", encoding="utf8")
        node0 = self.nodes[0]
        for spork in ("SPORK_17_QUORUM_DKG_ENABLED", "SPORK_21_QUORUM_ALL_CONNECTED", "SPORK_23_QUORUM_POSE"):
            node0.sporkupdate(spork, 0)
        self.wait_for_sporks_same()
        for _ in range(self.options.soak_evo):
            self.dynamically_add_masternode(evo=True)
        rust = [mn for mn in self.mninfo if self.rust_nodes.runs(mn.get_node(self))]
        assert len(rust) == 1, f"one rdashd masternode expected, got {[mn.nodeIdx for mn in rust]}"
        self.rust = rust[0]
        self.rnode = self.rust.get_node(self)
        self.core_mns = [mn for mn in self.mninfo if mn is not self.rust]
        self.log_path = os.path.join(self.rnode.datadir, self.chain, "debug.log")
        self.log_offset = 0
        self.stderr_dir = os.path.join(self.rnode.datadir, "stderr")
        self.stderr_offsets = {}
        self.names = {mn.proTxHash: mn.nodeIdx for mn in self.mninfo}
        self.by_protx = {mn.proTxHash: mn for mn in self.mninfo}
        self.mine_until_mns_confirmed_for_next_dkg()
        meta = {"rdashd_protx": self.rust.proTxHash, "rdashd_node": self.rust.nodeIdx,
                "core_mns": {mn.proTxHash: {"node": mn.nodeIdx, "evo": mn.evo} for mn in self.core_mns},
                "regular": self.options.soak_regular, "evo": self.options.soak_evo,
                "start_height": node0.getblockcount(), "rdashd_pid": self.rnode.process.pid,
                "seconds": self.options.soak_seconds}
        with open(os.path.join(out, "meta.json"), "w", encoding="utf8") as f:
            json.dump(meta, f, indent=1, sort_keys=True)
        self.drain_log()  # setup's lines are not a cycle's
        self.drain_stderr()

    def run_test(self):
        self.setup_soak()
        node0 = self.nodes[0]
        deadline = time.time() + self.options.soak_seconds
        cycles = 0
        while time.time() < deadline and (not self.options.soak_max_cycles or cycles < self.options.soak_max_cycles):
            if self.rnode.process.poll() is not None:
                self.record({"event": "rdashd-exited", "status": self.rnode.process.returncode,
                             "height": node0.getblockcount()})
                raise AssertionError(f"rdashd exited with status {self.rnode.process.returncode}")
            self.record(self.cycle())
            cycles += 1
        self.record({"event": "done", "cycles": cycles, "height": node0.getblockcount(),
                     "rdashd_log": self.drain_log() + self.drain_stderr()})
        self.jsonl.close()

    # ---- one cycle ----------------------------------------------------------------------------

    def enabled_types(self):
        """The LLMQ types node0's `quorum list` shows (Core lists the enabled types); stop on
        one the harness doesn't track."""
        names = set(self.nodes[0].quorum("list"))
        unknown = names - set(TYPES.values())
        if unknown:
            self.record({"event": "unknown-llmq-type", "types": sorted(unknown),
                         "height": self.nodes[0].getblockcount()})
            raise AssertionError(f"LLMQ types the soak doesn't track: {sorted(unknown)}")
        return names

    def cycle(self):
        node0 = self.nodes[0]
        started = time.time()
        height = node0.getblockcount()
        # Core runs a DKG for a type enabled at the base's predecessor (net_dkg.cpp:587,
        # utils.cpp:645) and `quorum list` reads the tip, so read the types one block early.
        self.step((CYCLE - 1 - height) % CYCLE)
        enabled = self.enabled_types()
        self.step(1)
        base = node0.getblockcount()
        sessions = {(t, 0): self.new_session(t, 0, base, enabled) for t in (CHAINLOCKS, V17, PLATFORM, ROTATION)}
        sessions[(ROTATION, 1)] = None
        for tip in range(base, base + 12):
            if tip == base + 1:
                sessions[(ROTATION, 1)] = self.new_session(ROTATION, 1, base + 1, enabled)
            live = [s for s in sessions.values() if s is not None and s["base"] <= tip]
            for s in live:
                if not s["empty"]:
                    self.wait_phase(s, expected_phase(s["base"], tip))
            final = [s for s in live if expected_phase(s["base"], tip) == 6 and "miner_wait" not in s]
            if final:
                self.wait_miner_commitments(final)
            self.step(1)
        rest = self.tail_of_cycle(base)
        for s in sessions.values():
            self.finish_session(s, base)
        return {"event": "cycle", "base": base, "wall_s": round(time.time() - started, 1),
                "t_start": round(started, 1), "t_end": round(time.time(), 1), "enabled_types": sorted(enabled),
                "sessions": [self.session_record(s) for s in sessions.values()], **rest}

    def step(self, blocks):
        nodes = self.all_nodes()
        for _ in range(blocks):
            self.bump_mocktime(1)
            self.generate(self.nodes[0], 1, sync_fun=lambda: self.sync_blocks(nodes))

    def all_nodes(self):
        return [self.nodes[0]] + [mn.get_node(self) for mn in self.mninfo]

    def dkgstatus(self, node, level=2):
        try:
            return node.quorum("dkgstatus", level)
        except Exception as e:  # an rdashd read it doesn't serve, or a node that went away
            return {"error": repr(e)}

    def new_session(self, llmq_type, index, base, enabled):
        on = TYPES[llmq_type] in enabled
        return {"type": llmq_type, "index": index, "base": base, "hash": self.nodes[0].getblockhash(base),
                "phases": {}, "members": None, "rdashd_status": None, "enabled": on, "empty": not on}

    def member_status(self, node, s):
        st = self.dkgstatus(node)
        for qs in st.get("session", []):
            status = qs.get("status", {})
            if qs.get("llmqType") in (TYPES[s["type"]], s["type"]) and status.get("quorumHash") == s["hash"]:
                return status
        return None

    def connections_ready(self, s, need):
        """Phase 1 also waits, as mine_quorum and mine_cycle_quorum do, until every member (rdashd
        included) has its size - 1 quorum connections (SPORK_21) and every Core member's inbound
        quorum peers have a fresh probe (SPORK_23): Core relays a DKG message only to the relay
        members connected when it is sent (net_dkg.cpp RelayInvToParticipants), so a phase stepped
        into before the connections exist loses that member's messages."""
        seen, probes = {}, True
        for protx in s["members"] or []:
            mn = self.by_protx.get(protx)
            if mn is None:
                continue
            node = mn.get_node(self)
            conns = session_connections(self.dkgstatus(node), TYPES[s["type"]], s["hash"])
            seen[str(mn.nodeIdx)] = None if conns is None else connected(conns)
            if mn is not self.rust and conns is not None:
                probes = probes and self.probes_fresh(mn, node, conns)
        ok = len(seen) == need and all(v is not None and v >= need - 1 for v in seen.values())
        return ok and probes, {"connected": seen, "probes_fresh": probes}

    def probes_fresh(self, mn, node, conns):
        """wait_for_masternode_probes' check for one Core member."""
        for c in conns:
            if c.get("outbound") or c["proTxHash"] == mn.proTxHash or c["proTxHash"] not in self.by_protx:
                continue
            try:
                if node.protx("info", c["proTxHash"])["metaInfo"]["lastOutboundSuccessElapsed"] > 55 * 60:
                    return False
            except Exception:
                return False
        return True

    def wait_phase(self, s, phase):
        """Wait until every Core member reports `phase` (and, for contribute and commit, every
        member's message; for init, every member's quorum connections), as wait_for_quorum_phase
        does; record the time and what each Core member had received from rdashd."""
        if phase in s["phases"]:
            return
        begin = time.time()
        counter = PHASES.get(phase)
        connections_bumped = 0
        connections = None
        while True:
            statuses = {mn.proTxHash: self.member_status(mn.get_node(self), s) for mn in self.core_mns}
            statuses = {k: v for k, v in statuses.items() if v is not None}
            if s["members"] is None:
                for v in statuses.values():
                    if "allMembers" in v:
                        s["members"] = v["allMembers"]
                        break
            need = len(s["members"] or [])
            done = bool(statuses) and all(v.get("phase") == phase for v in statuses.values())
            if done and counter in ("receivedContributions", "receivedPrematureCommitments"):
                done = all(len(v.get(counter, [])) >= need for v in statuses.values())
            if done and phase == 1 and need:
                done, connections = self.connections_ready(s, need)
            elapsed = time.time() - begin
            if done or elapsed > PHASE_TIMEOUT or not need and elapsed > 10:
                break
            if phase == 1 and connections_bumped < 60:
                self.bump_mocktime(1)  # quorum connections and probes, as mine_quorum's wait_proc
                connections_bumped += 1
            time.sleep(POLL)
        rust = self.rust.proTxHash
        seen = {}
        for protx, v in statuses.items():
            seen[protx] = {k: any(m.get("proTxHash") == rust for m in v.get(k, []))
                           for k in ("receivedContributions", "receivedComplaints", "receivedJustifications",
                                     "receivedPrematureCommitments", "badMembers", "weComplain")}
        s["phases"][phase] = {"wait_s": round(time.time() - begin, 2), "timed_out": not done,
                              "core_members": len(statuses), "from_rdashd": seen}
        if connections is not None:
            s["phases"][phase]["connections"] = connections
        if not need and not statuses:
            s["empty"] = True  # no Core member has this session: no quorum forms this cycle
        own = self.member_status(self.rnode, s)
        if own is not None:
            s["rdashd_status"] = {k: own.get(k) for k in ("phase", "sentContributions", "sentComplaint",
                                                          "sentJustification", "sentPrematureCommitment", "aborted")}

    def minable(self, node):
        st = self.dkgstatus(node, 0)
        return {(int(c["llmqType"]), c["quorumHash"]) for c in st.get("minableCommitments", [])
                if int(c["quorumPublicKey"], 16) != 0}

    def wait_miner_commitments(self, sessions):
        """Before the block that mines them: wait until the miner holds every final commitment a
        Core member of the session holds, as wait_for_quorum_commitments_on_miner does, so a
        late commitment is recorded instead of mined as a null one."""
        begin = time.time()
        while True:
            members = set().union(*(self.minable(mn.get_node(self)) for mn in self.core_mns))
            missing = members - self.minable(self.nodes[0])
            pending = [s for s in sessions if (s["type"], s["hash"]) in missing]
            if not pending or time.time() - begin > 20:
                break
            time.sleep(POLL)
        for s in sessions:
            s["miner_wait"] = {"wait_s": round(time.time() - begin, 2), "timed_out": s in pending,
                               "core_has": (s["type"], s["hash"]) in members}

    def finish_session(self, s, base):
        """The mined commitment for `s` (a block in the mining window), its valid and signer bits."""
        if s is None:
            return
        node0 = self.nodes[0]
        s["commitment"] = None
        for h in range(base + 10, min(base + 21, node0.getblockcount() + 1)):
            block = node0.getblock(node0.getblockhash(h), 2)
            for tx in block["tx"]:
                qc = tx.get("qcTx", {}).get("commitment")
                if qc and qc.get("llmqType") == s["type"] and qc.get("quorumHash") == s["hash"]:
                    null = int(qc.get("quorumPublicKey", "0"), 16) == 0
                    if s["commitment"] is None or s["commitment"]["null"]:
                        s["commitment"] = {"height": h, "null": null, **{k: qc.get(k) for k in (
                            "signersCount", "signers", "validMembersCount", "validMembers", "quorumIndex")}}
        info = None
        if s["commitment"] and not s["commitment"]["null"]:
            try:
                info = node0.quorum("info", s["type"], s["hash"])
            except Exception:
                info = None
        s["info_members"] = [m["proTxHash"] for m in info["members"]] if info else None

    def session_record(self, s):
        if s is None:
            return None
        rust = self.rust.proTxHash
        members = s["info_members"] or s["members"] or []
        selected = rust in members
        rec = {"type": s["type"], "index": s["index"], "base": s["base"], "quorum": s["hash"],
               "members": len(members), "selected": selected, "enabled": s["enabled"], "empty": s["empty"],
               "phases": s["phases"],
               "miner_wait": s.get("miner_wait"), "rdashd_status": s["rdashd_status"], "commitment": s["commitment"]}
        c = s["commitment"]
        if selected and c and not c["null"] and s["info_members"]:
            i = s["info_members"].index(rust)
            rec["rdashd_valid"] = bit(c["validMembers"], i)
            rec["rdashd_signer"] = bit(c["signers"], i)
        if selected:
            ph = s["phases"]
            rec["rdashd_contribution_seen_by"] = sum(v["receivedContributions"] for v in ph.get(2, {}).get("from_rdashd", {}).values())
            rec["rdashd_commitment_seen_by"] = sum(v["receivedPrematureCommitments"] for v in ph.get(5, {}).get("from_rdashd", {}).values())
            rec["rdashd_bad_by"] = sum(any(p.get("from_rdashd", {}).get(m, {}).get("badMembers") for p in ph.values())
                                       for m in {k for p in ph.values() for k in p.get("from_rdashd", {})})
            rec["rdashd_complained_by"] = sum(any(p.get("from_rdashd", {}).get(m, {}).get("weComplain") for p in ph.values())
                                              for m in {k for p in ph.values() for k in p.get("from_rdashd", {})})
        return rec

    # ---- the rest of the cycle: transactions, locks, signatures, health -----------------------

    def tail_of_cycle(self, base):
        node0 = self.nodes[0]
        nodes = self.all_nodes()
        txids, islocks = [], {}
        for _ in range(self.options.soak_txs):
            txids.append(node0.sendtoaddress(node0.getnewaddress(), 0.01))
        mempool = self.sync_mempool(nodes, txids)
        islock_wait = self.wait_islocks(txids, nodes)
        for txid in txids:
            islocks[txid] = {str(n.index): self.islocked(n, txid) for n in nodes}
        while node0.getblockcount() < base + CYCLE - 1:
            self.step(1)
        # At base+23 the signing height (tip - 8) is past this cycle's mined commitments, so the
        # quorums just formed sign.
        relay = {str(t): self.active_members(t) for t in (CHAINLOCKS, V17, ROTATION)}
        signs = [r for t in (CHAINLOCKS, V17, ROTATION) for r in self.sign_requests(base, t)]
        cl = self.chainlock_agreement(nodes)
        recsigs = self.chainlock_recsigs(base, nodes)
        pose = self.pose()
        health = proc_sample(self.rnode.process.pid)
        return {"mempool_sync": mempool, "islock_wait": islock_wait, "islocks": islocks,
                "active_members": relay, "signs": signs, "chainlock": cl, "cl_recsigs": recsigs,
                "pose": pose, "rdashd_proc": health, "rdashd_log": self.drain_log() + self.drain_stderr()}

    def active_members(self, llmq_type):
        """Node indexes of the members of `llmq_type`'s active quorums: Core sends plain
        recovered signatures (QSENDRECSIGS) only to quorum relay members, so only they are
        expected to hold one they did not recover themselves."""
        node0 = self.nodes[0]
        try:
            hashes = node0.quorum("list").get(TYPES[llmq_type], [])
            members = set()
            for h in hashes:
                members |= {self.names.get(m["proTxHash"], -1) for m in node0.quorum("info", llmq_type, h)["members"]}
            return sorted(members)
        except Exception as e:
            return repr(e)[:120]

    def sync_mempool(self, nodes, txids):
        """sync_mempools as wait_for_instantlock does it (mocktime +30, then +3 a poll), bounded."""
        self.bump_mocktime(30)
        begin = time.time()
        while time.time() - begin < 30:
            missing = {}
            for node in nodes:
                try:
                    pool = set(node.getrawmempool())
                    gone = [t for t in txids if t not in pool]
                except Exception as e:
                    gone = [repr(e)[:80]]
                if gone:
                    missing[str(node.index)] = len(gone)
            if not missing:
                return {"wait_s": round(time.time() - begin, 2), "timed_out": False}
            self.bump_mocktime(3)
            time.sleep(1)
        return {"wait_s": round(time.time() - begin, 2), "timed_out": True, "missing": missing}

    def islocked(self, node, txid):
        try:
            tx = node.getrawtransaction(txid, True)
            return bool(tx.get("instantlock"))
        except Exception as e:
            return repr(e)[:120]

    def wait_islocks(self, txids, nodes):
        begin = time.time()
        while time.time() - begin < 30:
            if all(self.islocked(n, t) is True for n in nodes for t in txids):
                return {"wait_s": round(time.time() - begin, 2), "timed_out": False}
            time.sleep(POLL)
        return {"wait_s": round(time.time() - begin, 2), "timed_out": True}

    def select_quorum(self, llmq_type, rid):
        """The quorum `quorum sign` picks for `rid` (Core's selectquorum on node0) and its members'
        proTxHashes, or (None, error)."""
        node0 = self.nodes[0]
        try:
            quorum = node0.quorum("selectquorum", llmq_type, rid)["quorumHash"]
            return quorum, [m["proTxHash"] for m in node0.quorum("info", llmq_type, quorum)["members"]]
        except Exception as e:
            return None, repr(e)[:120]

    def sign_requests(self, base, llmq_type):
        """--soak-signs control requests (every masternode asked) and as many rdashd-dependent
        ones (rdashd plus threshold - 1 Core members of the selected quorum asked)."""
        results = []
        for n in range(self.options.soak_signs):
            rid = hash256(b"mn-soak" + struct.pack("<iii", base, llmq_type, n))[::-1].hex()
            quorum, members = self.select_quorum(llmq_type, rid)
            results.append(self.sign_one(llmq_type, rid, base, n, quorum, members, self.mninfo, False))
        for n in range(self.options.soak_signs):
            quorum, members, rid = None, [], None
            for k in range(DEPENDENT_TRIES):
                rid = hash256(b"mn-soak-dep" + struct.pack("<iiii", base, llmq_type, n, k))[::-1].hex()
                quorum, members = self.select_quorum(llmq_type, rid)
                if quorum is None or self.rust.proTxHash in members:
                    break
            if quorum is None or self.rust.proTxHash not in members:
                results.append({"type": llmq_type, "dependent": True, "id": rid, "quorum": quorum,
                                "skipped": members if quorum is None else "rdashd in no selected quorum"})
                continue
            core = [self.by_protx[m] for m in members if m != self.rust.proTxHash and m in self.by_protx]
            asked = [self.rust] + core[:THRESHOLD[llmq_type] - 1]
            results.append(self.sign_one(llmq_type, rid, base, n, quorum, members, asked, True))
        return results

    def sign_one(self, llmq_type, rid, base, n, quorum, members, asked_mns, dependent):
        """Ask `asked_mns` to sign; then which nodes hold the recovered signature, and when."""
        msg = hash256(b"msg" + bytes.fromhex(rid) + struct.pack("<iii", base, llmq_type, n))[::-1].hex()
        asked = {}
        for mn in asked_mns:
            try:
                asked[str(mn.nodeIdx)] = mn.get_node(self).quorum("sign", llmq_type, rid, msg)
            except Exception as e:
                asked[str(mn.nodeIdx)] = repr(e)[:120]
        begin = time.time()
        seen = {}
        nodes = self.all_nodes()
        # Plain recovered sigs reach only quorum relay members, so stop once the signing quorum's
        # members hold it rather than wait the timeout out on non-members.
        member_nodes = sorted(self.names.get(m, -1) for m in members) if isinstance(members, list) else members
        targets = {str(i) for i in member_nodes} if isinstance(member_nodes, list) else {str(n.index) for n in nodes}
        while time.time() - begin < 20 and not targets <= set(seen):
            for node in nodes:
                if str(node.index) in seen:
                    continue
                try:
                    if node.quorum("hasrecsig", llmq_type, rid, msg):
                        seen[str(node.index)] = round(time.time() - begin, 2)
                except Exception as e:
                    seen[str(node.index)] = repr(e)[:120]
            time.sleep(POLL)
        sigs = set()
        for node in nodes:
            try:
                sigs.add(node.quorum("getrecsig", llmq_type, rid, msg)["sig"])
            except Exception:
                pass
        return {"type": llmq_type, "dependent": dependent, "id": rid, "quorum": quorum,
                "quorum_members": member_nodes, "rdashd_member": isinstance(members, list) and self.rust.proTxHash in members,
                "asked": asked, "recovered_after_s": seen, "distinct_sigs": len(sigs)}

    def chainlock_agreement(self, nodes):
        tip = self.nodes[0].getbestblockhash()
        begin = time.time()
        while time.time() - begin < 30:
            best = {}
            for node in nodes:
                try:
                    best[str(node.index)] = node.getbestchainlock()["blockhash"]
                except Exception as e:
                    best[str(node.index)] = repr(e)[:80]
            if all(v == tip for v in best.values()):
                return {"tip_locked_s": round(time.time() - begin, 2), "agree": True}
            time.sleep(POLL)
        return {"tip_locked_s": None, "agree": len(set(best.values())) == 1, "best": best}

    def chainlock_recsigs(self, base, nodes):
        """For each height of the cycle, which nodes hold the ChainLock's recovered signature."""
        counts = {str(n.index): 0 for n in nodes}
        heights = range(base, base + CYCLE)
        for h in heights:
            rid, msg = clsig_request_id(h), self.nodes[0].getblockhash(h)
            for node in nodes:
                try:
                    counts[str(node.index)] += bool(node.quorum("hasrecsig", CHAINLOCKS, rid, msg))
                except Exception:
                    pass
        return {"heights": len(heights), "held": counts}

    def pose(self):
        try:
            state = self.nodes[0].protx("info", self.rust.proTxHash)["state"]
            return {"penalty": state.get("PoSePenalty"), "ban_height": state.get("PoSeBanHeight"),
                    "revived_height": state.get("PoSeRevivedHeight")}
        except Exception as e:
            return {"error": repr(e)[:120]}

    def drain_log(self):
        """rdashd's new debug.log warning and error lines since the last call."""
        try:
            with open(self.log_path, "rb") as f:
                f.seek(self.log_offset)
                data = f.read()
                self.log_offset += len(data)
        except OSError:
            return []
        lines = data.decode("utf8", errors="replace").splitlines()
        return [line[:400] for line in lines if LOG_PROBLEM.search(line)]

    def drain_stderr(self):
        """rdashd's new stderr lines with a Rust panic since the last call."""
        found = []
        try:
            names = sorted(os.listdir(self.stderr_dir))
        except OSError:
            return found
        for name in names:
            try:
                with open(os.path.join(self.stderr_dir, name), "rb") as f:
                    f.seek(self.stderr_offsets.get(name, 0))
                    data = f.read()
            except OSError:
                continue
            self.stderr_offsets[name] = self.stderr_offsets.get(name, 0) + len(data)
            found += ["stderr: " + line[:400] for line in data.decode("utf8", errors="replace").splitlines()
                      if PANIC.search(line)]
        return found

    def record(self, obj):
        self.jsonl.write(json.dumps(obj, sort_keys=True) + "\n")
        self.jsonl.flush()


if __name__ == '__main__':
    MnSoak().main()
