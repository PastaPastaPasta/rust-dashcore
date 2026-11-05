#!/usr/bin/env python3
"""Fetch EV2b's w2 bench window (D32): a mainnet list and the blocks after it, with what the
build needs to replay them from that list.

Writes, gzipped, to OUT:
- `list <parent height> <hex>`: the list after the parent block, as `protx listdiff 1 <parent>`
  prints it, re-encoded the way evodb stores it (`encode_list`: internal ids and
  nTotalRegisteredCount derived, as dash-oracle-sys/scripts/dml-listdiff-check.py documents);
- `header <height> <hex>`: the headers from the oldest quorum base block a window block's
  commitment names up to the parent, exclusive;
- `quorum <llmqType> <quorum hash> <member>...`: for every non-null commitment in the window,
  the quorum's members in Core's order (`quorum_members`: CalculateQuorum over the list at the
  base block), each as evodb stores it;
- `<height> <hex>`: the parent block, then each window block.

`node-l1`'s `dml_window` and `dash-dml`'s `dml_window_fixture` read this. The reads are those
dml-listdiff-check.py makes (getblockhash, getblockheader, getblock and `protx listdiff`, the
pure reads of LQ1 §6.3.6), one at a time. Like that script, this refuses non-null commitments of
rotated quorums (their members come from quorum snapshots, which no pure read gives), so a
window must end before DIP24's first rotated commitment, and masternode states other than
versions 1 and 2.

With --stream it prints a window from the default list instead (EV2b D33, the first §6.4
window): the parent block, then for each block the `quorum` lines of the commitments it mines
followed by the block, so `dml_window` reads it as it comes.

Usage: dml-w2-fetch.py PARENT COUNT OUT.gz
       dml-w2-fetch.py --stream PARENT COUNT
"""
import gzip
import importlib.util
import sys
from pathlib import Path

CHECK = Path(__file__).resolve().parents[2] / "dash-oracle-sys" / "scripts" / "dml-listdiff-check.py"


def load_check():
    sys.dont_write_bytecode = True  # no __pycache__ beside the imported script
    spec = importlib.util.spec_from_file_location("dml_listdiff_check", CHECK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def commitments(check, chain, height):
    """Block height's commitments: (llmqType, quorum hash, base height, null) each."""
    net = chain.net
    found = []
    for tx in chain.rpc.call("getblock", chain.block_hash(height), 2)["tx"]:
        if tx.get("type") != 6:
            continue
        payload = tx["qcTx"]
        qc = payload["commitment"]
        llmq_type = qc["llmqType"]
        null = qc["signersCount"] == 0 and qc["validMembersCount"] == 0 and int(qc["quorumVvecHash"], 16) == 0
        if llmq_type in net["llmqs"]:
            interval = net["llmqs"][llmq_type][1]
        elif llmq_type in net["rotated_llmqs"] and null:
            interval = net["rotated_llmqs"][llmq_type]
        elif llmq_type in net["rotated_llmqs"]:
            raise ValueError(f"block {height}: a non-null commitment of rotated LLMQ type {llmq_type}")
        else:
            raise ValueError(f"block {height}: LLMQ type {llmq_type} is not supported")
        # Kept for null commitments too, which only makes the header prefix longer.
        base = check.quorum_base_height(payload, interval)
        assert chain.block_hash(base) == qc["quorumHash"], f"block {height}: the quorum base block"
        found.append((llmq_type, qc["quorumHash"], base, null))
    return found


def quorum_line(check, llmq_type, quorum_hash, members):
    return f"quorum {llmq_type} {check.internal(quorum_hash).hex()} {' '.join(members)}\n"


def stream(check, chain, parent, count):
    # `dml_window` starts a stream from the empty list Core holds before DIP3's activation.
    assert parent < chain.net["dip3_height"], "a stream starts before DIP3's activation"
    out = sys.stdout
    out.write(f"{parent} {chain.rpc.call('getblock', chain.block_hash(parent), 0)}\n")
    seen = set()
    for height in range(parent + 1, parent + count + 1):
        for llmq_type, quorum_hash, base, null in commitments(check, chain, height):
            if not null and (llmq_type, quorum_hash) not in seen:
                seen.add((llmq_type, quorum_hash))
                out.write(quorum_line(check, llmq_type, quorum_hash, check.quorum_members(chain, llmq_type, base)))
        out.write(f"{height} {chain.rpc.call('getblock', chain.block_hash(height), 0)}\n")
        if height % 1000 == 0:
            print(f"fetched through {height}, {len(seen)} quorums", file=sys.stderr, flush=True)
    out.flush()


def main():
    check = load_check()
    net = check.NETWORKS["mainnet"]
    chain = check.Chain(check.Rpc(net["rpc_port"], net["cookie"]), net)
    if sys.argv[1] == "--stream":
        return stream(check, chain, int(sys.argv[2]), int(sys.argv[3]))
    parent, count, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    first = parent
    quorums = {}
    blocks = []
    for height in range(parent, parent + count + 1):
        blocks.append((height, chain.rpc.call("getblock", chain.block_hash(height), 0)))
        if height == parent:
            continue
        for llmq_type, quorum_hash, base, null in commitments(check, chain, height):
            first = min(first, base)
            if not null and (llmq_type, quorum_hash) not in quorums:
                quorums[(llmq_type, quorum_hash)] = check.quorum_members(chain, llmq_type, base)
        if height % 100 == 0:
            print(f"fetched through {height}, {len(quorums)} quorums", file=sys.stderr, flush=True)
    with gzip.open(out, "wt") as f:
        f.write(f"list {parent} {check.encode_list(chain.list_json(parent), parent, chain).hex()}\n")
        for height in range(first, parent):
            f.write(f"header {height} {chain.rpc.call('getblockheader', chain.block_hash(height), False)}\n")
        for (llmq_type, quorum_hash), members in quorums.items():
            f.write(quorum_line(check, llmq_type, quorum_hash, members))
        for height, block in blocks:
            f.write(f"{height} {block}\n")
    print(f"{count} blocks after {parent}, headers from {first}, {len(quorums)} quorums", file=sys.stderr)


if __name__ == "__main__":
    main()
