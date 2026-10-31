# SB4 design review, round 1 — Sol

Reviewed `designs/SB4-dash-chain.md` **rev0 at 8ff4b56b (827 lines)**, **as amended by DEC-123**, against approved SB0 rev6.1, CH2/DEC-103, A1 §2.10, CS3's interface at **7c333502**, and Dash Core **v24.0.0-rc.3** (**da567d7dba945143eee4d2ce1621c21b32089989**). Design line references below identify 8ff4b56b; Core references are `src/` paths at that tag. At this pin the ChainLock implementation is `chainlock/chainlock.cpp`, rather than `llmq/chainlocks.cpp`.

DEC-123's rulings are binding. In particular I treat SPV's `AlwaysOn` text as superseded: SPV receives signed SPORK_19 state and suspends floor enforcement and new conflict marking while disabled. I do not reopen the residency allocation, separate harness task, clock ownership, missing-parent startup refusal, result shape, `leaves()` addition, or fatal-latch `None` answers. This is the Sol gate only; the Opus pass remains sequential under DEC-92.

**Result: REWORK. Sol gate satisfied: no.** Six majors concern publication, enforcement views, context reuse, cold state, spork lifecycle, and usable acceptance vectors. No blocker is reported.

## Major findings

### M1 — The SPV composite publishes tips below, or off, its new ChainLock floor

**Refs:** `designs/SB4-dash-chain.md:479`, `:529`, `:538`, `:725`; approved SB0 §4.1, §4.6 and §13's published-tip acceptance. **Core:** `validation.cpp:4096–4110` performs actual node disconnects; `chainlock/handler.cpp:299–309` permits the node to trail the locked block until activation completes.

The target refusal checks only the final `to`. The specified `set_active_tip` then disconnects and connects one step at a time, and every private `step` stores its snapshot in the reader-facing `ArcSwap`. §7.3 explicitly calls each intermediate snapshot ordinary. With B110 active, a fork at 80, and a new floor A100, the first published disconnect snapshot is B109; later snapshots include height 80 and A81…A99. None descends from A100. A-23 therefore fails under the stated publication algorithm even when `fork_target()` correctly returns A100.

This is the already-approved **SPV-only** invariant, not a proposal to restrict the node. SB0's Sol r7 explanation expressly did not require publication of the intermediate common ancestor below the SPV floor. The index and the F1 event sequence can represent the disconnect/connect trace without exposing each provisional SPV root as a new public chain snapshot.

**Fix:** specify separate publication behavior for node single steps and a committed SPV composite. Keep node per-step snapshots and trailing-floor behavior. For SPV, publish the final coherent root/tip/`tip_seq` after the header transaction, while delivering the ordered F1 events with their assigned sequences. Define its interaction with SB5's pending fanout and status publication. A-23 must sample actual reader-visible snapshots at every potential publication, rather than checking only final targets.

### M2 — `VirtualTip` supplies no virtual tip query for Core's walk-back algorithm

**Refs:** `designs/SB4-dash-chain.md:242`, `:453`, `:464`, `:468`, `:479`. **Core:** `validation.cpp:4087`, `:4096–4108`, `:4114–4121`; `chain.cpp:61–69`.

The host interface supplies only `disconnect_tip`. The pseudocode reads `active_tip` for `walk_tip` and `old_tip`, while the SPV host deliberately leaves the real active chain unchanged. Its description overrides only containment using a cursor height. Start with B110 as both active tip and best header, and mark B81 while enforcing A100. A literal implementation sees B110 as `old_tip` on every virtual disconnect: it walks best header back once to B109 and then stops. Core sees B110, B109, …, B81 and finishes at the common block at 80. Later ordinary tip steps do not perform the missing `MarkConflictingBlock` best-header hooks.

Containment also cannot simply call the bounded `ActiveChainView::at`: a remembered floor or enforcement walk can be below the resident window. Core's active vector still recognizes those ancestors. Delegating retained snapshots' below-window `at_height` to SB5 does not define the writer's enforcement view.

