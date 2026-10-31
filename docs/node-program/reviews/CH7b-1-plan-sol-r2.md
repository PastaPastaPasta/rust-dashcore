ACCEPT — 0 blocker, 0 major, 0 minor, 0 nit.

Narrow r2 correctness re-review of CH7b-1. Read the r1 report first, then REVIEW-COMMON.md, CP's 2026-10-31 02:03Z acceptance and OD-CP-4/5 rulings, PLAN.md §12, and the relevant C1–C4 sources at `f97aee23b9490a8b2cadb748ff9f25f7633dc225` in `/work/home/agent/workspace/rdc-wt/ch7b-1`. Core references below are from `git show v24.0.0-rc.4:src/...`.

“Closed” below means the amendment closes the plan finding. It does not certify execution of the remaining C5–C11 tests or mutation checks. This was a source review: no builds, tests or mutants were run; no tracked file was edited. The reviewed worktree was clean and remained at the stated head.

## F1–F4 closure

### F1 — CLOSED: C10's executable interleaving

`PLAN.md:438–448` adopts the requested order: start the real route-B rotation and park it immediately before `InlineRows::store` calls `RuntimeRows::write`, after its previous-cycle reads; then park the cold route-A getter after reading S0 with the section held. Resuming B must produce a section waiter while the raw-store write counter remains unchanged. After A resumes, both disk and cache must contain S1.

The unlocked-getter control explicitly waits for B's write **and cache insertion** before allowing A to insert S0. This establishes the stale insertion deterministically. It retains the real rotation and production write section and records the scheduling clarification for ADD case 7. The read-before-write obstruction identified in r1 is removed.

### F2 — CLOSED: checked in-crate visibility and overlap

`PLAN.md:450–464` replaces the occurrence scan with actual in-crate compile probes. Sibling-module probes exercise non-public fields, constructors, `on_v`, sealing and binding construction; actor-module probes exercise the distinct OnV/attempt borrow overlap in both orders. External examples remain isolated, and the second-open example takes `&mut VContext` as a parameter rather than depending on a constructor.

The runner checks the expected diagnostic and marked span, rejects unrelated errors, and compiles passing twins. The planned visibility-widening mutations check that the probes enforce the boundary. Together these cover the two omissions from r1: crate-internal widening that external doctests cannot detect, and OnV/attempt overlap that an occurrence scan cannot establish.

### F3 — CLOSED: deterministic M8 observation and separate M18 variants

`PLAN.md:466–475` specifies trip → sync → unlock in production and unlock → sync → trip for M8. At the sync point, the production test observes exclusive ownership. In the mutant, the observer waits for the reader to complete before permitting the trip, exposing the forbidden successful read rather than hoping the reader runs during a short race.

The C4 implementation matches this construction: `dashd-chainstate/src/llmq/facade.rs:192–206` trips the latch, invokes the test hook, and only then releases `_gate` through field destruction. In `dashd-chainstate/src/chain/tests/llmq.rs:325–369`, the reader is positively observed waiting before drop; the hook parks drop; the observer records `is_locked_exclusive()` and, if false, waits for the reader's completion before releasing the hook. The test asserts both the recorded ownership and the reader's error. Therefore the stated M8 mutation cannot pass merely because its reader was scheduled after the trip. This is source confirmation of the proof mechanism, not a claim that a mutant run passed.

M18a now means insert-before-write and is observed by the failed-write/old-row assertion. M18b means raw-write-before-section and is observed by F1's write counter while A still owns the section. They have separate result rows. M3 remains explicitly equivalent and uncounted, with its post-entry latch check retained.

### F4 — CLOSED: semantic gate before cache activity

`PLAN.md:477–485` adds parentless and disabled-at-parent cases with warm base and indexed entries, for both reset values. Empty output, unchanged contents **and recency**, and zero source access are explicit assertions. A separate enabled-base reset checks isolation of the other type. S1/S2/S3 separately skip or misplace the semantic gate and distinguish it from the serialization gate.

This covers the r1 counterexamples and matches Core `llmq/utils.cpp:644–659`: parentless/type-disabled rejection precedes the base-cache reset or lookup. Core's indexed reset is later at `:691–698`.

## Q2 and Q7

**Q2 — applied.** `PLAN.md:489–499` replaces the freely assembled adapter with non-public `AttemptBinding` state constructed only in the hook's binding path. It borrows the live attempt handle, takes ancestry and deployments from that attempt, takes BLS from H1's selected `attempt.bls`, refuses an unset selection, and takes the view from that call's target layer. The OnV binding borrows its command capability and binds the committed tip. The scheme-boundary/before-H1 and scratch-target regressions explicitly exercise the construction contract. The actual binding implementation remains C7's work.

