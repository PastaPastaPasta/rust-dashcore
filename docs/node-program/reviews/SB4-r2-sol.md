# SB4 implementation — Sol narrow r2

Scope: `node/sb4-dash-chain`, exact head `69745378a73ba29f5dad48e73bb5b2b41664eb0a`, fixes `63c513ac4` and `69745378a` on `558f54403`. Worktree clean at intake. This is a narrow follow-up to Sol r1, not a reopening of closed findings.

Contract: REVIEW-COMMON and approved SB4 rev7.1 §10.2. PLAN §7: codenames only; no private files read. The task-specific disk TMPDIR overrides REVIEW-COMMON's `/dev/shm` direction. Build/test commands use `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, `TMPDIR=/work/scratch/sb4-r2-sol/tmp` (0700). Branch is read-only; no commits, pushes or GitHub comments.

## Review progress

- Read the prior Sol r1 report and its two original scratch probes. Those probes assert that the old defects exist; r2 scratch copies assert the repaired behavior and preserve the original sources. Both pass against the freshly built exact-head all-features library (`libdash_chain-8acb1c9aed24c590.rlib`), resolved from Cargo's artifact output and dependency fingerprints.
- Verified exact head and both fix commits. Completed cold-leaf/overlay, ownership, active-ancestor reconciliation, fork selection, skip-height and loaded-record-boundary checks.
- The reported nightly A-4/A-31 results and unrelated workspace dashd startup failures are intake context, not independently reproduced evidence in this review.

## Findings

**No blocker, major, minor or nit findings in the requested narrow scope.** R1-M1 and R1-m1 are closed by independently reproduced behavior and the committed regressions. Closed r1 items were not reopened.

## Evidence and assessment

- Exact-head `cargo test --locked -p dash-chain --all-features`: **157 unit tests passed, 2 ignored**; benchmark fixture test and all **5 doctests passed**. Other integration tests remained ignored as marked. Log: `/work/scratch/sb4-r2-sol/crate-tests.log`.
- R1-M1 regression copy (`probe.rs`) **passed**: at base 30 / active height 40, cold stale `BlockId(2)` is reported at height 2, branch length 1, fork `BlockId(1)`, `HeadersOnly`.
- R1-m1 regression copy (`coldskip-probe.rs`) **passed**: `ReadKind::Leaves` failure is consumed by `chain_tips`, yields `ChainError::Storage`, and trips the latch. The cold self-skip yields `off its height`, not the 100th-read watchdog; another 95 by-id reads succeed after the failure. The healthy ancestry still returns genesis.
- `skip_height` differential **passed** on 4,194,517 height cases: every height 0 through 4,194,304, plus power-of-two boundary neighbors through `INT_MAX`. The helper bodies extracted from both Core pins (`v23.1.8`, `v24.0.0-rc.3`) are identical. Seventy walks through the exact production `walk.rs` with Core-shaped links also passed, including near `INT_MAX`. Sources/logs: `/work/scratch/sb4-r2-sol/core-skip.cpp`, `skip-parity.rs`, `skip-parity.log`.
- Ownership review: `chain_tips_with(&self)` (`chain.rs:2122`) cannot safely overlap `set_status(&mut self)` (`:1886`) or eviction (`:1968`, reached from stage 4). The overlay is writer-owned. `ChainReader` has no `chain_tips` method. Testing an overlay mutation or eviction literally during this query requires breaking this ownership boundary; valid concurrent readers instead use `ChainReader` while the writer evicts.
- Compiler proof: attempts to run `chain_tips` on a scoped thread while the same chain runs `set_active_tip` or `enforce_best_chainlock` each fail with **E0502**. These are deliberate compile-failure probes, not build failures of the reviewed branch (`ownership-refusal.rs`, `ownership-refusal-build.log`). A caller wrapping the chain in a lock must hold its read/write guard for the operation; the type does not permit concurrent writer mutation of its overlay or resident-leaf set.
- Concurrent-reader probe **passed**: 50 writer commit/activation/eviction steps advanced height 40 to 1,040; the writer called `chain_tips` between every step, preserving the cold stale leaf. A concurrent reader completed **17,360** ancestry/LCA/lookup rounds across eviction of the original active tip, with correct genesis/fork answers and no latch trip (`eviction-probe.rs`, `.log`). This exercises allowed reader/writer concurrency; there is no claim of simultaneous writer query and eviction.
- Cold overlay probe **passed** using public ChainLock enforcement: a cold height-2 leaf changes from `HeadersOnly` to `Conflicting` in the uncommitted writer overlay. Three consecutive queries report `Conflicting` while the direct store and `ChainReader` still report the unmarked committed status. After commit and `records_committed`, the store and query both report the marked status (`fresh-probe.rs`, `.log`). A change literally during one query is excluded by the ownership proof above.
- Store-leaf reconciliation **passed** for one and four pending children of the cold height-2 leaf. The store still lists that leaf; the query reports only the newest child. A directed transient also switches the active tip to the pending child while its cold ancestor remains in the committed leaf set: the ancestor is omitted and the active child is included exactly once (`ancestor-leaf-probe.rs`, `ancestor-leaf-probe-1.log`, `ancestor-leaf-probe-4.log`). That transient intentionally calls activation before committing; no new entry is evicted at those heights. The production protocol commits first. The committed four-state regression, including a 40-header reorg and below-base re-extension, also passed in the crate suite.
- Cold `fork_target` probe **passed**: with active height 0, an evicted height-80 leaf wins over a height-110 resident branch whose root carries an overlay conflict mark. The result is `BlockId(81)`, `NoFloor`. A subsequent `ReadKind::Leaves` error is consumed by the same `HeaderChain::cold_leaves` path, returns `ChainError::Storage`, and trips the latch (`fresh-probe.log`). No recursive-mutex deadlock occurred; the bound-check guard is released before entering the cold-read body.
- Targeted all-features/all-target clippy with `-D warnings` and workspace format check **passed** (`clippy.log`, `fmt.log`).

