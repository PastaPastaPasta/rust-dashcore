# SB4 design review — Opus high (sequential, after the Sol gate)

Reviewed `docs/node-program/designs/SB4-dash-chain.md` at **24098412** (blob `f816346f`, unchanged at node/integration HEAD 30bfbb1a), after Sol r1–r3 (`reviews/SB4-design-r{1,2,3}-sol.md`). Design line numbers below are at that blob. Core references are `src/` paths at **v24.0.0-rc.3**, read with `git show v24.0.0-rc.3:<path>` (extracted copies under `/work/scratch/sb4-opus-high/core/`).

REVIEW-COMMON and PLAN §7 handling were followed. The following are binding and were not reopened: DEC-119 (SB0 rev6.1), DEC-123/127 (SPORK_19 as Core, the shared spork core, declared SPV restart differences), DEC-128 (§9.1's single recipe, the `cl` harness order), DEC-132 (empty node stages), CH2 rev5 (DEC-103) and DEC-95/99/102 (Core semantics, no stricter invariants). Sol's closed findings were spot-checked rather than re-reviewed.

**Verdict: APPROVE-WITH-CHANGES.** There are no blockers and two majors. O-M1 needs a manager ruling: an SB0 erratum of the same kind as Q-r1-1. Both majors have concrete fixes that fit within §17's margin.

## Major findings

### O-M1 — SPV `fork_target` has no answer when the floor's own path carries a ChainLock mark; Core's tip then trails the floor

**Refs:** §6.2 steps 2–4 (`:500-506`), §6.2's `set_active_tip` refusal (`:516`), §9.1's A-23 predicate (`:715-719`), §7.5's lifecycle "Off → on … Marks stay" (`:634`), A-29 (`:946`). **Core:** `chainlock/handler.cpp:209-222` (reset), `chainlock/chainlock.cpp:98-104`, `:106-132`; `validation.cpp:4048-4078` (`EnforceBlock`, which calls `ResetBlockFailureFlags(pindex)` with `ignore_chainlocks = false`, declared at `validation.h:731`), `:4198-4203` (CONFLICT is kept when `ignore_chainlocks` is false), `:3544-3547` (`FindMostWorkChain` skips any CONFLICT on the path).

The design composes two behaviours that it tests separately: marks survive an off → on reset (A-29 (v)), and a lower-height lock is accepted after the reset (A-29 (i)). Together they reach a state where §6.2's predicate is unsatisfiable:

1. Branches fork at 80. B81…B110 is active, and A81…A95 holds header-only headers accepted earlier.
2. The spork is on and a lock arrives on B100. `EnforceBlock` marks A81 `CONFLICT` (A is not active, so only A81 is marked).
3. The spork goes off, then on. `ResetChainlock` clears `cl_best` and `cl_floor`, and A81 stays marked.
4. A lock on A90 arrives. Height 90 is accepted because the cleared best CL has height −1, so `cl_floor = A90`.
5. `enforce_block(A90)` marks B81 and disconnects the cursor to 80. B81…B110 become `CONFLICT`. `!A90.IsValid()` calls `ResetBlockFailureFlags(A90, false)`, which keeps A81's `CONFLICT` (`:4201-4203`).

At this point every descendant of the floor A90 has the marked A81 on its path, so it fails (a). Every other header fails (b). §6.2 step 4's claim that "the walk yields the floor descendant" is false. `fork_target()` has nothing to return, and `set_active_tip` must refuse any target with `NotFloorDescendant` (a debug panic). The SPV client is stuck on a stage-1 error after a sequence that a devnet or testnet spork toggle can produce.

Core is not stuck. After `EnforceBestChainLock`, its tip is the disconnected cursor (80): `ActivateBestChain` finds no usable candidate and the node trails the floor, just as the node does in A-26. A model of this case is in `/work/scratch/sb4-opus-high/model/ft_reset.py`:

| Rule | Result |
|---|---|
| (b) as specified, with (a) against the real or the cursor chain | `None` |
| (a) only, against the published active chain | B110, even though it is marked: the walk from B110 to the published chain is empty |
| (b′) below, with (a) against the cursor | 80, which is Core's tip |

The model's third row exposes a second imprecision. In stage 1 the plan is computed while the published chain still contains the entries that enforcement just marked. So (a)'s "down to the active chain" must mean the enforcement cursor's chain, or equivalently it must also check marks on the active chain above the fork. Otherwise step 4's "the active tip is always eligible by (a)" admits a marked tip. Today (b) masks this, because marked active entries never descend from the floor.

**Fix (decided here, for the manager to record).** Replace (b) with Core's actual constraint (b′): the candidate *does not conflict with the floor*. That means `ancestor(floor, height(c)) == c` (c is an ancestor of the floor or the floor itself) or `ancestor(c, floor.height) == floor` (c descends from the floor). This is the header-only form of `ConnectBlock`'s `HasConflictingChainLock` (`validation.cpp:2516-2518`), which is what actually stops Core from connecting a non-floor chain.
- Whenever an eligible floor descendant exists, it has more work than every ancestor of the floor, so (b′) selects exactly what (b) selects today. In the stuck case (b′) yields Core's trailing tip.
- In the leaf walk, a candidate C that fails (b′) is replaced by `lca(C, floor)`. That entry is on C's checked path or on the active chain, so it satisfies (a).
- Define (a) relative to the step's enforcement cursor (one `VirtualTip` per step; see O-m4).
- Weaken A-23's predicate and the `set_active_tip` refusal to "does not conflict with `cl_floor`".
- Record an SB0 §4.1 erratum next to Q-r1-1. SB0's "every published SPV tip descends from `cl_floor`" becomes "never conflicts with it". This is Core's node behaviour and adds no stricter-than-Core rule.
- Add A-29 (ix) for this sequence. Harness `cl` with `seed-cl`, two signed toggles and two locks; the oracle is Core's tip, statuses and `m_best_header` after the scheduled enforcement.
- Update the §6.2 equivalence proof: the maximum over leaf candidates with the lca replacement equals the maximum over all accepted headers satisfying (a) ∧ (b′).

### O-M2 — `TipHost` has no per-`MarkConflictingBlock` hook, so CH1-node cannot reproduce Core's per-call effects in Core's order

**Refs:** §4 `TipHost` (`:309-322`), `EnforceReport` (`:301-302`), §7.3 "Candidate-set erasure and re-add …, `ConflictingChainFound` and the signals are CH1-node's" (`:571`). **Core:** `validation.cpp:4114-4148`.

Each `MarkConflictingBlock` call ends with the following, in this order, under `cs_main` and before the next sibling is processed:
- `setBlockIndexCandidates.erase` of the walked and marked entries;
- the full-index candidate re-add scan against the *current* tip;
- `ConflictingChainFound(pindex)`, which logs and runs `CheckForkWarningConditions`;
- `SynchronousUpdatedBlockTip`/`UpdatedBlockTip(m_chain.Tip(), nullptr, IBD)`;
- `uiInterface.NotifyBlockTip(…, pindex->pprev)` when the block was in the chain.

`TipHost` exposes only `tip`, `contains` and `disconnect_tip`, and `EnforceReport` returns only id lists and a *count* of notifications after the whole `EnforceBlock`. So CH1-node can only replay those effects afterwards, and the observable order changes.

`EnforceBlock` marks upper-level siblings first, and only the bottom sibling disconnects. Take a lock on A100 with B110 active and C81/C90 siblings. Core emits `UpdatedBlockTip(B110, nullptr)` for each upper sibling, then the disconnects, then `UpdatedBlockTip(80, nullptr)`. ZMQ's `UpdatedBlockTip` publishes `hashblock` for a null fork (`zmq/zmqnotificationinterface.cpp:160-167`), so subscribers see B110 hashes before the disconnects. A replay after the fact either emits every notification with tip 80 or must reconstruct the tips. The notifications also drive the ChainLock handler's coalesced schedule (`chainlock/handler.cpp:185-207`). Adding the hook later would change a trait that V already implements, and focus item 6 asks for an API that won't need such changes.

**Fix:** add `fn conflicting_marked(&mut self, chain: &mut HeaderChain<S, R>, marked: BlockId, was_in_chain: bool) -> Result<(), Self::Error>` to `TipHost`. Call it at the end of each `mark_conflicting_block`, after the status writes (Core's `:4121`), and before `enforce_block` moves on to the next sibling. V performs the candidate work, `ConflictingChainFound` and the signals there, reading `host.tip()` for the argument Core passes. `VirtualTip` implements the hook by recording `(tip, nullptr)` for `updated_block_tip_schedules`, which replaces the bare count. Add one ScriptedV assertion that the hook sequence and its tip arguments match the harness `cl` trace.

## Minor findings

### O-m1 — A-4 compares `invalid` and `conflicting` exactly, but a header-only replay cannot produce them

**Refs:** A-4 (`:918`), §10.2's projection (`:779`). **Core:** `rpc/blockchain.cpp:1914-1936`.

In a live oracle, `invalid` comes from block validation (`FAILED_*` set by `ConnectBlock`, `InvalidateBlock` or `FindMostWorkChain` propagation). `conflicting` comes from the oracle's own ChainLock history. A-4's replay accepts headers with no blocks and no locks, so any oracle tip with either status is header-valid or active in SB4, and the row fails on the oracle's own history rather than on an SB4 defect. **Fix:** either project `invalid`/`conflicting` too and compare them only in harness scenarios that construct them, or have A-4 replay the oracle's best CL and filter tips whose status depends on block validation. State which oracle tips are expected to compare exactly.

### O-m2 — Node status oracles include `FindMostWorkChain`'s propagation, which belongs to CH1-node

**Refs:** A-16/A-18/A-21/A-26/A-28 node halves (`:931-945`), ScriptedV (`:905`), `cl` "statuses after every step" (`:897`). **Core:** `validation.cpp:3548-3570`.

When a downloaded candidate has a marked or failed ancestor, `FindMostWorkChain` writes `BLOCK_CONFLICT_CHAINLOCK` or `BLOCK_FAILED_CHILD` on every entry between them and marks them dirty. The `cl` mode emits statuses after the scheduled `EnforceBestChainLock`, which includes `ActivateBestChain`. So A-21's "C82…C105 unflagged" is Core's answer only when C has no block data. If C's blocks were downloaded, Core flags C82…C105, SB4 does not, and ScriptedV has no hook to reproduce it without copying Core's statuses, which would self-certify. **Fix:** state that SB4's node scenarios keep non-floor branches data-less, or have `cl` also emit statuses at the `EnforceBlock` boundary (before ABC) and compare SB4 there.

### O-m3 — Node publication inside `enforce_block` exposes states Core never releases `cs_main` on

**Refs:** §7.3 node host (`:572`), §9.1 node (`:705`), D28. **Core:** `EnforceBlock` holds `LOCK2(m_chainstate_mutex, cs_main)` for the whole walk (`validation.cpp:4052`). `InvalidateBlock` instead re-locks per disconnect (`:3942`), and ABC per step.

D28's justification for per-step publication (the tip trails the floor during ABC) holds for ABC and `InvalidateBlock`, but not inside `EnforceBlock`. Per-step node snapshots during a `MarkConflictingBlock` disconnect loop let a P2a reader see, for example, tip B95 with B81 not yet marked and `best_header` half walked back. Core's readers see only the state before or after `EnforceBlock`. R8 exists to prevent facts that never coexisted. **Fix:** inside `enforce_block`, the node keeps each step's `seq` and F1 event but stores the snapshot once, when `enforce_block` returns. Per-step publication stays everywhere else.

### O-m4 — One `VirtualTip` per step needs to be stated

**Refs:** §9.1 stage 1 (`:722`), `apply_chainlock(cl, host)` (`:234`), §7.3 (`:574`).

A step can run several enforcements: `apply_chainlock`'s internal one, plus a timer or `floor_moved` enforcement. If each starts a fresh cursor at the published tip, the second one re-disconnects entries that Core's `m_chain` already lost. That repeats `disconnected` and the notifications (and their `UpdatedBlockTip` schedules) and runs the walk-back comparisons against a tip Core no longer has. **Fix:** one `VirtualTip` per step. Every enforcement in the step and O-M1's (a) cursor use it.

### O-m5 — SPV cold `prev`: `HeaderRecord` has neither `mtp` nor `time_max`

**Refs:** §5.2 step 3 allows a cold prev (`:437`); §5.4 needs `prev.time_max` and `mtp` (`:467`); `ConsensusCtx.median_time_past = prev.mtp` (`:450`); `HeaderRecord` (`:384-385`).

A fork deeper than w (allowed with no floor) has an evicted parent. `add_to_block_index` then has no `prev.time_max`, and `header_ctx_for` has no `prev.mtp`. **Fix:** either add both fields to `HeaderRecord` (SB13's schema), or compute `mtp` from the `RuleContext` (R ≥ 11 already loads the ancestors) and state what `time_max` means in SPV.

### O-m6 — A-8 (and A-5…A-7 off the real chain) judge SB4 against SB4

**Refs:** A-5…A-8 (`:919-923`).

`complete()` is SB4's own arena. For A-8's synthetic maximum-length KGW walk there is no Core verdict at all. `dash_oracle_sys::Oracle::next_work_required` (`dash-oracle-sys/src/lib.rs:714`) is Core's `GetNextWorkRequired` over consecutive raw headers, so it can judge exactly these windows. **Fix:** keep the windowed ≡ `complete()` check and also compare `complete()`'s target with the oracle's for A-8's chain and for the A-5…A-7 boundary heights.

### O-m7 — The checkpoint cache after SPV reopen

**Refs:** §11.4 (`:816-818`), §1.1's SB5 column (`:44-45`). **Core:** `node/blockstorage.cpp:429-441`.

The cache is maintained only in `add_to_block_index` and `load`. A genesis-anchored SPV chain (regtest, devnet, or testnet without a pack) holds checkpoint hashes. After SB5's `open`, the cache is empty and the rule is silently off, so the reopened chain accepts forks that Core rejects. **Fix:** list "rebuild `highest_checkpoint` (one `by_hash` per checkpoint)" in SB5's `open`. Also reword "anchored above every checkpoint has none in its index": a pack header can itself be a Core checkpoint. That is harmless, because every new height is above the anchor, but the sentence is false.

### O-m8 — SPV status writes become reader-visible before commit and publication

**Refs:** §3.7 (`:153-156`), §9.1 (`:715`), A-35's failure arm (`:952`).

Resident marks are seqlocked writes in stage 1. Until stage 4, `IndexAccess::status` reports `CONFLICT` on entries in the *published* active chain (B81…B110 while the snapshot says tip B110). After a failed commit, memory and store disagree, as A-35 records. Core's readers see statuses and tip under one `cs_main`. **Fix:** in SPV, route every status write of the step through the overlay (resident entries included) and apply it at stage 4 under the tip lock. Alternatively, state that SPV status reads are not snapshot-coherent and that no SPV reader combines them with a snapshot.

### O-m9 — Small API gaps for the dependents

- `fork_target() -> Option<BlockId>` cannot express a storage error (§6.2 "a read error trips the latch") or, after O-M1, which branch it chose. SB14/SB15 code against it. Make it `Result<BlockId, ChainError>` in SPV.
- §4 says `ChainAccess` is implemented on `ChainReader` (`:416`). But P2a's trait (`P2a-outbound.md:284-298`) has `loading_blocks()` and `is_initial_block_download(snap, now)` without the flag, and returns infallible `entry`/`ancestor`. `ChainReader` has no loading flag. Name the implementer (CH1-node's `ChainView` wrapping `ChainReader`), or add the flag to `Shared`.
- CH1-node's `AcceptBlock` calls `AcceptBlockHeader` alone, without PNBH's per-header `CheckBlockIndex` or a status publish. Expose `accept_header(header, hash)` now.

### O-m10 — §7.4 underspecifies `ResetBlockFailureFlags`' conditions, which drive the dirty set

**Refs:** §7.4 (`:582`). **Core:** `validation.cpp:4176-4215`.

1. The ancestor loop acts on `nStatus & (FAILED_MASK | CONFLICT_CHAINLOCK)`. So with `ignore_chainlocks = false`, an ancestor carrying only `CONFLICT` is still dirtied and reconsidered, and its `FAILED_CHILD` children become `FAILED_VALID` and enter `m_failed_blocks`.
2. The descendant loop's `!IsValid()` uses the default `VALID_TRANSACTIONS`, so every header-only descendant is dirtied, even with no failure bit.

Both change the block-tree diff the `pnbh`/`cl` modes emit. **Fix:** state both conditions verbatim.

## Nits

- **n1** §5.2 step 8 (`:442`): say that step 11 (`AcceptedBlockHeader`) does not run for a header inserted with `CONFLICT`. Core returns at `:4698`, before `:4724`.
- **n2** C1 (`:998`) bundles two crates and two conventional subjects into one commit. Split it.
- **n3** `LoadError::GenesisMismatch` and `Empty` (`:411-413`, `:788`) are not Core's `LoadBlockIndex` behaviour. Core loads a second null-prev root and fails later at init's genesis check, and treats an empty index as `LoadGenesisBlock`. Declare them with D21.
- **n4** §3.6 (`:137`): "an unshared path is mutated in place" cannot happen while both `TipState.snapshot` and the `ArcSwap` hold the root. The copy cost is fine; reword.
- **n5** `ActiveChainView::fork_in_global_index(locator)` (`:139`) needs `LookupBlockIndex` and Core's `Genesis()` fallback (`validation.cpp:182-196`). Give it the reader (or a lookup) in the signature.
- **n6** §10.1 (`:768`): SPV's locator terminus, "the floor's lowest accepted entry", is ambiguous between the anchor and `headers[0]`. Pick one.
- **n7** (b)/(b′) is gated by the observed flag, but the Core mechanism it stands in for at the header level, `ConnectBlock`'s conflict check, is live-gated (`chainlock/chainlock.cpp:77-79`). The difference is transient (until the next check). List it with D12.
- **n8** Timer steps with an empty WriteSet still commit and publish every 5 s. Allow skipping stages 3–4 when nothing changed (mobile).
- **n9** ScriptedV should assert that SB4's `disconnect_tip` requests equal the trace's `DisconnectTip` sequence, not merely follow it.

## Focus items: what checks out

1. **`accept`:** Step-for-step Core. This covers:
   - the genesis short-cut, including the `AcceptedBlockHeader` call;
   - FAILED before CONFLICT on a duplicate;
   - PoW and `devnet-genesis` before the prev lookup;
   - FAILED before CONFLICT on prev;
   - the contextual rules before the lazy `m_failed_blocks` walk, under `!IsValid(SCRIPTS)`;
   - the conflict insert before `bad-chainlock`;
   - `CheckBlockIndex` after every header, and `pindexLast` only on acceptance.

   The checkpoint rule matches `GetLastCheckpoint` (a status-blind lookup, so conflict-inserted entries count, which the cache gets right through `add_to_block_index`). `time-too-new` reads `NodeClock`, and `bad-version` reads buried heights. The read envelopes match `pow.cpp`: the BTC ancestor at h − interval, the min-difficulty walk-back to the interval boundary, KGW's 4,032 entries plus the guard read, and DGW's 24 entries.
2. **`best_header`:** Matches Core in every case:
   - the strict-greater acceptance with no update for CONFLICT;
   - the four walk-back sites and `InvalidChainFound`;
   - the load comparator, applied after `FAILED_CHILD` propagation and including `VALID_TREE|CONFLICT` entries;
   - no recompute on reconsider, as in Core;
   - the floor's update rules: HeightMismatch records `cl_best` and keeps the old floor; Pending keeps the old floor; the late header is synchronous within PNBH (`validationinterface.cpp:269-272`).

   The SPV `fork_target` is correct except for O-M1.
3. **Publication:** Sound for the node apart from O-m3, and for SPV apart from O-m8. F2, R8 and the stage-4-only eviction hold.
4. **Codec:** Matches `CDiskBlockIndex` (`chain.h:391-413`) minus the version prefix and the inline hash: VARINT modes, conditional file/data/undo, header order, PoW on the stored hash, and the load's first-mention ids.
5. **Oracles:** The harness modes (`pnbh`, `cl` in DEC-128 order, `seed-cl` named as seeding) produce Core verdicts for the acceptance, lock and spork rows. O-m1, O-m2 and O-m6 are the exceptions.
6. **API:** Adequate for SB5 (same crate) and P2a, apart from O-M2 and O-m9.
7. **Schedule:** The arithmetic is correct (A: 15.5 days; B: 8 days, finishing before C6). C6 (3.5 days) carries most of the oracle-dependent rows. O-M1 and O-M2 add about 0.5–1 day there, which stays inside the ~7-day margin. As §17 already says for C6, C3's harness-dependent halves (A-1a, A-24) should be allowed to land as a follow-up commit before merge.

## Validation

- Read the full design and Sol r1–r3. Read the pinned Core sources listed above and checked each cited site, plus `pow.cpp`, `spork.cpp:163-196`, `rpc/blockchain.cpp:1745-1757` and `:1880-1940`, `txdb.cpp:300-344`, and `validation.cpp:3530-3575` and `:3925-3975`.
- `/work/scratch/sb4-opus-high/model/ft_reset.py` (run with `python3 -I`) models O-M1's sequence and shows the four results tabulated under O-M1.
- Read-only `getchaintips` on both oracles via `contrib/oracle-cli`, to check what A-4 would compare against.
- No dashd was started, no tests were run against an implementation (none exists yet), and nothing was committed, pushed or posted. This file is uncommitted.

## Verdict

**APPROVE-WITH-CHANGES.** O-M1 (adopt (b′) with the cursor-relative (a), plus an SB0 §4.1 erratum and A-29 (ix)) and O-M2 (the `TipHost::conflicting_marked` hook) must be applied before implementation starts. O-m1…O-m10 should be applied in rev3. The nits are optional. No further Sol round is needed unless the manager rejects the (b′) ruling.
