# SB4 design confirmation, round 7 — Sol

**Verdict: APPROVE-WITH-CHANGES.** No blocker or major; one minor explanatory correction. **R6-M1, R6-m1 and R6-n1 are closed.** DEC-146 supplies the missing retirement rule, and the requested Core state claims and new connected-A kept-mark fixture are confirmed. The minor below does not require a different algorithm, ruling or fixture.

## Scope and pinned evidence

Reviewed exactly:

```text
git diff d6e461fc~1 6a625933 -- docs/node-program/designs/SB4-dash-chain.md docs/node-program/designs/SB0-shared-base.md
```

Base: **3662470e0038b98f19b6024624f59fe5bfa58bce**; rev6: **d6e461fc32c39513ccf634bf9d00b419a115564e**; rev7: **6a6259334382b959800ff8065eb525cc9e967482**. Design line references below are to **rev7**, saved in `/work/scratch/sb4-design-r7-sol/`. The moving `node/integration` worktree advanced during review; later design revisions are outside this confirmation.

Read REVIEW-COMMON, PLAN §7, program context, Sol r6 and the relevant CH2 contract. Binding SB4 decisions in DEC-128…DEC-146 were treated as settled. PLAN §7 handling was followed; no private inputs were read.

All Core citations mean `src/` files from **v24.0.0-rc.3**, peeled release commit **0364ed626ec2eaa518bf1ae4a0566ce638c7fe18** (annotated tag object `da567d7dba945143eee4d2ce1621c21b32089989`). The harness branch was **42737ddf6367bb43e83dfe8050a633a33d72a9f0**, with shim pin **a9a483e51efdadeee37cf40ff8697da62b3cd012**. Compared the cited production files against the release: they agree; the only difference in `validation.cpp` is three unused test trampolines appended after the production code.

## Findings by severity

### R7-m1 — active membership is sufficient for the new fixture, not necessary for a kept mark

**Severity: minor.** **Refs:** `docs/node-program/designs/SB4-dash-chain.md:544`, `:684`, `:1303`.

The new statement that the future floor's branch can be marked **only if it was active** is too strong. `EnforceBlock` checks whether the **locked path** is active, then marks every sibling encountered on that path; it does not require the sibling to be active (`validation.cpp:4057–4064`). `MarkConflictingBlock` marks the sibling itself even when no disconnect occurs (`:4120–4122`).

Concrete Core replay: connect A1…A80; accept A81…A95 and B81…B110 as **headers only**; on/check; lock B100; off/check/on; lock A90. A81 becomes **130** at B100 and remains **130** under floor A90, although A81…A95 were never active. B81 also becomes 130; tip stays A80 and best header B110. The SPV predicate is blocked by the retained A81 mark and can trail at A80. Evidence: `kept-mark-without-active-A-output.json` in reviewer scratch.

**Suggested fix:** replace the “only if active” statements in §6.2 and §26 with “The connected-A fixture below supplies the kept-mark witness.” In §7.5 say a sibling on the future floor's path can retain a mark, including the connected-A case. Also phrase the (ix) control precisely: **a lock whose block is already on the active chain makes no marking walk**. Keep (x)'s specified connected-A script and expected values unchanged. This corrects a new explanatory restriction without reopening DEC-136.

## r6 closure confirmation

