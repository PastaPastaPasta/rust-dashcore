# DW E0-04.P2b — Opus high closure review (before merge)

Exact head `a2d47e9` (`dw/e0-04-p2b-mode-b`): code head `c66d661`, plus one docs commit for DEC-198. I reviewed it in
the detached worktree `/work/scratch/p2b-opus-closure/wt` against my CHANGES-REQUIRED review on `52215e3`
(`DW-E0-04-P2b-opus-high.md`).
- **Scope:** close O-1, O-2, O-3 and O-10, and check that nothing since `52215e3` breaks the invariants that review
  checked.
- **Rulings taken as given:** DEC-194 (PA-2 relaxed to the release-row authority), r7 item 1 (the lockless-store
  stop narrowed to lock removals), the O-2 known limits (history resend, a PSBT signed by another instance) and
  DEC-198 (Repair adoption deferred).
- **Reading:** the diff `52215e3..a2d47e9` covers 23 files, about 2,500 production lines, mostly r5–r7. I read the
  call/signer/release path, the record boundary, status derivation, coin holds, PSBT and mixing holds, the tap and
  the resume deferral myself.
- **Hygiene:** nothing in the implementer's worktree or branch was touched, nothing was committed or pushed, and
  this report is uncommitted.

## Verdict in one paragraph

All four items are **closed**. Each fix sits where I asked for it, and each has a regression that a mutation of its
guard fails at the intended assertion. The invariants hold at this head:
- I found no reachable `NotSent` for a step whose funding could still reach the chain.
- Repair's way out is kept: signed-lock inputs are held, and Repair alone may take them.
- An unread journal fails closed in every reporting API I checked.

The new release-row routes (C4 tombstone, C5 no-release) both rest on facts the code now enforces: the release row
is written before any vault signature, and one lock per call (`admits` plus `signed_only`). They do not rest on
prose.

I found one minor gap and three nits, none of them a safety hole:
- **C-1:** O-2's holds come from the journal, so while it is unread nothing is held.
- **C-2:** the signer's post-release `closed()` re-check has no killing test.
- **C-3:** the `recorded_all` guard is equivalent in every reachable state, and its comment overclaims.
- **C-4:** a permit that ends during its first release write can read `NotSent` for that write, then `MaybeSent`.

**Verdict: APPROVE-WITH-NITS.** C-1 should get a §2a.6 residual line before merge, or with DP1-02. The rest can be
follow-ups.

## Closure table

