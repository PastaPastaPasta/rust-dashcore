#!/usr/bin/env python3
# Copyright (c) 2026 The Dash Core developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or http://www.opensource.org/licenses/mit-license.php.
'''
feature_mn_nonrecovery.py (rust-dashcore's contrib/node/mn-soak, SG1-NR; not a Core test)

Proves that rdashd's sig share recovers a signature while rdashd is a non-recovery member.
mn-soak.py nonrec copies this file and feature_mn_soak.py (whose network bootstrap and DKG
cycle it reuses) into func-compat's G2 test tree and runs it with the mask "mn:K".

With SPORK_21 on, Core announces no sig shares: a member sends its share only to the recovery
members, in the order of CSigSharesManager::GetRecoveryMemberOrder, and only the first
recoveryMembers of that order recover (v24.0.2 llmq/signing_shares.cpp:631-660, :766-791,
:1025-1083).

Case A, llmq_test_dip0024 (size 4, threshold 3, recoveryMembers 3; v24.0.2 llmq/params.h):
- non-recovery requests: an id whose selected quorum holds rdashd as its last recovery-order
  member, signed by rdashd and threshold - 1 Core members only. It can recover only with
  rdashd's share, which reaches a recovery member only on rdashd's concentrated send;
- control: rdashd among the first recoveryMembers, signed the same way;
- fail-closed: a non-recovery id signed by the threshold - 1 Core members only, which must
  not recover.
The computed order is checked against every asked Core member's "sending to" line.

Case B, llmq_test (ChainLocks, 2 of 3): one Core member of a quorum rdashd is in is stopped,
blocks are mined until that quorum signs a ChainLock, and the CLSIG must form on every running
node with rdashd's TrySignChainTip line for the block.

Writes --soak-out/nonrec.jsonl (one line per request and per case B height),
--soak-out/nonrec.json (the verdict) and --soak-out/evidence/ (each request's log lines).
'''

import json
import os
import re
import struct
import time

from test_framework.messages import hash256
from feature_mn_soak import CHAINLOCKS, CYCLE, POLL, ROTATION, THRESHOLD, TYPES, MnSoak, clsig_request_id

# v24.0.2 llmq/params.h: llmq_test_dip0024 and llmq_test.
RECOVERY_MEMBERS = {ROTATION: 3, CHAINLOCKS: 3}
SIGN_TIMEOUT = 30
FAIL_CLOSED_TIMEOUT = 45
FAIL_CLOSED_BUMP = 3  # mocktime seconds per poll: past every recovery attempt's wait (2 s, doubling, 10 s cap)
ID_TRIES = 400
CL_TIMEOUT = 90
CL_BLOCKS = 40
SEND = re.compile(r"CollectSigSharesToSendConcentrated -- signHash=([0-9a-f]{64}), sending to ([0-9a-f]{64}), attempt=(\d+)")
SKIPPED = re.compile(r"CollectSigSharesToSendConcentrated -- signHash=([0-9a-f]{64}), skipped \d+ unreachable")
SHARE_IN = re.compile(r"ProcessMessageSigShare -- signHash=([0-9a-f]{64}), id=[0-9a-f]{64}, msgHash=[0-9a-f]{64}, member=(\d+), node=(\d+)")
RECOVERED = re.compile(r"TryRecoverSig -- recovered signature\. id=([0-9a-f]{64}), msgHash=([0-9a-f]{64})")


def recovery_order(members, rid):
    """CSigSharesManager::GetRecoveryMemberOrder (v24.0.2 llmq/signing_shares.cpp:766-782): the
    members sorted by SerializeHash((proTxHash, id)) as uint256s, which compare by memcmp over
    their serialized bytes. Hashes are in RPC (display) order, the reverse of serialized."""
    rid_bytes = bytes.fromhex(rid)[::-1]
    return sorted(members, key=lambda m: hash256(bytes.fromhex(m)[::-1] + rid_bytes))


def sign_hash(llmq_type, quorum, rid, msg):
    """llmq::SignHash (v24.0.2 llmq/signhash.cpp): SerializeHash((uint8 type, quorumHash, id,
    msgHash)), in display order."""
    data = bytes([llmq_type]) + b"".join(bytes.fromhex(h)[::-1] for h in (quorum, rid, msg))
    return hash256(data)[::-1].hex()


def request_id(tag, base, k):
    return hash256(tag + struct.pack("<ii", base, k))[::-1].hex()


def request_msg(rid):
    return hash256(b"nonrec-msg" + bytes.fromhex(rid))[::-1].hex()


