# RP1a-2c-1 review (Sol)

Verdict: ACCEPT

Counts: M-0, m-0, n-0.

## Findings

No findings.

## Review notes

- Compared `git log -p a7ae369d2..602bb9bf7` (all eight commits) with Dash Core v24.0.0-rc.4's `rpc/blockchain.cpp`, `core_write.cpp`, `evo/core_write.cpp`, `llmq/core_write.cpp`, `undo.h`, and `node/blockstorage.cpp`.
- Checked `getblock` verbosity parsing, transaction field order and types, coinbase handling, omission of `txinwitness`, fee arithmetic and formatting, `prevout.generated`/`height`, `cbTx`, and `instantlock` fields.
- Checked every implemented special payload renderer: ProReg, ProUpServ, ProUpReg, ProUpRev, CbTx, QcTx, MnHF, AssetLock, AssetUnlock, ProDis, ProUpShare, and ProUpSharedReg, including legacy/extended net-info handling, version-gated fields, address encoding, and BLS serialization choices.
- Checked the sighash decoder against Core's strict DER and defined-sighash checks, including malformed DER, unknown sighash bytes, zero-length/small pushes, `OP_RETURN`, and the 10,000-byte unspendable boundary.
- Checked undo index alignment (coinbase skipped), short transaction/input undo behavior, null-file positions, missing/unreadable undo omission, checksum verification, parent-hash use, and the block/undo chain queries and file locking.
- Reviewed the D38-D42 deferrals. The unknown `ExtNetInfo` version mutant survivor is acceptable for this range: Core itself reaches a `CHECK_NONFATAL` path for the service-enabled malformed Evo case, and no valid chain or fixture contains it; no safe live input was found that would make it a useful compatibility requirement.
- The fixture test compares complete serialized JSON for all 213 rows, so field additions, removals, renames, order changes, type changes, and number-format changes in the covered payloads are detected. The live DIFF covers verbosity 1-4, boolean forms, fees, multi-input spends, same-block spends, sighash asm, prevouts, no-undo blocks, and disconnect/reconnect behavior.

## Validation

- `TMPDIR=/work/scratch/rp1a-2c1-rv-sol/.tmp CARGO_BUILD_JOBS=8 nice -n 19 cargo test -p dashd-rpc --lib` — 193 passed.
- `git diff --check a7ae369d2..602bb9bf7` — clean.
- Implementer's reported evidence reviewed: 213 fixture rows byte-equal to rc.4, dashd-rpc 193 and dashd-node 168, Tier 1 gate-fast pass with two rerun-green dash-spv timeouts, and 16/17 mutants killed.
