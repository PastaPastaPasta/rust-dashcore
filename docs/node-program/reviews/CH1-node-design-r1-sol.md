# CH1-node design r1 — Sol

Reviewed `designs/CH1-node.md` at **3c7abeda**. Line references to CH1 below refer to that revision. Verdict: **REWORK**. The proposed ownership and Core lock scopes are reasonable, but the specified transitions contain one durability blocker and six major issues. Implementation should wait for their correction.

## Scope and evidence

Read `briefs/REVIEW-COMMON.md`, the charter/process, relevant PLAN task rows, A1's V/read boundary, SB0 and its errata, approved SB4 rev7.1, SB4 C0 at **f815ec15**, SB5 at **17750c58**, CH2, the EV addendum, and DEC-95/99/102/142/144–149/151. PLAN §7 handling: codenames only; no private issue investigation.

Core citations below are to **v24.0.0-rc.3**, obtained with `git show` from `~/workspace/dash`. Some CH1 comments have shifted line numbers; this report uses the actual tagged source. `v` = `src/validation.cpp`, `bs` = `src/node/blockstorage.cpp`, `h` = `src/chainlock/handler.cpp`, `cl` = `src/chainlock/chainlock.cpp`.

Scratch evidence is under `/work/scratch/ch1-node-design-r1-sol/`. No branch source was changed, committed, or pushed. The only repository addition is this requested review.

## Findings by severity

### R1-B1 — blocker: a disconnect freezes coins under the removed block's hash

**Location:** `designs/CH1-node.md:763`, also `:402–403` and `:999`.

`new = step.block()` is incorrect for `TipStep::Disconnected`. At f815ec15, `dash-chain/src/tip.rs:47–63`, `block()` returns the event's block in both variants. A disconnect event names the block being removed, not its parent; SB5 §3.1 deliberately aliases this exact type.

Concrete case: disconnect B110 to B109 and freeze. The connector has committed the coins' best block B109, and the private chain tip is B109. The returned event is `Disconnected { block: B110, seq: q+1 }`. CH1's rule consequently builds a packet with **B110's hash and height but B109's coins/evo delta**. CH2 §4.2 writes that packet's `new` into `meta.tip`/`meta.evo_tip`, or the split-flush marker. Reopening can load B110 as the chain tip over B109's UTXO state; replay likewise targets the wrong block. Subscriber acknowledgements are also labelled with the wrong destination.

Core does not make this pairing: the coins view is flushed before `SetTip` (`v:3285`, `:3304`, `:3316`), and the full flush passes the coins cache's `hashBlock` to the database (`v:3092`, `src/coins.cpp:254–255`). DEC-142's `commit(best_block)` makes the CH2 coins identity explicit rather than repairing this mismatch.

**Fix:** after the tip step, capture **`(private_tip.block, step.seq())` under the existing guard**. For a disconnect that is `(parent(removed), q+1)`. A post-step API can return this destination separately from the event. Assert packet `new.hash == coins.best_block == evo.best_block` when evo applies. Test a one-batch and split freeze after a disconnect, including reopen/replay and a synced subscriber.

### R1-M1 — major: accepting a late header never updates `ClSigs.best_known`

**Location:** `designs/CH1-node.md:509–511`, `:534`, `:655`, `:696`.

The mirror writes `best_known` only in the `Known` recording branch (`:615`). Both header acceptance paths can establish a floor through SB4's `AcceptedBlockHeader`, but neither updates the signature mirror. Publication alone does not perform that mutation.

Concrete case: record a verified lock on unknown A85. The `Pending` branch stores `best=A85` and leaves `best_known=None`. Accept A81…A85. SB4 now sets `cl_floor=A85`; CH1 still publishes `best_known=None`. Its own floor/known debug invariant fails, and a subsequent successful enforcement attempts to notify with no known signature. An existing lower known lock makes the problem worse: the notification can carry that older signature.

Core's late-header path sets **both** `bestChainLockWithKnownBlock` and `bestChainLockBlockIndex` (`cl:156–171`). The targeted Core run of `a29-ii-late-header-before-check` confirmed `floor=A85`, `best_known=A85@85` immediately after `pnbh`, before the following timer.

