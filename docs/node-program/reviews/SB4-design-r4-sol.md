# SB4 design confirmation, round 4 — Sol

Reviewed **36266b40442ba23a914700444ddd24dd3496c287** against **24098412**, specifically `docs/node-program/designs/SB4-dash-chain.md` and SB0's §17 errata. Line references below are to those exact blobs. The integration worktree had advanced, but these two design files had no subsequent diff when checked. Read REVIEW-COMMON, the Opus-high review and Sol r1–r3. DEC-119, 123, 127, 128, 132 and 136 remain binding; PLAN §7 handling was followed, with no private inputs read.

This confirmation covers only **O-M1/E2, O-M2 and O-m3**. Other Opus minors and settled decisions are not reopened.

**Result: REWORK. Sol gate satisfied: no.** The floor-selection correction and per-mark hook are sound. Two gaps remain in the requested wallet argument and node composite's reader boundary.

## Findings

### R4-M1 — E2's unchanged wallet recovery rule is unsafe across a lost spork-off suffix

**Severity:** major. **Refs:** `docs/node-program/designs/SB0-shared-base.md:1934` (E2's claim that every lost published tip agrees with the durable floor), `:1369` (unknown-hash recovery rule); `docs/node-program/designs/SB4-dash-chain.md:755` (a disabled published tip may leave the remembered floor's chain).

E2's cap proof works when the durable floor stays enforced and later floors descend from it. The binding spork behavior also permits a lost suffix in which enforcement was disabled. Checking the restored snapshot's enabled flag cannot establish what happened in that lost suffix.

Concrete counterexample, requiring only **one** verified ChainLock:

1. Durable SPV state is A100, with floor A70 and observed enabled. The lock was already on the active chain, so enforcement need not mark another branch.
2. A Buffered suffix observes SPORK_19 off, then accepts a previously unmarked branch B51…B110, forking at A50. Rule (a) passes; disabled (b′) and the disabled refusal permit publishing B110 while retaining floor A70.
3. The wallet processes that reorg and durably records B110, including a payment in B60, in its separate database. Its header steps committed first, satisfying the existing ordering requirement.
4. Power loss removes the SPV suffix, including the off transition and B's headers. Recovery restores A100/A70/enabled, but the wallet's B110 hash is unknown.
5. The prescribed recovery height is `H = min(70, 110, 100) = 70`. The unknown-at-or-below-floor exception does not apply because the wallet height is 110. `truncate_above(70)` retains B51…B70's wallet effects although the recovered chain contains A51…A70. The real common point is 50.

There are no conflicting verified locks in this witness, so the stated ChainLock safety assumption does not exclude it. The caps bound heights; they do not establish common ancestry after enforcement has been disabled. This is a consequence of composing E2's wallet claim with the already-binding spork behavior, not a request to change that behavior.

**Suggested fix:** for an unknown wallet hash, fall back to the established anchor unless persisted wallet checkpoint evidence proves that the durable floor remained applicable through that checkpoint. The current height/hash interface supplies no such evidence, so unconditional anchor fallback for the unknown case is the simplest safe rule. Record the wallet-rule correction and add this lost-disabled-suffix regression; a restored enabled flag alone is insufficient.

### R4-M2 — Node staging allows `subscribe_at` to admit an unpublished intermediate tip

