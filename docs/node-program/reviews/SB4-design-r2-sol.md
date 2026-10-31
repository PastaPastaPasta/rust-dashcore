# SB4 design confirmation review, round 2 — Sol

Reviewed **rev1 at 5f03a841**, compared with **rev0 at 8ff4b56b** and `reviews/SB4-design-r1-sol.md`. Design references below are to `docs/node-program/designs/SB4-dash-chain.md` at 5f03a841. Its blob is also unchanged in the integration worktree at review time. Core references are `src/` paths at **v24.0.0-rc.3, da567d7dba945143eee4d2ce1621c21b32089989**. At that pin, the requested `llmq/chainlocks.cpp` functionality is in `chainlock/chainlock.cpp` and `chainlock/handler.cpp`.

DEC-119, DEC-123 and DEC-127 are binding. This review accepts the SB0 floor-reset erratum, shared spork core, network-specific meaning of default on, staffing decision and declared SPV restart differences. It does not reopen them or the other settled sections. REVIEW-COMMON and PLAN §7 handling were followed. This is the Sol confirmation gate; the sequential Opus pass remains outstanding.

**Result: REWORK. Sol gate satisfied: no.** Two majors remain in the new lifecycle/oracle recipes, plus two minor precision fixes. No blocker.

## Major findings

### R2-M1 — The empty-header ChainLock recipe publishes and evicts before commit

**Severity:** major. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:623`, `:702`, `:708`, `:712`, `:849`; approved SB0 §4.4, §4.6 and §9.5. Affects the full M1/M4 closure.

§7.5 gives this explicit order for an SPV ChainLock step: check, enforce, **`set_active_tip`, commit**, `records_committed`. But §9.1 defines `set_active_tip` as the publication/fanout operation and also makes it evict. §9.1's driver recipe correctly puts **commit before `set_active_tip`**. Both recipes cannot be implemented as written.

A caller following §7.5 can expose the new snapshot and feed events before the status/floor/active/meta transaction succeeds. A failed commit or crash in that interval leaves an externally observed transition without its committed step. Premature eviction also breaks the assumption used by the cold overlay: an entry changed while resident can leave the arena before its replacement record commits. For example, in a B110-to-A120 lock reorg at w=10, B101 is resident before enforcement and gets CONFLICT; A120's eviction cutoff is 110. Evicting it before commit makes a cold read reach the previous unmarked record. The overlay specified for edits to *already evicted* entries does not repair this resident-to-cold transition. This remains a problem even if the WriteSet was captured before publication: until commit, the exposed cold tier is old.

This finding does **not** assert that Core durably saves every conflict mark. Core's `MarkConflictingBlock` does not dirty those marks (`validation.cpp:4114-4122`), and an uncommitted SPV step may legitimately disappear. The error is publishing/evicting before the atomic step that the design relies on, not loss of an uncommitted edit alone.

**Fix:** replace §7.5's recipe with §9.1's order, for timer, received-lock, late-header and reset-only steps alike: compute enforcement and the target/plan; capture dirty records plus CL/observed state and active/meta changes; commit; publish/fan out via `set_active_tip`; then `records_committed`. Use the same already-selected target/plan. Add failure/crash controls on the empty-header path, asserting no publication, fanout or eviction before commit and no surviving floor/active change without its status rows. A-30 currently tests successful reopen, not this failure boundary.

### R2-M2 — The `cl` oracle mode omits Core's state check before recording the incoming lock

**Severity:** major. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:879`, `:876`, `:926`; compare the correct port recipe at `:519`. **Core:** `chainlock/handler.cpp:136`, `:147`, `:168`, `:170-176`; `chainlock/chainlock.cpp:98-127`. Affects M6's oracle closure.

The harness recipe is **`UpdateBestChainlock`, then `CheckActiveState`, then enforcement**. Core calls `CheckActiveState` at the start of `ProcessNewChainLock`, before the stale-height checks and update, and checks again in the scheduled enforcement. Bypassing signature verification must preserve those checks and their order.

Two concrete counterexamples:

- Start with `isEnabled=false`, no remembered CL, and an active SPORK_19. The specified harness installs A100, then its first check resets and erases A100. Core first resets the empty state, then installs A100, and its subsequent check keeps it. This can invalidate the promised first-lock smoke test and the lock/status vectors.
- Record height 100 while observed off; enable the spork; deliver a valid lock at height 90. Core's initial check clears height 100, so height 90 is accepted. The harness tries the update against height 100 first and rejects it as stale; a later check cannot recover the incoming lock. This is precisely A-29(i)'s new regression case.

A fixture warm-up with the flag already true hides the first failure but cannot cover the second. The Rust port's §7.1 order is correct; the proposed reference executable would judge a different sequence.

**Fix:** make `cl` run initial `CheckActiveState` → the height/update path with only cryptographic verification bypassed → scheduled `CheckActiveState` → `EnforceBestChainLock`. Keep direct `UpdateBestChainlock` available as an explicitly named state-seeding operation, not as the oracle for receipt of a new lock. The smoke/vector plan must include the fresh-enabled first lock and off-to-on lower-height lock, with emitted best CL, floor, observed flag and statuses.

