# EV addendum to SB0 — narrow confirmation review, round 6 (Sol)

| | |
|---|---|
| Under review | `docs/node-program/designs/EV-addendum-SB0.md`, **rev5 at 726ff9a12ae52d0f05e78f1b94f882bdc4588ca4**, `node/integration` |
| Compared with | rev4 at **77c76faf4fe434416a504f3792fa8bbbeca36cc7** and `reviews/EV-addendum-r5-sol.md` |
| Contract | `briefs/REVIEW-COMMON.md`; PLAN §7 handling |
| Scope | R5-M1/R5-m1 closure; `VContext` borrowing; nested consumers; removal of member-source thread bounds; overlay reentrancy |
| Binding decisions | **DEC-111, DEC-113, DEC-117**; **SB0 approved under DEC-119**. None is reopened |
| Core | **v23.1.8** (`79b6c5215f33bd305ddf095747b932f516b7a9a7`) and **v24.0.0-rc.3** (`da567d7dba945143eee4d2ce1621c21b32089989`) |
| Date | 2026-10-09 |
| Result | **APPROVE. Sol gate satisfied: yes.** |

## Disposition of round 5

**R5-M1 — closed.** Both legitimate `OnValidation` implementations now require the same private, thread-bound `VContext`: `begin_attempt` borrows it mutably and returns a handle with that borrow's lifetime, and `on_v` does the same for `OnV` (`EV-addendum-SB0.md:248`, `:255-275`). The context is created once by the V loop, its constructor is module-private, and it is never handed out. Caller-thread code cannot obtain the constructor argument. The context and both capabilities are explicitly `!Send`/`!Sync`, so neither construction on V followed by transfer nor transfer of a borrowed capability restores the r5 bypass.

The lifetime tie is effective even though the sketch does not store an actual `&mut VContext` field. Rust rejects a second mutable borrow while the returned gate-owning capability remains live. Probes rejected an attempt while an `OnV` exists, an `OnV` while an attempt exists, and a second attempt while the first exists. They also rejected the overlap when the first guard has no subsequent explicit use: its destructor still keeps the borrow live. Releasing/dropping the first capability permits the next operation through the same context. The scheme avoids the shared-to-exclusive gate upgrade identified in r5.

Compile-fail case 6 now covers the missing-context constructor path and overlap; case 4 covers owned and borrowed attempt transfers (`:318-324`). CH7b's acceptance and EV0 commit 2 agree with the operative API (`:390`, `:472`). Constructor privacy is a normative requirement for the real implementation, including the absence of any public `Default`, clone or alternative context factory; the probes do not substitute for production acceptance.

**R5-m1 — closed.** The allow-list now explicitly permits `snapshot_for_block`'s snapshot-cache insertion and recency bump, while forbidding the other named caches, lazy production keys, computation, persistent writes and V callbacks (`:300-304`). This matches both exact Core pins: `llmq/snapshot.cpp` v24 `:307-313`, v23 `:270-276`; `unordered_lru_cache.h` v24 `:65`, v23 `:63`. The getter remains under the shared gate and its own cache lock. mnxcheck continues to use its separate cache-free backing reader (`:444-447`). The production `RawRows` transitive audit is now explicit.

## Soundness and consumer checks

**Nested commands still compose.** The exclusive borrow is of the loop's context during capability creation; the command receives a shared `&OnV`. That shared reference can be passed to multiple command-scoped adapters and nested composite functions. None needs to reborrow `VContext`, mint a second capability or acquire another gate. The specified `qrinfo` → `mnlistdiff` → scan/materializer → members composition compiled and ran through one borrowed capability. This follows `:269-271`, `:292` and `:327-329`; the scoped higher-ranked callback also rejected returning either the capability reference or a borrowing runtime adapter.

**Nested attempts need no `OnV`.** `AttemptHandle` implements both `OnValidation` and `QuorumMemberSource` (`:267-278`). Generic materializers/builders accepting `&C where C: OnValidation` therefore accept `&AttemptHandle`, and members/precompute calls share its overlay. The reconstruction ran a nested builder/scan/materializer/members chain, a later members read, and an overwrite within one attempt; the last value was visible and the effect order was preserved. No nested call accessed the context, dispatched or acquired the gate. This is consistent with `:281-285`, `:294` and the existing nested-validation acceptance at `:376`.

**Removing `Send + Sync` is compatible with the planned consumers.** It removes a guarantee from the trait; it does not make every implementor nontransferable. A concrete SPV source with suitable fields can still be `Send + Sync`. If SPV erases such a source and retains it across a task boundary, the object must explicitly be `dyn QuorumMemberSource<…> + Send + Sync`, or a generic consumer must require those bounds. A plain erased object no longer provides them. The probes accepted a concrete source, an explicitly bounded object crossing a thread, and a `Send` future retaining that object; they rejected the unbounded-object future as expected.