- **R6-M1 — closed by DEC-146.** H2/H4/H6/H7 (`SB4:949–954`) and D44 (`:1163`) now distinguish stable registration from live obligations. The monotone `SeqCst` closing store is the retirement point, with generations preventing reuse of the old identity. Capture/gate filtering excludes a slot closed beforehand; a slot closed afterward satisfies the outstanding wait through its closing flag. W registers its waker and rechecks before parking; deregistration wakes both V and W. Late reports are ignored. Thus dropping a closing slot's pending disconnect cannot leave it as a required W target. Its last durable locator and mandatory prune lock remain until mutex-taking deregistration completes after the hold; pruning still includes that locator. The Core comparison is appropriately limited: `BaseIndex::Stop` unregisters (`index/base.cpp:419–426`), and resume uses the stored locator through `FindForkInGlobalIndex` (`:72–84`); Core does not implement our asynchronous ack protocol.
- **A-40 — required strengthening present** (`SB4:1049`). S itself is mandatory and Synced, q is queued, q+1 is pending and removed by closing, and F1 identifies B109/q+1 while the snapshot remains at q. The row explicitly requires close-before and close-after capture/gate variants, a later F2 and second CRITICAL wait under the same hold, live M's progress, serviced `PruneClears`, retained S locator/prune lock and ordered post-hold mirrors. Both negative controls are present: W takes the prohibited hub lock; W ignores closing. The no-deleted-event-dependency hook is stronger than merely checking that the watchdog expires. CH2b must repeat it with real W, as specified.
- **R6-m1 — closed.** §9.2 (`SB4:813`), H2, D45 (`:1164`), SB0 F2 (`:1142`) and E4 (`:1953`) consistently give W the packet's matching private-tip/last-assigned-sequence pair. W reads neither the lagging snapshot nor `TipState` under a newly acquired mutex. A-40 and A-41 (`SB4:1049–1050`) assert the pairing. The freeze remains at CH2's existing pre-tip-step pressure site; the correction does not move it after the disconnect.
- **R6-n1 — closed.** D41 (`SB4:1160`) now includes the required final period in the one-wallet-transaction sentence.

## rev7 Core confirmations

1. **A-29(ix): no first mark; node A80, SPV A95.** B100 is contained by active B110, so the condition at `validation.cpp:4057` is false immediately. No sibling is marked, including A81. After observed off/on, A90 is accepted: `handler.cpp:209–221` resets the CL state, `chainlock.cpp:98–104` touches no index status, and `:110–127` installs the new lock. Its marking disconnects B110…B81 and sets them to **157** (`validation.cpp:4096–4122`). A81…A95 remain **2**. Core cannot activate their missing block data (`:3537–3547`, `:4133`, `:5348–5354`), so tip and best header end at A80 under floor A90. SB4's header-only predicate admits unmarked A95, descending from A90: **A95/Descends**, not A80/Trails (`SB4:543`, `:1038`). The no-observed-off control also reproduces: observed stays on, no reset occurs, and the height precheck rejects A90 as stale (`handler.cpp:136–150`).
2. **A-29(x): connected-A expected values confirmed by execution.** Ran §15's requested script through the existing Core-linked executable, independently of whether the harness task has committed its vector. After B100: **A81…A95=157**, tip A80, best header B110, floor B100. After A90: those A statuses stay 157; **B81=130**, B82…B110=2, tip A80, best header B110, floor A90, observed true. Identity-only best-header updates (`validation.cpp:4089–4090`, `:4107–4108`) never touch header-only B110. Connected A90 has sufficient validity and no FAILED bit (`chain.h:300–307`), so `EnforceBlock` does not call reset (`validation.cpp:4075–4076`). Both header walks stop at a marked sibling and the SPV target is **A80/Trails**. SB0 E2 (`:1931`) now uses this correct connected-A witness. The requested permanent vector remains a harness deliverable, not something this review added to that branch.
3. **Recorded while off: RPC receipt only; SPV drops.** P2P reads/deserializes CLSIG only inside the live-enabled branch (`net_processing.cpp:5794–5797`; `chainlock.cpp:33`). RPC has no spork gate before `ProcessNewChainLock` (`rpc/quorums.cpp:1364–1397`), although normal height, quorum and signature requirements still apply. Coinbase receipt is also live-gated (`handler.cpp:104–108`); direct seeds are fixture operations. Late headers can resolve a lock recorded earlier while on (`chainlock.cpp:152–172`). §7.5 (`SB4:658–659`, `:682–684`) correctly places SPV's live-view gate before verification, pending insertion and `apply_chainlock`. Its lack of an RPC route does not prevent an already-recorded pending lock from surviving into an off period. RPC-off and P2P-off controls reproduced their distinct outcomes.
4. **The three best-header quirks hold** (`SB4:516–522`). Mark/invalidate walk-backs compare identity (`validation.cpp:3951–3953`, `:3969–3971`, `:4089–4090`, `:4107–4108`); B96's mark leaves B110 selected. Existing headers return from `AddToBlockIndex` without running its strict-work update (`node/blockstorage.cpp:91–115`), so reconnecting after enforcement leaves best header at the fork: A990 while tip returns to A1000. Reconsider clears flags and updates candidates without recomputing best header (`validation.cpp:4152–4230`), reproducing A98 after the relevant reconsider operations. The separately listed `InvalidChainFound` ancestor-to-active-tip update remains intact (`:2043–2044`); `ConflictingChainFound` does not do it (`:2058–2073`).
5. **A-17's unpersisted mark is confirmed, with its existing qualification.** The mark sites do not insert into `m_dirty_blockindex` (`validation.cpp:4114–4122`). The specific vector reloads B96 from **130 to 2**, retaining best B110 (`SB4:606`, `:1024`, `:1134`). This does not mean CONFLICT can never persist: an already-dirty entry, another dirtiness site, or candidate propagation (`validation.cpp:3558–3561`) can persist it. D15 continues to follow Core's dirtiness sites; SPV still persists every status change.
6. **`address_tie` is correct.** Reload recomputes best header with the work comparator (`validation.cpp:5359–5360`), which compares work, sequence id, then **pointer address** (`node/blockstorage.cpp:32–45`). The C101/D101 reload vector reports the tied set for both best header and locator0. No particular Core winner is asserted; membership plus locator consistency is the correct normalized comparison (`SB4:522`, D9 `:1128`). The arena-id substitution remains the previously settled allowed difference.