**Fix:** expose/report the late-header floor transition, and update the node's matching known signature before any SB4 snapshot is wrapped or any reply is sent. Cover `ProcessHeaders` and `AcceptBlock`/`accept_header`, including partial batches, a prior lower known lock, and a known-height mismatch that must retain the old pair.

### R1-M2 — major: enforcement can notify with a signature from a newer command

**Location:** `designs/CH1-node.md:636–643`.

CH1 saves `floor`, then calls ABC, which explicitly services class M commands at release points. After that call it notifies with the **current** `cl.sigs.best_known`, rather than the signature belonging to the saved floor.

Concrete schedule: enforcement captures A100 and ABC starts activating its branch. At an ABC release point, `RecordChainLock(A101)` runs, records the known A101 signature, and defers its enforcement. ABC finishes with A100 contained in the active chain. The running command emits `NotifyChainLock(A100, signature-for-A101)`. The same error is possible with a higher lock on another branch or with an observed reset during the activation.

Core captures the pair once with `GetBestChainlockWithPindex` (`h:283`), carries `currentBestChainLockBlockIndex` through enforcement and ABC (`h:296–309`), and notifies with the **captured** `clsig` (`h:318–320`). No Core schedule can produce the mixed pair above.

**Fix:** capture `(floor, Arc<known_signature>)` together at enforcement entry and use that pair throughout the operation, including the final membership check and notification. Add a deterministic test that records a higher known lock at an ABC release point. Do not turn the final check into a stronger requirement that the captured lock still be the latest lock; Core does not require that.

### R1-M3 — major: a rejected CLSIG skips Core's observable active-state reset

**Location:** `designs/CH1-node.md:659–671`, particularly the claim that the virtual precheck is one Core schedule.

The off-V helper predicts the reset's height result, but the actual reset is deferred until `RecordChainLock`, which receives only successfully verified locks. A seen/stale rejection or signature failure can therefore return without performing Core's initial `CheckActiveState`.

Concrete case: observed state is off, an old A100 lock remains, and the live spork turns on. Receive a previously unseen lower-height CLSIG whose signature fails. Core first sets observed state on and clears the old lock (`h:136`, `:209–220`), then passes the cleared-height precheck and rejects the signature (`h:147`, `:154–163`). After processing that message, the best lock is empty. CH1's helper returns -1, verification rejects the message, and no record command runs: the published best remains A100 and observed remains off. A read after the completed rejection and before the timer cannot be reconciled with Core's ordered operation. A duplicate also reaches `CheckActiveState` before Core's seen-map rejection (`h:136–142`).

**Fix:** provide a class M intake/precheck phase that performs the actual active-state transition on V **before every early return**, then returns the precheck result and required verification context. Signature arithmetic may remain off V. If a different mechanism is chosen, specify how the reset completes before rejection is reported, including duplicate/stale inputs; predicting only a height is insufficient. Respect the EV addendum's whole-route V dispatch for quorum selection/materialization.

### R1-M4 — major: an `AcceptBlock` failure can leave the published best header and floor stale

**Location:** `designs/CH1-node.md:524–525`, `:534–545`.

SB4 C0's `accept_header` explicitly does **not** publish; its caller must publish. CH1 publishes only on the successful `ProcessBlock` path.

Concrete case: a new higher-work block has valid PoW/header and a valid context-free block structure, but a wrong coinbase height after HEIGHTINCB activation. The first `CheckBlock` passes. `accept_header` inserts its header, potentially changes `best_header` or resolves a pending lock, then `ContextualCheckBlock` rejects it as `bad-cb-height` (`v:4580–4585`, `:4777`, `:4819–4824`). CH1's `!ok` branch replies and returns before `publish_status`/`publish_view`. The writer index contains the new best header and failure flag while P2a's completed-operation snapshot still has the old best header/floor. This violates SB4 R3 and CH1 §6.2.

Core also returns from the failed acceptance (`v:4887–4893`), but its subsequent `cs_main` readers see the mutated index; there is no stale snapshot awaiting a later successful block.

**Fix:** publish status and the coherent signature/view changes on every nonfatal exit from an acceptance scope that changed shared facts, before the failure reply. Preserve Core's omission of `NotifyHeaderTip` on a failed `ProcessNewBlock`; snapshot publication and that notification are separate obligations. Add the contextual-rejection witness, including a pending lock resolved by the accepted header.

### R1-M5 — major: the cached-key candidate representation needs replacement semantics