## Minor findings

### R2-m1 — A-23 must condition its floor assertion on the snapshot's observed flag

**Severity:** minor. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:710`, `:920`; compare `:495`, `:511`, `:628`.

A-23 includes spork toggles but states an unconditional descent-from-`cl_floor` assertion. Turning off retains the stored floor while disabling predicate (b) and the floor refusal. Take A-22's floor on already-active A100 and a heavier, unmarked B110 branch. After observed off, selecting B110 is specified and permitted, although it does not descend from the remembered A100. The unconditional property would reject the correct implementation.

**Fix:** assert `!snap.cl_enabled_observed || snap.cl_floor.is_none() || descends(snap.tip, snap.cl_floor)`. Use the flag and floor from the same snapshot. Keep observing every store, and add the disabled-floor counterexample. Qualify §9.1's concluding sentence accordingly.

### R2-m2 — Preserve `UpdatedBlockTip`'s exact guard

**Severity:** minor. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:619`. **Core:** `chainlock/handler.cpp:185-205`; `validation.cpp:4140-4142`.

“Unless … no block connected” is not the literal Core condition. Core skips when `pindexNew == pindexFork` or `fInitialDownload`. `MarkConflictingBlock` sends `UpdatedBlockTip(m_chain.Tip(), nullptr, ...)`, which can schedule the check even though no block was connected. Replacing the pointer comparison with a connected-count test misses this refresh and can prolong live/observed disagreement until another callback or timer.

**Fix:** state and port the two exact guards. Include the non-IBD `(tip, nullptr)` notification as a control; it schedules the check.

## Confirmation of the r1 closures

- **M1:** the §9.1 composite fixes the original floor-violating intermediate snapshots. Each disconnect/connect gets a sequence, but only the final root/tip/floor tuple is published. Full closure is pending R2-M1; A-23 also needs R2-m1.
- **M2: closed.** `TipHost::{tip, contains, disconnect_tip}` is queried at Core's sites (`:306`, `:536`, `:549-558`). Virtual ancestry membership is independent of the resident window. The B110/A100 walk-back reaches 80; A-21a and A-21b cover that and a repeated enforcement below the window.
- **M3: closed.** R is recomputed per candidate; missing lower prefixes are fetched, excess path entries are trimmed, branch changes rebuild, and cached ids survive eviction without borrowing released entries (`:663-671`). A-5/A-6 now use warm consecutive accepts through all three boundaries; A-6a adds reorg/eviction controls. Implementation must keep the owned cold copies bounded by the current context as well as trimming its id path.
- **M4:** the whole-chain SPV children map is removed. Store-backed children plus `pending_new`, overlay precedence, dirty-record inclusion and commit acknowledgement define the missing cold mutation path (`:142-157`). Those mechanisms close the original structural issues **under §9.1's commit-first order**; R2-M1 prevents unconditional lifecycle closure. The 3.5 MB/560 MB/765 MB figures are estimates, not measured results or bounds on arbitrary fork populations. SB0's residency term remains window plus stale entries in the window, with transient overlay and retained-snapshot costs; A-31 is the fork-free mainnet measurement control.
- **M5: substantive closure confirmed.** §7.5 matches the pinned spork and reset logic, subject to the local recipe/guard corrections above. DEC-127 resolves all five proposed rulings; none is reopened.
- **M6:** CS3 rows are now correctly rule-level; re-mined acceptance cases receive new Core verdicts; A-28 fills the index-state omissions; rows name their oracles; tip comparisons project away block-data status. The acceptance/oracle plan still needs R2-M2's `cl` correction.
- **m1–m5: closed.** D9 declares and normalizes the observable id tie-break; §6.2 qualifies the leaf proof and A-27 supplies its control; §11.1 checks downward work subtraction and A-32 tests underflow; §15 includes test-support linking/provenance/smoke prerequisites; §17 totals 23.5 days, identifies the 15.5-day two-implementer path, and excludes review time. The manager's CPM/staffing work is covered by DEC-127.

## §7.5 audit against pinned Core

Every Core claim in the fact/read/call-site tables and lifecycle prose was checked against the pinned blobs, including the referenced headers and init ordering:

| Claim | Exact Core basis | Result |
|---|---|---|
| Live enable check | `chainlock/chainlock.cpp:33`; `spork.cpp:163-169` | Matches; strict value-before-adjusted-time comparison, not node time |
| Default off | `spork.h:69` | Matches: 4070908800 |
| Mainnet value; test-chain fallback | `spork.cpp:173-191`; `:22-30` | Matches: SPORK_19 returns 0 on mainnet; test chains use received active-map value or default |
| Signed-message validity | `spork.cpp:86-102` | Matches: the +2 h endpoint is allowed; later signing time and wrong/absent signer are rejected |
| Latest per id | `spork.cpp:110-124` | Matches; an equal or older signing time is ignored, so equal-time arrival does not replace the first value |
| Message/signature operations | `spork.cpp:264-336` | Cites resolve; intake belongs to the shared spork core. Preserve testnet's hash-signature path and the other networks' message-signature path |
| `sporks.dat` load/save | `spork.cpp:41-59`, `:62-84`; `init.cpp:1967-1974` | Matches: cache before chain load, signature cleanup at load, store in a valid manager's destructor; not per-arrival Core persistence |
| Cached flag and refresh | `chainlock/handler.h:55`; `handler.cpp:67`, `:136`, `:173`, `:200` | Four sites confirmed; correct the guard as R2-m2 |
| Off-to-on reset; off leaves state intact | `handler.cpp:209-222`; `chainlock.cpp:98-104` | Matches: reset best/hash/known-block/floor and notify pointer on observed false-to-true; no index mark is cleared |
| Recording and late-header updates while off | `chainlock.cpp:106-132`, `:152-172` | Matches: neither method has an enable gate |
| Live-gated CL queries; cached-gated enforcement | `chainlock.cpp:49-96`; `handler.cpp:275-287`, `:107` | Matches; recording state and enforcement state remain distinct |
| Existing conflict marks still act while off | `validation.cpp:4612-4619`, `:4643-4646`, `:3544-3547` | Matches; these reads have no spork gate |
| Node restart versus SPV restoration | Core state constructors and `handler.h:55`; design `:625-637` | Core starts without CL state and with observed false. SPV restoration/receipt persistence are accepted differences under DEC-127 |

The “up to 5 s” observation window describes the nominal timer cadence, not a hard scheduling deadline. A new lock and the qualifying tip callbacks can refresh it sooner.

## Publication and crash conclusions

Under **§9.1's correct order**, a snapshot reader sees either the old coherent snapshot with its old floor/flag, or the final coherent snapshot with the new floor/flag. A retained snapshot's cold `at_height` uses ancestry from its own retained root/lowest entry, as required by SB0 §4.4; it never uses `HeaderStore::active_at`. The live projection may change at commit, but is not a source for a retained snapshot. `lookup`, `entry` and status reads expose index facts, not a separately published active root. No floor-violating intermediate snapshot was found on these paths.

Feeds are populated after the final snapshot store. Existing subscribers may consume a disconnect/connect prefix and have intermediate *subscriber progress*; that does not publish another HeaderChain tip. Loading the chain snapshot at any such pop still gives the final tip. `subscribe_at` shares the tip lock, so it cannot take a cut inside sequence assignment. SB5 must retain ordered pending delivery, captured targets and bounded feed queues; its pending/handoff tests remain necessary. A-23's ArcSwap hook alone is not an execution test of those feed seams.

With dirty resident records and overlay replacements in **one** WriteSet alongside active/meta/CL/observed state, clearing the overlay only after commit cannot selectively lose a committed status edit. A precommit crash restores the old whole step; a surviving committed step includes the changed statuses. Buffered durability may lose a suffix of whole steps, per SB0. Core likewise does not promise preservation of unflushed conflict edits; SPV's stronger status persistence is already part of SB0. R2-M1 is the contradictory path that breaks the assumptions needed for these conclusions.

## Verification and limits

- Read pinned Core source rather than treating the local Core checkout as rc.3. Compared the exact rev0/rev1 design blobs and r1 findings. No live chain instance, private input, GitHub message, commit or push was touched.
- Ran `cargo test --locked -p dash-consensus --test checks_recorded --test kgw_boundaries --test history_sample` in the existing isolated review checkout at **7c333502** with its own target: **4 tests passed**, covering recorded rule verdicts, both history samples and KGW boundary vectors. An initial invocation used the nonexistent target `checks`; it was corrected before this passing run.
- Ran supplemental `spork_tests,llmq_chainlock_tests` in an existing Core test binary: **16 cases passed**. Its worktree HEAD is **9a51d11513fe84d417e846b1adc2a7e22624909b**, newer than the pin; the relevant spork/ChainLock implementation and suite source files have no diff from rc.3. This is supplemental smoke evidence, not a rebuilt rc.3 SB4 scenario harness.
- Ran `/work/scratch/sb4-design-r2-sol/spec_checks.py`: **13 review-model checks passed**, including warm envelopes, virtual walk-back, final-root/feed ordering, retained cold ancestry, both oracle-order counterexamples, the disabled-floor assertion, and six SQLite process-crash cuts around an atomic status/floor/active commit. These are specification counterexamples and transaction models, not execution of the unimplemented SB4 or proof of power-loss behavior of its future store. Logs and pinned copies are in that scratch directory.
- Tests used a mode-700 review TMPDIR under `/dev/shm`, removed after completion. The requested report is left uncommitted; no reviewed source/design file was edited.

## Verdict

**REWORK. Sol gate satisfied: no.** Correct R2-M1 and R2-M2 before advancing to the sequential Opus review; apply the two local minor fixes as well.
