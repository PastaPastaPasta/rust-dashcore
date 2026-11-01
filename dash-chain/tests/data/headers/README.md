# H-M and H-T: mainnet and testnet headers (SB4 §15)

`mainnet.bin` (heights 0..=2,551,766) and `testnet.bin` (0..=1,568,459) hold every header of a
Dash Core v24 reference node's best header chain, 80 bytes each in height order. They are the
two dumps `dash-consensus/tests/data/PROVENANCE.md` records, made by
`contrib/node/cs2-diff/dump_headers.py` with read-only RPC against the reference nodes under
`/work/chains` (docs/node-program/ORACLE.md). The SHA-256 of each is in `SHA256SUMS`.

They are 204 MB and 125 MB, so they are not checked in (`.gitignore`). `fetch.sh` puts them here:

```bash
dash-chain/tests/data/headers/fetch.sh            # copy from /work/scratch/cs2/headers
dash-chain/tests/data/headers/fetch.sh <dir>      # or from another copy
```

Without a copy, rebuild them byte for byte from a reference node that has moved past their tips
(`contrib/node/cs2-diff/README.md`, "The history sweep"):

```bash
contrib/node/cs2-diff/run.sh params v24.0 main > main-params.json
python3 -I contrib/node/cs2-diff/dump_headers.py mainnet mainnet.bin main-params.json \
    --until 2551766 --sha256 58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251
```

and the same for `testnet` (`params v24.0 test`, `--until 1568459`, the testnet sum), then
`fetch.sh <dir>`.

The ignored tests that read them (A-4, A-5, A-7, A-31; SB4 C3/C4) will look here, or in
`SB4_HEADERS_DIR`. `fetch.sh` uses GNU `cp` and `sha256sum`.

## The reference nodes' tips (A-4)

`tips-mainnet.json` and `tips-testnet.json` are checked in. Each holds the dump tip's chain work
and the stale branches a reference node knows, mapped onto the dump's heights (the nodes have
moved past the dumps: a tip above the dump stands for the dump's tip, or for the stale branch it
has there), with their headers and provenance. `fetch_tips.py` records them with read-only RPC
against `/work/chains/<net>` (docs/node-program/ORACLE.md):

```bash
python3 -I dash-chain/tests/data/headers/fetch_tips.py mainnet    # needs mainnet.bin here
```

The recorded ones have no stale branch at or below the dumps' heights: both nodes synced their
whole chains by IBD, so A-4's expected tips are the dump's tip alone.