**Location:** `designs/CH1-node.md:210–218`, `:501`, `:566–575`, `:998`.

The design changes from a set of block identities to a set of immutable keys, and specifies that insertion records `cand_key[b]`. It does not remove an existing key when `ReceivedBlockTransactions` changes a sequence id. That omission matters even without preciousblock: the design explicitly retains duplicate unlinked pairs and visits a child twice.

Concrete literal execution: `unlinked[P]=[C,C]`. Receiving P dequeues C twice (`v:4287–4297`). The first visit inserts `key(C, seq=10)`; the second inserts `key(C, seq=11)` and overwrites `cand_key[C]`. The BTreeSet now contains two keys for C. `InvalidBlockFound(C)` removes the seq-11 key, leaving the better seq-10 key. `FindMostWorkChain` repeatedly selects that failed ghost; erasure through the now-absent cached key cannot remove it. The specified loop need not terminate. Pruning also operates on obsolete keys.

This is a consequence of CH1's representation, not a request to deduplicate Core's multimap. Core assigns the sequence on each visit and passes a block identity to its candidate set (`v:4287–4290`); CH1's declared D2 must define the immutable-key adaptation completely.

**Fix:** specify one live key per block. Before a sequence write/reinsertion, remove its prior stored key; insertion then installs the current key. Every low-end prune must remove the corresponding map entry as well. Keep Core's duplicate queue visits and sequence increments. Test duplicate `(P,C)` visits, redownload/rekey, invalidation, and preciousblock with `candidates ↔ cand_key` consistency and terminating selection.

### R1-M6 — major: Core's callback queue and SB5's tip feed are assigned incompatible contracts

**Location:** `designs/CH1-node.md:192`, `:830–838`, `:973`; missing explicit backpressure loops at `:379–383` and `:402–406`.

CH1 correctly says that the F1 tip stream is separate from Core's `BlockConnected`/`UpdatedBlockTip` notifications, but calls the latter an SB5 `ChainEvent` feed and asks to enforce its bound only at Core's queue-limit sites. At 17750c58, **SB5 `ChainEvent` is exactly `TipStep`** (§3.1). It has no queue carrying those Core callbacks or registration API for the proposed synchronous listeners. SB5 §3.3 instead requires V to drain pending tip fanout **after every connect/disconnect**, before the next tip step.

Concrete case: a multi-disconnect ABC hold fills a slow subscriber's Q-sized F1 queue. Deferring all backpressure to `LimitValidationInterfaceQueue` at the next outer iteration leaves pending fanout when the next disconnect starts. That violates SB5's node writer rule (`SB5:201–205`) and can take its `ChainError::Internal` path. Expanding the tip feed to include the listed Core callbacks would also break its `TipStep` shape, sequence/compression semantics, and CH2's consumer contract.

**Fix:** give the Core validation notification dispatcher a separate owner/type/API and pending-callback count. Keep its nonblocking enqueue under the hold and its Core queue-limit sites. Retain SB5's F1 stream unchanged, and explicitly place `retry_pending`/CH2 service loops between every node tip step, using the existing held guard and excluding R/M command service. Coordinate this extension of composite-guard support with SB5; B1–B3 are not just edits to the existing SB5 event feed. Model Q=1 with two consecutive disconnects and a parked subscriber.

### R1-m1 — minor: R6's work bound is stronger than Core's release predicate

**Location:** `designs/CH1-node.md:153`, `:886`.

The pseudocode correctly uses the complete comparator (`:265`), but the normative explanation and reorg test say the hold ends only when work **exceeds** the starting tip's work. A precious equal-work branch with negative sequence id is already better under Core's comparator; ABC can release with exactly equal work (`v:3811`, `:3873–3883`). An invalid-candidate search can also break on no remaining work to do (`v:3777–3780`).

**Fix:** state/test the actual inner-loop predicate and its early exits. Ordinary IBD with valid candidates advances one block per iteration; that is not a universal bound on work spent rejecting invalid candidates. Add an equal-work precious reorg to the R6 test.

### R1-m2 — minor: the proposed reference/crash assertions reject allowed Core states

**Location:** `designs/CH1-node.md:880–882`, `:900`.

Three reference rules need correction:

