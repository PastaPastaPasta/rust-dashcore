# CH1-node N7b-1, Sol review round 2 (narrow)

Reviewed `0f6dde7b2..cae1d6187` (034956a01, 749e1f9c8, c789c3a22, 312783fd2, cae1d6187) at `cae1d6187`. The implementer's worktree was not modified.

## Round-1 items

| Item | Verdict | Rust evidence | Dash Core v24.0.0-rc.3 evidence |
|---|---|---|---|
| M-1: foreign genesis was rejected before block-file checks | **CLOSED** | `dash-chain/src/chain.rs:1004-1011, 1090-1115` adds `load_records`, which retains structural/PoW checks but does not require the configured network genesis; `dashd-chainstate/src/chain/load.rs:129-179` consumes it before `block_files_present` at 164-170 and performs the network-genesis check at 171-178. The persisted-record regression tests are `dashd-chainstate/src/chain/tests/blocks.rs:758-785`. | `src/txdb.cpp:300-344` (`LoadBlockIndexGuts`) has no network-genesis test; it checks PoW at 328-331. `src/node/blockstorage.cpp:354-390` performs `LoadBlockIndexDB` and opens block files before `src/node/chainstate.cpp:177-193` checks the network genesis. |
| M-2: no shutdown poll during store-record scan | **CLOSED** | `dash-chain/src/chain.rs:1054-1066` polls before the first scan read and before every subsequent `records.next()`, returning `LoadError::Interrupted`; `dashd-chainstate/src/chain/load.rs:143-150` maps that to `LoadFailure::Interrupted`. `dashd-chainstate/src/chain/tests/blocks.rs:810-837` proves a shutdown between records prevents the injected next-read error and produces no load log. | `src/txdb.cpp:307-309` polls before each valid cursor entry; the surrounding load path is classified by `src/node/chainstate.cpp:177-185`. The later index passes also poll at `src/node/blockstorage.cpp:296` and `src/validation.cpp:5342`. |
| N-1: stale listener-panic ordering comment | **CLOSED** | `dashd-chainstate/src/chain/signals.rs:202-204` now says the flag is set after the abort sink; the guard and sink order are visible at 260-266 and 238-241. | `src/shutdown.cpp:27-36` runs the abort reporting/exit-status work and calls `StartShutdown()` last at 36. |
| N-2: interim `-stopatheight` refusal was not pinned | **CLOSED** | The refusal is asserted at `dashd-node/src/init/support.rs:308-312` and `dashd-node/src/chain/tests.rs:326-350`. Once enabled, the port uses Core's type and predicate: `dashd-chainstate/src/chain/actor.rs:154-155,438-442,673-674` and `dashd-chainstate/src/chain/abc.rs:785-788`; ABC tests cover `-1`, `0`, ordinary height, and narrowing at `dashd-chainstate/src/chain/tests/abc.rs:628-665`. | `src/validation.cpp:3752` obtains the argument as `int64_t` and narrows to `int`; line 3830 tests nonzero and `nHeight >= nStopAtHeight`. Parsing is `src/util/system.h:327-328` and `src/util/system.cpp:548-560`. The option remains registered in `src/init.cpp:814`. |

No regressions were found in the changed paths.

## `034956a01` (`-stopatheight`)

The change matches Core's behavior. `GetIntArg` returns `int64_t`, then Core stores it in `int nStopAtHeight` (`validation.cpp:3752`); on the target platforms the Rust `i64 as i32` narrowing in `actor.rs:438-442` has the same low-32-bit result. The ABC predicate is equivalent to Core's `nStopAtHeight && height >= nStopAtHeight`: `n != 0` makes `-1` stop at genesis, positive values stop at or above the requested height, and zero disables it. The interim node option is still refused until slice 2c, and the new refusal test prevents accidental early enablement.

## D54 verdict

**Acceptable, documented deviation.** Core's `while (pcursor->Valid())` (`txdb.cpp:307`) does not poll an empty cursor, so Core proceeds to `LoadGenesisBlock` (`node/chainstate.cpp:207-211`) when the store is empty. `HeaderChain::load_records` polls before the `next()` that discovers end-of-store (`dash-chain/src/chain.rs:1054-1066`), so a shutdown arriving at that point returns `Interrupted` instead of writing genesis. The difference is limited to an empty store with a concurrent shutdown request; the node exits either way, and the deviation is explicitly recorded in `docs/node-program/designs/CH1-node-notes.md:48` and the API comment at `dash-chain/src/chain.rs:1004-1011`. It does not change populated-store ordering, foreign-datadir refusal, or persisted-index safety.

## Validation

All commands used the detached worktree `/work/scratch/ch1-node-n7b1-r2-sol`, with `CARGO_TARGET_DIR=/work/scratch/ch1-node-n7b1-r2-sol/target`, `TMPDIR=/work/scratch/ch1-node-n7b1-r2-sol/tmp`, and `CARGO_BUILD_JOBS=12`.

- `cargo fmt --all -- --check` — passed.
- `cargo clippy -p dash-chain -p dashd-chainstate -p dashd-node --lib --all-targets -- -D warnings` — passed.
- `cargo test -p dash-chain --lib --quiet` — 392 passed, 0 failed, 1 ignored.
- `cargo test -p dashd-chainstate --lib` — 378 passed, 0 failed.
- `cargo test -p dashd-node` — 95 passed, 0 failed, 1 ignored; asserted-log integration tests 11 passed, 0 failed.

Mutation checks:

1. Disabled the new `if interrupted()` at `dash-chain/src/chain.rs:1056`; `cargo test -p dash-chain --lib chain::tests::load_records_polls_for_shutdown_before_each_read` failed as intended with `Read(Io("reached"))` instead of `Interrupted`.
2. Moved the network-genesis check ahead of `block_files_present` in `dashd-chainstate/src/chain/load.rs`; `cargo test -p dashd-chainstate --lib chain::tests::blocks::a_stored_index_of_another_network_is_checked_for_files_then_network` failed as intended, returning `Incompatible` where the test requires Core's `LoadingBlockDb` for the missing file.

ACCEPT