| Item | Status | Code | Regression test(s) | Mutation → result |
|---|---|---|---|---|
| **O-1** one lock per call | **Closed** | `SignatureCount::admits` (`call.rs:327`) refuses a new sighash once a lock of the call is recorded; the signer checks it first (`signers.rs:273`). `record_signed_locks` sets `note_recorded` and then requires `signed_only` (`fence.rs:3636-3637`): exactly the lock's sighashes, nothing still signing. This catches two locks signed before the first record, R6-02, and stops before the send. `begin` (SeqCst `signing` increment, then `recorded` load) against the tap (`recorded` store, then `signing` load) is Dekker-ordered, so one side always sees the other. | `mode_b_tests::a_call_signs_no_second_lock_once_its_lock_is_recorded` (2355), `session_tests::mode_b_a_funding_signer_signs_one_lock_per_call` (1337, the real signer), `session_tests::mode_b_a_second_lock_under_one_permit_stops_at_the_first_record` (2860) | **A** `admits` → `true`: both first tests fail (`mode_b_tests.rs:2370` "a second lock"; `session_tests.rs:1380`). **B** drop the `signed_only` check: `…stops_at_the_first_record` fails ("the first record stops the process: ()"). |
| **O-2** signed-lock inputs freed once the row is gone | **Closed** (within the ruled limits) | `LeaseTable::unresolved_lock_inputs` (`mode_b.rs:1378`) lists the inputs of every signed lock of a step that reads neither `Sent` nor `NotSent`. `coin_snapshot` reads them as `reserved` plus `lock_input` (`coins.rs:614-624`), so every engine build refuses them and only Repair's `self_spend_selectable` takes them. PSBT sign and broadcast (`psbt.rs:452`, `:533`, under the owning wallets' fences) and the mixing session's final signing (`coinjoin.rs:2594`, under the fence) refuse them too. `held_funding_coins` names them `UnresolvedLock` for DP1-02's policy check. | `session_tests::mode_b_a_signed_locks_inputs_are_held_until_repair_resolves_its_step` (1815, my probe inverted), plus `…psbt_spending_an_unresolved_locks_input_is_refused`, `mode_b_coinjoin_funding_skips_…`, `mode_b_a_mixed_sweep_skips_…`, `mode_b_a_mixing_session_does_not_sign_…` | **C** coin snapshot ignores the set: the payment estimates Ok (`session_tests.rs:486`). **D** hold resolved steps too: fails at `:1842` (still held after Repair). **Q** drop the Repair exemption (`lock_input` without `unresolved_lock`): Repair refused `OutpointUnavailable` (`:327`). **E** drop `sign_psbt`'s check: the PSBT signs (`:3012`). Sol r8 killed the broadcast and mixing variants. |
| **O-3** unread journal reads `None` / `funds_committed` false | **Closed** | `derive_step` maps `None`/`NotSent` to `MaybeSent` while `journal != Ready` (`mode_b.rs:710`). Every reader goes through it: `funding_status`, `funding_outcome`, `funds_committed`, the gate and Repair. `registration_funding_steps` returns `Err(FundingStepsUnavailable)` (`:1420`). `record_release` and `record_self_spend` refuse, and `record_signed_locks` fails closed. | `mode_b_tests::a_journal_not_read_reads_a_marked_step_maybe_sent` (3224), `…the_funding_view_reads_the_journal_alone_and_refuses_a_closed_one` | **F** drop the mapping: `left: None` (`:3233`). **G** drop the view's journal check: `left: Ok([])` (`:3234`). |
| **O-10** `shows_conflict`'s empty-record guard untested | **Closed** | `!conflicts.is_empty()` (`mode_b.rs:314`); the journal now refuses such a row (O-8a, asserted in the test). | `mode_b_tests::an_empty_recorded_self_spend_without_a_signed_lock_never_reads_not_sent` (3264) | **H** guard → `true`: `left: Some(NotSent)` (`:3278`). |

## Invariants since `52215e3`

**No unsafe `NotSent`.** Here are the routes at this head (compare §1 of the earlier review).

| Route | Requires | Why it is sound now |
|---|---|---|
| N1: self-spend ChainLocked, `shows_conflict` | Unchanged; non-empty conflicts covering every signed lock | A lock the process forgot is covered by **enforced** one-lock-per-call (O-1). |
| N2: a `NotSent` row | Unchanged; zero signatures, no prior marker | — |
| C4: tombstone or resolving spend, flagged marker, `recorded_all` | `mode_b.rs:683` | A flagged marker (schema ≥ 7; 6 → 7 unflags) promises that every lock it signed was recorded before it could be stored. `signed_only` makes the recorded lock the only one. A lock sent unstored has no record, and its release row makes `recorded_all` false (but see C-3). |
| C5: a flagged `Marked` with no release row, no signed lock, no running call | `mode_b.rs:695-703` | The release row is durable before the vault is asked (`FundingRelease::record`, `signers.rs:281`). It is set in memory only in the J step that ends the landed write, and the signer re-checks `closed` after it. Mutation **J** (drop `!funding_call_of`) fails 7 tests. Sol r8 killed the `!released` removal. See C-4 for a transient window. |
| Z1 `None` | No marker loaded **and the journal Ready** (O-3) | — |

Other things I checked:
- The new `ended_funding` set keeps an ended call's sighashes recognised: a `Built` store of its lock after the
  permit fails closed (`fence.rs:3668-3678`).
- The resume pass defers a `Built` row a call of this process signed until its record is durable
  (`resume.rs` `defer_unrecorded`). This closes my old O-4(b) window.
- No `fail_closed` site can be reached with an in-memory signed lock left `durable: false` and no abort. The
  write-failure, marker-gone and journal-not-ready branches all abort.