## Regression check and validation limits

No other changed hunk breaks a settled SB4 invariant. DEC-128's compute/capture/commit/publish/clear recipe and DEC-132's node persistence exception remain intact. DEC-136's non-conflicting floor predicate, post-enforcement cursor and per-mark host hook are unchanged; (x) repairs the witness rather than tightening the predicate. DEC-140's continuous mutex hold and pre/post admission, DEC-144's one-wallet transaction and per-disconnect pressure site, DEC-145's per-step events and DEC-146's retirement/retained-prune-lock separation remain intact. The new SPV intake rule uses the live view; it does not erase the settled live/observed distinction.

Evidence is in `/work/scratch/sb4-design-r7-sol/`:

- `regen.py --check --bin /work/scratch/sb4-core-scenarios/out --jobs 4 --only ...`: **9 selected non-benchmark vectors matched byte-for-byte**, covering (ix) and its no-check control, RPC/P2P off receipt, late-header receipt, A-17, A-19, reconsider and the enforcement reorg. Full selection and output are in `selected-vectors.log`.
- Verified the executable SHA-256 against MANIFEST: `31dc7fa16b22b24ba4d7719f77a9c6aa27fa21c8b8b749a755a085cbdb295d15`. Ran the new (x) script and the inactive-header sibling negative control; both exit 0. These use the harness's declared signature seam and selected delivery policy, not a claim that its full callback trace is Core's unique permissible schedule.
- `confirm_checks.py`: **passed** the state assertions, both close/capture orders, future-target exclusion, retained locator/prune-lock model, all **10** register/recheck/park versus close/wake interleavings, and **2,000** pressure checks with **34** matching private-tip/sequence freezes and first native crossing at **102**. This is an independent abstract design check, not production A-40/A-41 or native-atomic verification.
- Re-ran r6's threaded live/pruner and prohibited-lock controls, missing-event deadlock witness and explicit-retirement progress control: expected completion/hang outcomes passed. Those probes remain supplementary models, not a complete CH2b implementation.

Tests used a mode-700 `/dev/shm/sb4-design-r7-sol-tmp`, removed afterward. No reviewed source, existing review, harness vector, live chain instance, GitHub message, commit or push was changed. This review is left **uncommitted**. Production SB4/SB5/CH2b tests and the harness's separate review gates remain implementation obligations.

## Verdict

**APPROVE-WITH-CHANGES.** All r6 findings are closed; the specific rev7 Core expectations are confirmed. Correct R7-m1's overbroad reachability explanation. There is no blocker or major and no need to reopen a settled ruling.

Sol gate satisfied: yes
