# SB4 design confirmation review, round 3 — Sol

Reviewed **rev2 at 26e8b115**, compared with **rev1 at 5f03a841** and `reviews/SB4-design-r2-sol.md`. References below are to `docs/node-program/designs/SB4-dash-chain.md` at 26e8b115 (blob `39632acf661df2bbf23500e46716f7d424910aa8`), unchanged in the integration worktree during this review. Core references are `src/` paths at **v24.0.0-rc.3, da567d7dba945143eee4d2ce1621c21b32089989**.

REVIEW-COMMON and PLAN §7 handling were followed. DEC-119, DEC-123, DEC-127, DEC-128 and DEC-132 are binding. This is a narrow confirmation of R2-M1, R2-M2, R2-m1 and R2-m2, including the wrapper feasibility and node-reader consequences requested in the brief. Settled sections and accepted differences are not reopened.

**Result: APPROVE-WITH-CHANGES. Sol gate satisfied: yes.** All four r2 findings are closed. No blocker, major or minor remains in scope; the sole change below is an editorial citation correction and needs no further Sol round. The sequential Opus pass remains outstanding.

## Closure confirmations

- **R2-M1: closed.** §9.1 (`:721-730`) specifies the same lifecycle for header batches, timers, received locks, late headers and reset-only steps. The target and `pending_plan` are selected before capture; the WriteSet contains dirty resident records, overlay replacements, CL/observed state and active/meta changes. Publication, event handoff and eviction are confined to stage 4; failed commit stops the driver at stage 3. Stage 4 executes the saved plan and treats a different target or missing plan as an internal error. A-33 (`:950`) observes all three prohibited effects on commit failure for every named step kind; A-34 (`:951`) checks whole-step persistence at the empty-header lifecycle cuts; A-35 (`:952`) contains the B110 → A120, w=10 resident-to-cold witness. In that witness B101's replacement record is committed before it can become cold. Failure leaves it resident, with no published transition. These are appropriate design acceptance requirements; they have not yet been executed against an SB4 implementation.
- **R2-M2: closed.** The receipt mode at `:897` preserves Core's initial state check, seen/height checks, lookup/update and scheduled check/enforcement. Direct update is confined to `seed-cl` (`:898`). The smoke requirements (`:894`) and A-29(i)/(vii) (`:946`) explicitly exercise both r2 counterexamples and emit the best CL, floor, observed flag and statuses. Core paths use the pinned `chainlock/` layout. The verification seam is feasible without replacing the handler; see below.
- **R2-m1: closed.** §9.1's predicate (`:715-719`) uses the observed flag and floor from one snapshot. A-23 (`:940`) applies it at every ArcSwap publication and includes the heavier, unmarked B110 branch while the remembered A100 floor is disabled. That snapshot correctly passes with the observed flag false.
- **R2-m2: closed.** The API (`:301-305`) and call-site table (`:624`) port the literal `new == fork` and IBD guards, preserving the non-IBD `(tip, nullptr)` notification. The table names all three Core producers, and the SPV report carries the `MarkConflictingBlock` notification count. A-29(viii) tests the positive control and both negative controls; `notify` exercises Core's callback and drains its scheduler. The citation typo below does not omit that coverage.

## Step-order search

Searched the complete pinned design for `set_active_tip`, `records_committed`, `tip_plan`, `pending_plan`, WriteSet/commit/publication/eviction wording and compute arrows, and inspected every match against the rev1 delta. The former executable recipe in §7.5 is gone: `:628` cites §9.1. The relevant API comments, §3.7, §7.1, §7.3, §13.3 and §15.2 now refer to §9.1 or its stages. **No competing step recipe or surviving publication-before-commit instruction was found.**

The document still describes individual ordering relationships in summaries and acceptance cases, including §21's statement that commit precedes `set_active_tip` (`:1093`). Those descriptions cite the normative recipe and agree with it; they do not establish another driver sequence. The internal stage-4 procedure and node reduction are both within §9.1. The ChainLock receipt order in §7.1/§15 is a distinct Core-handler sequence, also consistent with DEC-128.

## Verification seam at the pinned handler

`ProcessNewChainLock` calls the **four-argument** overload taking `(Consensus::Params const&, CQuorumManager const&, ChainLockSig const&, CBlockIndex const*)` at `chainlock/handler.cpp:156`. It is declared at `chainlock/clsig.h:69-70` and defined out of line in the separate `chainlock/clsig.cpp:263-269` translation unit. It is therefore an external linker reference, suitable for GNU `--wrap` when the harness links the unlocalized Core objects specified at design `:894`.

I compiled a typed wrapper returning `llmq::VerifyRecSigStatus::Valid` and partially linked it with an existing handler object using `ld -r --wrap=<that overload's mangled symbol>`. The handler source in that build worktree has no diff from the pin. The link check confirmed that the verification reference resolves to the wrapper, all **135 unrelated undefined references** remain unchanged, and all **23,226 bytes of the original handler's `.text`** remain unchanged. This checks the actual external call boundary, rather than relying on an assumed wrap of an inline or same-object call.