**Fix:** give enforcement an explicit view with `tip()` and `contains(id)` as well as disconnect, or carry that complete cursor view inside the enforcement routine. All `walk_tip`, `old_tip`, containment and outer-loop queries must use it. The SPV view must resolve below-window membership by ancestry/cold records. Add an SPV oracle comparison for the complete best-header walk-back in the B110/A100 case, plus repeated enforcement with an already-active floor below the window.

### M3 — Sliding the cached context by one does not handle a growing read envelope

**Refs:** `designs/SB4-dash-chain.md:491–511`, `:707–710`, D23. **Core:** `pow.cpp:140–167`, `:181–183`, `:206–210`, and KGW's guard at `:35–36`, `:64–65`.

The envelope sizes are correct, including KGW's 4,033rd guard entry. The reuse rule is not: an in-order parent alone triggers an O(1) slide. On mainnet, checking height 575 needs R=11, covering 564…574. Checking the next height 576 needs R=576, covering 0…575. Sliding the old path leaves 565…575 and cannot answer the retarget's ancestor-0 read. Similarly the in-order transition at 15,200 grows from BTC's 11 to KGW's 4,033. Rebuilding an individual boundary test from scratch would hide both failures. A cache of borrowed resident entries also needs a defined lifetime across eviction.

**Fix:** recompute R on every candidate; reuse only the overlap on the same branch, fetch the missing older prefix whenever R grows, trim when it shrinks, and rebuild on a branch change. Cache ids/owned cold contexts or bounded chunk guards with explicit eviction behavior, rather than dangling resident borrows. Use a saturating/clipped lower height near genesis. Make A-5/A-6 exercise consecutive accepts through the boundaries using an already-warm cache, and add warm-cache reorg/eviction controls. The occasional BTC rebuild still has constant amortized cost over an interval.

### M4 — SPV retains a whole-chain children map and has no defined cold-status mutation path

**Refs:** `designs/SB4-dash-chain.md:298–305`, `:390`, `:454`, `:472–476`, `:670`, `:735`; approved SB0 §4.4's bounded residency and O(window + windowed stale entries) reopen. **Core:** `node/blockstorage.cpp:123–125`, `:282–286`; `validation.cpp:4059–4064`, `:4114–4121`.

The writer's `children: HashMap<BlockId, SmallVec<…>>` adds an entry for every parent, even on a chain with no forks. Eviction removes chunks and resident hashes, but never these edges. A 2.55-million-header SPV history therefore retains roughly 2.55 million parent-map entries, rather than a window-sized index. Rebuilding it after restart also needs the whole chain; `from_height` plus `leaves` does not reconstruct all cold parent/child edges. Core keeps this map because its index is permanent; copying that storage choice into the windowed SPV configuration defeats SB0's agreed split.

The cold A-21 variant exposes a second missing part of the same mechanism. At w=10, B81 and C81 are evicted when the tip is B110. `EnforceBlock` must still find those siblings, mark their statuses, expose the updated marks to the eligibility walk, and emit their replacement records in the transaction. `modify` is described only as an entry seqlock write; `HeaderStore` has only read methods; no cold dirty overlay or its lookup precedence is specified. Merely reading the old records gives incorrect `fork_target`/restart answers.

**Fix:** keep Core's complete children map in `Permanent`; define a windowed children lookup backed by the already-approved `prev` database index, with a bounded resident cache. Add a writer-owned dirty overlay for evicted records, including status-read precedence, `take_dirty_records`, commit/discard behavior and cold-cache invalidation. Do not write the database before the header step commits. Test marking already-evicted B81/C81, extending the affected branches before publication, reopening, and memory growth on a long fork-free chain.

### M5 — DEC-123's spork amendment needs Core's re-enable reset and network-specific semantics

**Refs:** `designs/SB4-dash-chain.md:159–162`, `:186`, `:440–447`, `:801`; DEC-123 Q2. **Core:** `chainlock/handler.cpp:209–220`, `:273–286`; `chainlock/chainlock.cpp:33`, `:49–79`, `:98–103`, `:110–127`; `spork.cpp:163–191`, `spork.h:69`.

