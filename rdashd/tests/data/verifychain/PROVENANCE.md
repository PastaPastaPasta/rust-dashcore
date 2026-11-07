# `tests/verifychain.rs` fixtures (CH5-1a): provenance

Every byte here was produced by Dash Core: blocks by dashd, damage by Core's own serialization code.
Pre-proved on 2026-10-31 with dashd v24.0.0-rc.5 (`contrib/setup-dashd.py`, archive checked against
SHA256SUMS.asc); the test compares with the pinned rc.4, whose VerifyDB, DisconnectBlock, ConnectBlock
and `VerifyLoadedChainstate` are identical (`git diff v24.0.0-rc.4 v24.0.0-rc.5` touches none of them).

## `blocks.hex`
Regtest blocks 1–120 (`getblock <hash> 0`), non-evo (tip below DIP0003's 432), mined by dashd: wallet `w`,
110 blocks to one address with block 5's coinbase locked; blocks 111–120 each `generateblock` with three
`sendtoaddress 1.5` transactions; block 118 also spends block 5's coinbase with a raw transaction.
Tip `04f15abf…3aef`, block 118 `7db3cf25…1b22`.

## `undo118.hex`, `undo118-l4.hex` (the L4-only fixture)
Block 118's undo record and its checksum as dashd wrote them (`rev00000.dat`, 192 + 32 bytes), and the
same record with `vtxundo[0].vprevout[0]` (block 5's coinbase output) rewritten from P2PKH(b377f1…5e65) to
P2PKH(11…11), value, height and coinbase bit kept, the checksum recomputed as `UndoWriteToDisk` does
(`HashWriter << prevhash << undo`). Written by a tool compiled from rc.5's headers and linked against
Core's static libraries (`CBlockUndo` serializer, in place, same size enforced).
dashd: levels 0–3 pass at depths 1–6; level 4 at depth 3 or 6 fails, "found unconnectable block at 118 …
(block-validation-failed)"; a walk reaching block 5 fails level 3 ("last 116 blocks, 4 good transactions
before that"). Startup passes at the default checks and fails at `-checklevel=4 -checkblocks=6` and
`-checklevel=4 -checkblocks=0`.

## `l3-chainstate/` (the L3 fixture, dashd's side)
dashd's coins database at the tip with block 118's coinbase output 0 (`8f7a8c97…5d23:0`) worth
50000000001 duffs instead of 50000000000, written through `CCoinsViewDB` + `CCoinsViewCache::SpendCoin`,
`AddCoin`, `Flush` (Core's coin serializer, obfuscation and head-block markers; best block unchanged).
`000009.log` is LevelDB's write-ahead log (kept despite the repository's `*.log` rule).
dashd at `-checklevel=2`: levels 3–4 pass at depths 1–2 and fail from depth 3 ("coin database
inconsistencies found (last 3 blocks, N good transactions before that)", N = 0, 12, 138); level 4 never
runs. Startup fails at the default checks and at `-checklevel=4 -checkblocks=0`.
rdashd's side is the same coin rewritten in its coins column family by the test.

## Tools
`tools/fx.py` drove dashd for the chain and the pre-proofs; `tools/fixtool.cpp` wrote the damage
(`coin-value`, `undo-scan`, `undo-script`), built against rc.5's headers and Core's static libraries.
Neither runs in the tests.
