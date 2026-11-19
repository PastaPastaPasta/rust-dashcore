#!/usr/bin/env python3
"""Record native Core v24 quorum/signing traffic; never inject quorum messages."""
import argparse
import hashlib
import json
import os
import random
import re
import struct
import sys
from bisect import bisect_right
from io import BytesIO
from pathlib import Path
from types import SimpleNamespace

from capture_lq3a import RawObserver, display
from test_framework.messages import CBlock, CFinalCommitmentPayload
from test_framework.p2p import p2p_lock
from test_framework.test_framework import DashTestFramework
from test_framework.util import assert_equal, force_finish_mnsync

TYPES = {100: "llmq_test", 103: "llmq_test_dip0024", 106: "llmq_test_platform"}
SPORKS = ("SPORK_17_QUORUM_DKG_ENABLED", "SPORK_21_QUORUM_ALL_CONNECTED", "SPORK_23_QUORUM_POSE")
SIG_COMMANDS = {"qsigsesann", "qsigsinv", "qgetsigs", "qbsigs", "qsigshare", "qsigrec"}
WRITE = re.compile(r"ProcessRecoveredSig -- valid recSig\. signHash=([0-9a-f]{64}), id=([0-9a-f]{64}), msgHash=([0-9a-f]{64})")
INTAKE = re.compile(r"VerifyAndProcessRecoveredSig -- signHash=([0-9a-f]{64}), .*node=(-?\d+)")
RECOVER = re.compile(r"TryRecoverSig -- recovered signature\. id=([0-9a-f]{64}), msgHash=([0-9a-f]{64})")
CLEANUP = re.compile(r"CRecoveredSigsDb::(\w+) -- deleted (\d+) entries")
MOCKTIME = re.compile(r"\(mocktime: (\d+)\)")
RECEIVED = re.compile(r"received: (\w+) \((\d+) bytes\) peer=(\d+)")


def sha256d(data):
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def recsig_wire(sig):
    return (bytes([sig["llmqType"]]) + b"".join(bytes.fromhex(sig[k])[::-1]
            for k in ("quorumHash", "id", "msgHash")) + bytes.fromhex(sig["sig"])).hex()


def records(path):
    # Core: v24.0.0 src/net.cpp:5048-5076 CaptureMessageToFile
    with path.open("rb") as stream:
        while header := stream.read(24):
            assert len(header) == 24, (path, "short header")
            timestamp, command, size = struct.unpack("<Q12sI", header)
            payload = stream.read(size)
            assert len(payload) == size, (path, "short payload")
            yield timestamp, command.rstrip(b"\0").decode(), payload


