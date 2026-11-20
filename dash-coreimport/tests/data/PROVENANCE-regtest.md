# regtest-asset-locks-750.hex, regtest-asset-unlock-v2-969.hex

Raw blocks 750 and 969 of the regtest chain built by Dash Core v24.0.0-rc.3's functional test
`feature_asset_locks` (run by `conformance/scripts/regtest-v24.sh` on branch
`node/bs2-conformance-harness`, commit 5ae9d6c9). Copied from that branch's
`conformance/tests/data/regtest-feature_asset_locks-{750,969}.json` (`hex` field). The
expected block hashes and txids in `tests/special_txs.rs` are the `hash` and `tx[].txid`
fields of the same files' `getblock 2` output from dashd.

- 750: a CbTx (type 5) and three asset locks (type 8).
- 969: a CbTx and an asset unlock with payload version 2 (type 9, stable txid).
