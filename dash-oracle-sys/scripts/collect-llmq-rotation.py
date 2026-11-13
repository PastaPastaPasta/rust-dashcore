#!/usr/bin/env python3
"""LQ1b L2 (node-program LQ1 §6.3): collect what a node reads for every rotated (LLMQ_60_75)
cycle of a network, with pure reads only (getblockhash, getblockheader, getblock 2,
`protx listdiff`), one request at a time, mainnet at or below 2,090,000.

Per network, under OUT/<net>/:
- headers.bin: the 80-byte headers of heights first .. last (meta.json has first and last);
- lists/<w>.bin.gz: the list after each cycle's work block w = cycle - 8, encoded the way evodb
  stores it (dml-listdiff-check.py's encode_dmn), with internal ids fixed per proTxHash across
  the collection (ids.json); built by applying `protx listdiff w' w`
  to the previous cycle's list, and checked against a full `protx listdiff 1 w` at the first
  cycle, every CHECK_EVERY-th cycle, the last cycle, around every capture cycle and wherever an
  operator key changed (apply_diff); a list is
  written only after the next check confirms the diffs (on a mismatch the unconfirmed lists are
  read in full);
- cycles.jsonl: per cycle its work block's coinbase transaction (getblock 2) and the non-null
  rotated commitments mined in [cycle + 40, cycle + 60) (the mining window is 42..50).

Resumable: cycles already in cycles.jsonl are skipped (the list restarts from a full read).
Usage: collect-llmq-rotation.py --out DIR --net mainnet|testnet
"""

import argparse
import gzip
import importlib.util
import json
import time
from pathlib import Path

S = Path(__file__).parent
spec = importlib.util.spec_from_file_location("llmqregtest", S / "gen-llmq-regtest.py")
regtest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(regtest)
dml = regtest.dml

MAX_MAINNET_HEIGHT = 2_090_000
INTERVAL = 288
ROT_TYPE = 5
WORK_DIFF_DEPTH = 8
CHECK_EVERY = 8

NETS = {
    "mainnet": dict(
        dml.NETWORKS["mainnet"],
        first_cycle=1_737_792,
        last_cycle=2_089_728,
        captures=[1_737_792, 1_738_656, 1_899_072, 1_987_776, 1_988_064, 2_089_728],
    ),
    "testnet": {
        "chain": "test",
        "rpc_port": 37998,
        "cookie": "/work/chains/testnet/rpc.cookie",
        "dip3_height": 7000,
        "v19_height": 850100,
        "v20_height": 905100,
        "min_confirmations": 1,
        "pubkey_prefix": 140,
        "script_prefix": 19,
        "first_cycle": 769_824,
        "last_cycle": None,  # the last cycle whose mining window the tip holds
        "captures": [769_824, 838_368, 850_176, 905_184, 1_067_040, None],
    },
}


def apply_diff(mns, diff):
    """Apply `protx listdiff`'s JSON. Its updated entries print only the fields that differ
    between the two lists, and omit some that became empty (CDeterministicMNStateDiff::ToJson):
    an empty net info prints no `addresses`, an empty operator payout script nothing. A revoked
    masternode (ResetOperatorFields, a null operator key) has both empty, so they are cleared;
    a new operator key is ambiguous (a reset, then maybe a new service) and makes the caller
    read the list in full. An evonode's platform port change without a net info change prints
    the port under a 255.255.255.255 placeholder address. Returns whether the diff carried a
    new operator key."""
    ambiguous = False
    for pro in diff["removedMNs"]:
        del mns[pro]
    for mn in diff["addedMNs"]:
        mns[mn["proTxHash"]] = mn
    for upd in diff["updatedMNs"]:
        for pro, fields in upd.items():
            st = mns[pro]["state"]
            if "pubKeyOperator" in fields:
                if fields["pubKeyOperator"].strip("0"):
                    ambiguous = True
                else:
                    st["addresses"] = {k: v for k, v in st.get("addresses", {}).items() if k != "core_p2p"}
                    st.pop("operatorPayoutAddress", None)
            for k, v in fields.items():
                if k != "addresses":
                    st[k] = v
                    continue
                addresses = st.setdefault("addresses", {})
                for purpose, entries in v.items():
                    if len(entries) == 1 and entries[0].startswith("255.255.255.255:"):
                        port = entries[0].rsplit(":", 1)[1]
                        old = addresses.get(purpose, [])
                        host = old[0].rsplit(":", 1)[0] if old else "255.255.255.255"
                        addresses[purpose] = [f"{host}:{port}"]
                    else:
                        addresses[purpose] = entries
    return ambiguous


