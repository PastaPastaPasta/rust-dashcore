# EV addendum to SB0 — narrow confirmation review, round 5 (Sol)

| | |
|---|---|
| Under review | `docs/node-program/designs/EV-addendum-SB0.md`, **rev4 at 77c76faf4fe434416a504f3792fa8bbbeca36cc7**, `node/integration` |
| Compared with | rev3 at **42a20ebb98d34c9f2d5d426e2a10e57bf9085698** and `reviews/EV-addendum-r4-sol.md` |
| Contract | `briefs/REVIEW-COMMON.md`; PLAN §7 handling |
| Scope | R4-M1/R4-m1 closure; the raw-getter allow-list; Rust capability enforcement; composite dispatch and attempt progress |
| Binding decisions | **DEC-111, DEC-113 and DEC-117**; none is reopened |
| Core | Exact source at **v23.1.8** (`79b6c5215f33bd305ddf095747b932f516b7a9a7`) and **v24.0.0-rc.3** (`da567d7dba945143eee4d2ce1621c21b32089989`) |
| Date | 2026-10-09 |
| Result | **APPROVE-WITH-CHANGES. Sol gate satisfied: no.** One major capability-construction gap and one minor allow-list specification inconsistency remain. |

## Disposition of round 4

**R4-M1 — closed as a route/dispatch finding.** Rev4 dispatches the entire qrinfo, rotationinfo and direct `mnlistdiff` operation before any gate or cache acquisition, alongside every other route outside the closed allow-list (`EV-addendum-SB0.md:271`, `:277`, `:294`). The `use_rotation` split is removed from the operative contract. Nested `BuildSimplifiedMNListDiff` → `BuildQuorumChainlockInfo` → `GetQuorum` → members calls use the command's existing `OnV` inline. Attempt calls retain their existing `AttemptHandle`. Neither path reacquires the gate or dispatches to V.

Case 6-D now includes the post-V20, cold rotated-quorum composite fixture for all three builders, both queue positions and all three attempt outcomes (`:348-352`). Its old-route negative control is explicitly test-only. Position (c) uses an allow-listed getter (`:345`). This closes the r4 composite wait cycle. The new capability enforcement needs the separate correction in **R5-M1**; the route fix itself is sound.

**R4-m1 — closed.** Case 6 distinguishes a queued dispatched command from a caller-thread row observer parked on the gate (`:328-330`). All six success/failure/proposal × base/indexed-hit combinations remain, including delayed base-cache insertion on an indexed hit. The negative control explicitly bypasses both dispatch and the gate (`:332-337`). The pure `Blocked` model remains, and the cold-miss cases supplement the warm-hit matrix. CH7b's §5 acceptance matches these wait locations (`:449`).

**Follow-on edits — consistent.** One mnxcheck comparison is one V command under one `OnV` (`:421`); CH7b and SB19's rows agree (`:449`, `:452`). Its cache/key isolation rules remain unchanged. §8.1 accurately records the operative route and wait changes. The CPM tool is byte-identical between the reviewed revisions; REL11 **30.0** is retained. Settled algorithms, split, error policies, observer semantics and schedule decisions were not re-reviewed.

## Findings

### R5-M1 — The public attempt constructor provides an alternative route to `OnValidation`

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:245`, with the new capability contract at `:255-260`, `:298-308` and the sole-attempt-opener claim at `:311`.

The sketch still exposes `MembersFacade::begin_attempt(&self, id) -> AttemptHandle<'_>` publicly, without a V-only constructor restriction or a V-only argument. Rev4 then makes that handle a sealed `OnValidation` implementation. Restricting construction of `OnV` does not restrict construction of the other accepted capability.

A caller with the process-wide facade can therefore write the following using only safe, public APIs; `materialize` represents any newly gated runtime entry point:

```rust
fn caller_thread_route(facade: &MembersFacade, id: AttemptId) {
    let handle = facade.begin_attempt(id);
    materialize(&handle); // accepted: AttemptHandle implements OnValidation
}
```

**Concrete failure scenario:** a future caller-thread route obtains an attempt handle while V is between commands and invokes a members/materializer/composite operation directly on that caller. It has satisfied the sealed trait without dispatching the operation to V. The handle can also hold the exclusive gate against V; abandoning it invokes the specified fail-stop path. Making the handle `!Send` does not help: it is constructed and used on the wrong thread, rather than transferred there. An attempt ID and an exclusive lock prove attempt identity and exclusion, not V affinity.

A scratch reconstruction of the indicated API compiled this route and executed the gated operation on a spawned caller thread. The five listed compile-fail categories still reject their intended misuse while leaving this path available. This is a Rust API/design counterexample, not a claim that production CH7b already implements the sketch.

