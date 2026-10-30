# EV addendum to SB0 — narrow confirmation review, round 3 (Sol)

| | |
|---|---|
| Under review | `docs/node-program/designs/EV-addendum-SB0.md`, rev2 at **df34ea3e75eb6b9eb5da5d336e7fc5c8bcdd42c6**, and `docs/node-program/tools/cpm_ev_addendum.py` at that revision |
| Reviewer | Sol, sequential first pass under DEC-92 |
| Contract | `briefs/REVIEW-COMMON.md`; PLAN §7 |
| Scope | R2-M1 closure and the serialization changes in §8.1; DEC-109, DEC-111 and DEC-113 remain accepted |
| Core | Exact tag reads at **v23.1.8** and **v24.0.0-rc.3** |
| Date | 2026-10-09 |
| Result | **APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** One major dispatch/liveness gap remains; R2-M1's visibility defect is closed at the design level. |

## Disposition and confirmation

**R2-M1 — closed.** The attempt holds the exclusive gate through application of its whole log, on every outcome (`EV-addendum-SB0.md:261`, `:266`, `:274`). Every listed outside route takes the shared gate before reading state, and retains it through its nested work (`:267-272`, `:361`). The published M1 and unapplied S0 are therefore inaccessible together outside the attempt. This covers the runtime members facade; scans, selection and both materializer routes; the v24 predictor; direct qrinfo/rotationinfo snapshot reads; and both mnxcheck paths. An external runtime overwrite also cannot enter between attempt publication and effect replay.

**Nested acquisition — correctly forbidden.** Passing the existing `ExternalToken` to inner calls avoids the shared → queued writer → recursive shared deadlock. Validation-time CbTx, MNHF, asset-unlock, commitment and precompute calls must use the existing `AttemptHandle` all the way through scans/materialization, as `:258` and `:273` require. They must not enter an external wrapper to obtain another guard. A writer-preferring lock prevents new external readers from starving an already queued attempt. These rules solve gate re-entry; they do not solve the V-dispatch cycle below.

**Panic/latch — sound ordering as specified.** On partial log-application failure, the latch must be set while the exclusive guard is still held. Dropping an unreleased handle likewise sets it before releasing the guard (`:275-276`). A waiting route must check that latch **after acquiring** its shared guard and before any cache hit or row read. Thus a cache hit cannot bypass the fatal result, and a pre-wait check alone is insufficient. Successful release must disarm the handle's abandonment path; repeated latch trips must remain idempotent. These are implementable consequences of the stated contract, not claims that the future CH7b code already implements them. `EffectsApplied` remains attempt-specific and can be minted only after the whole log applies.

**Case 6 — correct visibility acceptance.** `:287-294` specifies the required success/failure/proposal × base/indexed-hit matrix, observable blocking, post-release `(M1, S1)`, and a gate-less `(M1, S0)` negative control. LQ1a's state-machine gate and CH7b's real-thread gate are appropriately separate; LQ1b reruns with real rotation. SB19 adds both mnxcheck paths. This case does not exercise a cold external computation requesting its write from V, which is the missing liveness acceptance in R3-M1.

**Core concurrency parity — no new objection to DEC-113.** Both tags write the snapshot row/cache before publishing computed members: v24 `utils.cpp:462` → `snapshot.cpp:326-328` → `utils.cpp:710-713,727-728`; v23 `utils.cpp:453` → `snapshot.cpp:289-291` → `utils.cpp:639-642,656-657`. Core's qrinfo builder already requires `cs_main` (v24 `snapshot.cpp:82`; v23 `:54`), and v24 `quorum rotationinfo` holds it through construction (`rpc/quorums.cpp:995-1000`). Other calls release it after taking their tip, including v24 `quorum dkginfo` before prediction (`:1053`, `:1146-1149`), and quorum scans (`quorumsman.cpp:151-152`). Deferring their membership/snapshot work until the attempt completes is an allowed Core schedule. A previously captured explicit base/tip can still be the input of a later call. The gate keeps external readers concurrent with each other. The major below concerns completing a permitted call, not requiring all Core interleavings to remain available.

**CPM — unchanged and correct.** The tool is byte-identical to rev1. All six runs, all 54 finish values, and the P/X limiting chains match §5. The normative edges remain EV0 `CS0, i:BS6a`, LQ1a `+i:SB7`, and LQ2 `+SB8`; P/P1 give REL11 **30.0**, P2/P3/X **31.5**, and X1 **32.0**. The input-hash negative control still rejects a changed `cpm.py`. No schedule ruling is reopened.

## Findings

