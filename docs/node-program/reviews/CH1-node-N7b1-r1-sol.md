# CH1-node N7b-1, round 1 — `549457767..0f6dde7b2`

Detached checkout: `/work/scratch/ch1-node-n7b1-r1-sol/src` at `0f6dde7b2`. Core comparisons below use only `v24.0.0-rc.3` via `git show`.

## Findings

### M-1 (major): the store loader rejects a foreign genesis before BL-127 can check block files

**Where:** `dash-chain/src/chain.rs:1040-1064`; `dashd-chainstate/src/chain/load.rs:94-126`; `dashd-chainstate/src/chain/tests/blocks.rs:650-658`.

**Scenario:** ND1's planned loaded-index path has to call `HeaderChain::load` to construct the `LoadedIndex::Loaded` argument. That loader returns `LoadError::GenesisMismatch` for a mainnet genesis index opened with testnet parameters. It therefore never calls N7b's `open_block_index`, which checks the blk files before the network at lines 109-124. A mainnet index with its blk file missing, opened as testnet, cannot produce Core's missing-file log and `Error loading block database` through this path. Mapping the early error to a broad failure class would still skip the check; with the blk file present, it would also need to preserve Core's incompatible-datadir result. Core's `txdb.cpp:300-344` accepts this genesis during index decode (both networks have the same PoW limit and genesis `nBits`), `node/blockstorage.cpp:377-390` checks the file, and only then `node/chainstate.cpp:188-193` checks the network. The current order test hands a previously built `HeaderChain` directly to `open_block_index`, so it does not exercise the real store path. A temporary scratch probe using serialized mainnet genesis index records and testnet parameters returned `genesis mismatch` from `HeaderChain::load` before the blk-file seam was reached.

**Fix:** give ND1 2b a staged load path that can represent a structurally loaded foreign-root index through the blk-file check, then run the network check; add a persisted-record integration test for both file-present and file-missing cases. Keep `open_block_index`'s current internal order.

### M-2 (major): the store-record scan has no index-load shutdown poll

**Where:** `dash-chain/src/chain.rs:1003-1035`; `dashd-chainstate/src/chain/load.rs:99-110`.

**Scenario:** `HeaderChain::load` reads and decodes its entire index before `open_block_index` receives the shutdown flag. Its record loop has no poll. Core polls `ShutdownRequested()` before each DB entry in `txdb.cpp:307-309`, as well as before entries in `node/blockstorage.cpp:296` and `validation.cpp:5342`. N7b covers the latter two in `NodeBlockIndex::load_until` and after the blk check, but cannot cover the first one from its current API. On a long scan, shutdown waits for the scan to finish. If a later record read fails after shutdown was requested, Rust returns that read failure before N7b can classify `Interrupted`, whereas Core exits the DB loop and `node/chainstate.cpp:183-185` classifies the failed load as `INTERRUPTED`.

**Fix:** pass cancellation into the store-record iteration (including a pre-scan poll), preserve it as a distinct load outcome, and map it to ND1's interrupted initialization path. Add a controlled iterator test that requests shutdown between two records and would fail on the next read if it were reached.

### N-1 (nit): the listener-panic order comment is stale

**Where:** `dashd-chainstate/src/chain/signals.rs:202-204`.

The comment says the listener panic sets the flags before the abort sink. `ValidationSignals::start` at lines 259-266 installs `ShutdownOnDrop`, calls the sink, then sets flags when the guard drops. That implementation matches Core's `shutdown.cpp:35-36`. Update the comment to match it.

### N-2 (nit): pin the interim `-stopatheight` guard explicitly

**Where:** `dashd-node/src/chain.rs:103-107`; `dashd-node/src/init/support.rs:119,288-308`.

The interim integration state is safe: `parts()` passes a fresh `LocalShutdown`, but `stop_at_height` is hardcoded to zero and the node refuses `-stopatheight` as unsupported. A real node cannot currently set that isolated flag through this option. The support tests check table coverage and several other refusals, but do not assert that `-stopatheight=1` is rejected. Add that assertion, and in ND1 2c enable the option only alongside the shared `ShutdownState` and an exit test.

## Checks and scope

- Read `REVIEW-COMMON.md`, the CH1-node design and notes, both N7a reviews, implementer plan §5, all nine commits in the specified range (`git log` reports `G` for each; the brief says eight), and the production diff. The review-round fixes M-1 and L-1 through L-6 are present; I found no remaining issue in their V-side behavior.
- D51: reviewed the single attach, rejected side's latch before drop, D38 exemption, and attach from inside V's first class C operation. D52: reviewed V's private latch versus the node flag, ABC and InvalidateBlock polls, listener panic, stop tail, and sink-before-flag ordering. D53: within `open_block_index`, checked `HAVE_DATA`, ascending file order, empty index, failure texts/classes, the pre-load, index-pass, and post-file load polls, and asserted-log row 141. The two major findings are at the earlier store-load boundary.
- `cargo fmt --all -- --check`: passed. With private target, `TMPDIR=/work/scratch/ch1-node-n7b1-r1-sol/tmp`, `CARGO_BUILD_JOBS=8`, and `nice -n 10`: `cargo test -p dashd-chainstate --lib` passed 372/372; `cargo test -p dashd-node` passed 94/94 runnable unit tests (one ignored) and 11/11 asserted-log tests.
- Seven scratch mutants were killed by the relevant tests: blk-check precedence, both index-pass shutdown polls, post-file shutdown poll, actor node-flag read, attach-once, and attach during the first class C operation. See `mutations.log`. The scratch wrong-network probe was removed after running; the checkout is clean.

CHANGES-REQUIRED
