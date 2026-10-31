# P3 S1 review — Sol round 1

**Head:** `f1a8a890d` against `6dc9720c5`  
**Verdict: CHANGES REQUIRED**

## Finding

### F1 (P2): the simulation exposes an unpublished block status

- **S1:** `dashd-p2p/src/sim/chain.rs:1113-1116` reads `Inner::status` directly, ignoring the drained snapshot; `publish_with` releases `inner` at lines 597-599, runs the between-stores hook, and only then stores the new `ChainView` at lines 612-616.
- **Core:** `src/net_processing.cpp:3202,3205` reads block validity under `cs_main` in `HeadersDirectFetchBlocks`; `src/validation.cpp:3761` locks `cs_main` for an activation step, and `src/validation.cpp:3471` advances the tip inside that step after connecting the block. A reader cannot observe B at `BLOCK_VALID_SCRIPTS` with the old A tip during a successful step. (B can gain `BLOCK_HAVE_DATA` before activation, so that weaker status is not the finding.) The accepted P3 §2.4 contract also says `status` returns the last **published** value.
- **Schedule:** Index B's header but leave B without block data, with A as tip. On V's RPC/import accept of B, pause at `SimChain`'s existing hook after the tip-cell store and before its `ChainView` store. A drain still returns the A view and its old `view_seq`, but `status(&old_view, B)` returns `validity = Scripts` from mutable `inner`. A consumer can therefore combine A's tip with B's unpublished connected status. A Core `cs_main` section sees either the pre-connection state or the post-connection B tip.
- **Proof:** I temporarily strengthened `a_drain_between_the_two_cells_neither_shows_the_new_tip_nor_applies_its_signals` to index B first and assert that `status(&old_view, B).validity` was not `Scripts` while V was paused. It failed with `old view exposed connected block status` (`Scripts` versus `Scripts` in the `assert_ne!` output). I restored the test. The existing test checks only tip and signal cutoff, so it misses this status leak.
- **Impact:** This is an S1 `ChainAccess` simulation contract failure. The current S1 download skeleton does not call `status`, so I found no current S1 peer frame changed by it. It can give later direct-fetch and compact-block tests a state Core cannot expose in one `cs_main` section. Publish a status mirror with `ChainView` and have `status` read that published mirror; add the status assertion to the two-cell test.

## Other reviewed surfaces

- The queue's `PreStart` guard, stamp-prefix drain, per-entry release, byte and entry bounds, close wake, ring, and C5′ quiescent wait match the S1 ordering. The block wait drops the peer region before polling V's reply and redrains before its tail. `Region` returns the header token on ordinary and unwinding exits; no internal path I found carries a header token into a V wait.
- D70 matches Core's `block` branch at `src/net_processing.cpp:5346-5382`: loading blocks is checked before decoding; a malformed payload throws at the decode without scoring or submission; mutation scoring runs only after successful decoding. The `sim_m2` bad-payload test verifies throw, score zero, no disconnect and no reply.
- D71 moves cases that require the S4 visit or the S6 host. Its reduced S1 tests exercise the mechanisms present here. I found no second S1 defect in those deferrals. The host `wait_published` release-point ordering and dropped-reply behavior recorded in D72 remain S6 integration obligations.
- **Wire:** Apart from the future-consumer risk in F1, I found no current S1 peer-visible difference from the cited Core branches.

## Mutation checks

All edits were temporary, implementation-only, made in the detached review worktree, and restored after each run.

| Mutation | Focused test | Result |
|---|---|---|
| Skip the `BlockStep::Redrain` queue drain but retain its fresh view | `sim_p3_s1::the_headers_continuation_reads_the_post_reply_drain` | **Caught:** expected one delivered signal, got zero (`[(0, 2)]` versus `[(1, 2)]`). |
| Disable `SimChain`'s pre-accept C5′ wait | `sim_p3_s1::c5_prime_defers_an_accept_until_every_signal_is_processed` | **Caught:** the reply became ready before the queued signal was processed. |
| Replace `mapBlockSource.emplace` with overwriting `insert` | `processing::download::tests::the_source_map_never_overwrites_and_keeps_the_guard_counter` | **Caught:** source became `(8, false)` instead of the first `(7, true)`. |

## Validation

- `cargo test -p dashd-p2p`: passed; 172 unit/integration tests and its empty doctest set, including all 15 `sim_p3_s1` tests.
- `cargo test -p dash-chain --doc`: passed; 1 positive and 11 compile-fail doctests, including the `Hold` `!Send` check.
- `cargo test -p dash-consensus --test block_mutation`: passed; all 4 Core-fixture cases.

All Cargo runs used `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, and a private target at `/work/scratch/p3-s1-r1-sol/target`.
