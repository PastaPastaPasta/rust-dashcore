# dash-consensus benchmark reports

One section per task: CS0 (node primitives) and CS5a (coin and undo codecs, at the end).

# CS0: node primitives

CS0 §9.6. CS0 records these numbers; there is no previous `node/integration` head to regress
against, so CS0 does not gate on them (PF0 will, once it exists). CS0 makes no 2× claim.

## Configuration

| | |
|---|---|
| Host | agentbox (AVX2, **no SHA-NI**; CS0 §7, DEC-28) |
| Rust | `dash-consensus` release profile; SHA-256 from `dash-sha256` (SH1: Core's transforms and runtime dispatch); X11 from base-sdk's `dash-pow` |
| Core | `bench_dash` built from the local branch `node/cs0-bench` (tag `cs0-bench-24rc3`, commit `76ebcf925592`, v24.0.0-rc.3 + the `DeserializeBlockHashMerkle` bench; DEC-16, DEC-30 N4), gcc 12.2.0, depends reused from B1's v24 build; binary sha256 `5c27ebe5ddaf` |
| Block | Core's `block813851.raw` (999 transactions, 749,213 bytes) |
| Metric | retired instructions (`perf stat instructions:u` for Rust; nanobench `ins/` for Core). Instructions are stable under load on this box (±0.04%); wall clock is not, and is left to a reserved quiet window (DEC-23(b)) |

Rust instructions are the per-iteration slope from 10 and 110 iterations, so fixed startup
(reading the block, preparing the leaves) is excluded.

## Results

| Workload | Rust | Core row | Core | Rust / Core |
|---|--:|---|--:|--:|
| `block_decode_hash` (decode + every tx hash + X11 + merkle) | 39.7 M ins/block | `DeserializeBlockHashMerkle` | 41.2 M ins/block | 0.96× |
| `block_txs_decode` (decode + every tx hash) | 38.0 M ins/block | `DeserializeBlockTest` | 39.6 M ins/block | 0.96× |
| `merkle_root_9001` (9,001 leaves) | 1,218 ins/leaf | `MerkleRoot` | 1,377 ins/leaf | 0.88× |
| — (diagnostic) | — | `DeserializeAndCheckBlockTest` | 44.6 M ins/block | — |

Before SH1, with `bitcoin_hashes` 1.2 (no SSE4 single-stream path on this host), the block rows
were 52.3 M and 50.6 M (1.27–1.28×): the whole gap was the single-stream SHA-256 backend.

## Reading

- The block workloads are dominated by SHA-256 of the transactions; with Core's own transforms
  the Rust rows use slightly fewer instructions than Core's, which re-serializes each
  transaction to hash it where the Rust decoder compares the received bytes and hashes them
  directly (CS0 §6.2; the comparison is ~1.8% of decode-and-hash).
- The remaining difference with `DeserializeBlockTest`: Core rewinds its stream inside the timed
  loop, and the Rust row decodes from a slice.
- Instructions are not time. The wall-clock comparison belongs to a manager-reserved quiet window.

# CS5a: coin and undo codecs (BG: crit coin decode)

`benches/coins.rs` against Core rows that do the same work on the same bytes. Core has no coin
bench, so the rows are a local patch of `src/bench/ccoins_caching.cpp` (DEC-16):

| | |
|---|---|
| Core | `bench_dash` from the local branch `node/cs5a-bench` (commit `3dac756dab`: v24.0.0-rc.3 + `CoinDecode<Kind>`, `CoinSerialize<Kind>`, `CoinToUndo<Kind>` and `BlockUndoDecode`), built incrementally in a copy of B1's v24 tree (`debian:bookworm`, gcc 12.2.0, B1's depends); binary sha256 `2c1a94111db6`. The coin code is identical at v23.1.8 |
| Work | Core decodes into a fresh `Coin`/`CBlockUndo` from a `DataStream` rewound to its start (one extra byte prevents its compaction, as in `DeserializeBlockTest`) and encodes into a cleared `DataStream`; Rust decodes from a slice, drops the result, and encodes into a cleared `Vec` |
| Inputs | built from the same fields on both sides and serialized by each side's encoder (the L1 comparisons show the encoders agree). The record is synthetic: 500 transactions of 4 inputs, and of every 20 coins 14 P2PKH, 4 P2SH, 1 compressed P2PK, 1 a raw 40-byte script (an assumed mix, not a measured one) |
| Metric | `instructions:u`: Rust the per-iteration slope from 1,000 / 101,000 / 201,000 iterations (10 / 110 / 210 for the record) of `coins --count N <row>`, pinned to one CPU, the two slopes within 0.1%; Core nanobench `ins/op`. Wall clock is left to a reserved quiet window (DEC-23(b)) |

Instructions per coin (per record for the last rows):

| Kind | `coin_decode` | Core `CoinDecode` | | `coin_encode` | Core `CoinSerialize` | | `coin_to_undo` | Core `CoinToUndo` | |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| P2PKH | 444 | 557 | 0.80× | 407 | 1,145 | 0.36× | 735 | 1,767 | 0.42× |
| P2SH | 370 | 477 | 0.78× | 486 | 969 | 0.50× | 748 | 1,511 | 0.50× |
| compressed P2PK | 338 | 564 | 0.60× | 475 | 883 | 0.54× | 696 | 1,512 | 0.46× |
| uncompressed P2PK | 45,006 | 46,626 | 0.97× | 1,535 | 1,992 | 0.77× | 45,313 | 48,683 | 0.93× |
| raw 40 bytes | 378 | 401 | 0.94× | 344 | 940 | 0.37× | 591 | 1,406 | 0.42× |

| Record row | Rust | Core `BlockUndoDecode` | Rust / Core |
|---|--:|--:|--:|
| `block_undo_decode` (owned) | 1,168,458 | 1,533,404 | 0.76× |
| `block_undo_ref_decode` (borrowed, `BlockUndoRef`) | 921,508 | — | 0.60× of Core's row |
| `block_undo_decode_bound` (`decode_block_undo`) | 1,954,078 | — | 1.27× of Core's row |

- `coin_ref_decode` (the borrowed decoder the cache uses on every access) is 223–333 for every
  kind: a special form stays compressed, so uncompressed P2PK pays its key decompression only
  when the script is expanded or the coin written to undo.
- `coin_to_undo` is end to end (database bytes → undo entry) because the sides pay the square
  root of an uncompressed key at different steps: Core when it reads the coin, the borrowed coin
  when it writes the entry (its re-serialization must detect an off-curve key).
- `block_undo_decode_bound` also decides `wire_is_canonical` by re-encoding into a comparison,
  which Core does not do; applying a record uses `BlockUndo::decode` or `BlockUndoRef::decode`.
- The owned decode keeps scripts up to 35 bytes inline (`CoinScript`); Core's `CScript` keeps 28
  inline and allocates for compressed P2PK, hence the largest decode gain on that row. Core's
  encode rows include `DataStream`'s per-write overhead.
- The uncompressed rows compare two vendored libsecp256k1 versions (secp256k1-sys 0.14.1 and
  Core v24's); both are dominated by the square root, and the row is not a claim about our code.
