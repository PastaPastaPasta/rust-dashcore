# DP1-02 Phase A — Sol narrow round 5 after rebase

Reviewed `dw/dp1-02-registration` at **262b90c7c87512b47b66bc769f57f620b90f7cd9**, rebased onto E0-04.P2b **9c9ed46**, against `backup/prerebase-5acc526`, Sol rounds 1–4, REVIEW-COMMON and DEC-171/173/179. Scope is Phase A: R4-01, derived-reservation edges, the five named rebase resolutions, R2-01..04, L1–L4 and dependencies on P2b's two major findings.

**R4-01 is closed. No new DP1-02 blocker or major. One non-blocking test-coverage nit remains.**

Read-only reviewed branch; no commits, pushes, GitHub comments, testnet transactions or private-path reads. PLAN §7 uses codenames only. Tests/builds used nice 10, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` and mode-0700 `/work/scratch/dp1-02-r5-sol/tmp`. The task's disk TMPDIR instruction overrides REVIEW-COMMON's older `/dev/shm` advice. Review-only changes live in an exact `git archive 262b90c` under scratch.

## R4-01 — closed by derived protection and owner isolation

**Mechanism:** `rust/crates/dw-engine/src/coins.rs:538` reads UTXOs and tracked-lock inputs from one wallet state. `pending_lock_inputs` (`:397`) derives protection for Built/Broadcast inputs. `PendingSpends` retains only operation-owned Tx/Mixing claims; DP1 adds no stored registration claim and has no stale-registration cleanup capable of deleting another owner. `lock_input` is true only when no engine owner claims the coin. Consequently, a Built input alone is exempt from the library funding refusal, but an overlapping send/Mixing claim makes it `CoinHold::Reserved` and refuses funding (`coins.rs:191`). Removing library tracking cannot release that claim.

**Adapted original same-coin probe:** pause the real `TxDraft::prepare` after selection, before signing. The new registration funding attempt queues behind P2b's send fence and never enters its callback: after the send reserves its input, it returns `CoinsLocked`. The old round-4 concurrent registration build is therefore excluded at this head.

To exercise the cleanup/alias boundary independently of that exclusion, the same probe inserts an overlapping library tracked lock while the send is paused, removes it before signing, and adds a Mixing owner on both the same coin and a different coin. Real send signing produces a live PreparedTx and its Tx owner. Subsequent coin snapshots preserve both owners. Releasing Mixing preserves the send; dropping the send then frees the coin. Final output: **`prepared=true, reserved=true, selectable=false`**. The tracking insertion/removal is explicit state injection, not a claim that an ordinary fenced registration can still overtake this send.

Evidence: `review_sol_r5_same_coin_preselected_send_preserves_claim_and_fences_new_funding`, [fresh probes](/work/scratch/dp1-02-r5-sol/fresh-probes.log). Committed same/different-coin Mixing and PreparedTx regressions also pass in the 41-test baseline.

## Derived-reservation lifecycle

- **Built and Broadcast:** both protect inputs from automatic and chosen engine selection, including real TxDraft preparation. A tracked-only input creates no stored claim and no held-funding refusal. Adding another owner overrides that exemption and refuses the funding callback. A separate real offline asset-lock build proves that the pinned library's own reservation prevents its immediate second build from selecting the same input. Thus exemption from host refusal is not permission to spend that pending input again.
- **Untracked:** with no other owner, the next snapshot frees the input directly; no reconciliation callback or cleanup pass is needed. The committed ordinary-start R3-01 regression passes. Other owners survive untracking, as the adapted same-coin probe demonstrates.
- **Spent / input restored:** an absent UTXO is not selectable. Reinsert the source UTXO while the lock remains Broadcast and it immediately reads reserved again, even after a previous snapshot observed no coin. Removing tracking then frees it. This tests the real coin view with explicit wallet-state mutations, not an actual chain reorg.
- **Untracking mid-call:** a reader queued while `PwLib::fenced` holds the fence cannot cross it. Remove tracking on another task, then deliver a delayed Built tap: the reader proceeds only after release and sees no stale hold. Ordinary start inserts exactly one row. The opposite order is covered too: a real send queued behind registration reads the new Built input and refuses preparation.
- **Reorg / unconfirm:** there is no ordinary block-confirmed asset-lock status between Broadcast and authenticated proof. A block sighting without proof stays pending; unconfirmation of that transaction does not remove the Built/Broadcast overlay. If its input reappears, the preceding restored-input probe applies. InstantSendLocked, ChainLocked, RecoveredFromChain and Consumed are outside this pending subset; they assert proof/finality or consumption, not a reversible ordinary confirmation. No full SPV/block-disconnection test was run here. Pinned recovery code's separate stale confirmed-conflict limitations are upstream, not introduced by this DP1 delta.
- **Library restart loses tracking, app row persists:** the fresh probe closes/reopens the real manager and stores after recording a funded app row, with the injected library tracking deliberately not persisted. It explicitly restores the source coin after reopen; this is not a pretend scan. The outpoint survives, tracking is empty, and the coin has no derived reservation. Nevertheless, row status remains `funds_committed=true`, discard is refused, ordinary start returns InProgress, and driving the surviving row creates zero replacement locks/identities.

**Boundary:** an app row alone cannot reconstruct which source inputs to exclude for an unrelated send after library tracking is lost. That coin can be selected again by an engine send; the surviving funded row prevents DP1 from registering again. This review does **not** certify global no-reselection after loss of library tracking. Durable dispatch/recovery is the already-deferred Phase B/P2b/DP1-05 work. Similarly, a still-tracked Built/Broadcast lock can protect an input indefinitely while it may be resent; that is conservative retention. There is no additional registration-owned reservation left to leak after spending or untracking.

Evidence: the seven remaining `review_sol_r5_*` probes in [fresh output](/work/scratch/dp1-02-r5-sol/fresh-probes.log), all passing.

## Rebase resolutions and R2 mechanisms

Full pre/post-rebase diffs for the five files and a full-range comparison are retained in scratch. The appdb registration implementation and registration_rows encodings are byte-for-byte unchanged from the pre-rebase head.

- **session.rs:** P2b's journal loading, tracked source/resumer, lock watching, initial refresh and SPV start/stop hooks survive. DP1's funded-backup callback/actor wiring remains (`:446`), as does aborting keyless registration tasks before close (`:683`).
- **journal.rs:** DP1 adds registration classification and the owned actor association (`:268`, `:478`). P2b's `note_tracked`, refresh and core-change hooks remain. There is no replacement of the dispatch observer with the DP1 tap.
- **coins.rs:** the DP1 delta merely exposes P2b's fenced snapshot/read helper and a test counter; P2b's one-state pending-input overlay, owner precedence, manual/dust locks and spent-claim pruning remain unchanged. All registration cleanup is gone.
- **names.rs:** P2b's call-lease/permit-counted name signer migration survives. DP1 adds validation visibility and its name-credit quote helper; the base's write behavior is preserved.
- **registration_flow.rs:** changes from the pre-rebase head remove the reservation callback, use P2b's fence API and replace obsolete call_running with plain call permits. Actor acquisition, persisted generations, generation-loss stop, ambiguity parking and owned tap association are retained.

**R2-01:** cancelled acquisition cannot overtake unload/reopen; a driver losing its persisted generation stops before funding (`registration_flow.rs:545`, `:1547`, `:1773`). Both independent replay probes pass.

**R2-02:** unknown funding plus reconstructed index 0 remains parked; newer profile, phase, malformed outpoint and missing-index variants each remain unchanged, refuse discard and retain possible-funding evidence. Independent replay passes.

**R2-03:** the tap's row selection and owned write remain one actor job (`registration_flow.rs:1189`). The unreadable older index owner receives no donated lock, while the runnable row takes its distinct next index. Independent replay and the committed tap/index-reassignment test pass.

**R2-04:** policy is checked under the same fence held through library selection/signing (`registration_flow.rs:2364`). The old manual-lock-after-snapshot probe signs no input and reports CoinsLocked. No-tap failure retains the fence until return. Both new send-versus-registration orderings pass. The deleted reserve_spends helper has not reopened this gap: real TxDraft now fences selection through signing/reservation.

Evidence: [nine independent closure replays](/work/scratch/dp1-02-r5-sol/r3-closure-replay.log), [registration baseline](/work/scratch/dp1-02-r5-sol/registration-baseline-corrected.log), [journal tests](/work/scratch/dp1-02-r5-sol/journal-tests.log).

## L1–L4 and P2b dependencies

**L1 — accepted Phase A boundary, with more precise wording.** CallPermit owns a SignatureCount, but DP1's funding/identity calls use lease signers, so their permit count is unconnected; they also use no durable Mode B funding marker (`registration_flow.rs:1884`, `:2038`, `:2462`; `lease/call.rs:616`). Names already use P2b's counted permit signer. Phase B must connect the funding/identity signers, charging and durable markers. This is not certification of the full Mode B outcome contract.

**L2 — safe at this pin, conservatively retained.** Built is tracked in memory before the synchronous store/tap path and before broadcast. The full-tap probe for Built **index 0** releases the fence and writes the claimed row before the funding callback ends. A second full-tap Broadcast probe confirms that DP1 holds the fence until call return. A future buffering/coalescing persister could delay coin-control actions for the proof wait if it skips Built; Phase B should use P2b's any-status release. At this pin, no early-release or double-selection hole is shown.

**L3 — accepted transitional duplication.** Phase A's plain permit does not take P2b's FundingFence, so the two holder implementations do not recursively acquire this call's mutex. Phase B must transfer the already-held fence into the funding permit and remove the local holder; acquiring both would deadlock.

**L4 — independent mechanism proved; commit the regression (N1 below).** The new probe uses the real session tap and routes classify → record → lock_tracked → release_built. A queued reader must complete while the funding callback is still running, reading derived protection and no stored claim. It also checks the persisted claim owner and FundingSent association. This passes; direct calls to lock_tracked cannot substitute for that whole path.

**P2b R1-01 (proved-Sent journaling / restored-wallet Repair):** Phase A's `funding_status` (`registration_flow.rs:1102`) derives from app rows and tracked locks, not `lease/mode_b.rs:637`'s event-only Sent resolution. DP1 does not invoke Repair or treat its result as permission to discard/refund. The real restart probe keeps the funded row conservative despite losing tracking. Phase B's planned journal integration must wait for P2b's fix and a recovery-completeness guard.

**P2b R1-02 (Lock report):** this head does not bind a Mode B funding step. Public Lock reporting remains P2b-owned and is not a reliable replacement for RegistrationStatus.funds_committed. DP1's plain failed call can also report Cancelled because its signatures are uncounted (L1); a resumed funded row may have no call history. The row's discard/funding gates do not consume those reports. Phase B must bind funding history and use the corrected report path; do not infer NotSent from this Phase A report.

## N1 — nit: commit the whole tap/fence regression

**Location:** `rust/crates/dw-engine/src/platform/journal_tests.rs:662`; direct fence tests: `registration_tests.rs:2147`; seam: `registration_flow.rs:1189`.

Current committed tests separately cover tap association and direct lock_tracked fence release. They would miss a classification/wiring change that records a Built row but fails to release the held fence; coin-control actions would then wait through the library proof wait. The independent whole-path Built-index-0 and Broadcast-control probes pass at this head. Suggested fix: promote these bounded offline probes into a committed regression. This is coverage hardening, not a demonstrated production defect or merge blocker.

## Validation and limits

- Independent exact-archive closure replay: **9/0**.
- Independent fresh lifecycle/ownership/full-tap probes: **8/0**.
- Corrected registration baseline using the existing worktree test artifact: **41/0**, including the committed R3/R4 regressions.
- Independent exact-archive flow/row support: **4/0 + 4/0**; journal tests: **22/0**.
- Two independent rustc test builds succeeded without diagnostics. The captured Cargo compiler recipe uses current dependency artifacts as read-only inputs; output and incremental state are scratch-owned. No shared CARGO_TARGET_DIR was introduced.
- `cargo fmt --all -- --check`, branch diff whitespace checks and `git apply --check` of the review-only patch pass. The implementer's **1007/0** whole gate was not rerun in full; no testnet or real chain transactions were executed.
- Initial supplemental tests failed fixture admission because the scratch root was mode 0775 (TMPDIR itself was 0700). After correcting both root and TMPDIR to 0700, the complete fresh baseline passes. The [isolated setup diagnostic](/work/scratch/dp1-02-r5-sol/bootstrap-diagnostic.log) and initial log are retained; those failures never reached registration logic.
- [Review-only patch](/work/scratch/dp1-02-r5-sol/review-probes.patch), source archive, both binaries, compiler recipe, full rebase comparisons and logs remain under `/work/scratch/dp1-02-r5-sol/`. Temporary test data and incremental cache are removed after all owned build/test processes exit.
- Reviewed HEAD remains 262b90c and the target worktree is clean. This report is uncommitted. The accepted Phase A limits above are not reopened as DP1 blockers.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes
