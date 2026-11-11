#!/usr/bin/env python3
"""Stream mainnet blocks for EV2b's DML window (`node-l1` example `dml_window`).

Prints `<height> <raw block hex>` for every height in [first, last], in order, on stdout.
Blocks come from the gzip cache file when it holds them, else from the mainnet reference
oracle through `contrib/oracle-cli`: `getblockhash` and `getblock <hash> 0` only, which
ORACLE.md lists as pure reads, one call at a time (the oracle rule). Fetched blocks are
appended to the cache, so a rerun reads no RPC.

Usage: dml-window-fetch.py FIRST LAST CACHE.gz
"""
import gzip
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CLI = os.path.join(ROOT, "contrib", "oracle-cli")


def rpc(*args):
    for attempt in range(5):
        out = subprocess.run([CLI, "mainnet", *map(str, args)], capture_output=True, text=True)
        if out.returncode == 0:
            return out.stdout.strip()
        # The oracle service restarts with a new cookie now and then; oracle-cli rereads it.
        sys.stderr.write(f"oracle-cli {args[0]} failed ({out.stderr.strip()}), retrying\n")
    sys.exit(f"oracle-cli {' '.join(map(str, args))} keeps failing")


def main():
    first, last, cache = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    have = {}
    if os.path.exists(cache):
        with gzip.open(cache, "rt") as f:
            for line in f:
                height, _, block = line.rstrip("\n").partition(" ")
                if block:
                    have[int(height)] = block
    with gzip.open(cache, "at") as out:
        for height in range(first, last + 1):
            block = have.pop(height, None)
            if block is None:
                block = rpc("getblock", rpc("getblockhash", height), 0)
                out.write(f"{height} {block}\n")
                if height % 1000 == 0:
                    out.flush()
                    sys.stderr.write(f"fetched {height}\n")
            sys.stdout.write(f"{height} {block}\n")
    sys.stdout.flush()


if __name__ == "__main__":
    main()