**Severity:** major. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:745` (release the tip mutex after each staged disconnect), `:752` (the SPV composite's handoff exclusion), `:992` (A-26); `docs/node-program/designs/SB0-shared-base.md:1150` (F10 checks `p == TipState.tip` and makes the slot live at `next_seq`).

Withholding ArcSwap stores protects snapshot readers, but the node composite releases the mutex that also protects SB5's handoff. The staging flag currently specifies suppression of snapshot stores and event handoff; it does not suppress `subscribe_at` or its durable `finish()`.

Concrete interleaving:

1. A subscriber retained an earlier snapshot at B109 and is still catching up to it. V has since connected B110 and published `{ tip: B110, tip_seq: q }`.
2. `enforce_block(A100)` stages its first disconnect, B110. Under the tip mutex, `step` changes `TipState.tip` to B109, consumes `seq = q + 1`, and leaves `next_seq = q + 2`. It then releases the mutex. The published snapshot still says B110/q.
3. The subscriber finishes catching up to its retained B109 and calls `subscribe_at(p = B109)`. The unchanged F10 check succeeds against the staged `TipState.tip`; `finish()` runs and the slot becomes live at `q + 2`.
4. Enforcement continues to the fork at 80 and only then publishes its snapshot and staged events.

B109 is neither the before-enforcement tip B110 nor the after-enforcement tip 80. The handoff has exposed a cut inside the composite, while its published snapshot still describes the earlier cut. SPV excludes this race by holding the tip mutex across the whole composite. A-26 currently observes stores and event order only, so its stated assertions all pass despite this race.

**Suggested fix:** make enforcement staging visible under `TipCell`'s mutex, and have `subscribe_at` return `Moved` without calling `finish()` while staging is active, or otherwise defer it until final publication and event handoff complete. Preserve target capture at event assignment and the existing pending/deregistration rules. Extend A-26 with the delayed B109 subscriber above, asserting that no handoff succeeds during the walk and that the final handoff has a matching published cut. This needs no per-step disk commit and does not reopen DEC-132.

## Confirmations within scope

- **O-M1 selection: confirmed.** SB4 `:520–542` and E2 `:1931–1933` use (a) against the single post-enforcement cursor and (b′) as ancestor/self/descendant compatibility. For targets above the floor, compatibility is the effective constraint from checking the connected ancestry, rather than just calling `HasConflictingChainLock` on the target, which returns false above the floor's height. Core bases: `chainlock/chainlock.cpp:73–96`, `validation.cpp:2516–2518`, `:3533–3575`. While enforcement is off or there is no floor, the floor filter is absent; existing marks still act through (a).
- **Reset witness and A-21: confirmed.** After B100, an observed off→on reset and A90, A81's kept mark excludes A's descendants; B81 and the disconnected B suffix exclude B. The cursor 80 is eligible and returns `Trails`. Core's reset does not clear marks, and `ResetBlockFailureFlags(_, false)` preserves CONFLICT. With A100/B110/C105 and the A100 lock, the answer remains A100; C105 fails both predicates even though C82…C105 remain unmarked at the enforcement boundary. A-21's data-less C stipulation makes that boundary comparison appropriate.
- **Leaf proof: confirmed.** A leaf's candidate dominates every eligible ancestor below its lowest bad entry. If C conflicts with the floor, any eligible E below C must be a common ancestor of C and the floor, so `lca(C, floor)` still dominates E and lies on the checked path or cursor chain. The cursor fallback covers candidates whose bad-entry predecessor is already active. Strictly positive work preserves the maxima and ties. `Result<ForkTarget>`, `FloorRelation::Trails` and `ConflictsWithFloor` match the amended predicate. This remains a proof of the SPV predicate, not equality with Core's downloaded candidate set.
- **Wallet caps with uninterrupted compatible floors: confirmed, but insufficient globally.** Under that premise, both chains agree through `min(f, durable_tip_height, wallet_height)`, including either tip trailing F. The new trailing case itself does not invalidate the arithmetic. R4-M1 identifies the missing lifecycle premise in E2's general claim.
- **O-M2 and A-36: confirmed.** `TipHost::conflicting_marked` at SB4 `:329` is called per mark at `:590`, before the next sibling. `:598–605` assigns V the five phases in Core's order: erase/mempool repair, re-add against the current tip, `ConflictingChainFound`, synchronous/asynchronous tip signals, conditional UI notification. Core `validation.cpp:4080–4150` supports this ordering. In A-36, the C90 mark signals B110 before the B disconnects, and the B81 mark signals 80 afterwards; its `was_in_chain` distinction and null-fork arguments are correct. A-36 specifies the right signal trace; no replay-after-enforcement is needed.
- **O-m3 snapshots/events: confirmed, reader exclusion incomplete.** The stated staging yields one final snapshot, one sequence number/event per disconnect, and ordered SB5 delivery; ABC remains per step. Core holds `cs_main` across `EnforceBlock` (`validation.cpp:4053`). Coherent queries on V cannot interleave with it. Immutable ancestry and retained roots remain safe. The settled per-entry seqlock interface is still a live index-fact interface, not a transaction spanning entries and a snapshot; this review does not reopen that contract. The new handoff race in R4-M2 prevents affirming the requested composite boundary for all reader paths.

## Validation and limits

- Read the exact rc.3 source blobs, not the current Core checkout, for enforcement, candidate walking, floor checks, reset, callbacks and notification order. Core tag: **da567d7dba945143eee4d2ce1621c21b32089989**.
- Ran `/work/scratch/sb4-design-r4-sol/spec_checks.py`: **87,480** exhaustive/random leaf-versus-all-entry contexts passed, including unequal work, ties and marks on the cursor/floor paths; **64** effective-floor comparisons and **1,890** compatible-floor wallet-cap tuples passed. Fixed A-21/A-29(ix) checks and the staged snapshot/event check passed. Separate executable witnesses demonstrate R4-M1 and R4-M2. These are specification models, not execution of an SB4/SB5 implementation.
- Ran `spork_tests` and the three receipt/cache tests from `llmq_chainlock_tests` in the existing `dash-wt/pr7841/src/test/test_dash`: **6 cases, 5,050 assertions passed**. Its relevant spork/ChainLock implementation and test sources match rc.3; its newer build is supplemental smoke evidence.
- Attempted four optional Core scenarios with an existing unmerged harness draft. All aborted with an uncaught `UniValue` before producing JSON; they supply no scenario verdict and are not used to certify the design. No harness or reviewed implementation was changed.
- Artifacts and logs are under `/work/scratch/sb4-design-r4-sol/`. Tests used a mode-700 review TMPDIR under `/dev/shm`, removed afterwards. No live chain instance, private input, GitHub message, commit or push was involved. Only this requested review file is left uncommitted in the integration worktree.

## Verdict

**REWORK. Sol gate satisfied: no.** Correct the wallet recovery premise/rule and exclude handoffs from the node enforcement composite, with the two concrete regression cases above. O-M1's floor algorithm and O-M2's per-mark hook otherwise satisfy this confirmation.