**Suggested fix:** restrict **both** capability minting paths to V. Make attempt construction private to the V/CH7a execution boundary, or require an unforgeable, thread-bound V execution context created by the command loop. A merely `pub(crate)` constructor is insufficient if caller routes share that crate. Do not use the existing shared-gate `OnV` guard as a naive prerequisite for taking the exclusive gate: that would introduce a read-to-write self-deadlock. Use a V-affinity proof independent of that guard, or a consuming transition that drops the shared guard first. Extend CH7b's compile-fail acceptance to reject obtaining an `AttemptHandle` from an external/caller-thread context. Keep the existing five categories. This implements DEC-117 rather than changing it.

### R5-m1 — `GetSnapshotForBlock` is cacheful, contrary to the blanket raw-getter restriction

**Severity: minor. Location:** `docs/node-program/designs/EV-addendum-SB0.md:286-289`; compare the already correct isolation statement at `:424`.

The allow-list calls `snapshot_for_block` a **cache-through** read, but its next restriction says that a getter reaching **a cache** can never be added. The claimed pure/no-cache characterization is false at both pins:

- v24 `llmq/snapshot.cpp:307-313` locks the snapshot cache, uses `quorumSnapshotCache.get`, and inserts the row on a disk hit.
- v23 `llmq/snapshot.cpp:270-276` does the same.
- A warm hit also mutates recency: `unordered_lru_cache.h` v24 `:65`, v23 `:63` increments `accessCounter`. It can change subsequent eviction decisions. Two small executables built against the exact pinned cache headers demonstrated that effect.

**Concrete failure scenario:** a caller-thread `RawRows::snapshot_for_block` reads a persisted row absent from the snapshot cache. The getter inserts it and may trigger cache eviction; even a warm read changes access order. An implementation following the named Core getter therefore fails a no-cache/no-mutation interpretation of `RawRows`, while an implementation following the blanket prohibition cannot use that getter as named. The document already recognizes this distinction for mnxcheck at `:424`.

This getter does **not** compute members, materialize a quorum, persist a row or wait on V. Its snapshot-cache bookkeeping under the shared gate and cache mutex does not recreate R4-M1's deadlock. That is why this is a specification correction, not another major route finding.

**Suggested fix:** explicitly permit this one snapshot-cache read-through/recency operation, while prohibiting access to members, quorum-object, scan, memo and lazy production-key caches and any computation/persistent write/V callback. Narrow `:289` accordingly and stop describing the entire list as pure or cache-free. DEC-117's raw-getter/no-materialization ruling permits that clarification. If strict cache-free reads are intended instead, specify a direct backing-row reader and the resulting cache behavior; do not equate it with Core's cached getter.

## Checks requested for this round

**Mined getters — no hidden quorum materialization found.** Both pins' `GetMinedCommitment` are record reads: v24 `blockprocessor.cpp:675-684`, v23 `:534-543`. The index walks and per-index helpers are v24 `:687-815`, v23 `:546-674`. They seek/iterate the commitment index, read heights and map through `GetAncestor`; they never call `GetQuorum`, members, snapshot stores, contribution builders or member/object/scan caches. Core's commitment contains eager BLS value types; decoding the returned record is not lazy construction of a shared quorum or DML operator key. `HasMinedCommitment`, which has its own application cache, is not on this allow-list.

EV0's actual provided `mined_until`, `nth_mined_by_index` and `newest_by_index` similarly use only `mined_index`, chain height and `base_through` (`node/ev0-evo-interfaces` at `3a5cb631`, `dash-evo/src/store/mod.rs:180-240`). Its `MemEvoView` primitives are map/range and ancestry reads (`store/mem.rs:243-293`). The six existing store tests passed, including forks, rotated index reads and qrinfo's stale-request mapping. The future production `RawRows` implementation still needs the same transitive audit; a reader trait alone does not establish implementation purity.

**Rust capability behavior — distinguish thread affinity from suspension.** `PhantomData<*const ()>` makes `OnV` both `!Send` and `!Sync`; the probes reject moving it or `&OnV` to another thread. A private constructor plus an inaccessible sealed supertrait rejects direct external construction and external trait implementations. Ordinary safe coercions do not convert `ExternalToken` into an `OnValidation`. R5-M1 is a route through a legitimate implementation, not an ability to implement the sealed trait.

`!Send` does **not** prohibit retaining a capability across `.await` or putting an owned capability into a non-`Send` local future/task. A probe accepting `OnV<'static>` into a `spawn_local`-shaped future compiled; requiring that same future to be `Send` failed. This is not a new violation of the combined design: A1 §3.1 (`A1-architecture.md:1711-1712`) requires synchronous evo/chainstate/store APIs and prohibits locks across awaits. The V command must be a synchronous, scoped execution boundary; a private constructor alone is not that boundary. A higher-ranked borrowed-capability callback can prevent returning the capability/reference, and a probe of that form rejected a reference escaping the command. Do not infer a general “no await” property from the marker or add a local executor inside the guarded V operation.