def canon(mns):
    return json.dumps(sorted(mns.values(), key=lambda m: m["proTxHash"]), sort_keys=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--net", required=True, choices=NETS)
    a = ap.parse_args()
    net = NETS[a.net]
    rpc = dml.Rpc(net["rpc_port"], net["cookie"])
    chain = dml.Chain(rpc, net)
    out = Path(a.out) / a.net
    (out / "lists").mkdir(parents=True, exist_ok=True)

    tip = rpc.call("getblockchaininfo")["blocks"]
    last_cycle = net["last_cycle"] or (tip - 100) // INTERVAL * INTERVAL
    captures = [c if c is not None else last_cycle for c in net["captures"]]
    first = net["first_cycle"] - 3 * INTERVAL - WORK_DIFF_DEPTH
    last = last_cycle + 60
    if a.net == "mainnet":
        assert last <= MAX_MAINNET_HEIGHT
    meta = {
        "net": a.net,
        "chain": net["chain"],
        "first": first,
        "last": last,
        "first_cycle": net["first_cycle"],
        "last_cycle": last_cycle,
        "captures": captures,
        "tip_at_collection": tip,
    }

    hdr_path = out / "headers.bin"
    have = hdr_path.stat().st_size // 80 if hdr_path.exists() else 0
    if have < last - first + 1:
        t = time.time()
        with open(hdr_path, "ab") as f:
            f.truncate(have * 80)
            for h in range(first + have, last + 1):
                f.write(bytes.fromhex(rpc.call("getblockheader", chain.block_hash(h), False)))
                if (h - first) % 20000 == 0:
                    print(f"{a.net}: header {h} ({time.time() - t:.0f} s)", flush=True)
    (out / "meta.json").write_text(json.dumps(meta, indent=1))
    print(f"{a.net}: headers {first}..{last} done", flush=True)

    must_check = {c - k * INTERVAL - WORK_DIFF_DEPTH for c in captures for k in range(4)}
    jl = out / "cycles.jsonl"
    done = {json.loads(line)["cycle"] for line in jl.read_text().splitlines()} if jl.exists() else set()

    # Internal ids, one per proTxHash for the whole collection, as Core assigns them once at
    # registration: rotation mixes entries of several cycles' lists in one scratch list, whose
    # AddMN refuses a second masternode with the same id (encode_list's ids are positions in one
    # list, which differ from list to list).
    ids_path = out / "ids.json"
    ids = json.loads(ids_path.read_text()) if ids_path.exists() else {}

    def encode(mns, w):
        if w < net["dip3_height"]:
            return dml.encode_list(None, w, chain)
        order = dml.by_registration(list(mns.values()))
        for mn in order:
            ids.setdefault(mn["proTxHash"], len(ids))
        head = [dml.internal(chain.block_hash(w)), dml.i32(w), dml.u32(len(ids)), dml.compact_size(len(order))]
        return b"".join(head + [dml.encode_dmn(mn, ids[mn["proTxHash"]], w, chain) for mn in order])

    def full_list(w):
        return {m["proTxHash"]: m for m in chain.list_json(w) or []}

    def cycle_record(cycle, w):
        coinbase = rpc.call("getblock", chain.block_hash(w), 2)["tx"][0]["hex"]
        mined = []
        if cycle >= net["first_cycle"]:
            for h in range(cycle + 40, cycle + 60):
                block = rpc.call("getblock", chain.block_hash(h), 2)
                for qc, payload in regtest.mined_commitments(block):
                    if qc["llmqType"] == ROT_TYPE:
                        mined.append(
                            {
                                "index": qc["quorumIndex"],
                                "quorum_hash": qc["quorumHash"],
                                "commitment": payload.hex(),
                                "mined_height": h,
                            }
                        )
        return {"cycle": cycle, "work": w, "coinbase": coinbase, "mined": mined}

    # Lists built from diffs stay pending until a full read confirms the latest one; on a
    # mismatch every pending list is read in full instead.
    mns, mns_at, pending, refetched = None, None, [], 0
    cycles = list(range(net["first_cycle"] - 3 * INTERVAL, last_cycle + 1, INTERVAL))
    t0 = time.time()
    with open(jl, "a") as log:

        def flush():
            ids_path.write_text(json.dumps(ids))
            for rec, enc in pending:
                with gzip.open(out / "lists" / f"{rec['work']}.bin.gz", "wb", compresslevel=6) as f:
                    f.write(enc)
                log.write(json.dumps(rec) + "\n")
            log.flush()
            pending.clear()

        for n, cycle in enumerate(cycles):
            if cycle in done:
                continue
            w = cycle - WORK_DIFF_DEPTH
            check = mns is None or w in must_check or n % CHECK_EVERY == 0 or cycle == cycles[-1]
            if mns is None:
                mns = full_list(w)
            elif apply_diff(mns, rpc.call("protx", "listdiff", mns_at, w)):
                check = True
            mns_at = w
            if check:
                got = full_list(w)
                if canon(got) != canon(mns):
                    bad = sorted(k for k in set(got) | set(mns) if got.get(k) != mns.get(k))
                    print(
                        f"{a.net}: diff-applied list at {w} differs ({len(bad)} entries, e.g. {bad[:2]}); "
                        f"reading {len(pending)} pending lists in full",
                        flush=True,
                    )
                    refreshed = []
                    for rec, _ in pending:
                        full = full_list(rec["work"])
                        rec.update({"full_check": True, "mns": len(full)})
                        refreshed.append((rec, encode(full, rec["work"])))
                    pending = refreshed
                    refetched += len(pending)
                    mns = got
            rec = cycle_record(cycle, w)
            rec.update({"full_check": check, "mns": len(mns)})
            pending.append((rec, encode(mns, w)))
            if check:
                flush()
            print(
                f"{a.net}: cycle {cycle} mns {len(mns)} mined {len(rec['mined'])} check {check} "
                f"refetched {refetched} ({time.time() - t0:.0f} s)",
                flush=True,
            )
        flush()
    print(f"{a.net}: done", flush=True)


if __name__ == "__main__":
    main()