The approved SB0 engine stores its own SML/quorum/snapshot state and constructs per-call LQ1 contexts (`SB0-shared-base.md:930-954`), rather than requiring a long-lived node `DmlMemberSource`. The addendum's SPV path uses shared algorithms/`members_uncached` and an injected verifier source (`EV-addendum-SB0.md:424`, `:431`). It need not carry an `OnV` or an attempt handle across tasks. mnxcheck's shadow engine stays on its own task; the node-side comparison runs as the specified whole V command (`:444`). Node QM1/DK1/LQ5/RPC callers retain their dispatch-facing state and consume owned results; their capability-bound runtime sources stay inside the command. Reintroducing `Send + Sync` on those adapters would contradict that scope, so superseding EV0 Q10 is appropriate. No planned consumer requires that contradiction.

**`RefCell` has no required reentrancy panic path.** Serial execution alone does not prevent reentrant borrowing. The safe implementation keeps each overlay guard inside the individual row operation: `get` clones the owned `QuorumSnapshot` and releases its `Ref` before backing reads or further computation; `store` finishes its insertion and releases its `RefMut` before calling the effect sink. Keeping an `AttemptRows` view alive is harmless: it holds a reference to the cell, not a cell guard. Independent nested views can use the same cell in sequence.

The signatures return owned rows (`:164-167`), and no specified nested members call must run under an overlay guard. Backing row reads remain raw reads, and EV0's actual `EffectSink` contract only records effects (`dash-evo/src/effect.rs:146-159` at `3a5cb631`). A reconstruction with short borrow scopes passed both nested-view use and an instrumented emission check verifying that the cell was already available after insertion. A deliberately broad mutable borrow across a nested borrow panicked, as expected. That is an implementation pattern to exclude, not a necessary consequence of this design. CH7b's implementation review should verify the short scopes; an outer `RefMut` retained for an entire members/materializer call would be incorrect. No change back to `Mutex` or shared member-source thread bounds is needed.

## Findings

**None.** No blocker, major, minor or nit remains in this confirmation scope. The borrow-scope and explicit SPV object-bound notes above explain implementation constraints, not new design gates.

§8.1 accurately records the rev5 delta. The CPM tool is byte-identical between rev4 and rev5; its plan-of-record run still gives **REL11 30.0**. Settled algorithms, error policies, observer semantics and schedule decisions were not re-reviewed.

## Evidence, limits and cleanup

- Standalone Rust API reconstruction built with `rustc 1.98.1`, edition 2021, with warnings denied: **6 tests passed**. These cover shared-capability composites, attempt nesting/read-after-write/overwrite, SPV transfer, short overlay scopes, emission without a retained overlay guard, and the broad-borrow panic control.
- **25 compile-fail expected outcomes verified**: caller/token/`OnV` attempt construction; private context and capability construction; sealing/allow-list restrictions; owned and borrowed thread bounds; capability/adapter escape; all three capability-overlap directions and guard-drop lifetime retention; and the unbounded SPV object future.
- **7 pinned-document checks passed**, including the revised signatures, acceptance cases, snapshot-cache exception, nested paths, scoped boundary, unchanged CPM tool, and unchanged reviewed design blob at the integration tip. The CPM script was run with `python3 -B`; run P retained REL11 **30.0**.
- Exact pinned Core getter/cache sources, EV0's real source/sink contracts and consumer stubs, and the approved SB0 consumer API were inspected. These probes establish Rust/API feasibility; they do not execute future CH7b or SB11 code, prove production fairness, or replace their acceptance suites. No full workspace or Dash daemon suite was needed for this documentation-only confirmation.

Reviewer scratch `/work/scratch/ev-add-r6-sol.87niiubo/` and test temp `/dev/shm/ev-add-r6-sol.87niiubo-tmp/` were removed, including generated sources, binaries and compiler diagnostics. No other agent's files, private material, daemon/datadir or GitHub state was touched. PLAN §7 handling was followed. This review is **uncommitted**; no branch code was modified.

## Verdict

**APPROVE.** R5-M1 and R5-m1 are closed. The single-context capability scheme permits the required nested calls without a second context borrow, and neither the trait-bound change nor the overlay type forces a consumer or reentrancy defect. DEC-111, DEC-113, DEC-117 and DEC-119 remain binding.

**Sol gate satisfied: yes.**
