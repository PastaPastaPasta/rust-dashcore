CHANGES-REQUIRED — 1 major, 3 minor, 0 nit.

Reviewed PLAN.md against the approved CH7 design and EV addendum, the integration worktree at `9fdaf8c5bd5589ee4244b73458774ed0a900c973`, and Dash Core `v24.0.0-rc.4`. This is a plan review: no implementation was built or tested, and no tracked file was edited. The worktree's CH7-notes contains D60–D71; I found no D72 at this tip.

## Findings

### F1 — major: C10 cannot reliably reach the write whose blocking it asserts

**Location:** `/work/scratch/cp/ch7b-1/PLAN.md:226–233` (C10); also M17/M18 at lines 292–293.

**Failure scenario:** Route A pauses after its disk read while holding the single snapshot-cache mutex. Route B then starts a real rotated members miss. Before storing K's new row, the rotation reads previous-cycle snapshots: `dash-evo/src/llmq/rotation.rs:353–371` calls `rows.get` before `compute_by_quarter_rotation` calls `rows.store` at line 431. For a normal cycle with a nonnegative predecessor, route B therefore blocks on its first backing snapshot **read**, even if that predecessor's row is cached. It never reaches the write-waiter flag that C10 waits for. The correct implementation times out, or the test is weakened to observe a read wait and ceases to prove the intended write section. The current startup order does not establish the required interleaving. This problem also exists in the addendum's literal case-7 narrative; copying that narrative is not enough to make the runtime test executable.

**Requested change:** Start the route-B computation first and pause it immediately before `InlineRows::store`, after all previous-cycle reads. Then start route A and pause its cold K getter after reading S0. Resume B, positively observe its store waiting before the raw write, resume A, and assert S1 on disk and in cache. In the unlocked-getter control, explicitly wait for B's write and insertion to complete before resuming A, producing the required stale S0 insertion. Keep real rotation and document this scheduling clarification to ADD §2.6 case 7. Do not satisfy the case with a writer hook that bypasses the production row section.

### F2 — minor: the proposed source scan does not prove the in-crate capability boundaries

**Location:** `PLAN.md:235–243,363` (C11 and X9); Q6.

**Failure scenario:** Widening `OnV`'s fields or `VContext::on_v` to `pub(crate)` leaves the existing construction/call sites in actor.rs. The proposed occurrence scan still passes, and external rustdoc examples still fail because crate-internal members remain unavailable to the doctest crate. A sibling module could then mint the capability, contrary to ADD's requirement that only the loop module may do so. Separately, a scan of `.on_v(` occurrences says nothing about whether its return lifetime actually keeps the mutable VContext borrow alive. C11's second-`begin_attempt` example is useful but does not exercise the distinct `OnV`/attempt overlap contract.

**Requested change:** Rustdoc compile-fail tests with passing twins are acceptable tooling; trybuild is not intrinsically required. However, retain actual rejection coverage for the internal boundaries, using an in-crate compile-check fixture or an equivalent checked mechanism. If CP accepts structural coverage for the privacy half, it must inspect the actual visibility of fields, constructors, and `on_v`, rather than just existing uses, and must demonstrably fail when those are widened. Compile the second-open test using a function parameter `&mut VContext`—no constructor is needed—and cover overlapping OnV/attempt borrows in the loop's own visibility scope. Give each forbidden operation its own failing example and ensure the passing twin validates its scaffolding. The current simple scan plus external doctests is not the whole specified suite.

### F3 — minor: several promised mutant kills require stronger observations than the plan specifies

**Location:** `PLAN.md:125–130,226–233,278–294` (especially M8 and M18).

**Failure scenario:** M8 releases the gate before tripping the latch, but the awakened reader may not be scheduled until after the trip. The stated waiter test then passes the mutant; twenty repetitions are not a proof of ordering. M18 also combines different mutations. A raw write moved before acquisition of the snapshot mutex can still finish with S1 on disk and S1 cached: the writer waits to insert until the paused getter inserts S0 and unlocks, then overwrites that cache entry with S1. Merely observing a later lock waiter and the final state does not kill that variant or prove Core's raw-write-plus-insert section.