## Query cost and call sites

`chain_tips_with` unconditionally invokes `cold_leaves` for `Evicting` (`chain.rs:2131`); `Permanent` never enters it. The helper scans the step's dirty resident ids to collect parents, performs one `HeaderStore::leaves()` call, tests returned leaves for residency, refreshes the cold-work bound, and filters uncommitted parents (`:765–775`). Cost depends on dirty entries and stored branches, rather than a scan of all history. The later per-leaf fork walk can still issue many cold reads. There is no cold LRU in this SB4 head; SB5 owns it.

A counting `HeaderStore` wrapper over the same memory store, with active height 100 and `w = 10`, measured these actual store calls for a query. It counted the adapter calls and returned leaf records, not noisy wall-clock timing:

| Cold stale leaves at height 2 | `Leaves` calls | Returned leaf records | `ById` calls | `ByHash` calls |
|---:|---:|---:|---:|---:|
| 0 | 1 | 1 | 0 | 0 |
| 1 | 1 | 2 | 24 | 24 |
| 100 | 1 | 101 | 2,400 | 2,400 |

Ten repeated queries per case each made exactly one `Leaves` call. The larger counts include resolving the cold leaf/status and finding its fork through cold ancestry; the records returned by `leaves()` are currently reduced to ids and subsequently resolved again. This establishes I/O shape, not SQLite latency or a PF0 performance gate (`cost-probe.rs`, `.log`).

`chain_tips` is **not a hot header-processing path on this head**: a repository-wide Rust call-site search finds its implementation, tests and replay comparisons, with no production caller or invocation from accept, tip publication, feeds or fork selection. It is a whole-index diagnostic query; future drivers should use the cheap snapshots for frequent tip displays and measure any chosen diagnostic polling rate with the production adapter. The header-step selection path is `fork_target`, whose cold read remains guarded by the conservative cold-work bound (`chain/enforce.rs:447`). In each counted healthy case above, routine `fork_target` returned the active tip with **zero store reads**, including after `chain_tips` refreshed the bound.

The refresh assigns the exact maximum of committed **nonresident** store leaves, rather than retaining a historical maximum. It intentionally computes it before filtering parents with uncommitted children (`chain.rs:771–774`): these committed leaves remain a conservative bound until commit. That can cause an extra cold selection read, but cannot suppress a winner. The committed regression verifies the bound drops to zero after the pending child is committed.

## Loaded-record boundary

The boundary is **sound for the height/termination invariant addressed by R1-m1**. New resident links are derived by SB4's own `BuildSkip` and parent-height computation (`chain.rs:590`). Node `load` validates parent availability, derives the immutable fields in height order and refuses a mismatching stored height (`:941–962`); it rebuilds skips, like Core, rather than importing stored skip ids. The resident panic is therefore an invariant failure, not the intended response to arbitrary persisted input. Fallible SPV walks instead call the corruption helper and trip the latch when a traversed cold or resident-to-cold link lands at the wrong height.

SB5's SPV `open` is the correct owner of validation before persisted links become trusted resident entries. It must validate every loaded record's parent at `h−1`, and every present skip at `skip_height(h)` with the floor/anchor rules, checking targets outside the loaded window too, before publishing the chain or allowing its resident walks. Merely checking that a target id exists is insufficient. The approved SB5 §8 requires these checks. A read-only inspection of the current SB5 worktree's `chain/sb5/open.rs` also finds a parent-height check in `load_record` and a second-pass `check_skip` over every loaded record, including cold-target heights, before active-chain reconstruction/publication. That observation is not an SB5 review or validation claim.

Height checks establish strict descent and correct landing heights; they do **not** authenticate a skip to a same-height entry on another branch, as `walk.rs:38` explicitly states. The original r1 finding was a nonterminating wrong-height link. No broader claim that SB4 detects all possible database corruption is made here. Reopen validation, production-adapter consistency and the larger windowed/unwindowed recovery suite remain the previously assigned SB5/SB13 obligations.

## Reproduction and limits

All probe sources, the fingerprint-aware `build_probe.py`, Cargo build artifact log, compiler refusal, test and lint logs are under `/work/scratch/sb4-r2-sol/`. Original r1 sources remain unchanged. Build/test commands used the specified nice invocation, eight Cargo jobs, disabled Rust wrapper and disk TMPDIR; the empty temporary directory was removed afterward. No source edits, commits, pushes, GitHub comments, process killing, private-file reads or protected datadir operations were performed. Final SB4 head remains `69745378a` with a clean tracked worktree. This report remains uncommitted in the integration worktree.

This review reran the relevant crate suite and narrow probes, not A-4/A-31's full nightly replays or the workspace gate. The reported dash-spv/dash-spv-ffi dashd startup failures still need their promised low-load rerun; the focused Sol gate does not certify that unrelated workspace gate is green.

## Gate decision

The two actionable r1 defects are fixed on the exact reviewed head. The requested directed checks found no new in-scope regression. Query completeness, overlay precedence, cold-read failure handling and Core-shaped skip compatibility have independent passing evidence.

**Verdict:** APPROVE

Sol gate satisfied: yes
