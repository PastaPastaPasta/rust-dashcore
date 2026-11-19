# Provenance of `test-vectors/bip152/`

Real testnet captures for the node's BIP152 short-ID tests (`dashd-p2p/src/processing/compact.rs`,
`testnet_captures_*`). Nothing here is hand-written: no built, all-zero or ground inputs.

| File | Copied from | sha256 |
|---|---|---|
| `compact.json5` | base-sdk branch `pasta/p2p-bip152-blocktx` at `681099d4542340d23c59a6dd176f5d56aba35fa4`, `pkgs/p2p_core/corpus/compact.json5` (sha256 `89baff365197911cb02f27c037a9f0094de06318c2fa0324e05263859c49eaee`) | `0d780a148e46670f11922d913b8768086f610035830f5a7b214d6d525ba1490c` |

The copy is byte-for-byte except for one added header line naming that source. The file's own header
records the capture: one testnet peer on 2026-11-16, blocks under 4 kB among 1592890 to 1592894 (the two
`cmpctblock` replies are blocks 1592894 and 1592891), each with the `getblocktxn` sent for every
transaction it left out and the `blocktxn` reply. It also records how the sections were checked against
Dash Core v24.0.0 (938fba5028).

This directory is not part of the `test-vectors/p2p-decode/` corpus, which holds the fork-differential
corpora that `dash-p2p/tests/read_shadow.rs` scans.