class LaneCapture(DashTestFramework):
    def add_options(self, parser):
        self.add_wallet_options(parser)
        parser.add_argument("--out", required=True)
        parser.add_argument("--seed", type=int, default=2311)

    def set_test_params(self):
        self.set_dash_test_params(9, 8, [["-capturemessages", "-testactivationheight=v19@100",
            "-vbparams=testdummy:999999999999:999999999999",
            "-vbparams=v24:999999999999:999999999999",
            "-vbparams=evo_shares:999999999999:999999999999"] for _ in range(9)])
        self.set_dash_llmq_test_params(7, 4)
        for args in self.extra_args:
            args += ["-watchquorums=1", "-llmqtestplatformparams=3:2", "-llmqinstantsenddip0024=llmq_test_dip0024"]
        self.extra_args[0].remove("-capturemessages")
        self.extra_args[1].append("-maxrecsigsage=60")
        self.supports_cli = False
        self.recording = False
        self.phase = None
        self.statuses = {}
        self.peer_history = {}
        self.evidence_root = None

    def begin_control_phase(self, label):
        if self.control_phase is None or self.phase_complete:
            self.control_phase = len(self.data["control_phases"])
            self.data["control_phases"].append({"phase": self.control_phase, "label": label,
                "starts": {str(node.index): self.pos(node) for node in self.nodes}})
            self.phase_complete = False
        else:
            self.data["control_phases"][self.control_phase]["label"] = label

    def complete_barrier(self, kind, polls, **fields):
        held = self.mocktime
        self.data["control_phases"][self.control_phase].update(
            {"held_mocktime": held, "barrier": kind, **fields})
        for node in self.nodes:
            self.data["events"][str(node.index)].append({"phase": self.control_phase,
                "pos": self.pos(node), "mocktime": held, "kind": "barrier", "barrier": kind,
                "polls": polls.get(str(node.index), []), **fields})
        self.phase_complete = True

    def logpath(self, node):
        path = Path(node.datadir) / "regtest" / "debug.log"
        if path.exists() or self.evidence_root is None:
            return path
        return self.evidence_root / f"node{node.index}" / "regtest" / "debug.log"

    def pos(self, node):
        return self.logpath(node).stat().st_size

    def logrows(self, node, start=0):
        with self.logpath(node).open("rb") as stream:
            stream.seek(start)
            while raw := stream.readline():
                pos = stream.tell() - len(raw)
                line = raw.decode(errors="replace")
                yield pos, line

    def fact(self, kind, **fields):
        if self.recording:
            assert not self.inputs_held
            self.begin_control_phase(kind)
            for node in self.nodes:
                self.data["events"][str(node.index)].append({"phase": self.control_phase,
                                                           "pos": self.pos(node), "mocktime": self.mocktime,
                                                           "kind": kind, **fields})

    def bump_mocktime(self, t, *args, **kwargs):
        if self.recording:
            self.fact("setmocktime", value=self.mocktime + t)
        super().bump_mocktime(t, *args, **kwargs)

    def generate(self, *args, **kwargs):
        result = super().generate(*args, **kwargs)
        if self.recording:
            self.fact("tip", height=self.nodes[0].getblockcount(), block_hash=self.nodes[0].getbestblockhash())
        return result

    def rpc(self, node, command, *params, exact=True, sign_exact_reason=None):
        assert not self.inputs_held
        if exact:
            assert self.phase_complete or sign_exact_reason == "threshold_submit"
            if command in ("sign", "platformsign"):
                assert sign_exact_reason in ("threshold_submit", "isolated_after_barrier")
        before = self.pos(node)
        result = node.quorum(command, *params)
        after = self.pos(node)
        if exact:
            observation = {"node": node.index, "phase": self.control_phase,
                "method": "quorum " + command, "params": list(params), "result": result}
            if sign_exact_reason:
                observation["sign_exact_reason"] = sign_exact_reason
            self.data["observations"].append(observation)
        if command in ("sign", "platformsign"):
            self.data["events"][str(node.index)].append({"pos": before, "pos_after": after,
                "phase": self.control_phase, "mocktime": self.mocktime, "kind": "sign_rpc", "method": command,
                "role": "exact" if exact else "barrier_input", "sign_exact_reason": sign_exact_reason,
                "params": list(params), "result": result})
        elif not exact:
            self.data["events"][str(node.index)].append({"pos": before, "pos_after": after,
                "phase": self.control_phase, "mocktime": self.mocktime, "kind": "rpc_input",
                "method": command, "params": list(params), "result": result})
        return result

    def peers(self):
        for node in self.nodes:
            history = self.peer_history.setdefault(str(node.index), {})
            for peer in node.getpeerinfo():
                history[str(peer["id"])] = {k: peer[k] for k in
                    ("id", "addr", "addrbind", "inbound", "verified_proregtx_hash") if k in peer}

    def wait_for_quorum_phase(self, quorum_hash, phase, expected_member_count, check_received_messages,
                              check_received_messages_count, mninfos, llmq_type_name="llmq_test", **kwargs):
        # Core: v24.0.0 test/functional/test_framework/test_framework.py:2153-2181 wait_for_quorum_phase
        base_height = self.nodes[0].getblockheader(quorum_hash)["height"]
        checks = [(llmq_type_name, expected_member_count)]
        if base_height % 24 == 0:
            checks += [(name, size) for name, size in ((TYPES[100], 7), (TYPES[106], 3))
                       if name != llmq_type_name]

        def ready():
            counts = {name: 0 for name, _ in checks}
            for mn in self.mninfo:
                for session in mn.get_node(self).quorum("dkgstatus")["session"]:
                    status = session["status"]
                    name = session["llmqType"]
                    if name not in counts or status["quorumHash"] != quorum_hash:
                        continue
                    if status["phase"] != phase:
                        return False
                    expected = dict(checks)[name] if check_received_messages in (
                        "receivedContributions", "receivedPrematureCommitments") else check_received_messages_count
                    if check_received_messages and status[check_received_messages] < expected:
                        return False
                    counts[name] += 1
            return all(counts[name] >= size for name, size in checks)

        def advance():
            if ready():
                return True
            self.bump_mocktime(1)
            return False

        self.wait_until(advance, timeout=120, sleep=1)

    def wait_for_quorum_connections(self, quorum_hash, expected_connections, mninfos, llmq_type_name="llmq_test", **kwargs):
        super().wait_for_quorum_connections(quorum_hash, expected_connections, mninfos, llmq_type_name, **kwargs)
        if self.phase:
            self.statuses[(self.phase, quorum_hash, llmq_type_name)] = {
                str(node.index): node.quorum("dkgstatus") for node in self.nodes}
            self.peers()

    def wait_for_masternode_probes(self, quorum_hash, mninfos, timeout=30, wait_proc=None,
                                   llmq_type_name="llmq_test"):
        # Core: v24.0.0 test/functional/test_framework/test_framework.py:2113-2151 wait_for_masternode_probes
        def ready():
            for mn in mninfos:
                status = mn.get_node(self).quorum("dkgstatus")
                if not any(session["llmqType"] == llmq_type_name and
                           session["status"]["quorumHash"] == quorum_hash for session in status["session"]):
                    continue
                for quorum in status["quorumConnections"]:
                    if quorum["llmqType"] != llmq_type_name or quorum.get("quorumHash") != quorum_hash:
                        continue
                    for peer in quorum.get("quorumConnections", []):
                        if peer["proTxHash"] == mn.proTxHash or peer["outbound"]:
                            continue
                        info = mn.get_node(self).protx("info", peer["proTxHash"])
                        meta = info["metaInfo"]
                        online = any(other.proTxHash == peer["proTxHash"] for other in mninfos)
                        if meta["lastOutboundSuccessElapsed"] > 55 * 60 and (
                                online or meta["lastOutboundAttemptElapsed"] > 55 * 60):
                            if wait_proc:
                                wait_proc()
                            return False
            return True

        self.wait_until(ready, timeout=timeout, sleep=1)

    def write_barrier(self, requests):
        polls = {str(node.index): [] for node in self.nodes}
        held = self.mocktime
        def complete():
            results = []
            for node in self.nodes:
                for request in requests:
                    params = [request["type"], request["id"], request["msgHash"]]
                    result = node.quorum("hasrecsig", *params)
                    polls[str(node.index)].append({"method": "quorum hasrecsig", "params": params,
                        "result": result, "pos": self.pos(node)})
                    results.append(result)
            assert self.mocktime == held
            return all(results)
        self.inputs_held = True
        try:
            self.wait_until(complete, timeout=180, sleep=1)
        finally:
            self.inputs_held = False
        self.complete_barrier("writes", polls, expected=[request["signHash"] for request in requests])

    def cleanup_barrier(self, node, start, function):
        polls = {str(node.index): []}
        held = self.mocktime
        def complete():
            positions = [pos for pos, line in self.logrows(node, start)
                         if (match := CLEANUP.search(line)) and match[1] == function]
            polls[str(node.index)].append({"method": "log cleanup", "function": function,
                "matching_positions": positions, "result": bool(positions)})
            assert self.mocktime == held
            return bool(positions)
        self.inputs_held = True
        try:
            self.wait_until(complete, timeout=60)
        finally:
            self.inputs_held = False
        self.complete_barrier("cleanup", polls, cleanup_node=node.index, function=function)

    def sign_request(self, llmq_type, quorum_hash, request_id=None, msg_hash=None, explicit=True, label="ordinary"):
        self.begin_control_phase(f"{self.phase}:{label}")
        rid = request_id or f"{self.rng.getrandbits(256):064x}"
        msg = msg_hash or f"{self.rng.getrandbits(256):064x}"
        qhash = quorum_hash
        selection = None
        if not explicit:
            selection = self.rpc(self.nodes[0], "selectquorum", llmq_type, rid, exact=False)
            qhash = selection["quorumHash"]
        members = self.get_quorum_masternodes(qhash, llmq_type=llmq_type)
        key = f"{llmq_type}:{qhash}"
        already_recorded = any(q["type"] == llmq_type and q["quorumHash"] == qhash
                               for phase in self.data["phases"].values() for q in phase["quorums"])
        if not already_recorded and key not in self.data["signing_quorums"]:
            self.data["signing_quorums"][key] = {"type": llmq_type, "quorumHash": qhash,
                "quorum_info": {str(node.index): {str(flag).lower(): node.quorum("info", llmq_type, qhash, flag)
                    for flag in (False, True)} for node in self.nodes}}
        request = {"phase": self.phase, "label": label, "type": llmq_type, "quorumHash": qhash,
                   "control_phase": self.control_phase,
                   "id": rid, "msgHash": msg, "explicit_quorum": explicit,
                   "tip_height": self.nodes[0].getblockcount(), "mocktime": self.mocktime,
                   "offline_shares": {}, "submissions": [], "selectquorum": selection}
        for mn in members:
            share = self.rpc(mn.get_node(self), "sign", llmq_type, rid, msg, qhash if explicit else "", False, exact=False)
            request["offline_shares"][mn.proTxHash] = share
        sign_hashes = {share["signHash"] for share in request["offline_shares"].values()}
        assert len(sign_hashes) == 1
        sign_hash = sign_hashes.pop()
        request["signHash"] = sign_hash
        starts = {str(node.index): self.pos(node) for node in self.nodes}
        request["log_start"] = starts
        assert not self.nodes[0].getrawmempool(), "non-coinbase transaction during R3 signing"
        self.rng.shuffle(members)
        fresh_id = not any(previous["type"] == llmq_type and previous["id"] == rid
                           for previous in self.data["requests"])
        threshold = {100: 4, 103: 3, 106: 2}[llmq_type]
        request["fresh_id_at_phase_start"] = fresh_id
        request["threshold"] = threshold
        for ordinal, mn in enumerate(members):
            node = mn.get_node(self)
            exact = fresh_id and ordinal < threshold
            reason = "threshold_submit" if exact else None
            if llmq_type == 106:
                result = self.rpc(node, "platformsign", rid, msg, qhash if explicit else "", True,
                                  exact=exact, sign_exact_reason=reason)
            else:
                result = self.rpc(node, "sign", llmq_type, rid, msg, qhash if explicit else "", True,
                                  exact=exact, sign_exact_reason=reason)
            request["submissions"].append({"node": node.index, "proTxHash": mn.proTxHash,
                "mocktime": self.mocktime, "result": result, "role": "exact" if exact else "barrier_input"})
        self.write_barrier([request])
        assert not self.nodes[0].getrawmempool(), "non-coinbase transaction during R3 signing"
        request["log_end"] = {str(node.index): self.pos(node) for node in self.nodes}
        request["hasrecsig"] = {}
        request["recsig_by_node"] = {}
        request["isconflicting"] = {}
        for node in self.nodes:
            name = str(node.index)
            assert self.rpc(node, "hasrecsig", llmq_type, rid, msg)
            request["hasrecsig"][name] = True
            sig = self.rpc(node, "getrecsig", llmq_type, rid, msg)
            request["recsig_by_node"][name] = sig
            assert_equal(sig["quorumHash"], qhash)
            conflict = self.rpc(node, "isconflicting", llmq_type, rid, msg)
            assert conflict is False
            request["isconflicting"][name] = conflict
        request["recsig"] = request["recsig_by_node"]["0"]
        if not explicit:
            observed_selection = self.rpc(self.nodes[0], "selectquorum", llmq_type, rid)
            assert_equal(observed_selection, selection)
            request["selectquorum"] = observed_selection
        self.data["requests"].append(request)
        self.peers()
        self.log.info("settled %s type %s request %s", label, llmq_type, rid)
        self.checkpoint()
        return request

    def capture_quorum(self, llmq_type, qhash):
        infos = {str(node.index): {str(flag).lower(): node.quorum("info", llmq_type, qhash, flag)
                                  for flag in (False, True)} for node in self.nodes}
        info = infos["0"]["false"]
        block = self.nodes[0].getblock(info["minedBlock"], 2)
        commitments = [{k: tx[k] for k in ("txid", "type", "extraPayload", "qcTx")}
                       for tx in block["tx"] if tx.get("type") == 6]
        assert commitments, block
        # Core: v24.0.0 src/rpc/quorums.cpp:192-247 BuildQuorumInfo
        entry = {"type": llmq_type, "quorumHash": qhash, "height": info["height"],
                 "quorum_info": infos, "commitment_transactions": commitments,
                 "dkgstatus": self.statuses.get((self.phase, qhash, TYPES[llmq_type]))}
        assert entry["dkgstatus"] is not None, (self.phase, qhash, "missing DKG snapshot")
        self.data["phases"][self.phase]["quorums"].append(entry)
        for _ in range(3):
            self.sign_request(llmq_type, qhash)
        if llmq_type == 106:
            first = self.sign_request(llmq_type, qhash, label="platform_same_id_first")
            second = self.sign_request(llmq_type, qhash, request_id=first["id"], label="platform_same_id_second")
            for node in self.nodes:
                assert self.rpc(node, "hasrecsig", llmq_type, first["id"], first["msgHash"])
                assert not self.rpc(node, "isconflicting", llmq_type, second["id"], second["msgHash"])
        else:
            conflict = f"{self.rng.getrandbits(256):064x}"
            req = self.data["requests"][-1]
            req["conflicting_msgHash"] = conflict
            req["conflict_observations"] = {}
            for node in self.nodes[:2]:
                result = self.rpc(node, "isconflicting", llmq_type, req["id"], conflict)
                assert result is True
                req["conflict_observations"][str(node.index)] = result

    def selection_window(self, new_quorum):
        ids = [hashlib.sha256(f"lane-c-selection-{i}".encode()).hexdigest() for i in range(20)]
        for offset in range(1, 10):
            self.generate(self.nodes[0], 1, sync_fun=self.sync_blocks)
            request = self.sign_request(100, new_quorum, explicit=False, label="selection_window")
            results = [self.rpc(self.nodes[0], "selectquorum", 100, rid) for rid in ids]
            self.data["selection_window"].append({"phase": self.phase, "new_quorum": new_quorum,
                "offset": offset, "height": self.nodes[0].getblockcount(), "ids": ids,
                "results": results, "request_id": request["id"], "recsig": request["recsig"]})

    def cleanup(self):
        node = self.nodes[1]
        candidates = [q for q in self.nodes[0].quorum("list", 3)["llmq_test"]
                      if self.mninfo[0] in self.get_quorum_masternodes(q)]
        assert candidates, "age-limited MN not in any active quorum"
        qhash = candidates[0]
        request = self.sign_request(100, qhash, label="cleanup_boundary")
        write_time = request["mocktime"]
        self.bump_mocktime(write_time + 60 - self.mocktime, update_schedulers=False)
        self.write_barrier([request])
        assert self.rpc(node, "hasrecsig", 100, request["id"], request["msgHash"])
        before = self.pos(node)
        self.bump_mocktime(1, update_schedulers=False)
        self.bump_mocktime(6, update_schedulers=False)
        self.cleanup_barrier(node, before, "CleanupOldRecoveredSigs")
        assert not self.rpc(node, "hasrecsig", 100, request["id"], request["msgHash"])
        rid, msg, other = (f"{self.rng.getrandbits(256):064x}" for _ in range(3))
        assert self.rpc(node, "sign", 100, rid, msg, qhash, True, sign_exact_reason="isolated_after_barrier")
        before = self.pos(node)
        vote_time = self.mocktime
        self.bump_mocktime(61, update_schedulers=False)
        self.bump_mocktime(6, update_schedulers=False)
        self.cleanup_barrier(node, before, "CleanupOldVotes")
        assert not self.rpc(node, "hasrecsig", 100, rid, msg)
        assert self.rpc(node, "sign", 100, rid, other, qhash, True, sign_exact_reason="isolated_after_barrier")
        self.data["cleanup"] = {"node": 1, "write_mocktime": write_time, "request_id": request["id"],
                                "vote_only_id": rid, "vote_time": vote_time, "vote_messages": [msg, other]}

    def checkpoint(self):
        Path(self.options.out).write_text(json.dumps(self.data, sort_keys=True, indent=2) + "\n")

    def run_test(self):
        self.rng = random.Random(self.options.seed)
        for _ in range(3):
            self.dynamically_add_masternode(evo=True)
        # The framework's dynamic datadir copier cannot copy message_capture directories.
        # Enable controller/Evo capture only after the last Evo registration has finished.
        self.restart_node(0, self.nodes[0].extra_args + ["-capturemessages"])
        for mn in self.mninfo:
            if mn.nodeIdx >= 9:
                self.restart_node(mn.nodeIdx, mn.get_node(self).extra_args + [
                    "-capturemessages", "-masternodeblsprivkey=" + mn.keyOperator])
            self.connect_nodes(mn.nodeIdx, 0)
        for node in self.nodes:
            force_finish_mnsync(node)
        self.nodes[0].sporkupdate("SPORK_19_CHAINLOCKS_ENABLED", 4070908800)
        self.nodes[0].sporkupdate("SPORK_2_INSTANTSEND_ENABLED", 4070908800)
        self.wait_for_sporks_same()
        self.sync_all()
        self.data = {"network": "regtest", "bls_scheme": "basic", "schema_version": 2,
            "provenance": {"seed": self.options.seed, "dashd_version": os.environ["LQ3A_DASHD_VERSION"],
                "dashd_sha256": hashlib.sha256(Path(os.environ["DASHD"]).read_bytes()).hexdigest(),
                "core_tag": os.environ["LQ3A_CORE_TAG"], "extra_args": [node.extra_args +
                    (["-capturemessages"] if "-capturemessages" not in node.extra_args else [])
                    for node in self.nodes],
                "deployments": self.nodes[0].getdeploymentinfo(), "spork_timeline": []},
            "operators": {mn.proTxHash: mn.keyOperator for mn in self.mninfo},
            "signers": {str(node.index): {"proTxHash": next((mn.proTxHash for mn in self.mninfo if mn.nodeIdx == node.index), None),
                "evo": any(mn.evo and mn.nodeIdx == node.index for mn in self.mninfo), "signing_allowed": True}
                for node in self.nodes},
            "events": {str(node.index): [] for node in self.nodes}, "observations": [], "control_phases": [],
            "phases": {}, "requests": [], "quorum_list": [], "selection_window": [], "signing_quorums": {},
            "quorumdb": {"available": False, "reason": "dash-coreimport has row codecs but no LevelDB directory reader (CH6b)"}}
        self.start_positions = {str(node.index): self.pos(node) for node in self.nodes}
        self.data["provenance"]["log_start_positions"] = self.start_positions
        self.control_phase = None
        self.phase_complete = False
        self.inputs_held = False
        self.recording = True
        self.fact("signer", signers=self.data["signers"])
        self.fact("tip", height=self.nodes[0].getblockcount(), block_hash=self.nodes[0].getbestblockhash())
        subscriber = self.nodes[0].add_p2p_connection(RawObserver("subscriber"))
        subscriber.start()
        silent = self.nodes[0].add_p2p_connection(RawObserver("silent"))
        subscriber.sync_with_ping()
        silent.sync_with_ping()
        for phase in ("ring", "mesh"):
            self.phase = phase
            for name, value in zip(SPORKS, (0, 4070908800 if phase == "ring" else 0, 4070908800 if phase == "ring" else 0)):
                self.nodes[0].sporkupdate(name, value)
            self.wait_for_sporks_same()
            values = {name: self.nodes[0].spork("show")[name] for name in SPORKS}
            self.data["phases"][phase] = {"sporks": values, "quorums": []}
            self.data["provenance"]["spork_timeline"].append({"phase": phase, "mocktime": self.mocktime, "values": values})
            self.fact("spork", values=values)
            q = self.mine_quorum(expected_connections=3 if phase == "ring" else 6)
            self.capture_quorum(100, q)
            q = self.mine_quorum(expected_connections=3 if phase == "ring" else 6, skip_maturity=True)
            self.selection_window(q)
            self.capture_quorum(100, q)
            q = self.mine_quorum(llmq_type_name=TYPES[106], llmq_type=106, expected_members=3,
                expected_connections=2, expected_contributions=3, expected_commitments=3)
            self.capture_quorum(106, q)
            # The earlier DKG rounds already supplied the rotation history since V20@100.
            self.cycle_quorum_is_ready = True
            infos = self.mine_cycle_quorum()
            for info in infos:
                self.capture_quorum(103, info["quorumHash"])
            self.sign_request(100, q, explicit=False, label="default_selection")
            self.data["quorum_list"].append({"phase": phase, "height": self.nodes[0].getblockcount(),
                "default": self.nodes[0].quorum("list"), "count_4": self.nodes[0].quorum("list", 4)})
        self.cleanup()
        subscriber.arm(True)
        self.wait_until(subscriber.idle, timeout=90)
        subscriber.sync_with_ping()
        silent.sync_with_ping()
        with p2p_lock:
            assert any(c["command"] == "qsigrec" for c in subscriber.captured)
            assert not silent.captured and not silent.unsolicited and not silent.invs_seen
            self.data["relay"] = {"subscriber": {"captured": list(subscriber.captured),
                "invs": [{"type": t, "hash": f"{h:064x}"} for t, h in sorted(subscriber.seen)]},
                "silent": {"captured": list(silent.captured), "unsolicited": list(silent.unsolicited), "inv_count": silent.invs_seen}}
        # Five newer llmq_test quorums exceed Core's keepOldKeys=4.
        self.phase = None
        for _ in range(5):
            self.mine_quorum(expected_connections=6)
        target = max(634, ((self.nodes[0].getblockcount() - 58) // 576 + 1) * 576 + 58)
        self.generate(self.nodes[0], target - self.nodes[0].getblockcount(), sync_fun=self.sync_blocks)
        self.data["tip_height"] = self.nodes[0].getblockcount()
        self.peers()
        self.data["peer_history"] = self.peer_history
        self.data["chain"] = [self.nodes[0].getblock(self.nodes[0].getblockhash(h), 0)
                              for h in range(self.data["tip_height"] + 1)]
        self.write_barrier([])
        self.data["final_hasrecsig"] = {str(node.index): {request["signHash"]:
            self.rpc(node, "hasrecsig", request["type"], request["id"], request["msgHash"])
            for request in self.data["requests"]} for node in self.nodes}
        self.stop_nodes()
        self.checkpoint()
        self.finish_messages()
        self.finish_events()
        self.data["complete"] = True
        self.checkpoint()

    def finish_messages(self):
        protx_nodes = {mn.proTxHash: mn.nodeIdx for mn in self.mninfo}
        endpoint_nodes = {}
        for node, history in self.peer_history.items():
            for peer in history.values():
                if peer.get("addrbind"):
                    endpoint_nodes[peer["addrbind"]] = int(node)
        self.incoming = {}
        messages = []
        contribs = {}
        for node in self.nodes:
            history = self.peer_history.get(str(node.index), {})
            received = {}
            for pos, line in self.logrows(node):
                match = RECEIVED.search(line)
                if not match or match[1] not in SIG_COMMANDS:
                    continue
                peer = history[match[3]]
                endpoint = peer["addr"].replace(":", "_")
                received.setdefault(endpoint, []).append((pos, match[1], int(match[2]), peer))
            root = Path(node.datadir) / "regtest" / "message_capture"
            if not root.exists() and self.evidence_root is not None:
                root = self.evidence_root / f"node{node.index}" / "regtest" / "message_capture"
            for path in sorted(root.glob("*/msgs_recv.dat")):
                receipts = iter(received.get(path.parent.name, []))
                for order, (timestamp, command, payload) in enumerate(records(path)):
                    if command == "qcontrib":
                        t, qhash, member = payload[0], display(payload[1:33]), display(payload[33:65])
                        by_member = contribs.setdefault((t, qhash), {})
                        assert by_member.setdefault(member, payload.hex()) == payload.hex()
                    if command not in SIG_COMMANDS:
                        continue
                    pos, logged_command, size, peer = next(receipts)
                    assert (logged_command, size) == (command, len(payload)), (node.index, path, order)
                    source = protx_nodes.get(peer.get("verified_proregtx_hash"), endpoint_nodes.get(peer["addr"]))
                    assert source is not None, (node.index, peer, "unmapped signature-message sender")
                    entry = {"from": source, "to": node.index, "peer": peer["id"], "receive_pos": pos, "endpoint": path.parent.name,
                        "order": order, "mocktime": timestamp // 1000000, "command": command, "payload": payload.hex()}
                    messages.append(entry)
                    if command == "qsigrec":
                        assert len(payload) == 193
                        sign_hash = display(sha256d(payload[:97]))
                        self.incoming.setdefault((node.index, sign_hash), []).append(entry)
                assert next(receipts, None) is None, (node.index, path, "uncaptured receive line")
        self.data["sig_messages"] = messages
        commitments = self.commitments_from_chain()
        for phase in self.data["phases"].values():
            for q in phase["quorums"]:
                self.attach_quorum_evidence(q, contribs, commitments)
        for q in self.data["signing_quorums"].values():
            self.attach_quorum_evidence(q, contribs, commitments)
        self.assert_referenced_quorums_complete()

    @staticmethod
    def commitment_rpc(tx, payload):
        commitment = payload.commitment

        def bitset(bits):
            result = bytearray((len(bits) + 7) // 8)
            for index, enabled in enumerate(bits):
                if enabled:
                    result[index // 8] |= 1 << (index % 8)
            return bytes(result).hex()

        fields = {"llmqType": commitment.llmqType,
                  "membersSig": commitment.membersSig.hex(),
                  "quorumHash": f"{commitment.quorumHash:064x}",
                  "quorumIndex": commitment.quorumIndex,
                  "quorumPublicKey": commitment.quorumPublicKey.hex(),
                  "quorumVvecHash": f"{commitment.quorumVvecHash:064x}",
                  "quorumSig": commitment.quorumSig.hex(),
                  "signers": bitset(commitment.signers),
                  "signersCount": sum(commitment.signers),
                  "validMembers": bitset(commitment.validMembers),
                  "validMembersCount": sum(commitment.validMembers),
                  "version": commitment.nVersion}
        return {"txid": tx.hash, "type": 6, "extraPayload": tx.vExtraPayload.hex(),
                "qcTx": {"commitment": fields, "height": payload.nHeight,
                         "version": payload.nVersion}}

    def commitments_from_chain(self):
        commitments = {}
        for raw_block in self.data["chain"]:
            block = CBlock()
            block.deserialize(BytesIO(bytes.fromhex(raw_block)))
            for tx in block.vtx:
                if tx.nType != 6:
                    continue
                payload = CFinalCommitmentPayload()
                payload.deserialize(BytesIO(tx.vExtraPayload))
                commitment = payload.commitment
                if not any(commitment.quorumPublicKey):
                    continue
                tx.calc_sha256()
                key = (commitment.llmqType, f"{commitment.quorumHash:064x}")
                commitments.setdefault(key, self.commitment_rpc(tx, payload))
        return commitments

    def attach_quorum_evidence(self, quorum, contributions, commitments):
        key = (quorum["type"], quorum["quorumHash"])
        quorum["qcontrib"] = contributions.get(key, {})
        if "commitment_transactions" not in quorum:
            commitment = commitments.get(key)
            quorum["commitment_transactions"] = [commitment] if commitment else []
        else:
            matching = [tx for tx in quorum["commitment_transactions"]
                        if tx["qcTx"]["commitment"]["llmqType"] == quorum["type"]
                        and tx["qcTx"]["commitment"]["quorumHash"] == quorum["quorumHash"]]
            quorum["commitment_transactions"] = matching[:1]
        assert quorum["qcontrib"], key
        assert len(quorum["qcontrib"]) == {100: 7, 103: 4, 106: 3}[quorum["type"]], key
        assert len(quorum["commitment_transactions"]) == 1, key
        assert quorum["commitment_transactions"][0]["qcTx"]["commitment"]["quorumHash"] == quorum["quorumHash"], key

    def referenced_quorum_keys(self):
        keys = {(q["type"], q["quorumHash"])
                for phase in self.data["phases"].values() for q in phase["quorums"]}
        keys.update((q["type"], q["quorumHash"]) for q in self.data["signing_quorums"].values())
        keys.update((request["type"], request["quorumHash"]) for request in self.data["requests"])
        keys.update((100, result["quorumHash"])
                    for window in self.data["selection_window"] for result in window["results"])
        return keys

    def assert_referenced_quorums_complete(self):
        available = {(q["type"], q["quorumHash"]): q
                     for phase in self.data["phases"].values() for q in phase["quorums"]}
        available.update({(q["type"], q["quorumHash"]): q
                          for q in self.data["signing_quorums"].values()})
        for key in self.referenced_quorum_keys():
            quorum = available.get(key)
            assert quorum and quorum.get("qcontrib") and quorum.get("commitment_transactions"), key

    def finish_events(self):
        known = {r["signHash"]: r for r in self.data["requests"]}
        assert len(known) == len(self.data["requests"])
        assert len({(r["type"], r["id"], r["msgHash"]) for r in known.values()}) == len(known)
        ids = {(r["id"], r["msgHash"]): r for r in known.values()}
        for node in self.nodes:
            seen = set()
            intakes = {}
            recoveries = {}
            phase_starts = [phase["starts"][str(node.index)] for phase in self.data["control_phases"]]
            for pos, line in self.logrows(node, self.start_positions[str(node.index)]):
                assert "conflicting recoveredSig" not in line
                mock = MOCKTIME.search(line)
                phase = bisect_right(phase_starts, pos) - 1
                assert phase >= 0
                base = {"pos": pos, "phase": phase, "mocktime": int(mock[1]) if mock else None}
                if m := INTAKE.search(line):
                    if m[1] not in known:
                        continue
                    intakes.setdefault(m[1], []).append((pos, int(m[2])))
                    self.data["events"][str(node.index)].append({**base, "kind": "recsig_in",
                        "sign_hash": m[1], "from_peer": int(m[2]), "wire_hex": recsig_wire(known[m[1]]["recsig"])})
                elif m := WRITE.search(line):
                    assert m[1] in known, (node.index, line, "unexpected signature in R3 phase")
                    assert (m[2], m[3]) == (known[m[1]]["id"], known[m[1]]["msgHash"])
                    assert phase == known[m[1]]["control_phase"]
                    assert base["mocktime"] == self.data["control_phases"][phase]["held_mocktime"]
                    assert m[1] not in seen, (node.index, m[1], "duplicate valid recSig; rerun phase")
                    seen.add(m[1])
                    remote = bool(re.search(r"\[\s*recsigs\]", line))
                    assert remote or "[sigsh-work" in line, line
                    wire = recsig_wire(known[m[1]]["recsig"])
                    event = {**base, "kind": "write", "source": "remote" if remote else "local",
                             "sign_hash": m[1], "wire_hex": wire}
                    if remote:
                        assert m[1] in intakes, (node.index, line, "remote write without earlier intake")
                        prior = intakes[m[1]][-1]
                        assert prior[0] < pos
                        captures = [c for c in self.incoming[(node.index, m[1])] if c["payload"] == wire and c["peer"] == prior[1]]
                        assert captures, (node.index, m[1], prior, "missing peer payload")
                        event["from_peer"] = prior[1]
                        event["capture_order"] = captures[0]["order"]
                        event["recsig_hash"] = display(sha256d(bytes.fromhex(wire)))
                    else:
                        assert m[1] in recoveries and recoveries[m[1]] < pos
                        event["recovery_pos"] = recoveries[m[1]]
                    self.data["events"][str(node.index)].append(event)
                elif m := RECOVER.search(line):
                    if (m[1], m[2]) in ids:
                        recoveries[ids[(m[1], m[2])]["signHash"]] = pos
                        self.data["events"][str(node.index)].append({**base, "kind": "local_recovery",
                            "sign_hash": ids[(m[1], m[2])]["signHash"]})
                elif m := CLEANUP.search(line):
                    self.data["events"][str(node.index)].append({**base, "kind": "cleanup", "function": m[1], "deleted": int(m[2])})
            assert seen == set(known), (node.index, "missing writes", set(known) - seen)
            self.data["events"][str(node.index)].sort(key=lambda e: e["pos"])


def reprocess(raw_path, run_dir):
    capture = LaneCapture.__new__(LaneCapture)
    capture.data = json.loads(raw_path.read_text())
    was_complete = capture.data.get("complete", False)
    capture.nodes = [SimpleNamespace(index=int(node), datadir=str(run_dir / f"node{node}"))
                     for node in capture.data["signers"]]
    capture.mninfo = [SimpleNamespace(proTxHash=signer["proTxHash"], nodeIdx=int(node))
                      for node, signer in capture.data["signers"].items() if signer["proTxHash"]]
    capture.peer_history = capture.data["peer_history"]
    capture.evidence_root = run_dir.parent.parent / "evidence"
    capture.start_positions = capture.data["provenance"]["log_start_positions"]
    capture.options = SimpleNamespace(out=str(raw_path))
    capture.incoming = {}
    capture.finish_messages()
    if not was_complete:
        capture.finish_events()
    capture.data["complete"] = True
    capture.checkpoint()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--reprocess"]:
        parser = argparse.ArgumentParser(description="Reparse a stopped-node checkpoint and its unchanged native evidence")
        parser.add_argument("--reprocess", type=Path, metavar="RAW")
        parser.add_argument("run_dir", type=Path)
        options = parser.parse_args()
        reprocess(options.reprocess, options.run_dir)
    else:
        LaneCapture().main()