def classify(position, recovery_members):
    """'control' for a recovery member, 'nonrec' otherwise."""
    return "control" if position < recovery_members else "nonrec"


def first_send(lines, shash):
    """The proTxHash a node's first concentrated send of `shash` went to (attempt 1), or None;
    None too if the node skipped an unreachable recovery member first."""
    if any(m and m.group(1) == shash for m in map(SKIPPED.search, lines)):
        return None
    for line in lines:
        m = SEND.search(line)
        if m and m.group(1) == shash and m.group(3) == "1":
            return m.group(2)
    return None


def shares_from(lines, shash, member):
    """The peer ids a node received quorum member `member`'s share of `shash` from."""
    return sorted({int(m.group(3)) for m in map(SHARE_IN.search, lines)
                   if m and m.group(1) == shash and int(m.group(2)) == member})


def recovered_here(lines, rid, msg):
    return any(m and m.group(1) == rid and m.group(2) == msg for m in map(RECOVERED.search, lines))


def recovered(row):
    """A non-recovery or control request counts as recovered only when a Core recovery member
    (`core_recovery_nodes`: the first recoveryMembers of the order, rdashd excluded) holds the
    recovered sig, so rdashd's own hasrecsig never vouches for it. A fail-closed request counts
    as recovered when any node, rdashd included, holds it."""
    held = row["recovered_after_s"]
    if row["kind"] == "fail-closed":
        return bool(held)
    return any(str(n) in held for n in row["core_recovery_nodes"])


def verdict(rows, need_nonrec, need_quorums, rdashd_send=False):
    """Case A's verdict over its rows: PASS when every non-recovery and control request
    recovered on a Core recovery member, no fail-closed request recovered anywhere, no request's
    recovered sigs differ between nodes, there are at least `need_nonrec` non-recovery requests
    over `need_quorums` quorums, and every order check that logged agreed. With `rdashd_send`,
    every non-recovery request also needs rdashd's own first "sending to" line naming the first
    recovery member."""
    nonrec = [r for r in rows if r["kind"] == "nonrec"]
    control = [r for r in rows if r["kind"] == "control"]
    closed = [r for r in rows if r["kind"] == "fail-closed"]
    checks = [c for r in rows for c in r["order_check"]]
    problems = []
    if len(nonrec) < need_nonrec:
        problems.append(f"{len(nonrec)} non-recovery requests (< {need_nonrec})")
    if len({r["quorum"] for r in nonrec}) < need_quorums:
        problems.append(f"non-recovery requests over {len({r['quorum'] for r in nonrec})} quorum(s) (< {need_quorums})")
    problems += [f"non-recovery {r['id']} did not recover on a Core recovery member" for r in nonrec if not recovered(r)]
    problems += [f"control {r['id']} did not recover on a Core recovery member" for r in control if not recovered(r)]
    problems += [f"fail-closed {r['id']} recovered" for r in closed if recovered(r)]
    problems += [f"{r['kind']} {r['id']}: recovered sigs differ between nodes" for r in rows if r["distinct_sigs"] > 1]
    if not control:
        problems.append("no control request")
    if not closed:
        problems.append("no fail-closed request")
    if not checks:
        problems.append("no Core send line to check the recovery order against")
    problems += [f"order check: node {c['node']} sent {c['logged']} first, computed {c['expected']}"
                 for c in checks if not c["ok"]]
    if rdashd_send:
        problems += [f"non-recovery {r['id']}: rdashd's first send {r['rdashd_send']['logged']}, expected {r['rdashd_send']['expected']}"
                     for r in nonrec if not r["rdashd_send"]["ok"]]
    return ("PASS" if not problems else "FAIL"), problems