There is also an implementation constraint to preserve when wiring the new capability: a borrowed `&OnV` cannot be stored in an adapter required to be `Send + Sync` by the shared `QuorumMemberSource` trait (`EV-addendum-SB0.md:137`). The probe rejected that adapter. Use explicitly capability-parameterized runtime calls/command-scoped views with compatible bounds; do not validate a capability once in a factory and return a freely transferable, ungated members/writer object. The new entry-point contract must remain effective throughout nested calls. These probes establish language properties, not completed production adapter wiring.

**Self-deadlock — no new cycle in compliant composite routes.** Before dispatch the caller holds no gate/cache guard. During a command, nested builders, materializers and members use the same `OnV`; there is no inner dispatch or recursive shared acquisition. During an attempt, they use the same handle. A caller-thread raw getter needs no V callback and can release its shared guard while V waits for the exclusive gate. The controlled composite and raw-reader schedules finish with these rules. The constructor boundary in R5-M1 must be fixed to make the sole-V-attempt-opener assumption enforceable.

**Attempt starvation and PF — writer preference does not preempt a running command.** A long qrinfo operation occupies the sole V thread, so an attempt behind it cannot begin until that command finishes. There is no general wall-clock bound: cold caches and local I/O can make the service time large. Both Core pins already run `BuildQuorumRotationInfo` under `cs_main` (v24 `snapshot.cpp:82`, v23 `:54`), so delaying validation behind a finite complete build is an allowed schedule. This is a latency/throughput cost for PF, not a new incorrect state or result.

With a fairly serviced FIFO command queue and terminating local operations, one finite builder cannot block an admitted attempt **indefinitely**: it and the finite commands ahead complete, and later arrivals do not overtake that attempt. The queue probe confirms this finite ordering; it does not prove a production scheduler's fairness or bound disk latency. Writer preference only addresses new raw readers once V actually requests the exclusive gate. It says nothing about time already spent in V's queue. A nonterminating command, an unfair selection policy, or a callback waiting on work V must service would be a real progress defect; PF measurements cannot excuse indefinite starvation. No such dependency is required by rev4's compliant nested-call contract. PF should measure V queue wait and service time under cold composite requests alongside validation/signing load; no one-attempt latency promise is restored.

## Evidence, limits and cleanup

- `cargo test -p dash-evo --test store -- --nocapture` on the approved EV0 worktree: **6 passed**. The build used that worktree's own `target/` and `CARGO_BUILD_JOBS=12`.
- Standalone Rust route probes built with `rustc --edition=2021 --test -D warnings`: **4 tests passed**, covering the **six** warm-hit combinations and their caller-thread row observers/double-bypass control, **18** composite route/position/outcome combinations, raw-reader release against an exclusive waiter, and FIFO attempt progress behind a paused long command. Handshakes establish the controlled positions; bounded channel receives detect blocked replies. Every thread joins.
- Rust capability probes: **11 expected outcomes verified**. They cover the five stated rejection categories (owned and borrowed thread transfer separately), the public attempt-constructor counterexample, local-await versus `Send` behavior, the borrowed adapter's trait bounds, and scoped-reference escape.
- Pinned-source/document checks with `python3 -B`: **5 tests passed**, including the two exact-header C++ cache probes built with `g++ -std=c++17 -Wall -Wextra -Werror`, the getter audits, the operative route/matrix changes and the unchanged CPM tool.

The standalone probes reconstruct the specified ordering/API; they do not execute future CH7b or real rotation, establish production admission/fairness, or replace its acceptance suites. The review remained pinned to 77c76faf while unrelated integration work advanced; the design blob stayed identical. No Dash daemon/oracle, chain datadir, private material or GitHub operation was used. PLAN §7 handling was followed. Reviewer scratch `/work/scratch/ev-add-r5-sol.b8i1nh81/` and test temp `/dev/shm/ev-add-r5-sol.b8i1nh81-tmp/` were removed, including all generated sources, executables and compiler diagnostics. Other agents' files were left untouched. This review is uncommitted.

## Verdict

**APPROVE-WITH-CHANGES.** R4-M1 and R4-m1 are closed. **R5-M1** must close the public attempt-construction path before DEC-117's compile-time enforcement can be accepted; **R5-m1** must reconcile the cached snapshot getter with the allow-list wording. DEC-111, DEC-113 and DEC-117 remain binding.

**Sol gate satisfied: no.**