### R3-M1 — Specify V dispatch before taking the gate for external operations that can write

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:262`, `:266-269`, `:277`. Relevant standing contracts: `designs/A1-architecture.md:542`, `:1709-1711`; `designs/CH2-coins-flush.md:1684`, `:1696`; `designs/LQ1-quorum-members.md:736`.

Rev2 says non-attempt callers use the facade directly, hold its shared gate for the whole call, and synchronously write through on a rotated miss. A1 gives evo mutations to the validation thread V; CH2 specifically assigns rotation-snapshot effect writes and their presumed-effect ledger to V. LQ1 names CH7b's evo writer and cites that same single-writer rule. Rev2 does not say which thread runs an external operation that reaches this writer, or where its command dispatch sits relative to acquiring the gate.

**Concrete failure scenario:** an RPC or QM1 caller acquires an `ExternalToken` and misses the rotated members cache. V starts the next validation attempt and blocks in `begin_attempt`, waiting for the token's shared guard. The external computation reaches `SnapshotRows::store` and synchronously requests its snapshot write from V, preserving A1/CH2's writer ownership. The caller cannot release its token until the write completes; V cannot service the write while blocked on that token. The wait cycle is **external caller → V write reply → exclusive gate → external caller**. A runtime scan whose materializer misses has the same problem. Read-only mnxcheck is not needed to reproduce it.

The no-nested-take rule does not help: this is a wait on V, not a second acquisition in the same stack. The statement that only P2P waits and nonlocal I/O are excluded does not explicitly exclude command/reply waits on validation. Its literal local-reads-only wording also conflicts with the synchronous local writes and log application the preceding bullets require. Writing from the external caller instead silently changes the standing writer/ledger contract; the document must name that choice if intended.

**Suggested fix:** keep DEC-113's one gate, overlay and ordered effects. Explicitly route an external operation that can write through V **before** it takes any gate token, and take/reuse its token inside the dispatched operation, wholly outside an attempt. Never hold a shared or exclusive gate while synchronously waiting for a `ChainCommand` or other callback into V. Read-only routes can continue using the shared gate directly over published views. If instead CH7b is intended to write these runtime rows inline from external threads, explicitly reconcile that exception with A1's ownership and CH2's presumed-effect ledger/barrier rather than leaving implementers to infer it.

Add a real-thread liveness case: pause a cold external members/scan computation before its synchronous snapshot write, start validation, then let it reach the writer. Both calls must finish with the row/cache ordering intact. Include the queue-after-token arrangement as a deadlock negative control. Validation-time nested scans/materializers must also complete using the existing handle without dispatching back to V.

A compiled Rust two-thread probe reproduces the wait cycle using a shared `RwLock` guard and a synchronous command/reply channel. A separate escape releases the guard for test cleanup; without it the reply cannot be produced. The companion probe dispatches the entire external operation before taking its guard and completes. This establishes the problematic integration ordering; it does not assert that an unimplemented CH7b already contains that ordering.

### R3-m1 — Writer preference does not imply the stated one-attempt wait bound

**Severity: minor. Location:** `docs/node-program/designs/EV-addendum-SB0.md:277`, `:417`.

**Concrete scenario:** an external reader waits during attempt A. After releasing A, V reacquires the gate for attempt B before the reader is scheduled. A writer-preferring lock alone does not require handing ownership to that waiting reader between attempts. The call can therefore wait for multiple attempts, contrary to “bounded by one attempt plus its log application” and “at most one attempt.” The contract model admits this schedule. Even an eventually fair lock does not establish that exact one-attempt bound.

**Suggested fix:** describe the wait as lasting until the reader acquires the gate after an attempt's log application, with its latency/fairness measured by PF. If the exact bound is required, specify and test a waiting-reader handoff before the next attempt. This is a latency claim correction, not an objection to the accepted serialization policy.

## Evidence and limits

Review used a detached checkout of the exact revision under `/work/scratch/ev-add-r3-sol/repo/`. Tests used `/dev/shm/ev-add-r3-sol-tmp`, mode 700. No Dash daemon was started, no oracle was mutated, and no tracked implementation or design file was changed. No commit, push or GitHub comment was made.

- `python3 docs/node-program/tools/cpm_ev_addendum.py`: reproduced the six runs and both chains.
- Review-only Python checks: **7 passed**, covering all 54 finish values, graph edges, changed-input rejection, the six case-6 combinations over eight entry-route categories, the gate-less control, latch-before-release for application failure and abandoned/panicked attempts, writer preference/token reuse, and the counterexample to the single-attempt latency bound.
- Review-only Rust command-dispatch probes: built with `rustc --edition=2021 --test`; **2 passed**, reproducing the V/gate wait cycle and demonstrating dispatch-before-acquisition completion. The reproducer uses explicit channel handshakes; a bounded receive detects the blocked reply, and a separate reviewer-only escape allows both threads to join.
- Core sources were read from the exact tags; lock ownership, direct snapshot readers, prediction and snapshot-write/cache-publication order were checked against code.

The Python model supplies the specified invariants by construction; it checks their consequences and negative controls, not production route wiring or a concrete RwLock's fairness. The proposed CH7b and LQ1a acceptance suites do not exist at this revision. Their implementation remains subject to its own review and real-thread tests. All reviewer-created checkouts, probes, binaries, imported bytecode and temporary files were removed after review. This review is uncommitted.

## Verdict

**APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** R2-M1 is closed. Resolve **R3-M1** before the sequential Opus-high pass under DEC-92; correct the minor latency claim as well. DEC-109, DEC-111 and DEC-113 stand unchanged.
