#!/usr/bin/env python3
"""Package the rotated-quorum captures (test-vectors/lq1-rot/<network>-<cycle>.json.gz;
node-program LQ1b, LQ1 §6.4): one LLMQ_60_75 cycle per era and network, holding what a node
reads to compute the cycle's 32 quorums and what Core computed from it.

Inputs, both produced by Dash Core:
- the pure-read collection of collect-llmq-rotation.py (headers, the lists after the work blocks
  as `protx listdiff` printed them, re-encoded the way evodb stores them, the work blocks'
  coinbases, the mined rotated commitments);
- the replay output of node-l1's `rotated_history_matches_core` (l1_llmq_history.rs): the row
  Core's context stored for each cycle and the members it computed for the capture cycles (the
  replay has already checked them against the node and verified each mined commitment with them).

A capture holds:
- `headers`: the headers from `first_height` (the cycle's base - 3 * 288 - 8) through the last
  commitment's mined block;
- `dmns` and `lists`: the lists Core reads (the cycle's work block always; a previous cycle's
  work block only when that cycle has a row), as distinct evodb masternode records (internal ids
  are fixed per proTxHash across the collection, so a masternode unchanged between lists is one
  record) and, per list, its head (block hash, height, registered count) and its records in
  order: the list's evodb encoding is the head, CompactSize(count), then the records;
- `coinbases`: those work blocks' coinbase transactions;
- `rows`: the rows of the previous three cycles Core stored (absent before the first rotated
  cycle), and `row`, the one it stores for this cycle;
- `quorums`: per index, the base block, the members Core computed (proTxHash, RPC order) and
  the mined commitment with its height (none for a failed DKG) and whether it verifies with
  those members (signatures included);
- `triggers` (LQ1 §6.4): the skip modes of the rows used and whether the cycle is pre-V19.

Usage: gen-llmq-rotation-captures.py --collection DIR --replay DIR --out test-vectors/lq1-rot
"""

import argparse
import gzip
import importlib.util
import json
from pathlib import Path

spec = importlib.util.spec_from_file_location("dml", Path(__file__).with_name("dml-listdiff-check.py"))
dml = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dml)

INTERVAL = 288
WORK_DIFF_DEPTH = 8
V19 = {"mainnet": 1_899_072, "testnet": 850_100}


def read_varint(b, i):
    """Core's VARINT (VarIntMode::DEFAULT)."""
    n = 0
    while True:
        c = b[i]
        i += 1
        n = (n << 7) | (c & 0x7F)
        if not c & 0x80:
            return n, i
        n += 1


def read_compact(b, i):
    n = b[i]
    if n < 0xFD:
        return n, i + 1
    width = {0xFD: 2, 0xFE: 4, 0xFF: 8}[n]
    return int.from_bytes(b[i + 1 : i + 1 + width], "little"), i + 1 + width


def split_list(enc, ids):
    """An evodb list's head (block hash, height, registered count) and its records
    (dml-listdiff-check.py's encode_dmn layout: state versions 1 and 2, one CService). Each
    record must start with a proTxHash and the internal id the collection gave it (`ids`), so a
    wrong split cannot go unnoticed."""
    n, i = read_compact(enc, 32 + 4 + 4)
    records = []
    for _ in range(n):
        start = i
        internal_id, i = read_varint(enc, i + 32)
        pro_tx_hash = enc[start : start + 32][::-1].hex()
        assert ids.get(pro_tx_hash) == internal_id, f"record at {start}: not a masternode record"
        i += 32 + 4 + 2 + 7 * 4 + 2 + 32 + 32 + 20 + 48 + 20 + 18
        for _ in range(2):  # payout and operator payout scripts
            size, i = read_compact(enc, i)
            i += size
        i += 20 + 2 + 2 + 2  # platform node id and ports, the masternode type
        records.append(enc[start:i])
    assert i == len(enc), "bytes after the last record"
    assert len({r[:32] for r in records}) == n, "a masternode twice in one list"
    return enc[:40], records