- The brute-force candidate rule requires data over the whole path. Core stops walking at the active chain (`v:3537`) and retains the active tip even when older active blocks have been pruned. An already-active pruned A100 is not disqualified for lacking A1's data. Evaluate only the non-active suffix and preserve the active-tip fallback.
- `take_dirty()` returns a delta, not a complete index database. Load equivalence must apply all index batches to a retained database image before `NodeIndex::load`. Reload also intentionally resets sequence counters and reconstructs bookkeeping; compare with Core's load rules, not every live side table indiscriminately.
- Crash recovery cannot always reach the uncrashed run's final tip. With durable A100, invalidate A90 and crash before the new dirty flags reach B1: the live run ends at A89, but reopening the old index can retain A100. CH1 itself correctly allows this at `:796`. DEC-148 also permits a read to fail-stop. Judge recovery against the selected durable prefix, followed by ordinary ABC, or the permitted fail-stop outcome.

**Fix:** amend the test oracles and include these directed controls. DEC-95/99/102 require Core semantics, not stronger recovery/selection invariants.

### R1-m3 — minor: failed header batches emit a notification Core skips

**Location:** `designs/CH1-node.md:512–514`.

A batch accepting H1 and rejecting H2 returns false before Core reaches `NotifyHeaderTip` (`v:4739–4741`, `:4747`). CH1 invokes it regardless of `res.invalid`. It therefore emits an extra signal/progress log and advances `last_notified_header`; a later successful operation can lose the notification Core would emit then.

**Fix:** still publish the accepted prefix as SB4 R3 requires, but notify/log only on complete batch success. Test accepted-prefix-plus-invalid-header followed by a successful batch.

### R1-m4 — minor: H1 must record verdicts by invocation, not only by hash

**Location:** `designs/CH1-node.md:840`, `:975`.

`ConnectBlock` verdicts depend on state as well as block bytes. For example, download A on a side branch, clear its conflict marks with `reconsiderblock(..., true)` while a competing live lock remains: a connect attempt can fail `HasConflictingChainLock` (`v:2516–2518`). Turn the spork off and reconsider it again: the same bytes can connect. A single `BlockChecked` verdict per hash cannot replay both attempts, and a failed acceptance callback is not a connector invocation.

**Fix:** H1 should emit an ordered transcript of call phase, block hash, result/state, and attempt occurrence, including repeated attempts, with consumption assertions. Keep the connector seam separate from `BlockChecks` acceptance verdicts. H4's random schedules should preserve that association.

## Decisions on the requested focus areas

**§4 fidelity.** The ordinary comparator is correct: `(work, Reverse(sequence), Reverse(id))` matches `bs:32–49` with SB4 D9's address substitution. Low-end pruning against the **complete** tip key matches `v:3584–3593`. Preciousblock's erase-before-change, negative counter reset on increased active work, and saturation at `i32::MIN` match `v:3864–3883`. Retaining unlinked duplicates and insertion order matches `equal_range` plus queue order (`v:4292–4297`); pruning must erase every matching `(prev,id)` pair (`bs:149–156`). R1-M5 supplies the missing immutable-key rule.

The `:3983` condition is copied faithfully. The tagged source compares the walk tip's parent with `to_mark_failed`, then assigns `to_mark_failed=walk_tip` (`v:3983–3988`, `:4003`); it must not be changed to fit the misleading comment. The targeted invalidate run retained `FAILED_VALID` on both A100 and A99 (status 61). `InvalidChainFound(front())` also really receives the descending batch's top (`v:3634–3640`, `:3649–3650`), not its failed seventh block. The failed/missing/conflicting ordering, best-invalid work comparison, descendant dirtying and unlinked reinsertion in §4.3 match `v:3544–3574`.

**Q1: accept the hold refinement**, with R1-M6 fixed. `v:3763–3827` holds `cs_main` and the mempool lock across the ABC inner loop; publishing the fork point between its disconnects would weaken that boundary. SB4 D28's per-step ABC statement needs the proposed erratum. DEC-144's pressure decision and CRITICAL wait remain at **every** disconnect. DEC-145's events remain at each step; snapshots remain at release.