Even granting the amendment's disabled-state behavior, changing the injected boolean from off to on leaves `cl_best` and `cl_floor` intact. Core's `CheckActiveState` explicitly calls `ResetChainlock` on that transition to avoid enforcing stale locks from before the disablement. SB4 has no reset hook, and §7.1 only moves the remembered height upward. A remembered A100 can therefore become binding again immediately, or make a post-reset lock at height 90 return `Stale`, when Core has cleared its remembered height and floor.

The signed-state/default wording also needs a network qualification. On mainnet, v24 `GetSporkValue` hardens SPORK_19 to 0 and ignores received overrides (`spork.cpp:173–180`). On test chains it reads active signed values and otherwise uses the default 4,070,908,800, i.e. off (`:185–191`, `spork.h:69`). A blanket default-on, signed-off-controlled service is not that behavior. This inconsistency belongs in the manager's DEC-123 clarification, rather than silently becoming an undeclared protocol difference.

Existing conflict bits are a separate matter: Core's duplicate/parent checks and candidate walk consult them unconditionally (`validation.cpp:4612–4618`, `:4638–4646`, `:3544–3547`). Disabling the spork does not justify clearing or ignoring old marks wholesale.

**Fix:** specify a writer-side gate transition/reset operation and its snapshot/persistence effects; use it in both hosts. Resolve the default/network qualification with the manager and document the approved result. Add off→on, pending late-header across the transition, lower-height post-reset lock, restart, and pre-existing-mark controls to the harness and SPV tests. Do not merely replace `AlwaysOn` with a boolean provider.

### M6 — The proposed oracle rows are not full header-acceptance vectors

**Refs:** `designs/SB4-dash-chain.md:695–706`, `:718–728`, `:743–746`. **Core:** `validation.cpp:4607–4651`, `:4673–4698`; `chain.cpp:32–46`; `rpc/blockchain.cpp:1914–1936`. **CS3:** `dash-consensus/tests/support/checks.rs`'s `Input::ContextualHeader` arm and `tests/data/checks_recorded.json` at 7c333502.

A-1/A-2 promise to replay every CS3 header verdict through `accept`, preserving its strings. The recorded header rows exercise **ContextualCheckBlockHeader** over constructed windows, without first accepting that ancestry or mining the candidate. Full acceptance first checks PoW and parent/index state. The scratch probe of the exact CS3 revision confirms **five of its ten contextual header rows** are masked by `high-hash`: three `bad-diffbits` rows, the MTP equality row and the future-limit-plus-one row. Rebasing a header onto an anchor also changes its hash/PoW and possibly its contextual inputs; the old contextual oracle judgment is no longer an oracle judgment of the rebased acceptance case.

The 26 rows also do not explicitly exercise known FAILED/CONFLICT duplicates, FAILED-before-CONFLICT precedence, immediate invalid parents versus lazy `m_failed_blocks` ancestry, batch `last_accepted` on partial failure, unknown/height-mismatched/late locks, or the spork transitions in M5. CS3's pure-rule rows cannot cover those index-state branches. A-4's header-only implementation versus a live full node's `getchaintips` status is another mismatched comparison: a live oracle's stale tip can have downloaded transactions and `valid-fork`, while the SB4-only side necessarily says `headers-only`.

**Fix:** retain CS3 vectors as rule-level controls. Generate separate **ProcessNewBlockHeaders** vectors with an explicit valid or deliberately seeded pre-state, mined candidates where the test needs to reach a contextual rule, and new Core verdicts after rebasing. Emit result class, reason/debug text, last accepted hash, statuses and dirty records from the harness. Add the omitted acceptance/lock cases and cross-check header-only tips against a header-only oracle state, or explicitly project away block-data-dependent status. `ScriptedV` should replay node events, not manufacture the expected header verdicts.

## Minor findings

### m1 — The address proxy is observable; first-mention ids do not reproduce address order