def rebuild_list(head, records):
    return b"".join([head, dml.compact_size(len(records))] + records)


def by_cycle(path):
    """The jsonl records of `path`, keyed by their cycle."""
    records = (json.loads(line) for line in path.read_text().splitlines())
    return {rec["cycle"]: rec for rec in records}


def row_mode(row_hex):
    """A row's skip list mode (its first i32)."""
    return int.from_bytes(bytes.fromhex(row_hex)[:4], "little", signed=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--collection", required=True)
    ap.add_argument("--replay", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    for net in ("mainnet", "testnet"):
        col = Path(a.collection) / net
        meta = json.loads((col / "meta.json").read_text())
        first = meta["first"]
        headers = (col / "headers.bin").read_bytes()
        ids = json.loads((col / "ids.json").read_text())
        cycles = by_cycle(col / "cycles.jsonl")
        replay = by_cycle(Path(a.replay) / f"{net}-replay.jsonl")
        for cycle in meta["captures"]:
            rec, rep = cycles[cycle], replay[cycle]
            prev = [cycle - k * INTERVAL for k in (1, 2, 3)]
            # A previous cycle has no row before DIP0024 and where Core returned early (28e). Core
            # stops at the first missing one, so later rows kept after a gap are unused inputs.
            rows = [{"cycle": c, "snapshot": replay[c]["row"]} for c in prev if replay.get(c, {}).get("row")]
            works = [cycle - WORK_DIFF_DEPTH] + [r["cycle"] - WORK_DIFF_DEPTH for r in rows]
            dmns, index, lists = [], {}, []
            for w in works:
                with gzip.open(col / "lists" / f"{w}.bin.gz") as f:
                    enc = f.read()
                head, records = split_list(enc, ids)
                entries = []
                for r in records:
                    if r not in index:
                        index[r] = len(dmns)
                        dmns.append(r)
                    entries.append(index[r])
                assert rebuild_list(head, [dmns[e] for e in entries]) == enc  # the dedup round-trips
                lists.append({"height": w, "head": head.hex(), "entries": entries})
            mined = {q["index"]: q for q in rec["mined"]}
            first_height = cycle - 3 * INTERVAL - WORK_DIFF_DEPTH
            last = max([cycle + 31] + [q["mined_height"] for q in rec["mined"]])
            quorums = []
            for i, members in enumerate(rep["members"]):
                q = {"index": i, "base_height": cycle + i, "members": members}
                if i in mined:
                    q["commitment"] = mined[i]["commitment"]
                    q["mined_height"] = mined[i]["mined_height"]
                    q["verifies"] = i not in rep["rejected"]
                quorums.append(q)
            capture = {
                "network": net,
                "chain": meta["chain"],
                "cycle": cycle,
                "first_height": first_height,
                "headers": headers[(first_height - first) * 80 : (last + 1 - first) * 80].hex(),
                "dmns": [d.hex() for d in dmns],
                "lists": lists,
                "coinbases": [{"height": w, "tx": cycles[w + WORK_DIFF_DEPTH]["coinbase"]} for w in works],
                "rows": rows,
                "row": rep["row"],
                "quorums": quorums,
                "triggers": {
                    "row_modes": [row_mode(r["snapshot"]) for r in rows],
                    "new_row_mode": row_mode(rep["row"]) if rep["row"] else None,
                    "pre_v19": cycle + 1 < V19[net],
                },
            }
            path = out / f"{net}-{cycle}.json.gz"
            with gzip.GzipFile(path, "wb", compresslevel=9, mtime=0) as f:
                f.write(json.dumps(capture, separators=(",", ":")).encode())
            print(
                f"{path}: {len(dmns)} records, {len(lists)} lists, {len(rows)} rows, "
                f"{sum('commitment' in q for q in quorums)} commitments, {path.stat().st_size} bytes"
            )


if __name__ == "__main__":
    main()
