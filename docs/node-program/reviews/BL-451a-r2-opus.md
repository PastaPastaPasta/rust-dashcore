# BL-451a review r2 (Opus, narrow): the r1 fix round

- rust-dashcore `node/bl451-oracle-mnsync` at f707f473b, delta `1f7b10257..f707f473b` (5 signed
  commits, 13 files, +380/−114)
- Rulings: DEC-656 (fix round), DEC-659 (routing); plan DEC-626 (P2)

**Verdict: ACCEPT.** 0 Major, 0 minor, 0 nits.

Every r1 finding is closed as r1 asked, M-1 by the full fix DEC-656 required. I reran three
of r1's four mutants at f707f473b, and l1_mnsync kills all three; the implementer's run killed
the fourth. It also kills seven new mutants of mine. An eighth survives, which is inherent (see
the observation). Production
behaviour of the `Own` layout is unchanged, and the Core layout can't be reached from any
non-test, non-feature build. One observation is below, outside this round's scope and not
counted.

## r1 closure

| r1 | Status | Where | Evidence |
|---|---|---|---|
| node-fuzz lock | CLOSED | `node-fuzz/Cargo.lock` (9ffe1f2c0) | Adds 23 lines: the `dashd-masternode` package and node-l1's three new edges. No version moves. `cargo metadata --offline --locked` in node-fuzz exits 0. |
| M-1 | CLOSED (the full fix) | `dashd-masternode/src/flatdb.rs:241-256` (`Layout`, `Own`), `:350-360` (`core_layout::Core`), `:414-475` (`read_into`/`read` over `L`), `:489-496` (`load_as`), `:525-545` (`store_as`); `netfulfilled.rs:153-196`; `meta.rs:299-342`; `node-l1/src/mnsync.rs:747-781` (`node_start`), `:784-808` | `loaded`, the store held after the load, and `file_after` now come from the production `load_cache` → `load_as` → `read` (recreate or refuse) and `store_as` (verify, then write), for both files. r1's RVMUT=1 (Format fatal) fails `cleared_files_load_alike` with `mn-infos.dat: load verdict`, so the mncache side (formerly `\|obj\| obj`) is now tested too. Short-recreates (RV2MUT=11), skipping `CheckAndRemove` (RV2MUT=7) and no write over a missing file (RV2MUT=9) are all killed. |
| m-1 | CLOSED | `node-l1/src/mnsync.rs:296-334` (pool of 8: two IPv4 addresses, one of them on two ports; `2001:db8::1`; `::1`; three Tor v3), `node-l1/tests/l1_mnsync.rs:286-317` (`fulfilled_keys_order_and_reload_agree`) | r1's RVMUT=2 (v1-byte order) fails the new row and `random_fulfilled_scripts_agree`. "Last stays" on reload (RV2MUT=8) fails the new row: the two onions on :9999 carry different request names, so a collapse that keeps the wrong one shows. |
| m-2 | CLOSED | `node-l1/tests/l1_mnsync.rs:401-420` | Per the implementer's run, r1's RVMUT=3 (`get` doesn't refresh) is killed; I didn't rerun it. In my runs, `meta_corners_agree` fails under RV2MUT=3 (`AlreadyHavePlatformBan` doesn't refresh) and RV2MUT=4 (a re-emplace keeps the old use). The row exercises each of Core's three bump paths: `exists`, `get` and `_emplace` on an existing key (`unordered_lru_cache.h:39-80`). |
| n-1 | CLOSED | `node-l1/src/mnsync.rs:157-164` (top bit → start 1..=899), `l1_mnsync.rs:158-177` (`early_unforced_reset_agrees`, skipped at 100 and 899, done at 900) | r1's RVMUT=4 fails the fixed row and `random_sync_scripts_agree`. |
| n-2 | CLOSED | `node-l1/src/mnsync.rs:723-729` (edit 6), `l1_mnsync.rs:503-518` | Checking the network before the magic (RV2MUT=6) fails `core_files_load_alike` with `read result`. The failure is on the new both-wrong row: the wrong-network-only row before it agrees under that mutant. |
| n-3 | CLOSED | `dashd-masternode/src/lib.rs:19-24`; recipes at BS4B-EVIDENCE.md:131 and :207, SP1-notes D24, `node-l1/examples/{chain_sample,dml_window,versionbits_chain}.rs`, lq1-rot PROVENANCE.md:64 | The guard has the same form as dashd-node's `test-support` and dashd-chainstate's, with the same clippy exemption. `cargo check --release -p dashd-masternode --features core-layout` fails with the guard's message. With the documented `--config` it finishes. `cargo clippy --release --features core-layout` finishes. dash-dml's bench README is left to BL-495, as the brief says. |

## Focus 2 (M-1 and P2): met

- **Production is unchanged.**
  - `load`, `store` and both managers' `load_cache`/`store` are now one-line delegations to `*_as::<Own>`.
  - The bodies moved verbatim: the same `tracing::info!` lines in the same order, verify (`read`) before `Writing info`, `write_durably` and `log_write`, and `is_valid` set before `check_and_remove`.
  - `Own::{decode_into, encode}` are the old free functions.
  - `read`/`read_into` stay private.
  - `Layout`, `Own`, `load_as` and `store_as` are `pub(crate)`, so the public API is unchanged without the feature.
- **The Core layout can't be reached.**
  - `core_layout` (with `Core`) and the four `*_core_layout` methods are `#[cfg(any(test, feature = "core-layout"))]`.
  - Only node-l1's `oracle` turns the feature on. Its users are conformance's `bsdk-judges` and node-fuzz, whose `[profile.release]` sets `debug-assertions = true`, so `node-fuzz/run.sh:729`'s `--release` build passes the guard.
- **The feature doesn't leak into product builds.** `cargo tree -e features -i dashd-masternode` shows no `core-layout` for:
  - `-p rdashd`, with or without `--all-features`;
  - `-p dashd-node`, with or without `--all-features`;
  - `--workspace`.

  It shows the feature for `--workspace --all-features` and `-p dash-conformance --features bsdk-judges`. The first is already refused in optimized builds by dashd-node's `test-support` guard, so this round adds no new class of failing command.
- **No other release recipe needs the `--config`.** Every `--release` recipe I found that pulls in `node-l1/oracle` either has it or is BL-495's. The rest are debug builds or don't use oracle: dash-oracle-sys `--features oracle` doesn't reach dashd-masternode, nor do `bs7-tool` or `quorum_root_history`.

## Focus 3 (`node_start` vs Core's lifecycle): faithful

- **Core's lifecycle.**
  - `LoadCache(true)` is `is_valid = Load()`, then `CheckAndRemove()` if valid (netfulfilledman.cpp:21-29). mncache has no `CheckAndRemove` (meta.cpp:31-36).
  - `Load` is `Read`, which treats `FileError` and `IncorrectFormat` as true (flat-database.h:160-186).
  - The destructor stores if `is_valid` (netfulfilledman.cpp:15-19, meta.cpp:25-29), and `Store` re-runs `Read` before `CoreWrite`.
- **The harness.** `node_start` runs `load_cache_core_layout(path, ms, true)` on a manager with `MockClock::at(load.mocktime)` (Core's `CheckAndRemove` runs at the mock time). It calls `store_core_layout` if `is_valid()`, and reads back whatever file is left, `None` if there is none. That matches Core step for step. The missing-file case (recreate, then the destructor writes the empty object) is covered by the generated inputs, which kill RV2MUT=9.
- **The `is_valid()` gate** is the flag dashd-node's real shutdown reads: `steps.rs:505-509` stores `self.netfulfilled.take().filter(|m| m.is_valid())`, and the same for `metaman`. `load_cache` returns the value it stores into `is_valid`, so f707f473b changes no outcome. It ties the harness to the production flag instead of to the return value.

## Observation (outside this round, not a finding)

E4 compares only Core's `LoadCache(true)`. `fLoadCacheFiles` is false on `-reindex`/`-reindex-chainstate` or with no tip (init.cpp:2273-2289). In that case Core's `LoadCache(false)` runs `Store` over the existing file, and that `Store`'s verify (a refused file → "Failed to clear") has no Core cross-check.

My RV2MUT=10 (`store_as` skips the verify) survives all 14 l1_mnsync tests. That is inherent to E4's shape: after an accepted load, the shutdown store's verify re-reads the file the load just accepted, so no input can make it fail. The mutant is killed by dashd-masternode's own `netfulfilled::tests::load_cache_outcomes` (`netfulfilled.rs:359`, "a file Core refuses is not cleared") and by the sporks round-trip test, so the behaviour is pinned on the Rust side.

The node-side seam already takes `load: bool`, so a `LoadCache(false)` leg would need only a shim argument (an ABI bump). That's the manager's call; DEC-626 doesn't ask for it.

## Checked and ran

- Read r1, DEC-656, DEC-659, the full delta, and Core at c3e388c24ce: `flat-database.h:150-205`, `netfulfilledman.cpp:1-40`, `masternode/meta.cpp:15-45`, `unordered_lru_cache.h:20-100`, `init.cpp:2273-2295`. Also read dashd-node `steps.rs:190-215, 495-515`.
- Review copy `/work/scratch/bl451a-rv2-opus`, a reflink of the frozen worktree at f707f473b; `git status` was clean. Mutants were reverted afterwards (the files were copied back from the worktree). Tools and logs are in `/work/scratch/bl451a-rv2-opus-tools/`.
- `cargo test -p node-l1 --features oracle --test l1_mnsync`: 14/14 pass at f707f473b, and 14/14 again with the mutant build and no `RV2MUT`.
- `cargo test -p dashd-masternode`: all pass (42 + 3 + 1 + 3 + 10).
- Env-gated mutants (`RV2MUT=n`, one build, `mutate.py`). I wrote my own at f707f473b because the implementer's `/work/scratch/bl451/mut2` is at an earlier state: its `.git` points at the shared worktree gitdir, so `rev-parse` reads f707f473b, but its sources still have the dropped `load_with_magic`/`store_with_magic` and the `loaded`-gated store.

  | RV2MUT | Mutant | l1_mnsync |
  |---|---|---|
  | 1 | `IncorrectFormat` fatal in `read` (r1 RVMUT=1) | killed: `cleared_files_load_alike` (`mn-infos.dat: load verdict`) |
  | 2 | netfulfilled keys written in v1-byte order (r1 RVMUT=2) | killed: `fulfilled_keys_order_and_reload_agree`, `random_fulfilled_scripts_agree` |
  | 3 | `AlreadyHavePlatformBan` doesn't refresh | killed: `meta_corners_agree` |
  | 4 | a re-emplace keeps the old use | killed: `meta_corners_agree` |
  | 5 | a zero last tip update reads as "never" (r1 RVMUT=4) | killed: `early_unforced_reset_agrees`, `random_sync_scripts_agree` |
  | 6 | network checked before magic | killed: `core_files_load_alike` (both-wrong row) |
  | 7 | no `CheckAndRemove` after the load | killed: `core_files_load_alike` |
  | 8 | of two collapsed services, the last stays | killed: `fulfilled_keys_order_and_reload_agree` |
  | 9 | the store doesn't write over a missing file | killed: `generated_inputs_compare`, `random_flat_files_agree` |
  | 10 | the store skips its verify | survives (see the observation); dashd-masternode kills it |
  | 11 | `Short` recreates | killed: `cleared_files_load_alike` |

  r1's RVMUT=3 (`get` doesn't refresh) was already killed by the implementer's run, and its row has the same shape as my RV2MUT=3 and 4, so I didn't rerun it.
- `cargo check --release -p dashd-masternode --features core-layout`: fails with the guard's message.
- The same with `--config 'profile.release.package.dashd-masternode.debug-assertions=true'`: OK.
- `cargo clippy --release -p dashd-masternode --features core-layout`: OK.
- `cargo tree -e features -i dashd-masternode` for the seven selections listed above.
- `cargo metadata --offline --locked` in node-fuzz: OK.
- grep for `--release` recipes that pull in `node-l1/oracle` or `bsdk-judges`.