**Q7 = M — applied.** `PLAN.md:504–509` requires a queued OnV to execute between two ABC holds while ABC is still active, with no open attempt, seeing the first hold's committed layer and published scheme before the next hold connects. Class-C mutation M28 delays it until ABC ends and fails that requirement. This uses the existing release point at `dashd-chainstate/src/chain/abc.rs:963`. It matches the relevant Core distinction: the operation retains `m_chainstate_mutex` (`validation.cpp:3744`), while the `cs_main` hold ends at `:3833` between outer iterations. It does not authorize dispatch between individual connects within one hold.

## Reported C1–C4 drift

### 1 — ACCEPTED: EffectsApplied binding moved into C4

`dashd-chainstate/src/evo/effects.rs:57–69,124–135` keeps the proof's attempt field non-public and constructs it only after the entire supplied log succeeds. `evo/node.rs:924` supplies `attempt.id`. `llmq/facade.rs:86–109` rejects a different id and a handle from another facade before releasing normally. Moving this prerequisite earlier makes C4's release checks concrete without changing effect order. C7 still owns production handle acquisition and release integration.

### 2 — ACCEPTED: caches, params and overlay moved to C5; infallible C4 constructor

At this head, `llmq/facade.rs:27–47` constructs only the gate, existing runtime rows and latch. There is no parameter-dependent cache construction to fail, so the infallible `MembersFacade::new(Arc<RuntimeRows>, latch)` is sound for C4. No production members algorithm or attempt overlay consumer has been introduced prematurely. Moving those fields to C5 is a commit-boundary adjustment; C5 must still supply the one handle-owned overlay and validated cache construction promised by the plan, rather than a per-call overlay. The adjustment does not weaken Q2's binding contract.

### 3 — ACCEPTED: indexed key `(BlockHash, u16)`

This matches the existing `dash-evo/src/llmq/reference.rs:220–243` cache key and `MembersRoute::Rotated.index` in `dash-evo/src/llmq/members.rs:139–168`. `LlmqCtx::new` limits rotated active counts to 65,536 (`llmq/ctx.rs:46–68`), so every valid index is at most 65,535. Core uses an `int` key (`llmq/utils.cpp:639`), but the narrower Rust representation preserves every index in the supported domain, including all shipped Core parameter sets. The runtime members caches themselves remain C5 work at this head.

### 4 — ACCEPTED: latched external error and mismatched release error

`llmq/facade.rs:56–64` acquires the shared gate **before** checking the latch, returns `Fatal::reported` with the latched message, and produces no token on failure. That preserves the critical post-wait refusal. A mismatched release trips the latch and returns `Fatal::abort_node`; the unreleased handle's drop keeps its gate until after the trip. The reporting choices agree with `chain/seams.rs:94–102,145–150`: an already reported latched failure needs no duplicate report, while the explicit abort is reported by V even with the latch set. No row-read or release-order regression is introduced.

### 5 — ACCEPTED: undecodable snapshot row reads as absent, without caching

Core rc.4 confirms the reported behavior:

- `dbwrapper.h:314–327`: after a successful raw read, `ssValue >> value` is inside a `try`; a `std::exception` returns `false`, just as an absent key does at `:318–319`.
- `evo/evodb.h:136–140` and `dbwrapper.h:647–668`: the transaction read forwards to its parent when the key has no pending write; failed reads are not inserted into a transaction read cache. Snapshot writes use the raw DB (`llmq/snapshot.cpp:327`).
- `llmq/snapshot.cpp:307–317`: the snapshot section covers lookup and read; only a successful `Read` inserts a positive cache entry. A deserialization failure returns `nullopt` and creates no negative entry. An existing positive cache hit is returned before touching disk.

The new `RocksEvoStore::quorum_snapshot` (`dashd-chainstate/src/evo/store.rs:108–115`) propagates backend read errors and maps only decoding failure to `None`. `RuntimeRows::snapshot_for_block` (`llmq/rows.rs:116–139`) preserves the warm-hit-first behavior and inserts only `Some`, under the section throughout. Thus this failure is absent, not fatal and not negatively cached; subsequent cold reads can retry it. The directed truncated-row source test is at `llmq/rows_tests.rs:188–201`. No change is requested for this drift.

No additional clear correctness defect was found in C1–C4 within this re-review's scope. Remaining implementation tests and recorded mutant kills are still required by the amended plan.

Verdict: ACCEPT — 0 blocker, 0 major, 0 minor, 0 nit.
