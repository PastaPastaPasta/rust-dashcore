# CH3 design review r1

Reviewed `docs/node-program/designs/CH3-chainstate.md` at `b5ede63b5` against the pinned v24.0.0-rc.3 Core sources, O8, CH1-node, CH2 and the cited PLAN rows. This is a design review; I made no branch edits. I assume the M3 `NodeHelper` and `NoEvo` can be instantiated by a production binary unless a runtime guard is specified. The severity below follows the brief's parity bar, including crash and replay behavior.

## Blocker

### B1. Interim helpers can accept blocks that a synced Core node rejects, with no runtime limit

**Location:** §2.7, §3.3/D18, H3–H5 in §3.1, R6, §9.1. The doc explicitly says `sb_required=false` and `gov_valid=false` until SP1/GV1b and accepts the live-tip difference, while `NoEvo` returns success for H4/H5 until CH7a. It cites milestone exits as the reason no live mainnet/testnet tip is reached, but specifies no guard on the binary or connector.

**Scenario:** An M3 binary catches up to a mainnet or testnet superblock height above its best ChainLock, then receives an overpaying, untriggered coinbase within the superblock maximum. A synced Core node with governance data returns `bad-cb-amount` (or `bad-cb-payee` for a wrong triggered payment); `NodeHelper` chooses `NoCheck` and `gov_valid=false`, so CH3 accepts it. Before CH7a, a regtest/devnet chain that reaches DIP3 also has H4/H5 no-ops and can accept a block without the required CbTx/MN checks. These are actual acceptance differences, not merely different logs. A design milestone does not stop an operator from starting the binary or a test from extending its chain.

**Fix:** Specify an enforced readiness boundary in the production connector and node startup path. Before CH7a, fail closed before the first active Evo rule on every network. After CH7a and until SP1/GV1b, allow historical IBD/reindex only under the explicitly documented Core not-synced regime, but stop block activation and serving when the node reaches a live mainnet/testnet tip where superblock decisions need missing state. Apply the check to tip connects, proposals, ABC/recovery and restart; use a system/readiness failure, never an invalid-block verdict. Add a test that drives a block through the boundary and a restart there. Remove the guard only when the real helper inputs and their tests land.

## Major

### M1. Deferred effect failure occurs after the parent coins cache was changed

**Location:** §2.11 commit point, §2.13/D14, §3.2/D19 (`connect_inner` then `finish_attempt`), §5.3. Core executes nontransactional effects inside `ConnectBlock`, before `ConnectTip` calls `view.Flush()` and commits EvoDB (`validation.cpp:3430–3448`). The proposed connector commits its overlay inside `connect_inner`, then `finish_attempt` applies the effect log and can return `Fatal` on an effect-write failure.

**Scenario:** A ChainLocked block emits `RemoveConflictingIsLock`; the effect store faults when `finish_attempt` applies it. In Core the attempted child view has not been flushed into `CoinsTip`, so the parent coins cache still names the old tip. In the proposed order the parent cache already contains the new block's spends and outputs, while V receives `Fatal` and has not advanced the chain tip. The same ordering matters for a failed snapshot write in CH7b. The fail-stop latch may prevent a durable flush, but it does not make the in-memory transition or crash image identical; the design's existing C3/FA7 tests do not inject a fault at this boundary.

**Fix:** Keep the overlay and Evo transaction uncommitted through effect application, then commit them only after `finish_attempt` succeeds. Preserve Core's emission order and the rule that effects survive invalid/`just_check` attempts. Add a fault-injection test at each effect-application boundary asserting old parent coins/evo best block, no published tip, and recovery parity. If the architecture deliberately commits first, state and prove a stronger invariant that no reader, freezer or crash recovery can observe the changed parent after an effect failure.

### M2. A digest cannot seed `MuHash3072` from the golden `muhash` value

**Location:** §2.15/D17 says the running `MuHash3072` is “seeded at the replay's start from the oracle's golden at the start height”; §7.2 promises a 600,000→625,000 per-PR window before O9b segment snapshots exist. O8 §5.3 defines the golden `muhash` as the 32-byte RPC text. Core's `MuHash3072::Finalize` hashes the 384-byte group element to that 32-byte value (`crypto/muhash.cpp:314–323`); it is not a serializable accumulator state.