**Repair liveness.**
- The way out from my O-2 scenario (rejected after signing, row gone, payment or CoinJoin) is kept and tested
  (mutations C, D and Q).
- NoKnownLock is still reachable after a release without a record (the r6 cost, documented), and Repair cannot help
  there. That is the honest state.
- C-1 is the one remaining way to lose the inputs from inside this instance.

**Journal fail-closed.**
- Every new write path refuses or aborts while the journal is not Ready: `record_release`, `record_signed_locks`
  and `record_self_spend`.
- Every read path reads `MaybeSent` or refuses.
- The exception is the coin holds (C-1): they fail *open*, which is liveness only.

**Lock order.**
- `broadcast_psbt` is the one multi-fence taker (ascending wallet id). It reads each wallet's `state()` before
  taking that wallet's fence, so the order is fence → state across wallets. No single-fence holder waits on
  another fence. I checked this independently of Sol r8.
- The mixing session's new fence take (`coinjoin.rs:2590`) is called from `dw-coinjoin/src/client.rs:585` with no
  lock held, so there's no cycle with a funding call waiting on wallet state.

## New findings

### C-1 (Minor): O-2's holds come from the journal, so a session whose journal is unread holds no signed lock's input

**Where:**
- `LeaseTable::unresolved_lock_inputs` (`mode_b.rs:1378-1392`) walks `fence.funding_steps_of` and `signed_locks`,
  and both load only from `dispatch.sqlite`.
- `coin_snapshot` (`coins.rs:614`) relies on it.
- Design §6.4 (`E0-04-grants-leases.md:1983-1986`): "The journal cannot be opened … Unleased paths still work."

**Scenario:**
1. A step reads `MaybeSent` with signed lock L and no row. This is my old O-2 setup, and the inputs are held.
2. At the next launch `dispatch.sqlite` can't be read: an I/O error, `NewerSchema`, or one corrupt row.
3. The step correctly reads `MaybeSent` (O-3), but `unresolved_lock_inputs` is empty. A payment, or the unattended
   CoinJoin pass or sweep, can now select L's inputs.
4. Once that transaction is ChainLocked and the journal is back, Repair answers `LockInputsSpent(L)` for good, and
   the step reads `MaybeSent` permanently. That is O-2's wedge, reached through O-3's condition.

**Evidence:** probe `opus_closure_probe_no_hold_while_the_journal_is_not_read`
(`/work/scratch/p2b-opus-closure/probes/opus_closure_probe.diff`) passes at the head. With the journal read, the
lock's input is held. With `load_journal_with(None …)` the step reads `MaybeSent` and the held set is empty.

**Severity:** Minor. Safety is intact: funding permits are refused `JournalUnavailable`, and the lock is dead once
its inputs are spent. It needs a rare journal fault plus a spend of those exact coins in that session. It is not in
the ruled known limits, which name history resend and other-instance PSBTs only.

**Fix (any one):**
- (a) Name it in the §2a.6 residuals next to the known limits. This is the minimum.
- (b) While the journal is not Ready, hold back the unattended spenders (the CoinJoin automatic pass and sweeps),
  and let the user's payment path warn.
- (c) Keep a copy of the hold set outside `dispatch.sqlite`, for example in `app.sqlite`.

(a) now and (b) as a follow-up seems proportionate.

### C-2 (Nit, test gap): the signer's re-check of `closed()` after the release write has no killing test

**Where:** `signers.rs:289`, new in r6. `FundingRelease::record().await` is a new await point between `begin()` and
the vault.

**Mutation:** **K** (drop the re-check) survives: 227/227 `platform::lease` tests pass, including
`mode_b_a_signer_that_outlives_its_permit_signs_nothing`. That test closes before `begin`, not during the write.

**Effect without it:** a permit that ends while its first release write is in flight still lets the vault sign.
- The outcome is still `MaybeSent`, because `close()` counted the in-flight signing, and the release row covers the
  step.
- A later `Built` store fails closed through `ended_funding`. So the loss is not unsafe, but it breaks M-1's "a
  signer that outlives its permit signs nothing" and turns a clean `Locked` failure into a process abort.