The H1–H7 contracts extend to an ABC hold without a new lock cycle: W uses its own channel/lock-free registry and closing flags, V services `PruneClears`/`FlushDue`, F1 fanout progresses under the held guard, and subscriber processing/ack/pop never takes that guard. SB5's live subscribers can progress while new registrations/handoffs wait. Lock-free snapshot readers continue seeing a complete older snapshot. R/Query and M commands wait for Core's release point; a deep reorg can take arbitrarily long, as Core's can. There is no justified fixed real-time latency bound. Bounded release-point service plus FIFO deferred work gives progress under finite operations, eventual I/O completion, and weak scheduling fairness. W must not be starved by delaying required F1 fanout or by requiring the deferred control mirror before its direct acknowledgement channel.

**Q3: accept the split in principle, reject its current packet identity (R1-B1).** After the connector commits, the intervening mempool operations and tip step do not change the frozen coins/evo/index payload. Capturing the correct post-step destination and sequence can therefore preserve durability while aligning CH2's subscriber protocol with E4/D45. Specify that all freeze causes serviced in a wait respect this pairing; a pending automatic freeze must not consume the new coins while still labelling them with the pre-step tip.

The claimed synchronous-hook deadlock is real **for CH2's adapted protocol** if a correctly labelled backward freeze starts before its disconnect event: W's rewind gate waits for the synced subscriber to rewind, the subscriber needs the not-yet-produced disconnect, and V waits for W's completion under `-dbcrashratio`. A forward case can likewise need the not-yet-produced sequence. Core's synchronous flush has no subscriber-ack wait of this kind. Moving the freeze after the F1 step fixes that dependency provided pending fanout continues to be serviced during the wait. Keep pressure evaluation and waiting for a previous F at Core's sites; do not reinstate the rejected end-of-hold pressure deferral.

**R/M/C: accept the basic partition.** Headers and acceptance-only work at ABC release points, with class C tails deferred, are schedules Core can produce under its two locks. The record/enforce split is necessary; Core records before returning the relay inventory and schedules enforcement separately (`h:166–182`). FIFO deferred ordering is a permissible schedule. The actual violations are R1-M2/M3, not the existence of the split. Correct R6 as R1-m1 requests.

**§2.6: the no-cycle argument is conditionally sound.** H1–H7 and LO-5/LO-6 remove the return edges to V/L1; the EV allow-list removes gate-holder dispatch back to V, and `OnV`/attempt capabilities prevent self-upgrade. The important test obligation is to model the real channel and feed waits, not merely mutex order. Give SB4 index **read** locks a place in the table (they can occur under L1/L3 but have no other-thread writer), and require callbacks under registration `finish()` and wakers to obey LO-5/6 too. Those clarifications do not require a new architecture.

**Q4: accept CH1-node ownership.** It resolves the duplicate PLAN assignment and agrees with SB4 §1.2. CH3a remains the contents/undo/evo connector and CH3b owns real reorg/mempool integration and functional/crash tests. Remove the stale “per-profile” wording in `:46`, `:773`, `:988`; RP-V24/DEC-82 left one target release. The N0–N9 dependencies are plausible, but the SB4 API and SB5 queue/guard changes must land before N3/N5, with actual flush integration tested again at CH2b rather than inferred from `NoFlush`.

**Q10: accept a harness follow-up**, with R1-m4. H1–H3 are necessary to compare real V inputs and index bookkeeping; H4 is useful for randomized differential coverage. H4 is a random **operation** generator, distinct from the existing random callback-delivery policy; keep both and DEC-147's semantic-state/idempotent-repeat comparison. Record the scheduling policy used by real V, and add controlled release-point injections for R1-M2/M3. Replaying all existing vectors also needs explicit handling of fixture ops such as `seed-cl`, `mark-conflicting`, `notify`, and checkpoints. Obtain those through narrow test adapters, preserving the EV prohibition on public/test constructors of V capabilities. Harness `reload` explicitly force-flushes before teardown; a recovery replay must reproduce that stimulus and must not mistake it for a power-loss test.

## Assessment of the eleven SB4 API asks

