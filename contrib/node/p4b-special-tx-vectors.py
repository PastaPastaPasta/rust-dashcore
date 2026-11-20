#!/usr/bin/env python3
"""Extract the v24 special transactions P4b's bloom vectors use (P4b D8, review r1 M-3).

Usage: p4b-special-tx-vectors.py <conformance/tests/data> <OUT_DIR>

The source files are `getblock <hash> 2` replies of dashd v24.0.0-rc.3 for blocks of the regtest
chains Dash Core's own functional tests build (`conformance/scripts/regtest-v24.sh`). Each
transaction is written as Core printed its `hex`, to `v24-<name>.hex`; the hex is hashed and
checked against the `txid` Core printed (and the pinned one, where VECTORS names it).
"""

import hashlib
import json
import pathlib
import sys

# (file name, source block file, txid)
VECTORS = [
    ("proregtx-payouts", "regtest-feature_masternode_payout_shares-121",
     "5bb21f874f50967df6faf711521c13d0723991d8d1e8c00669c9f49eea248a01"),
    ("proupregtx-payouts", "regtest-feature_masternode_payout_shares-124", None),
    ("proupservtx-v3", "regtest-feature_masternode_payout_shares-125",
     "ae738cc6547cf27fab740924adb65af5b71496c77cd4dd76c078a0300c562085"),
    ("proregtx-shares", "regtest-feature_masternode_shares-273",
     "45574307d12b8ebd87493f6844e7732a718202716bd9b8765d4b481557989ee4"),
    ("prodistx", "regtest-feature_masternode_shares-327", None),
    ("proupsharetx", "regtest-feature_masternode_shares-330", None),
    ("proupsharedregtx", "regtest-feature_masternode_shares-335", None),
    ("assetlocktx", "regtest-feature_asset_locks-750",
     "0d88508724d5ec566e1b8e9d848394f4d1703ab5e58ea2720d158a6d3389fea5"),
]

# The payload object Core prints for each kind, to find the one transaction of it in a block.
KINDS = {
    "proupregtx-payouts": "proUpRegTx",
    "prodistx": "proDisTx",
    "proupsharetx": "proUpShareTx",
    "proupsharedregtx": "proUpSharedRegTx",
}


def main():
    data = pathlib.Path(sys.argv[1])
    out = pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for name, source, txid in VECTORS:
        txs = json.loads((data / f"{source}.json").read_text())["getblock2"]["tx"]
        if txid is None:
            found = [t for t in txs if KINDS[name] in t]
            assert len(found) == 1, (name, len(found))
            tx = found[0]
        else:
            tx = next(t for t in txs if t["txid"] == txid)
        raw = bytes.fromhex(tx["hex"])
        got = hashlib.sha256(hashlib.sha256(raw).digest()).digest()[::-1].hex()
        assert got == tx["txid"], (name, got, tx["txid"])
        (out / f"v24-{name}.hex").write_text(tx["hex"] + "\n")
        print(f"v24-{name}.hex {tx['txid']} {source} {len(tx['hex']) // 2} bytes")


if __name__ == "__main__":
    main()