**Refs:** `designs/SB4-dash-chain.md:414`, `:590`, `:721`, `:798`. **Core:** `node/blockstorage.cpp:32–49`, `validation.cpp:5359–5360`, `chain.cpp:39–40`.

For equal-work, sequence-0 entries after reload, choosing a different best header changes the first locator **hash**. Equal-work branches can also have different heights. Heap address order is not guaranteed to be allocation order, so first-mention ids are a deterministic substitute, not evidence of Core-identical behavior. SB0 already accepted this load comparator substitution; I do not reopen that ruling. Document it as an observable allowed difference and normalize only that tie in oracle comparisons. D9 additionally applies the proxy to failed-block iteration and RPC ordering; enumerate those effects rather than claiming that absence of a tie-dependent fixture proves unobservability.

### m2 — Qualify the leaf proof: it proves the approved SPV predicate, not Core's actual candidate set

**Refs:** `designs/SB4-dash-chain.md:420–432`. **Core:** `validation.cpp:3537–3547`, `:3583–3589`, `:4131–4135`, `:5348–5354`.

The leaf maximum proof works for SB0's header-only eligibility predicate with positive per-block work. Core's actual candidates additionally require transaction validity/download history, have-data paths and sequence-id ordering; its `FindMostWorkChain` does not itself apply SPV's separate descent-from-floor predicate. A heavier downloaded branch after a lock on an already-active block can still be considered before a later connect rejects the conflict. Replace “the answer equals Core's candidate set's” with the precise SPV equivalence. The referenced FT-3 row does not exist in §15: give the randomized leaf-versus-all-accepted-records comparison an actual test ID, including cold inputs and bad suffixes.

### m3 — Require checked subtraction for pack work verification

**Refs:** `designs/SB4-dash-chain.md:601`. **Core:** `chain.cpp:132–145`, `node/blockstorage.cpp:110`.

Deriving predecessor work downward from the trusted absolute anchor work is correct; independently adding work upward from zero would not be. Specify **non-wrapping/checked** subtraction for this verifier: ordinary `Arith256` subtraction can underflow to a positive value, defeating “every derived work must stay positive.” Check each subtraction before assigning context work. Add a low-absolute-work/underflow negative case and assert each derived parent/child work relation. This is verification of the supplied pack, not a stricter rule for Core's live chain-work arithmetic.

### m4 — The harness is feasible, but the cited build recipe needs test-support objects

**Refs:** `designs/SB4-dash-chain.md:696`, `:781`, `:811`. **Core:** `test/llmq_chainlock_tests.cpp:397–401`, `:462–464`; `test/util/setup_common.h:110–114`. **Build inputs:** `contrib/node/build-oracle.sh:237–240`, `contrib/node/bs5-chainparams/build.sh:116–120`, `:130–140`.

Core already has a RegTestingSetup spork activation/toggle path using its public regtest test signer. The stated SPORK_19 uncertainty does not require a masternode-chain fallback. However both cited build recipes disable tests; BS5 builds `dash-tx`, and the published O1 archive localizes Core symbols. Neither by itself supplies a linkable `RegTestingSetup` scenario executable. The separate harness task should build the unlocalized server dependencies plus the required test utility objects at the pinned shim commit, record provenance, and smoke-test one lock/reload/codec scenario before promising the full vector set. Do not charge an unnecessary quorum-network setup to this design's critical path.

### m5 — Correct the critical-path arithmetic and account for review/gate time

**Refs:** `designs/SB4-dash-chain.md:769–784`; PLAN §4's L estimate and §8.3's 1.5-week slack.

The ten estimates total 24 days. Moving only the three-day C8 harness off the implementer's path leaves **21 days**, not 19: **4.2 five-day weeks**. Against PLAN's L=3 weeks this consumes **1.2 of the 1.5 weeks of merge slack**, leaving roughly 1.5 working days before accounting for sequential Sol→Opus review, fixes and the full-chain/performance gates. Front-loading stubs permits dependent coding overlap but does not reduce the implementer's remaining sum. Reconcile the estimate and rerun CPM with C8 genuinely separate under DEC-123; identify whether validation/review time is included. The supplied arithmetic does not establish the claimed 3.5-week fit.

