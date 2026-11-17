# Provenance of the `dash-filters` oracle vectors

`core_v24_special_tx_filters.json` holds Dash Core's own basic block filters for 149 regtest blocks.
Nothing in it is computed by this repository: the blocks, the spent scripts and the filters are
read from dashd.

| | |
|---|---|
| Producer | `dashd` of `dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz`, `Dash Core version v24.0.0-rc.3`, fetched by `DASHVERSION=24.0.0-rc.3 python3 contrib/setup-dashd.py` |
| Binary sha256 | `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4` (`bin/dashd`) |
| Chains | the datadirs of Core's functional tests `feature_asset_locks`, `feature_evo_shares_activation`, `feature_llmq_evo`, `feature_masternode_payout_shares`, `feature_masternode_shares`, `feature_protx_version` and `rpc_netinfo` at tag v24.0.0-rc.3, run on 2026-10-08 with the same release binary and kept with `--nocleanup` by `dash/contrib/core-vectors/run-functional-tests.sh` (branch `node/v24a-dashcore-wire`; the tests' own debug.log records `Dash Core version v24.0.0-rc.3 (release build)` for every node) |
| Extraction | `contrib/core-vectors/extract_filters.py` (this crate): copies node0 of each chain, restarts it with the chain parameters of its last start and `-blockfilterindex=1 -rpcdoccheck=0 -disablewallet=1`, waits for `getindexinfo` to report the block filter index synced at the tip, then reads `getblock <hash> 0` (raw block), `getblock <hash> 3` (`vin[].prevout.scriptPubKey.hex`, the scripts the block spends) and `getblockfilter <hash>` (`filter`, `header`) |
| Command | `python3 -I contrib/core-vectors/extract_filters.py ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd <ft-dir> <work-dir> core_v24_special_tx_filters.json` |
| File sha256 | `51be07ae3279299d97fc27af056abcd4c8ab0f9d6f0a4af8f7bf3854da83b645` |

## What is kept

Per chain: every block with a transaction of a type other than 0 (plain), 5 (coinbase) and 6
(quorum commitment, which adds no filter element and is in nearly every block), plus the first
three blocks that hold a quorum commitment and the first three with a plain spend. 110 blocks are
`special`, 21 `quorum commitment`, 18 `plain spend` (field `kind`). Transaction types present:
0 (60 blocks), 1 (34), 2 (20), 3 (5), 4 (6), 5 (149), 6 (74), 7 (3), 8 (6), 9 (16), 10 (6), 11 (9),
12 (9): provider register, update service, update registrar, update revoke, coinbase, quorum
commitment, MNHF signal, asset lock, asset unlock, provider dissolve, update share and update
shared registrar. The provider payloads include versions 1, 2 and 3 (extended addresses and payout
lists) and shared masternodes with collateral shares.

Row fields: `height`, `hash` (display order), `block` (raw hex), `prev_scripts` (the script of each
spent output, non-coinbase inputs in block order), `prev_header` (the filter header of the block
before, zeros for genesis), `filter` (Core's encoded filter), `header` (Core's filter header, display
order), `tx_types`, `kind`.

## Regenerating

Regtest keys, txids and timestamps differ between runs of the functional tests, so a regenerated
file differs byte-wise but covers the same layouts; `the_vectors_cover_every_special_transaction_type`
checks the coverage and the count (update the 149 with it).

## `blockfilters.json`

`tests/core_vectors.rs` reads Core's `src/test/data/blockfilters.json` through `dash-core-vectors`
(`test-vectors/core/0364ed626ec2/`, the v24.0.0-rc.3 commit). The file is byte-identical to the
copy under `dash/tests/data/` (sha256
`59b744f4e70c9bd84c6f0651bf9fa2e9e9bc2ba3dd51d1c80d9576b30651cf54`).