The surrounding source paths remain Core's: `CheckActiveState` at `:136`; duplicate/stale checks at `:140-151`; signing-start-block selection at `:154-155`; the verification-result branch at `:156-164`; `LookupBlockIndex` at `:166`; update at `:168`; and scheduler dispatch/check/enforcement at `:170-176`. Returning `Valid` selects the ordinary successful-verification branch. The harness must wrap this overload and invoke the real handler, as the rev2 recipe requires; wrapping the other overload or linking the already-localized oracle archive would not establish this oracle.

## DEC-132 and node readers

**No reader-state discrepancy introduced by the node reduction was found.** Empty per-step capture/commit/acknowledgement stages do not change the node's live publication sites, entry writes or feed sequencing. The node uses `Permanent`/`NoStore`, has no eviction or cold-status overlay, and retains dirty entries until CH2 takes them at a flush (`:189`, `:570`, `:730`, `:784`).

- **Live snapshot and index readers:** §9.1's existing per-tip publication, coherent retained active root, status publication and tip lock still apply. A live tip or header can precede its disk-index write, as in Core: `FlushStateToDisk` writes the index only under its flush/periodic-write conditions (`validation.cpp:3042-3048`, `:3061-3067`), whereas tip updates and their notifications proceed at `:3316-3326` and `:3473-3478`. A remembered floor can lead the node tip while enforcement/activation proceeds (`chainlock/chainlock.cpp:115-127`; `chainlock/handler.cpp:296-309`). DEC-132 introduces neither another published intermediate state nor a promise that a live snapshot is durable. Per-entry index reads remain live index facts under the already-reviewed contract.
- **Subscriber durability and disk readers:** CH2's F7 and flush steps 4/5 still publish `ChainStateFlushed`/`FlushCommit` and advance `T_ack` only after durable B1. Tip/data events cannot stand in for that barrier. Copy-point RPCs still receive the guarded snapshot taken after Bk, and recovery still loads the durable block index before validating the recovered tip (CH2 §2.6, §4.2, §4.3, P10). The reduction does not move any of those boundaries or let an unflushed live tip certify durable state.

This accepts DEC-132's existing CH2 contract and the settled live-reader architecture. It does not require a per-tip node transaction or stronger persistence of Core's non-dirty conflict marks.

## Finding

### R3-n1 — `notify` cites the adjacent A-29 case

**Severity:** nit. **Ref:** `docs/node-program/designs/SB4-dash-chain.md:899`.

The `notify` mode points to A-29(vii), which is the fresh-enabled first-lock case. Its `(tip, nullptr, false)` callback control is A-29(viii). A harness implementer following that cross-reference reaches the wrong subcase, although the mode text and A-29 row already specify the correct behavior.

**Suggested fix:** change the citation to `A-29 (viii)`. No semantic change or additional confirmation round is needed.

## Validation and limits

- Compared the exact rev1/rev2 design blobs, read the r2 findings and binding decisions, searched all ordering references, and checked the relevant Core handler, verification, callback, tip and flush sites at the pinned tag.
- The typed wrapper compilation and partial-link/code-byte checks passed. Artifacts and logs are under `/work/scratch/sb4-design-r3-sol/`, including `verify-wrapper.cpp` and `wrap-check.log`. This is linkage evidence, not execution of the future `sb4-core-scenarios` smoke test.
- Ran the three relevant Core receipt/cache tests (`stale_chainlocks_are_remembered_for_duplicate_suppression`, `seen_chainlock_cache_is_bounded`, `best_chainlock_is_already_have_after_seen_cache_eviction`) and `spork_tests`: **6 cases, 5,050 assertions passed**. The existing binary is in the `pr7841` worktree (HEAD `ecae783f2d8630124638f7c2880f7dc58f1993b6`); its relevant handler/ChainLock/spork implementation and test sources match the pin. It is supplemental smoke evidence from a newer build, not a rebuilt pinned SB4 harness. An initial combined filter selected no cases; the corrected colon-separated filter produced the passing run.
- Supplemental specification witnesses retain the fresh-enabled first lock, accept the lower-height lock after the initial reset, admit the disabled-floor example, and cover twelve exact-guard combinations. They do not execute SB4. A-33–A-35 and SB5's reopen/power-loss work remain implementation gates, not demonstrated implementation results.
- The memory figures are now explicitly estimates (`:163`, `:176`, `:186`, `:1077`). No new size claim is made here.
- Tests used a private mode-700 TMPDIR under `/dev/shm`, removed after completion. No live chain instance, private input, reviewed source/design edit, GitHub message, commit or push was involved. This report is left uncommitted.

## Verdict

**APPROVE-WITH-CHANGES. Sol gate satisfied: yes.** R2-M1, R2-M2, R2-m1 and R2-m2 are closed. Correct the one citation nit and advance to the sequential Opus review; no further Sol confirmation is required for that edit.