**Requested change:** For M8, add a test-only synchronization point between the mutant's unlock and trip, let the reader acquire and attempt its read there, and assert the violation. Split M18 into insert-before-write and write-outside-section variants. With the corrected C10 schedule, assert that the counting/raw store has not been entered while the getter owns the section, in addition to observing the writer wait and checking the final state. List independently exercised variants separately in the results. M3 should remain explicitly uncounted: the current table has 28 labels but only 27 non-equivalent labeled mutations, with several labels containing multiple variants.

### F4 — minor: make the semantic members gate an explicit cache-order regression

**Location:** `PLAN.md:136–162,274–301` (C5 and mutant table).

**Failure scenario:** The listed directed C5 tests exercise cache hits, rotation, reset and work-block selection, but do not explicitly require the parentless/type-disabled gate to precede a warm base hit or a reset. Moving `members_gate` below the base-cache return can return a preloaded roster when Core returns empty. Moving it after reset can clear entries that Core leaves untouched. A differential over a fixed enabled rotated fixture need not generate either counterexample; the plan does not promise that it does.

**Requested change:** Add directed parentless and disabled-at-parent cases with populated caches, for both reset values. Assert empty output, unchanged contents and recency, and no source/computation access. Add mutations that skip this semantic gate or move it after the base-cache operation, and a two-type check that reset leaves the other type intact. Distinguish this semantic gate from the RwLock serialization gate in the mutant table.

## Q1–Q8

- **Q1 — accept.** Rename the existing test probe and its payload to `TestUnderV`, retain its class-C behavior and test-only access to `&mut V`, and record the §4.6 erratum. Runtime OnV supplies a deliberately narrower capability, so converting those probe uses wholesale would change their purpose.

- **Q2 — accept with an explicit construction contract.** H1 runs after begin_attempt (`evo/node.rs:888–940`), and the target-layer view is built inside the relevant hook (`:966–967`, `:1125–1127`). Deferring binding until the hook is sound. Borrowing `&AttemptHandle` retains the exclusive gate and the one shared overlay and makes the adapter unable to outlive that attempt or cross threads. It does **not**, by itself, prove that the separately supplied `llmq` and `source` describe that attempt. Keep fields non-public and production construction in the hook's binding path; take BLS from `attempt.bls` after H1, ancestry/deployments from `attempt.cx`, and the view from that call's `EvoTarget`. Reject an unset H1 choice rather than falling back to the published tip scheme. Construct/read the source only while the handle is held. The OnV variant must likewise borrow its command capability. Add a binding regression across a scheme boundary and a scratch target distinct from the active layer. These constraints preserve ADD §2.6; a public freely assembled triple is not a replacement for §4.2.3's claim that the context cannot come from another attempt.

- **Q3 — accept OD-CP-4 and apply it consistently.** Core `evo/cbtx.cpp:192–195` maps every failed `ReadBlockFromDisk` to nullopt. `EvoBlocks::read` already collapses missing position/data and read/decode failure to None (`evo/node.rs:176–185`). C3 should map every such None to `Some(CoinbaseFacts::BlockDataUnavailable)`, never the outer `Ok(None)`/Missing and never a new fatal read distinction. Remove C3's unresolved wording and replace §4.2.5's conflicting sentence. Test a failing block reader as well as pruning.

- **Q4 — accept.** A smaller closed allow-list is safe. Defer mined getters together with their coherent caller-thread view. CH7b-2 must resolve X10 before exposing them; release-before-commit does not by itself pin the layer/index/DB state for a caller-thread mined read.

- **Q5 — accept.** `PerTypeLru::sized` is the appropriate interface now. Later, use one mutex for objects and scans, keepOldKeys for objects, and Core's max_cycles(keepOldConnections) × mining-window-width formula for scans. Implement LQ3b's `ScanCacheStore` around those caches; do not introduce a second scan algorithm. If landing order changes, explicitly move the dependency/acceptance owner rather than adding a temporary scan implementation.

