#!/usr/bin/env python3
"""Write tests/data/payments-v24.0.json.gz, the fixture of tests/payments.rs (node-program
O2a-GV), from the reference dashd nodes' RPC (pure reads, one at a time, mainnet heights
≤ 2,090,000).

payments: samples as payments-check.py builds them (see the `payments_check` example), with
each masternode list trimmed to the masternodes paid by the block's coinbase plus the first
valid ones by registration (the voters): GetMNPayee() picks the minimum of the list under its
order, so a subset holding the real payee picks the same one. Two variants derive from the
lists: `banned` (mainnet) PoSe-bans every masternode (no payee: GetBlockTxOuts() fails, R4
F6); `strict` (testnet, where V24 can activate: mainnet's DEPLOYMENT_V24 is NEVER_ACTIVE at the
pin, chainparams.cpp:213) gives every masternode the coinbase pays a 50% operator reward paid
to its own payout script (GetBlockTxOuts() then expects two equal outputs when the masternode
reward is even).

credit: oracle_credit_pool_step inputs: block h, the headers of h - 576 .. h - 1, block
h - 576 (the window start GetCreditDataFromBlock() reads), getcreditpoolinfo's pool after
h - 1 (null at the V20 activation height: the empty pool) and after h.

Usage: gen-payments-fixture.py [--out tests/data/payments-v24.0.json.gz]
"""

import argparse
import copy
import gzip
import importlib.util
import json
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("payments_check", HERE / "payments-check.py")
pc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pc)
dml = pc.dml

VOTERS = 12  # above nGovernanceMinQuorum (10) on mainnet
WINDOW = pc.WINDOW
DEPLOYMENTS = pc.DEPLOYMENTS  # V24 DEFINED, as getdeploymentinfo reports it on both reference nodes


def trimmed(mns, coinbase_addresses):
    """The masternodes paid by the coinbase, plus the first VOTERS valid ones by registration."""
    paid = {a for a in coinbase_addresses if a}
    keep = [mn for mn in mns if mn["state"]["payoutAddress"] in paid or mn["state"].get("operatorPayoutAddress") in paid]
    valid = [mn for mn in dml.by_registration(mns) if mn["state"]["PoSeBanHeight"] == -1]
    for mn in valid[:VOTERS]:
        if mn not in keep:
            keep.append(mn)
    return keep


def payments(chain, height, name, variants=()):
    net = chain.net
    sample = pc.payments_sample(chain, height, DEPLOYMENTS)
    sample["name"] = name
    mns = chain.list_json(height - 1)
    if mns is None:
        return [sample]
    addresses = pc.coinbase_addresses(chain, chain.block_hash(height))
    mns = trimmed(mns, addresses)
    sample["prev_list"] = dml.encode_list(mns, height - 1, chain).hex()
    if sample["superblock"]:
        sample["voters"], sample["weighted"] = pc.voters(mns, net)
    out = [sample]
    payees = set(addresses)
    for variant in variants:
        list_ = copy.deepcopy(mns)
        for mn in list_:
            if variant == "banned":
                mn["state"]["PoSeBanHeight"] = height - 2
            elif mn["state"]["payoutAddress"] in payees:  # "strict": the payee among them
                mn["operatorReward"] = 50
                mn["state"]["operatorPayoutAddress"] = mn["state"]["payoutAddress"]
        out.append(dict(sample, name=variant, prev_list=dml.encode_list(list_, height - 1, chain).hex()))
    return out


def credit(chain, height, name):
    net = chain.net
    prev = None if height == net["v20_height"] else pc.pool_of(chain.rpc.call("getcreditpoolinfo", height - 1))
    return {
        "name": name,
        "chain": net["chain"],
        "height": height,
        "first_height": height - WINDOW,
        "headers": chain.headers_hex(height - WINDOW, height),
        "window_block": chain.rpc.call("getblock", chain.block_hash(height - WINDOW), 0),
        "block": chain.rpc.call("getblock", chain.block_hash(height), 0),
        "prev": prev,
        "expected": pc.pool_of(chain.rpc.call("getcreditpoolinfo", height)),
        "deployments": DEPLOYMENTS,
    }


