# Real-header slices for the `sb4-core-scenarios` replays (SB4 §15.1)

Byte ranges of H-M and H-T (`../headers/README.md`: `mainnet.bin` SHA-256
`58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251`, `testnet.bin` SHA-256
`a88526024d3c3a2a82c797bd6348a8789ce5ee388ecf33742f3987e414659b27`), 80 bytes per header in
height order. The lib tests replay Core's verdicts on them without the untracked dumps.

| File | Heights | Vector | SHA-256 |
|---|---|---|---|
| `mainnet-0-301.bin` | 0..=300 | `a1a-mainnet-stale-branch-chainlock` (preload) | `fb3bb5aa34ef3c9b643429da40cdb6aaad747ab0de45cd530c604577fde462a9` |
| `testnet-0-5001.bin` | 0..=5,000 | `a1a-testnet-min-difficulty` (preload) | `a83c127d0383a91b3b0dadccd4a87c563f747c7b6af6c142c32c6face005eca4` |
| `testnet-999975-1002000.bin` | 999,975..=1,001,999 | `pnbh-testnet` (pack and batch) | `cc6fb922cb0396dbc282a9b0e08100f99d955bcd57bc6b54cc6cb91d6088af9a` |

The two preload slices are the prefixes the harness preloaded: their SHA-256 is the vector's
`provenance.inputs_sha256` entry (`mainnet.bin[:301 headers]`, `testnet.bin[:5001 headers]`), and
the replay checks it. The `pnbh-testnet` slice holds the 25 headers below the batch (the anchor
pack) and the batch.

```bash
dd if=mainnet.bin of=mainnet-0-301.bin bs=80 count=301
dd if=testnet.bin of=testnet-0-5001.bin bs=80 count=5001
dd if=testnet.bin of=testnet-999975-1002000.bin bs=80 skip=999975 count=2025
```

`pnbh-mainnet-dgw` and `pnbh-mainnet-kgw` replay on the bench slices (`../../../benches/data`).