| Ask | Decision against rev7.1 and f815ec15 |
|---|---|
| A1 | Necessary. Generalize the owned composite guard and node publication policy; preserve per-step seq/fanout, pending drains, reentrant checks and pre/post handoff exclusion. Requires the D28 erratum and SB5 coordination. |
| A2 | Necessary under a lagging snapshot. Return an optional pre-genesis tip and an explicit post-step destination; never derive a disconnect freeze from the event's removed block. Existing best-header hooks must also use the private active chain. |
| A3 | Necessary full-index traversal capability for the stated Core scans. An iterator/callback over resident node entries is sufficient; no SPV residency-policy expansion is implied. |
| A4 | Convenience, not a correctness requirement. V's own seqlock reads cannot race another writer. Add only if measurement warrants it; keep entries borrowed/read-only so writes still go through `modify` and dirty sites. |
| A5 | Necessary node scheduling split. Preserve the record-only no-spork-gate semantics, stale/mismatch retention, active-state call order and SPV's fused composition. Add the late-header mirror transition required by R1-M1. |
| A6 | Useful, not uniquely necessary. The existing `ClActiveState::TurnedOnReset` can suffice if every caller observes it. A generation is acceptable when nested calls conceal the transition; it does not solve rejected-input ordering by itself. |
| A7 | Needed behaviour, but mostly already present: f815ec15's `EnforceReport` **already has `reset: Option<ResetReport>`**, and `enforce_best_chainlock` already returns `Option`. Preserve `Some` for a performed no-op enforcement, rather than treating an empty report as skipped. Capture the floor/signature for the node tail. |
| A8 | Necessary private-tip IBD evaluation using the existing shared latch. Do not create another latch or a cached time-dependent boolean. |
| A9 | Reasonable, necessary extension point for one coordinated Core DFS. Specify shared `pindexFirst*` state and report failures without exposing mutation to visitors. |
| A10 | Reasonable contextual-header validation seam for `TestBlockValidity`. It must be non-inserting/non-publishing and cover the actual proposal-time rules (`v:4934–4963`); a wrapper around insertion/duplicate acceptance is insufficient. |
| A11 | Necessary node-only empty active-chain state. It changes concrete C0 types: `TipState.tip`, `TipFacts.tip`, `ActiveChainView::tip`, and private-tip APIs currently require a block. Specify absent tip/height -1 representation and initial snapshot/IBD/query behaviour. Keep SPV's anchor construction contract. |

The substantive approved-SB4 refinements are A1/D28 and the exact freeze sequencing under its holds, plus the node record/enforce separation. They are compatible with rev7.1's intent once explicitly adopted. A11 needs an actual API/type change rather than only a new constructor rule. None justifies changing Core's `:3983`, batch-top invalid-chain argument, conflict dirtiness, or permitted trailing-floor tip.

## Validation and limits

- Reviewed the tagged Core bodies and the exact C0 type/method definitions. Verified the Core-linked oracle pin **a9a483e51efdadeee37cf40ff8697da62b3cd012** differs from the reviewed `validation.cpp` only by appended oracle trampolines; the compared chainlock/blockstorage bodies are unchanged.
- Ran four pinned input scenarios against a private copy of the available Core-linked harness: `a29-ii-late-header-before-check`, `a18-invalidate-walkback`, `a18-invalid-chain-found`, `a19-first-seen-tie`; all exited 0. Asserted the late-header floor/known-signature pair and inspected invalidate status changes. Results and stderr are in `oracle/` beneath the scratch directory named above.
- This was **not** a byte-for-byte regeneration assertion for harness 7c35a6c6: its worktree is undergoing changes, and the copied binary's manifest/source SHA identifies a newer harness snapshot. The immutable copied binary SHA-256 is `75b924c4d85d0bd3c2a6d813f0d950add03c91b9521db9c1519cdc56fa081e86`. The tagged source, rather than repeat counts from this binary, establishes the review conclusions.
- Ran `python3 /work/scratch/ch1-node-design-r1-sol/design_witnesses.py`: **8 tests passed**. These execute the specified transition witnesses and exhaustively compare the comparator over 3,600 input pairs. They demonstrate design errors/expected cases; they are not CH1 implementation, real-W, or threaded liveness tests.
- Used a private mode-700 `/dev/shm/ch1-node-design-r1-sol-tmp` for oracle fixtures and removed it after the runs. No existing dashd datadir or service was touched. No source build/full workspace gate was needed for this document review; CH1 and the requested APIs do not yet exist as an implementation to validate.

## Verdict

**REWORK.** Keep the proposed V ownership, Q1 lock scope and Q4 split. Correct R1-B1 and R1-M1–M6, refine the Core-based test oracles, and review the resulting SB4/SB5/CH2 seam changes before implementation approval.

Sol gate satisfied: no