## What checks out, and limits

- **Acceptance order:** §5.2 otherwise preserves Core's genesis shortcut, known-hash handling, PoW before parent lookup, FAILED before CONFLICT, contextual checks before lazy failed-ancestor propagation, and conflict insertion before `bad-chainlock`. The known-header fast path correctly does not deliver a new AcceptedBlockHeader callback. Do not strengthen those rules.
- **Best header/status:** strict-greater acceptance, no acceptance promotion for a conflict-marked insertion, the walk-back/InvalidChainFound hooks, and the load recompute including `VALID_TREE|CONFLICT` are the correct sites. FAILED_CHILD marking and dirtying during the lazy invalid-ancestor walk and load match the cited Core conditions. Node MarkConflictingBlock marks are deliberately not dirty; Core's later candidate-walk conflict propagation is dirty and remains CH1-node's responsibility (`validation.cpp:3558–3561`).
- **Persistence:** §10.3 describes the approved A1/CH2 internal record format: Core CDiskBlockIndex field order **with client-version prefix and inline hash deliberately removed**, not an unmodified Core disk value (`chain.h:391–412`). Conditional file/data/undo varints, stored-hash PoW without X11, ordered first-mention assignment after parallel decoding, and height-ordered reconstruction are compatible with that contract. MissingParent is DEC-123's accepted deviation. B1 ownership remains CH2's; these design checks do not establish crash durability of future code.
- **Concurrency:** the atomic-field seqlock fences and single-writer ownership have no apparent torn-tuple error. Index insertion follows immutable/status initialization, and snapshot publication follows id publication, so an issued id does not precede its initialized entry. Arc-held chunk guards and never-reused SPV ids prevent eviction-related id ABA. A u32 seqlock counter still has the ordinary 2^31-write wrap caveat; the proposed finite loom/stress runs cannot prove an unlimited suspended reader safe from wrap. No concrete practical wrap scenario is made a gate finding. M1/M2 concern view/publication semantics, not the atomic fence protocol.
- **Feeds and coverage:** SB4 correctly leaves F3–F6/F10/F11 and feed staging/wakes to SB5, retains the tip lock/seq basis for F2/P7, and keeps P10 records in CH2 B1. SB5's deep-reorg, retained cold snapshot, restart and handoff properties remain required; four SB4-side seam rows do not replace them. Core-style node tip changes must remain unrestricted by SPV's floor refusal.

## Verification

- Read the review protocol and PLAN §7 handling; inspected the pinned Core blobs rather than the local Core checkout, whose HEAD is not rc.3. No live dashd was started or changed, no private material was searched, and no GitHub action, commit or push was taken.
- Ran `/work/scratch/sb4-design-r1-sol/spec_checks.py` with a mode-700 review TMPDIR: **9 tests passed**. These reproduce the two envelope expansions, virtual-vs-real best-header walk-back, intermediate SPV floor violation, linear children-map growth and schedule arithmetic; check downward work/underflow and reset behavior; and compare leaf selection with a brute-force accepted-header scan on **1,000 random trees**. They are specification models, not executions of the unimplemented SB4.
- In a separate detached CS3 checkout at 7c333502 with its own target directory, ran `cargo test --locked -p dash-consensus --test kgw_boundaries --test history_sample`: **3 tests passed** (mainnet history, testnet history, Core KGW boundary vectors). Ran the review-owned `sb4_accept_probe` test against that same source: **1 test passed**, confirming the five masked contextual errors above. Logs and the probe source are under `/work/scratch/sb4-design-r1-sol/`. The active CS3 worktree had unrelated edits; they were not changed. An initial test run there was stopped and superseded by this isolated verification.
- Final whitespace/path checks passed. The review-owned TMPDIR was removed after the tests; the requested report is the only change made to node/integration by this review.

## Verdict

**REWORK. Sol gate satisfied: no.** Resolve M1–M6 in the design and its oracle/acceptance plan before advancing to the sequential Opus review. The request's report is uncommitted.