**Fix:** add a test with a FakeJournal release-write pause: end the permit inside the pause, release it, and assert
`SignerError::Locked` and no vault call.

### C-3 (Nit): `recorded_all` is equivalent in every reachable state, and its comment overclaims

**Where:** `mode_b.rs:672-677`, the C4 condition at `:683`. Mutation **I** (`recorded_all = true`) survives
227/227.

**Why it survives:**
- `locked` (a tombstone or a resolving self-spend) needs a recorded self-spend.
- `record_self_spend` → `stuck_funding` refuses `NoKnownLock` with no signed lock (`:1047`).
- So `released && signed.is_empty() && locked` cannot be reached.

**Why the comment overclaims:** if it were reached, falling past C4 does not give `MaybeSent`. `derive_funding`'s
self-spend arm (`:559`) still returns `NotSent` unless an unattributed row exists, because `shows_conflict` over
zero signed locks is vacuously true for any non-empty record. The comment says "neither rule below … holds", but
the guard protects only the unattributed check, not the outcome.

**Fix:** make it real or drop the claim. For example, derive `MaybeSent` whenever `info.released &&
signed.is_empty()` before either self-spend rule. That is one line, conservative, and killable with a crafted
FakeJournal row like `push_empty_self_spend`. Otherwise, reword the comment as defence in depth over an
unreachable state.

### C-4 (Nit): a permit that ends during its first release write reads `NotSent`, then `MaybeSent`

**Where:** `CallPermit::end` (`call.rs` ~`:875-905`), `record_release` (`fence.rs:3360`), C5 (`mode_b.rs:695-703`).

**Sequence:**
1. The signer is in `record().await`, so `signing` is 1.
2. The permit ends: `close()` counts the signing, so the outcome is `MaybeSent`. The call leaves `i.calls` and moves
   to `ended_funding`.
3. Until the write lands, the step is `Marked`, flagged, `!released`, has no signed lock, and no call runs, so C5
   reads `NotSent`. `rederive` can emit `DispatchResolved(NotSent)`.
4. When the write lands, `released` is set and the step reads `MaybeSent` with `NoKnownLock`.

**Why it's safe:** the signer then refuses `Locked`, so nothing was signed and acting on the `NotSent` pays no one
twice. The cost is a flapped resolution event and a step stuck `MaybeSent` with nothing signed.

**Reachability:** only when signing runs on a task that outlives the permit. A dropped library future drops the
signer too, which takes the `Cancelled` path correctly. Not probed.

**Fix:** have C5 also require that no `ended_funding` entry of the step is still signing, or count a release write
in flight as `funding_call_of`.

## Mutation table

Each mutation was applied alone in a separate detached worktree (`wtm`, own target), the named tests were run, and
the source was restored. `git status` was clean afterwards. A "killed" result is a test-assertion panic, not a
build error.

| # | Guard (file:line) | Mutation | Tests run | Result |
|---|---|---|---|---|
| A | `call.rs:327` `admits` | → `true` | `a_call_signs_no_second_lock…`, `mode_b_a_funding_signer_signs_one_lock_per_call` | **killed** (2/2 fail) |
| B | `fence.rs:3637` `signed_only` | check disabled | `mode_b_a_second_lock_under_one_permit_stops_at_the_first_record` | **killed** |
| C | `coins.rs:614` hold set | empty set | `mode_b_a_signed_locks_inputs_are_held_until_repair_resolves_its_step` | **killed** |
| D | `mode_b.rs:1382` status filter | hold resolved steps too | same | **killed** |
| Q | `coins.rs:623` Repair exemption | `lock_input` without `unresolved_lock` | same | **killed** |
| E | `psbt.rs:452` `sign_psbt` hold | check removed | `mode_b_a_psbt_spending_an_unresolved_locks_input_is_refused` | **killed** |
| F | `mode_b.rs:710` O-3 mapping | disabled | `a_journal_not_read_reads_a_marked_step_maybe_sent` | **killed** |
| G | `mode_b.rs:1420` view journal check | disabled | same | **killed** |
| H | `mode_b.rs:314` `!conflicts.is_empty()` | → `true` | `an_empty_recorded_self_spend_without_a_signed_lock_never_reads_not_sent` | **killed** |
| I | `mode_b.rs:677` `recorded_all` | → `true` | all `platform::lease::` (227) | survived (C-3: equivalent) |
| J | `mode_b.rs:699` C5 `!funding_call_of` | removed | all `platform::lease::` | **killed** (7 fail) |
| K | `signers.rs:289` post-release `closed()` | removed | all `platform::lease::` (227) | survived (C-2) |
| — | `DW_LEASE_MUTATION=CallBeforeMarker` | env mutation | `a_call_steps_marker_is_durable_before_its_permit_returns` | caught (1 failed) |