def find_lock_unlock(chain, start, stop):
    """The first block in start .. stop with both an asset lock (type 8) and an asset unlock
    (type 9), else the first with an unlock."""
    first_unlock = None
    for h in range(start, stop):
        txs = chain.rpc.call("getblock", chain.block_hash(h), 2)["tx"]
        types = {tx.get("type") for tx in txs}
        if 9 in types and 8 in types:
            return h
        if 9 in types and first_unlock is None:
            first_unlock = h
    if first_unlock is None:
        raise SystemExit(f"no asset unlock in testnet blocks {start} .. {stop - 1}")
    return first_unlock


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=str(HERE.parent / "tests/data/payments-v24.0.json.gz"))
    ap.add_argument("--unlock-from", type=int, default=1148500, help="testnet search start (WITHDRAWALS)")
    args = ap.parse_args()
    main_chain = pc.open_chain("mainnet")
    test_chain = pc.open_chain("testnet")

    samples = []
    samples += payments(main_chain, 100_000, "pre-budget")
    samples += payments(main_chain, 332_320, "budget-window")  # 20 * 16616: offset 0 of the cycle
    samples += payments(main_chain, 332_420, "budget-outside")  # offset 100: past the window
    samples += payments(main_chain, 2_077_000, "superblock")  # 125 * 16616
    samples += payments(main_chain, 2_077_001, "regular", variants=("banned",))
    samples += payments(test_chain, 1_500_000, "testnet-superblock")  # 62500 * 24
    samples += payments(test_chain, 1_500_001, "platform", variants=("strict",))  # MN_RR era
    # Era and rule boundaries (EV3-1 independent review N1), after the samples above so their
    # names and order stay. Mainnet's MN_RR (2,128,896) is above the 2,090,000 cap.
    samples += payments(main_chain, 614_819, "superblock-start-prev")
    samples += payments(main_chain, 614_820, "superblock-start")  # nSuperblockStartBlock
    samples += payments(main_chain, 631_408, "first-superblock")  # 38 * 16616
    samples += payments(main_chain, 1_047_199, "dip3-enforcement-prev")
    samples += payments(main_chain, 1_047_200, "dip3-enforcement")  # the payee check starts
    samples += payments(main_chain, 1_700_000, "classic-realloc")  # mid reallocation schedule
    samples += payments(main_chain, 1_977_304, "pre-v20-superblock")  # 119 * 16616, the last
    samples += payments(main_chain, 1_987_775, "v20-prev")  # Classic era, the last block
    samples += payments(main_chain, 1_987_776, "v20")  # CreditPool era, the first block
    samples += payments(test_chain, 1_066_899, "testnet-mn-rr-prev")  # CreditPool era, last
    samples += payments(test_chain, 1_066_900, "testnet-mn-rr")  # EvoReward era, first
    unlock_height = find_lock_unlock(test_chain, args.unlock_from, args.unlock_from + 20_000)
    steps = [
        credit(main_chain, main_chain.net["v20_height"], "v20-activation"),
        credit(test_chain, unlock_height, "unlock"),
        # EV3-2 independent review (BL-260): soon after testnet's V20 the window start's balance
        # is small, so V24's limit takes its 2000 DASH floor and the clamp to the balance.
        credit(test_chain, test_chain.net["v20_height"] + 700, "testnet-v20-early"),
    ]
    head = subprocess.run(["git", "-C", str(HERE), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    versions = {name: chain.rpc.call("getnetworkinfo")["subversion"] for name, chain in
                (("mainnet", main_chain), ("testnet", test_chain))}
    fixture = {
        "provenance": {
            "generator": "dash-oracle-sys/scripts/gen-payments-fixture.py",
            "rust_dashcore_head": head,
            "dashd": versions,
        },
        "payments": samples,
        "credit": steps,
    }
    with gzip.open(args.out, "wt") as f:
        json.dump(fixture, f, indent=0, sort_keys=True)
    print(f"wrote {args.out}: {len(samples)} payments samples, credit steps at {[s['height'] for s in steps]}")


if __name__ == "__main__":
    main()
