# O6 review (round 3, Sol)

## Verdict

**ACCEPT**

Findings: **0**.

## Scope and evidence

The target is `node/o6` at `e93455676`; the fixes are present in `264f43e44`. I reviewed in the reflink copy `/work/scratch/o6-rv-sol3/o6`.

Both round-2 majors are closed:

1. **Scan (e) MNHF bit bound — closed.** Dash Core v24.0.0-rc.4 defines `VERSIONBITS_NUM_BITS = 29` (`versionbits.h:22`) and rejects `signal.versionBit >= VERSIONBITS_NUM_BITS` in `MNHFTxPayload::IsTriviallyValid` (`evo/mnhftx.cpp:95-103`). The scanner uses that bound at `conformance/src/scan/classify.rs:475-490`, emitting `e.bit_out_of_bounds` and skipping activation classification for out-of-bounds bits. `conformance/src/scan/params.rs:46-48` records the Core constant. Fixtures at `conformance/src/scan/tests.rs:667-690` cover bit 27 accepted, bit 28 in bounds and refused by activation, and bits 29–32 out of bounds; the old `e-neg-bit-31` fixture is absent. `EBound32` and `EBoundLe` are included in the killed-mutant set (`conformance/src/scan/tests.rs:804-810`), and the scan library suite reports `mutants_are_killed ... ok`.

2. **Output guard with symlinked `blocks/` — closed.** `conformance/src/scan/run.rs:146-171` protects both the canonical resolved blocks tree and the datadir derived from the absolute supplied path after canonicalizing only its parent (`:156-160`), preserving the final `blocks` symlink. The `<datadir>/blocks` and `<datadir>/testnet3/blocks` cases are covered by `conformance/tests/scan_run.rs:233-270`; the current test passes. I copied that regression test into the old `aecdcd026` scratch worktree; the old guard accepted an output under the datadir and the test failed on `unwrap_err`, confirming the regression.

The requested rerun checks also pass. The six `scan-{a..f}.ndjson` files in each of `mainnet` and `testnet` are byte-identical to their `*-r2` counterparts. The pinned testnet rerun `testnet-r3b` records `--to 1580928`; its six scan outputs match `testnet-r2`. Mainnet's partial status remains `partial: oracle block data ends at 2,094,670; see manager` in the JSON and `oracle block data ends at 2,094,670; see manager` in the Markdown, unchanged from the prior revision; no new partial-record explanation was added.

Validation in the reflink copy:

- `cargo fmt --all -- --check` — passed.
- `cargo clippy -p dash-conformance --all-targets --all-features -- -D warnings` — passed.
- `cargo test -p dash-conformance --lib scan::tests --all-features -- --nocapture` — 11 passed.
- `cargo test -p dash-conformance --test scan_run --all-features -- --nocapture` — passed (8 tests plus the traced child).