Sol r8's six mutations of the r7 range (the lockless-store trigger, PSBT hold and fence wait, mixing hold and fence
wait, the release-absence guard) are not repeated.

## Validation

Every run used `nice -n 10`, umask 022, `CARGO_BUILD_JOBS=12` and `TMPDIR=/work/scratch/p2b-opus-closure/tmp`
(0700). The target was reflink-seeded from `dw-wt/dp1-02/rust/target` (same `Cargo.lock`; the P2b target had been
swept). Load average was about 35–45.
- **Build:** `cargo test --locked -p dw-engine -p dw-appdb --no-run` gave BUILD EXIT 0.
- **Full `cargo test --locked -p dw-engine --no-fail-fast`:** **544 passed, 0 failed** (462 lib plus 16
  integration binaries); ENGINE EXIT 0. **`-p dw-appdb`:** **62/0.**
- **e0_05_bringup (BL-100):** 13/0 in the full run with no wall-clock flake, and **13/0 rerun alone** (21 s).
- **BL-76:** the full run's pass was a skip (no `DASHD_PATH`, 0.00 s). It was rerun with
  `DASHD_PATH=~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd` and **passed 1/0 (13.5 s)**.
- **Focused suites:** `platform::lease::mode_b_tests::` **76/0**; `platform::lease::session_tests::mode_b` **18/0**;
  `platform::lease::` **227/0**, re-measured as the survivor runs of I and K.
- **Stress:** `DW_LEASE_STRESS_SEEDS=10 … stress` **3/0** (580 s).
- **Design models:**
  - `e0_04_design_model.py`: **86 PASS, 0 FAIL**, exit 0.
  - `e0_04_dispatch_model.py`: exit 0.
  - `e0_04_split_model.py`: exit 0, all cells `none`.
  - `e0_04_mutations.py`: exit 0, baseline clean, each mutant flagged.
- **Probe:** C-1's probe passes, which demonstrates the gap. Its source is kept as a diff and was reverted from the
  worktree.
- **Not run:** clippy, fmt, the workspace-wide suite and Swift. The implementer's gate and Sol r8's fmt run cover
  those.

Logs are in `/work/scratch/p2b-opus-closure/` (`full-engine.log`, `full-appdb.log`, `bl76.log`, `e005.log`,
`stress.log`, `cbm.log`, `modeb.log`, `session-modeb.log`, `m-*.log`, `mut/m-*.log`, `probe.log`).
- **Mutation runner caveat:** its own verdict column mislabels killed mutants as `BUILD-ERR`. Its pattern matched
  cargo's `error: test failed` line. Every killed mutant's log shows a test-assertion panic, as cited above.
- **Cleanup:** after this report both review worktrees (`wt`, `wtm`) and both targets (`target`, `targetm`) were
  removed. Nothing was killed by pattern.

Observed in passing, not re-verified in depth: O-5/O-13b (typed `StuckFunding` codes with `retry_later`), O-7
(`uncounted_funding_signer` is `pub(crate)`), O-8a (the journal refuses empty-conflict rows), O-8c, O-9 (supersede
`debug_assert`, `fence.rs:1185`) and O-11 (`call_permit_step` is `pub(crate)`; the funding gate runs before
`StepKeyMismatch`) look addressed.

**Verdict:** APPROVE-WITH-NITS
