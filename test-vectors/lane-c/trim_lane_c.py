#!/usr/bin/env python3
"""Deterministically compact one successful capture; never repair missing evidence."""
import argparse
import copy
import hashlib
import json
from pathlib import Path


def hash_hex(payload):
    return hashlib.sha256(hashlib.sha256(payload).digest()).digest()[::-1].hex()


def trim(raw):
    data = copy.deepcopy(raw)
    assert data["complete"] is True
    assert data["schema_version"] == 2
    assert set(data["phases"]) == {"ring", "mesh"}
    assert len(data["signers"]) == 12
    assert sum(signer["evo"] for signer in data["signers"].values()) == 3
    assert len(data["operators"]) == 11
    for phase in data["phases"].values():
        types = [q["type"] for q in phase["quorums"]]
        assert types.count(100) >= 2 and types.count(103) >= 2 and types.count(106) >= 1
    assert len(data["selection_window"]) == 18
    chain = data.pop("chain")
    assert len(chain) == data["tip_height"] + 1
    assert all(bytes.fromhex(block) for block in chain)
    data["provenance"]["raw_canonical_sha256"] = hashlib.sha256(
        json.dumps(raw, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    quorums = [q for phase in data["phases"].values() for q in phase["quorums"]]
    quorums += list(data["signing_quorums"].values())
    available = {(q["type"], q["quorumHash"]): q for q in quorums}
    referenced = set(available)
    referenced.update((request["type"], request["quorumHash"]) for request in data["requests"])
    referenced.update((100, result["quorumHash"])
                      for window in data["selection_window"] for result in window["results"])
    for key in referenced:
        quorum = available.get(key)
        assert quorum and quorum.get("qcontrib") and len(quorum["qcontrib"]) == {100: 7, 103: 4, 106: 3}[key[0]]
        assert len(quorum.get("commitment_transactions", [])) == 1
        assert quorum["commitment_transactions"][0]["qcTx"]["commitment"]["quorumHash"] == key[1]
    for q in quorums:
        for status in (q.get("dkgstatus") or {}).values():
            status.pop("time", None)
            status.pop("timeStr", None)
        transactions = q.get("commitment_transactions", [])
        matching = [tx for tx in transactions if tx["qcTx"]["commitment"]["llmqType"] == q["type"]
                    and tx["qcTx"]["commitment"]["quorumHash"] == q["quorumHash"]]
        if transactions:
            assert len(matching) == 1
            # Core: v24.0.0 src/llmq/commitment.h:147-160 CFinalCommitmentTxPayload
            q["commitment_hex"] = matching[0]["extraPayload"][12:]
            q["commitment_transactions"] = matching
        variants = []
        nodes = {}
        for node, flags in sorted(q["quorum_info"].items(), key=lambda p: int(p[0])):
            public = flags["false"]
            secret = flags["true"].get("secretKeyShare")
            assert {k: v for k, v in flags["true"].items() if k != "secretKeyShare"} == public
            if public not in variants:
                variants.append(public)
            nodes[node] = {"variant": variants.index(public)}
            if secret:
                nodes[node]["secretKeyShare"] = secret
        q["quorum_info"] = {"variants": variants, "nodes": nodes}
    for request in data["requests"]:
        phase = data["control_phases"][request["control_phase"]]
        assert phase["held_mocktime"] == request["mocktime"]
        exact_submits = sum(submit["role"] == "exact" for submit in request["submissions"])
        assert exact_submits == (request["threshold"] if request["fresh_id_at_phase_start"] else 0)
        assert all(sig == request["recsig"] for sig in request.pop("recsig_by_node").values())
        sig = request["recsig"]
        prefix = bytes([sig["llmqType"]]) + b"".join(bytes.fromhex(sig[key])[::-1]
            for key in ("quorumHash", "id", "msgHash"))
        wire = prefix + bytes.fromhex(sig["sig"])
        assert hash_hex(prefix) == request["signHash"]
        # Core: v24.0.0 src/llmq/core_write.cpp:103-113 CRecoveredSig::ToJson
        assert hash_hex(bytes.fromhex(sig["sig"])) == sig["hash"]
        request["recsig_hash"] = hash_hex(wire)
        assert all(share["signHash"] == request["signHash"] for share in request["offline_shares"].values())
    data.pop("peer_history")
    # Receive-log order is local to each node, with no cross-node or send-pass ordering.
    data["sig_messages"].sort(key=lambda m: (m["to"], m["receive_pos"]))
    for node, events in data["events"].items():
        assert events == sorted(events, key=lambda e: e["pos"])
        barriers = {event["phase"]: event for event in events if event["kind"] == "barrier"}
        assert set(barriers) == set(range(len(data["control_phases"])))
        for barrier in barriers.values():
            if (barrier["barrier"] == "writes" and barrier["expected"]) or (
                    barrier["barrier"] == "cleanup" and barrier["cleanup_node"] == int(node)):
                assert barrier["polls"][-1]["result"] is True
        writes = [e["sign_hash"] for e in events if e["kind"] == "write"]
        assert len(writes) == len(set(writes)) == len(data["requests"])
    for observation in data["observations"]:
        assert "settled" not in observation and "racy" not in observation
        if observation["method"] in ("quorum sign", "quorum platformsign"):
            assert observation["sign_exact_reason"] in ("threshold_submit", "isolated_after_barrier")
    return data, chain


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("raw", type=Path)
    parser.add_argument("out_dir", type=Path)
    args = parser.parse_args()
    data, chain = trim(json.loads(args.raw.read_text()))
    args.out_dir.mkdir(parents=True, exist_ok=True)
    (args.out_dir / "regtest-v24.0.0.json").write_text(json.dumps(data, sort_keys=True, separators=(",", ":")) + "\n")
    (args.out_dir / "chain-v24.0.0.hex").write_text("\n".join(chain) + "\n")


if __name__ == "__main__":
    main()
