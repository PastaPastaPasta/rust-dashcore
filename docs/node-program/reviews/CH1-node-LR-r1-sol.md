# CH1-node-LR VerifyLoadedChainstate review, round 1

## Verdict

**ACCEPT**

## Findings

No confirmed correctness, reliability, concurrency, or wiring findings. The implementation matches the cited Dash Core v24.0.0-rc.3 behavior and DEC-272 for the reviewed scope.

## Answers to the review questions

### 1. Core parity

The order and gates match Core in [`dashd-chainstate/src/chain/verify.rs:124`](dashd-chainstate/src/chain/verify.rs:124): the pruned-datadir warning, empty-coins/EvoDB branch, future-tip check, VerifyDB result mapping, and level-3 reset. The future check is strict `>` at [`verify.rs:142-146`](dashd-chainstate/src/chain/verify.rs:142), so exactly `MAX_FUTURE_BLOCK_TIME` is accepted and one second beyond it fails. Empty coins skip VerifyDB and the reset; a non-empty EvoDB returns Core's `Error initializing block database` text.

`VerifyDbArgs::new` narrows the raw `i64` values to `i32` before VerifyDB's level/depth bounds, while the reset gate at [`verify.rs:164-166`](dashd-chainstate/src/chain/verify.rs:164) reads the original `i64`. This preserves Core's differing behavior for values such as `2^32 + 2` and `-(2^32) + 3`. The BLS legacy-scheme transitions have no V equivalent because this implementation has no corresponding global scheme state, as documented in D64.

The node path intentionally derives `coins_empty` from the opened coins tip and leaves reindex flags and EvoDB handling to their current unsupported/unimplemented owners ([`dashd-node/src/steps.rs:178-184`](dashd-node/src/steps.rs:178)); rdashd rejects those options and opens no EvoDB.

### 2. Post-reset prune

The reset uses the Core ancestor comparison in [`verify.rs:190-209`](dashd-chainstate/src/chain/verify.rs:190). The Core candidate prune runs only when that reset actually ran and a tip exists ([`verify.rs:164-171`](dashd-chainstate/src/chain/verify.rs:164)). `NodeBlockIndex::prune_block_index_candidates` uses the Core work/sequence/address ordering and the same strict-worse removal rule.

This is the DEC-272 outcome: a reset that restores a failed tip prunes candidates below that tip, including the sanctioned D36 extension where the tip outranks all tied fallbacks. A reset that misses the tip leaves the D36 fallback set intact. The V tests cover both states and exercise the subsequent `CheckBlockIndex` path; the conditional also prevents the erroneous unconditional-prune behavior.

### 3. Concurrency

`ChainCommand::VerifyLoaded` is classified as class M and is explicitly exempt from D38's startup hold ([`dashd-chainstate/src/chain/command.rs:235-285`](dashd-chainstate/src/chain/command.rs:235)). V handles one command at a time. The reset and the post-reset prune execute in the same `handle` invocation ([`dashd-chainstate/src/chain/actor.rs:1142-1147`](dashd-chainstate/src/chain/actor.rs:1142)), so another chain command cannot enter between them. Before startup's ABC, D38 holds ordinary commands; the exempt `SetLoading` and ABC are the startup sequence, and a shutdown control command is the expected abort path.

### 4. dashd-node wiring

`ChainSteps::load` reads `-checkblocks`, `-checklevel`, and the explicit-option flag before opening the chainstate ([`dashd-node/src/steps.rs:134-140`](dashd-node/src/steps.rs:134)). It starts V, attaches the coins, sends `VerifyLoaded`, and returns its result before `Chain::import` sends `SetLoading` and the startup ABC ([`steps.rs:169-198`](dashd-node/src/steps.rs:169)). The three VerifyLoaded statuses map to Core's load outcomes in [`steps.rs:342-354`](dashd-node/src/steps.rs:342): recoverable failure/reindex question, interrupted shutdown, and incompatible/insufficient dbcache.

### 5. Tests and mutants

The 11 V tests cover reset targets, raw-level gating versus narrowed VerifyDB arguments, empty coins and EvoDB, future-tip boundaries, VerifyDB status mapping, hold exemption, the DEC-272 prune case, and the no-reset fallback case. The six node-l1 Core sessions cover the matching restart, level, empty-coins, side-branch, and future-tip behavior. The reported mutation run caught 21 of 22 mutants.

The L12 survivor (`None => false` for the reset's no-tip comparison) is unreachable in the production VerifyLoaded precondition. Core skips VerifyLoaded's reset branch when the coins view is empty. A `best_invalid` entry requires a loaded block index. The current rdashd path starts V with a fresh genesis index, so it has no `best_invalid` entry; the restart harness explicitly calls `load_chain_tip` before sending VerifyLoaded, and the eventual loaded-index path pairs a non-empty coins tip with that active V tip. The survivor therefore does not identify a production failure for this checkpoint.

## Validation evidence

- `cargo test -p dashd-chainstate chain::tests::verify --all-features`: 11 passed.
- `cargo test -p dashd-node --all-features`: 126 passed, 1 ignored.
- `cargo fmt --all -- --check`: passed.
- The read-only worktree remained clean. Core node-l1 sessions and live oracles were not run, per the brief.

## Assumptions and scope

- VerifyDB itself is the documented CH5 placeholder; this review verifies its argument construction, result handling, ordering, and reset gate.
- Reindex flags, EvoDB opening, and persisted block-index loading are outside this checkpoint's rdashd scope, as recorded in D64.