class MnNonRecovery(MnSoak):
    def add_options(self, parser):
        super().add_options(parser)
        parser.add_argument("--nonrec-requests", type=int, default=12, help="non-recovery requests (case A)")
        parser.add_argument("--nonrec-quorums", type=int, default=2, help="distinct quorums they must span")
        parser.add_argument("--nonrec-per-cycle", type=int, default=6, help="non-recovery requests per cycle")
        parser.add_argument("--nonrec-max-cycles", type=int, default=10, help="DKG cycles before giving up")
        parser.add_argument("--nonrec-skip-b", action="store_true", help="skip case B")
        parser.add_argument("--nonrec-rdashd-send", action="store_true",
                            help="require rdashd's own llmq-sigs 'sending to' line for each non-recovery request")

    def set_test_params(self):
        super().set_test_params()

    def run_test(self):
        self.setup_soak()
        out = self.options.soak_out
        self.evidence_dir = os.path.join(out, "evidence")
        os.makedirs(self.evidence_dir, mode=0o700, exist_ok=True)
        self.nonrec = open(os.path.join(out, "nonrec.jsonl"), "a", encoding="utf8")
        rows, cycles = [], 0
        want = self.options.nonrec_requests
        while cycles < self.options.nonrec_max_cycles:
            self.record(self.cycle())
            cycles += 1
            rows += self.case_a_cycle(rows)
            nonrec = [r for r in rows if r["kind"] == "nonrec"]
            if len(nonrec) >= want and len({r["quorum"] for r in nonrec}) >= self.options.nonrec_quorums:
                break
        result_a, problems = verdict(rows, want, self.options.nonrec_quorums, self.options.nonrec_rdashd_send)
        result = {"case_a": result_a, "case_a_problems": problems, "cycles": cycles,
                  "rdashd_protx": self.rust.proTxHash, "rdashd_node": self.rust.nodeIdx}
        self.write_result(result)
        if result_a != "PASS":
            raise AssertionError(f"case A {result_a}: {problems}")
        if not self.options.nonrec_skip_b:
            result["case_b"] = self.case_b()
            self.write_result(result)
        self.nonrec.close()
        self.jsonl.close()

    def write_result(self, result):
        with open(os.path.join(self.options.soak_out, "nonrec.json"), "w", encoding="utf8") as f:
            json.dump(result, f, indent=1, sort_keys=True)

    def emit(self, row):
        self.nonrec.write(json.dumps(row, sort_keys=True) + "\n")
        self.nonrec.flush()

    # ---- case A -------------------------------------------------------------------------------

    def rdashd_quorums(self, llmq_type):
        """Active quorums of `llmq_type` with rdashd a valid member: hash -> (member proTxHashes
        in quorum order, quorumIndex)."""
        node0 = self.nodes[0]
        found = {}
        for qh in node0.quorum("list").get(TYPES[llmq_type], []):
            info = node0.quorum("info", llmq_type, qh)
            members = [m["proTxHash"] for m in info["members"]]
            valid = {m["proTxHash"] for m in info["members"] if m.get("valid")}
            if self.rust.proTxHash in valid and len(valid) == len(members):
                found[qh] = (members, info.get("quorumIndex"))
        return found

    def case_a_cycle(self, rows):
        """This cycle's requests: up to --nonrec-per-cycle non-recovery ones, one control and one
        fail-closed, each on an id whose selected quorum holds rdashd."""
        node0 = self.nodes[0]
        base = node0.getblockcount() - node0.getblockcount() % CYCLE
        quorums = self.rdashd_quorums(ROTATION)
        if not quorums:
            self.emit({"kind": "no-quorum", "base": base, "list": node0.quorum("list").get(TYPES[ROTATION], [])})
            return []
        left = self.options.nonrec_requests - sum(1 for r in rows if r["kind"] == "nonrec")
        quota = {"nonrec": max(1, min(left, self.options.nonrec_per_cycle)), "control": 1, "fail-closed": 1}
        per_quorum = {"nonrec": -(-quota["nonrec"] // len(quorums))}
        taken, done = {}, []
        for k in range(ID_TRIES):
            if all(sum(1 for r in done if r[0] == kind) >= n for kind, n in quota.items()):
                break
            rid = request_id(b"mn-nonrec", base, k)
            qh = node0.quorum("selectquorum", ROTATION, rid)["quorumHash"]
            if qh not in quorums:
                continue
            members, qindex = quorums[qh]
            position = recovery_order(members, rid).index(self.rust.proTxHash)
            kind = classify(position, RECOVERY_MEMBERS[ROTATION])
            if kind == "nonrec" and sum(1 for r in done if r[0] == "nonrec") >= quota["nonrec"]:
                kind = "fail-closed"
            count = sum(1 for r in done if r[0] == kind)
            if count >= quota[kind]:
                continue
            if kind == "nonrec" and taken.get(qh, 0) >= per_quorum["nonrec"] and len(quorums) > 1 and k < ID_TRIES // 2:
                continue
            if kind == "nonrec":
                taken[qh] = taken.get(qh, 0) + 1
            done.append((kind, rid, qh, members, qindex, position))
        out = []
        for kind, rid, qh, members, qindex, position in sorted(done, key=lambda d: d[0] == "fail-closed"):
            core = [self.by_protx[m] for m in members if m != self.rust.proTxHash]
            core = core[:THRESHOLD[ROTATION] - 1]
            asked = core if kind == "fail-closed" else [self.rust] + core
            row = self.request(kind, base, rid, qh, qindex, members, position, asked)
            self.emit(row)
            out.append(row)
        return out

    def node_log(self, node):
        return os.path.join(node.datadir, self.chain, "debug.log")

    def log_sizes(self, nodes):
        sizes = {}
        for node in nodes:
            try:
                sizes[node.index] = os.path.getsize(self.node_log(node))
            except OSError:
                sizes[node.index] = 0
        return sizes

    def log_since(self, node, offset):
        try:
            with open(self.node_log(node), "rb") as f:
                f.seek(offset)
                return f.read().decode("utf8", errors="replace").splitlines()
        except OSError:
            return []

    def request(self, kind, base, rid, qh, qindex, members, position, asked_mns):
        msg = request_msg(rid)
        shash = sign_hash(ROTATION, qh, rid, msg)
        order = recovery_order(members, rid)
        nodes = self.all_nodes()
        offsets = self.log_sizes(nodes)
        asked = {}
        for mn in asked_mns:
            try:
                asked[str(mn.nodeIdx)] = mn.get_node(self).quorum("sign", ROTATION, rid, msg, qh)
            except Exception as e:
                asked[str(mn.nodeIdx)] = repr(e)[:160]
        member_nodes = {str(self.names[m]) for m in members}
        timeout = FAIL_CLOSED_TIMEOUT if kind == "fail-closed" else SIGN_TIMEOUT
        begin, seen = time.time(), {}
        while time.time() - begin < timeout:
            for node in nodes:
                if str(node.index) in seen:
                    continue
                try:
                    if node.quorum("hasrecsig", ROTATION, rid, msg):
                        seen[str(node.index)] = round(time.time() - begin, 2)
                except Exception:
                    pass
            if kind != "fail-closed" and member_nodes <= set(seen) and len(seen) == len(nodes):
                break
            if kind != "fail-closed" and member_nodes <= set(seen) and time.time() - begin > min(timeout, max(seen.values()) + 5):
                break
            if kind == "fail-closed":
                self.bump_mocktime(FAIL_CLOSED_BUMP)
            time.sleep(POLL)
        sigs = set()
        for node in nodes:
            try:
                sigs.add(node.quorum("getrecsig", ROTATION, rid, msg)["sig"])
            except Exception:
                pass
        rust_member = members.index(self.rust.proTxHash)
        core_recovery = [self.names[m] for m in order[:RECOVERY_MEMBERS[ROTATION]] if m != self.rust.proTxHash]
        lines = {node.index: self.log_since(node, offsets[node.index]) for node in nodes}
        rust_peer = self.rdashd_peer_ids(nodes)
        checks, share_in, recovered_by = [], {}, []
        for node in nodes:
            ls = lines[node.index]
            if recovered_here(ls, rid, msg):
                recovered_by.append(node.index)
            got = shares_from(ls, shash, rust_member)
            if got:
                share_in[str(node.index)] = {"peers": got, "from_rdashd_peer": rust_peer.get(node.index) in got}
            if str(node.index) in asked and node is not self.rnode:
                sent = first_send(ls, shash)
                if sent is not None:
                    checks.append({"node": node.index, "logged": sent, "expected": order[0], "ok": sent == order[0]})
        evidence = os.path.join(self.evidence_dir, f"{kind}-{rid[:16]}.log")
        with open(evidence, "w", encoding="utf8") as f:
            for node in nodes:
                for line in lines[node.index]:
                    if shash in line or rid in line:
                        f.write(f"node{node.index}: {line}\n")
        first = min(seen, key=seen.get) if seen else None
        rdashd_sent = first_send(lines[self.rnode.index], shash)
        row = {"kind": kind, "base": base, "id": rid, "msg": msg, "sign_hash": shash, "quorum": qh,
                "quorum_index": qindex, "rdashd_position": position,
                "order": [self.names[m] for m in order], "asked": asked, "core_recovery_nodes": core_recovery,
                "recovered_after_s": seen, "first_holder": first, "recovered_by": recovered_by,
                "rdashd_share_received": share_in, "order_check": checks, "distinct_sigs": len(sigs),
                "rdashd_send": {"logged": rdashd_sent, "expected": order[0], "ok": rdashd_sent == order[0],
                                "to_node": self.names.get(rdashd_sent)},
                "rdashd_lines": [line[:300] for line in lines[self.rnode.index] if shash in line or rid in line],
                "evidence": evidence}
        row["recovered"] = recovered(row)
        return row

    def rdashd_peer_ids(self, nodes):
        """Each Core node's peer id for its connection to rdashd (verified proRegTx hash)."""
        ids = {}
        for node in nodes:
            if node is self.rnode:
                continue
            try:
                for p in node.getpeerinfo():
                    if p.get("verified_proregtx_hash") == self.rust.proTxHash:
                        ids[node.index] = p["id"]
            except Exception:
                pass
        return ids

    # ---- case B -------------------------------------------------------------------------------

    def case_b(self):
        """Stop one Core member of an llmq_test quorum rdashd is in, then mine until that quorum
        signs the tip's ChainLock: 2 of 3 with rdashd and one Core member left."""
        quorums = self.rdashd_quorums(CHAINLOCKS)
        if not quorums:
            self.record(self.cycle())
            quorums = self.rdashd_quorums(CHAINLOCKS)
        if not quorums:
            return {"result": "DROPPED", "why": "no active llmq_test quorum with rdashd"}
        qh, (members, _) = sorted(quorums.items())[0]
        core = [self.by_protx[m] for m in members if m != self.rust.proTxHash]
        stopped, left = core[0], core[1]
        self.stop_node(stopped.nodeIdx)
        running = [n for n in self.all_nodes() if n.index != stopped.nodeIdx]
        heights = []
        for _ in range(CL_BLOCKS):
            offsets = self.log_sizes(running)
            self.bump_mocktime(1, nodes=running)
            self.generate(self.nodes[0], 1, sync_fun=lambda: self.sync_blocks(running))
            h = self.nodes[0].getblockcount()
            rid = clsig_request_id(h)
            if self.nodes[0].quorum("selectquorum", CHAINLOCKS, rid)["quorumHash"] != qh:
                continue
            row = self.chainlock(h, rid, qh, members, running, offsets, stopped, left)
            self.emit(row)
            heights.append(row)
            if len(heights) >= 2:
                break
        ok = bool(heights) and all(r["locked"] and r["agree"] and r["rdashd_tried"] for r in heights)
        return {"result": "PASS" if ok else ("DROPPED" if not heights else "FAIL"), "quorum": qh,
                "members": [self.names[m] for m in members], "stopped": stopped.nodeIdx, "left": left.nodeIdx,
                "heights": [r["height"] for r in heights]}

    def chainlock(self, h, rid, qh, members, running, offsets, stopped, left):
        blockhash = self.nodes[0].getblockhash(h)
        begin, best = time.time(), {}
        locked = False
        while time.time() - begin < CL_TIMEOUT:
            best = {}
            for node in running:
                try:
                    best[str(node.index)] = node.getbestchainlock()["blockhash"]
                except Exception as e:
                    best[str(node.index)] = repr(e)[:80]
            if all(v == blockhash for v in best.values()):
                locked = True
                break
            self.bump_mocktime(1, nodes=running)
            time.sleep(POLL)
        lines = {node.index: self.log_since(node, offsets[node.index]) for node in running}
        tried = [line[:300] for line in lines[self.rnode.index] if "TrySignChainTip -- trying to sign" in line and blockhash in line]
        msg_hash = blockhash
        shash = sign_hash(CHAINLOCKS, qh, rid, msg_hash)
        rust_member = members.index(self.rust.proTxHash)
        share_in = {str(n.index): shares_from(lines[n.index], shash, rust_member) for n in running}
        holders = []
        for node in running:
            try:
                if node.quorum("hasrecsig", CHAINLOCKS, rid, msg_hash):
                    holders.append(node.index)
            except Exception:
                pass
        evidence = os.path.join(self.evidence_dir, f"chainlock-{h}.log")
        with open(evidence, "w", encoding="utf8") as f:
            for node in running:
                for line in lines[node.index]:
                    if shash in line or rid in line or blockhash in line:
                        f.write(f"node{node.index}: {line}\n")
        return {"kind": "chainlock", "height": h, "blockhash": blockhash, "quorum": qh,
                "members": [self.names[m] for m in members], "stopped": stopped.nodeIdx, "left": left.nodeIdx,
                "locked": locked, "locked_after_s": round(time.time() - begin, 2) if locked else None,
                "agree": len(set(best.values())) == 1, "best": best, "rdashd_tried": bool(tried),
                "rdashd_lines": tried, "rdashd_share_received": {k: v for k, v in share_in.items() if v},
                "recsig_holders": holders, "recovered_by": [n.index for n in running if recovered_here(lines[n.index], rid, msg_hash)],
                "evidence": evidence}


if __name__ == '__main__':
    MnNonRecovery().main()
