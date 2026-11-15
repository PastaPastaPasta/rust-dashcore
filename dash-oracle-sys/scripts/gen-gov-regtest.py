#!/usr/bin/env python3
"""Write tests/data/gov-regtest-v24.0.json.gz and gov-regtest-v24.0.governance.dat, the fixture
of tests/gov_regtest.rs (node-program O2a-GOV), from the output of gov_capture.py.

Nothing here computes a verdict: the JSON is gov_capture.py's capture.json with the masternode
list after the tip encoded as evodb stores it (dml-listdiff-check.py encode_list, offline over
the captured block hashes; internal ids are assigned in registration order, which no governance
verdict reads), and the governance.dat is node 0's file, copied byte for byte.

Usage: gen-gov-regtest.py <capture dir> [--out-dir tests/data]
"""

import argparse
import gzip
import hashlib
import importlib.util
import json
import shutil
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("dml_listdiff_check", HERE / "dml-listdiff-check.py")
dml = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dml)

# Regtest (chainparams.cpp CRegTestParams): base58 prefixes 140 and 19; a masternode's
# confirmedHash is set one block after its registration (nMasternodeMinimumConfirmations 1).
REGTEST = {"chain": "regtest", "min_confirmations": 1, "pubkey_prefix": 140, "script_prefix": 19}


class CapturedChain:
    """dml.Chain over the captured block hashes instead of RPC."""

    def __init__(self, capture):
        self.net = dict(REGTEST, dip3_height=capture["dip3_height"])
        self.hashes = dict(enumerate(capture["block_hashes"]))

    def block_hash(self, height):
        return self.hashes[height]

    confirmed_hash = dml.Chain.confirmed_hash


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("capture", type=Path)
    parser.add_argument("--out-dir", type=Path, default=HERE.parent / "tests" / "data")
    args = parser.parse_args()

    capture = json.loads((args.capture / "capture.json").read_text())
    dat = (args.capture / "governance.dat").read_bytes()
    assert hashlib.sha256(dat).hexdigest() == capture["governance_dat_sha256"], "governance.dat changed"

    diff = capture["listdiff"]
    assert not diff["removedMNs"] and not diff["updatedMNs"], "a diff from an empty list"
    chain = CapturedChain(capture)
    capture["tip_list"] = dml.encode_list(diff["addedMNs"], capture["tip"], chain).hex()

    out = args.out_dir / "gov-regtest-v24.0.json.gz"
    with gzip.GzipFile(out, "wb", mtime=0) as f:
        f.write((json.dumps(capture, indent=1, sort_keys=True) + "\n").encode())
    shutil.copyfile(args.capture / "governance.dat", args.out_dir / "gov-regtest-v24.0.governance.dat")
    print(f"wrote {out} and the governance.dat (sha256 {capture['governance_dat_sha256']})")


if __name__ == "__main__":
    main()
