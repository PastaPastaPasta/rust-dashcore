# CS0 partial-history evidence

CS0 §9.2 and §14. `dash-conformance`'s `dash_consensus` leg (`conformance/src/consensus.rs`) ran
over each oracle's synced chain. This is **partial history**: mainnet's oracle had not reached the
tip, and no stale block was reachable (below). PLAN's full-history criterion is an M1 exit item,
not CS0's merge.

## Configuration

| | |
|---|---|
| Code | rust-dashcore `39e7867e6804` (`node/cs0-node-primitives`); harness code identity `f1a699609990ef437dd54dbf8319a315` (sources) `.fafc3b2e7b58b79756154ac959c3daf5` (executable) |
| Base SDK | fork `481f8c351c4f` (BS4a, BS11a-t) |
| Oracles | the shared v24.0.0-rc.3 oracles (`/Dash Core:24.0.0/`, client version 240000, protocol 70242), read only through JSON-RPC: `getblockhash`, `getblock 0/1`, and `getblock 2` for sampled blocks (every 1,000th and every block with a special transaction of types 1–4 and 7–12) |
| Mode | RPC, release build, 8 threads, `nice -n 19 ionice -c 3`, a fresh output directory per network, 2026-10-08 |
| Rules | V24_0 (the oracles' release) for the comparison; V23_1 alongside for every txid (CS0 §9.2) |

## Results

| Network | Heights | Blocks | Transactions | `dash_consensus` blocks | Txids differing under V23_1 | Mismatches |
|---|---|--:|--:|--:|--:|--:|
| testnet | 0–1,568,782 (the tip) | 1,568,783 | 8,574,172 | 1,568,783 | 0 | 0 |
| mainnet | 0–1,465,718 (the oracle's tip; about 2.55 M headers) | 1,465,719 | 35,832,809 | 1,465,719 | 0 | 0 |

Every block decoded exactly and canonically through `decode_block`; `core_serialize`,
`Decoded::core_bytes` and `size` gave back the oracle's bytes; the block hash, every txid and the
merkle root (unmutated) equal the oracle's under V24_0. Each run's gate reports every unit in
scope covered and one chain ending in the pinned tip. The two libraries' legs ran in the same
passes and also report no mismatch.

- **V23_1 against V24_0:** no transaction on either chain has a txid that differs between the
  two rule sets (an asset unlock with a stable txid). On mainnet that is the evidence A1 §2.25
  relies on for comparing against the v24 oracle; any such transaction would have failed the run
  (`consensus.release_txid`).
  *(Superseded by DEC-82, v24 only: the V23_1 rules, `consensus.release_txid` and the
  `stable_txids` total were removed; this records what the run checked.)*

## Coverage limits

- **Stale blocks: none.** Neither oracle holds a fork with block data (`getchaintips`: testnet
  only the active tip; mainnet the active tip and one headers-only tip), so RPC mode reaches no
  stale block. Stale blocks are in the oracles' block files, which these runs did not read (blk
  mode reads the datadirs directly). The leg's stale path is exercised by blk mode's stale
  fixture in `cargo test -p dash-conformance` (`blk_mode_end_to_end`).
- **Mainnet above 1,465,718** had not been synced by the oracle; the run covers 57% of the chain.
- The v24 regtest chains of `conformance/tests/data/` (asset locks and unlocks, payout shares,
  ProTx versions) run through the leg in `cargo test`; one of their transactions has a stable
  txid, and the tests show the leg flags it when it is treated as mainnet.

## Reproducing

```sh
cargo build --release -p dash-conformance
nice -n 19 ionice -c 3 target/release/dash-conformance rpc --net testnet --out <dir> --resume --fail-on-mismatch --threads 8
nice -n 19 ionice -c 3 target/release/dash-conformance rpc --net mainnet --out <dir> --resume --fail-on-mismatch --threads 8
```

The summary's `totals` carry `consensus_blocks` and `stable_txids`; `runs.ndjson` the code,
oracle and base SDK identities.