**Scenario:** Starting the 25,000-block window at height 600,000 with only the golden record for 599,999 cannot produce a running MuHash for 600,000. Inserting and removing that block's coins into a new empty accumulator gives a hash of the block's delta, not the UTXO set. The a7 gate requiring per-block `muhash` cannot run as written. The cumulative unspendable total also needs a starting scalar, although that scalar is available from the golden.

**Fix:** For a replay from genesis, initialize the empty accumulator and evolve it from every block. For a segment, import a verified full MuHash internal state alongside a UTXO snapshot, or scan the starting UTXO set once to reconstruct it; seed the cumulative counters from verified starting values. Until a segment seed exists, make the per-PR gate replay from genesis through 625,000 or use a suffix check that does not claim per-block MuHash parity. Test a segment run against a full replay at its first and last heights.

### M3. Recorded IS-conflict answers are advanced twice in the override loop

**Location:** §3.3 `RecordedHelper`: “`remove_conflicting_is_lock` consumes the next recorded `is_conflict` answer in Core's order.” O8 §7.1 records one `is_conflict` answer at each `ConflictingISLockIfAny` call, including the next loop iteration; Core `validation.cpp:2806–2810` calls `ConflictingISLockIfAny`, removes the lock, then asks again at the top of the loop. The removal itself is not an `is_conflict` query.

**Scenario:** The recorded sequence for a ChainLocked transaction is `[conflict A, null]`. The first `is_conflict` call consumes A. If the removal consumes `null`, the next loop condition has no answer and strict replay fails or wrongly reports a decision miss. With two conflicts, it can skip the second lock and change the emitted removal effects. This directly breaks the stated O8 strict-mode and `feature_llmq_is_cl_conflicts` replay gate.

**Fix:** Only `is_conflict` advances the recorded query cursor. `remove_conflicting_is_lock` acts on the just-returned conflict and updates the live attempt-local removal view; it must not consume the next query answer. Assert the exact recorded query sequence and effect sequence for `[A, null]` and `[A, B, null]`, including a failed attempt after a removal.

## Minor

### m1. Slice a7's declared dependencies omit features its own gate requires

**Location:** §8.1 says a7 needs only a3 and a5, yet §7.2 requires replay at maximum `-par` with `dep`/`bur` fields, while a4 lands the check queue and a8 wires CS4 deployments on V (§2.16). A branch containing a3, a5 and a7 but neither a4 nor a8 cannot run that acceptance gate as specified; the script failure mode and deployment answers are missing. Add a4 and a8 as a7 dependencies, or explicitly split a7 into an earlier serial/partial digest slice and a later full L2 gate. Keep the full gate on the latter.

### m2. The F01 process test can wait on the V thread it has paused

**Location:** §4.6/D28 step 1 pauses V between `AcceptBlock` and ABC; step 2 invokes `ChainCommand::Flush(ALWAYS)`. CH1-node §2.4 classifies `Flush` as a V class-M command, served only when V runs or at a release point. If the hook leaves V paused while the test waits for `Flush` to finish, no durable index-only write occurs and the test deadlocks; killing then does not exercise F01. Specify a hook that schedules the flush at that release point and resumes V before waiting for its completion, or perform an explicitly scoped index-only durable write on V before pausing for the kill. Assert the durable B record before `kill -9`.

## Scope notes

The serial/parallel failure precedence, BIP30/BIP34 shortcut, assumevalid scope, undo byte layout and `HAVE_UNDO` ordering agree with the pinned Core paths on review. Core's disconnect pool budget can change which transactions return to the mempool, so D25's exact Linux accounting and generated trim vectors matter for mempool parity; it does not change block validity or the UTXO set. The three-way disconnect result and CONS-11 restart split are appropriately called out. D14's `BlockChecked` reordering has no demonstrated ordinary-node observer beyond the two named state-only listeners; M1 concerns the separate effect-failure boundary.

**Summary:** 1 Blocker, 3 Major, 2 Minor, 0 Nit. The interim acceptance boundary and three sequencing/replay gates need changes before implementation is approved.

VERDICT: CHANGES-REQUIRED