- **Q6 — accept rustdoc as tooling, but change the proposed coverage as F2 requests.** Stable rustdoc's failure to enforce Exxxx tags is tolerable with isolated cases and passing twins. The unproved in-crate visibility/overlap halves are the substantive issue, not the absence of trybuild.

- **Q7 — M.** Core ABC retains `m_chainstate_mutex` for the operation (`validation.cpp:3744`) but releases `cs_main` between outer iterations (`:3768–3833`). External quorum work does not require ABC's mutex: for example, `quorum rotationinfo` takes `cs_main` at `rpc/quorums.cpp:995`, and the builder asserts it at `llmq/snapshot.cpp:82`; a tip scan obtains the tip under `cs_main` at `quorumsman.cpp:151`. These callers can run at ABC's cs_main release while ABC remains active. Class M at the existing release point (`chain/abc.rs:963`) models that interleaving. This does not permit running after every individual ConnectBlock inside one hold: Core keeps cs_main across that hold. Make C6's release-point test explicit: a queued OnV runs after one hold and before the next while ABC is still active, with no open attempt, and observes the current committed layer and published scheme. Class C would defer it through the whole ABC operation and fail that test. M28 is appropriate once this ruling is recorded.

- **Q8 — accept the stated moves.** Land every members-route 6-D row now, including queue-after-token and the real-thread success/failure/proposal matrix. Assign scan-route and nested validation-scan rows to CH7b-2 and composite rows to the actual builder slice, with acceptance entries in those plans. Those tests cannot be declared passed at CH7b-1. No test-only second materializer/scan is needed.

## Core order, interfaces, and M3

C1's default LRU policy and per-type registration behavior match Core's `unordered_lru_cache.h` and `llmq/cache.h`. Eager initialization is answer-neutral for fixed, validated chain parameters. C2 retains the required read-through and raw-write/cache-insert sections; D80's direct snapshot-CF read is justified by the snapshot key's raw-write-only ownership. The ledger remains recorded exactly once after a successful write.

C5's proposed member order matches `utils.cpp:644–729`: semantic gate; base reset/hit; rotation; range check before indexed reset; indexed hit copied to the actual base hash; every cycle index inserted on a miss; final actual-base insertion. The combined members mutex is acceptable because all production members-cache accesses run serially on V, and computation occurs outside it. Precompute correctly obtains the enabled vector eagerly at pprev, evaluates rotation at the block before the height modulo, and passes reset=true only for Undo (`blockprocessor.cpp:37–40`). C7 correctly retains NoMembers/readiness gates while introducing production attempt ownership and applies the log before release on every normal outcome.

**M3 is equivalent under the stated invariant.** Only V can queue/acquire the exclusive attempt gate. While V executes an OnV command, no attempt or queued writer exists on another thread; other external gate holders are shared readers. Removing only that command's shared acquire therefore changes neither blocking nor answers. It must leave the post-entry latch check intact and leave capability lifetime/dispatch intact. Removing `external()` wholesale, including its latch check, is a different, non-equivalent mutation. M3 does not excuse omitting the actual shared gate from the implementation.

The CH7b-2/3 direction fits OD-CP-3: capability-scoped materialization plus the single LQ3b scan, with the future cache host implementing its trait. Keeping handle and log in disjoint fields permits lending the bound adapter and mutable effect sink together. The widened `ClVerifier` must receive that same binding and attempt sink, rather than constructing a tip binding, opening another handle, or dispatching. The stated Cached-versus-ChainBound distinction should be carried through to the LQ3b call. Accessors such as `rows()` and `caches` in §7 must remain internal implementation plumbing or require a live capability; they must not expose a transferable ungated writer/members object.

The mutant set is a useful starting set, but its count alone does not establish acceptance. Address F1–F4, retain the post-acquire latch check for both external and OnV entry, and demonstrate each non-equivalent variant's kill with the intended assertion rather than an incidental timeout or compile error.

Verdict: CHANGES-REQUIRED — 1 major, 3 minor, 0 nit.
