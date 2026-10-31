# DP1-02 Phase A — Sol narrow round 4

Reviewed `dw/dp1-02-registration` at **`8bff0328887e15b6e0d2d99d54478e918c46d624`**, narrowly for R3-01, against REVIEW-COMMON, prior Sol reports, and DEC-171/173. Accepted residuals 1–3 remain outside reopening.

Read-only reviewed branch; no commits, pushes, GitHub comments, testnet transactions, or private-path reads. PLAN §7 handling is codenames only. All test/build commands use nice 10, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and mode-0700 `/work/scratch/dp1-02-r4-sol/tmp`; the explicit task TMPDIR constraint takes precedence over REVIEW-COMMON’s older `/dev/shm` instruction.

**The original R3-01 ordinary-gate blockage is fixed. The required reservation-ownership boundary remains open: one major, R4-01.**

## R4-01 — major: stale registration cleanup erases a later send’s same-coin reservation

**Locations:** `rust/crates/dw-engine/src/coins.rs:433`; shared store: `rust/crates/dw-engine/src/send/mod.rs:344`, `send/mod.rs:362`; real send insertion: `send/mod.rs:1078`.

The reconciliation removes the untracked lock’s recorded input from `PendingSpends` unconditionally. That store is a per-wallet set, not an operation-owned reservation. Knowing that the registration used a coin does not prove that its current protection belongs only to that registration.

**Concrete offline reproduction:** pause a real ordinary `TxDraft::prepare` after its policy-aware selection, before signing. While it is paused, a registration’s real fenced Built tap reserves that same input. Let the library stop tracking the registration lock after its call returns, leaving the input unspent and the registration’s host reservation pending reconciliation. Now allow the preselected send to finish its real signing and reservation. The send has a live `PreparedTx` and a signed transaction selecting the coin. The next normal coin snapshot reconciles the old registration and removes the send’s reservation too: `raw still present=true`, `reserved=false`, `auto_selectable=true`. The probe fails only at the final required preservation assertion; selection, tracked-state insertion/removal, send signing, and send reservation assertions all passed.

This does not reopen the accepted P2b selection/fencing residual. The send signs **after** the old registration is untracked, and the finding asks only that cleanup of the old registration preserve another operation’s current reservation. The new contract caveat at `docs/contracts/m4-dashpay-engine.md:234` expands the P2b residual to a shared reservation; it does not satisfy this round’s explicit “never touch other operations’ reservations” / same-coin preservation condition. The committed preservation regression uses a **different** coin and therefore misses this alias.

**Suggested fix:** give registration reservations their own namespace or owner/token in the shared store, and remove only the untracked registration’s ownership. A coin remains protected while any other owner or still-tracked registration lock holds it. This can stay bounded to reservation ownership and policy overlays; it does not require the full P2b TxDraft migration. Add this same-coin ownership-transfer regression.

**Evidence:** `review_sol_r4_untracked_cleanup_preserves_preselected_sends_same_coin_reservation`; [isolated send probe](/work/scratch/dp1-02-r4-sol/same-coin-send-alone.log). The repeat reproduces the identical final assertion and transaction: [repeat isolated output](/work/scratch/dp1-02-r4-sol/same-coin-send-repeat.log). Both runs pass every setup assertion; this is not a wall-clock failure.

## Confirmed ordinary-gate closure

The final strengthened r3 probe, copied unchanged from the r3 scratch archive and compiled against `8bff032`, now passes: `Spendable { duffs: 100000000, inputs: 1, protected: 0 }`, ordinary `start = Ok("1")`, one durable registration row. Its former r3 output was `protected: 1`, `start = Err(CoinsLocked)`, zero rows. Evidence: [ordinary r3 gate](/work/scratch/dp1-02-r4-sol/ordinary-r3-gate.log). An additional fresh probe makes ordinary `start` the **first policy snapshot after untracking**; it also returns `Ok` and inserts exactly one row, with no explicit cleanup call beforehand.

## R2 closure spot checks

All **11/0** final r3 closure probes pass unchanged against the fixed head. R2-01’s cancelled in-flight acquisition/reopen and generation-loss stop, R2-02’s unknown funding and profile/phase/outpoint/index ambiguity parking, R2-03’s unreadable index-owner tap check, and R2-04’s reservation-to-Built race, prior-reservation refusal, no-tap fence retention, and manual-lock-after-snapshot regression remain closed. Evidence: [r3 closure probes](/work/scratch/dp1-02-r4-sol/r3-closure-probes.log). The existing registration baseline is **48/0**: `registration_tests` **40/0**, including both committed r4 regressions, plus registration flow/row support tests **8/0**. Logs: [registration tests](/work/scratch/dp1-02-r4-sol/registration-baseline.log), [flow and row support](/work/scratch/dp1-02-r4-sol/registration-support-baseline.log).

## Fresh bypass checks

The six remaining fresh probes all pass (**6/0**): [fresh bypass output](/work/scratch/dp1-02-r4-sol/fresh-bypasses.log).

- **Still tracked:** Built and Broadcast both retain the input reservation across policy snapshots. A new fenced funding refuses before its callback, and CoinJoin’s real `reserve_spends` refuses that same coin.
- **Another operation on the freed coin:** after reconciliation frees the registration’s reservation, CoinJoin’s reservation of that same coin succeeds and survives subsequent snapshots. The overlapping preselected-send case is the separate failed required check, R4-01.
- **Snapshot queued before the Built tap:** it waits for the funding fence; the tap records both reservation structures before releasing the fence; the waiting snapshot sees the new tracked lock and preserves its input.
- **New funding queued behind reconciliation:** hold the real wallet state write guard so reconciliation owns the coin fence while waiting to read the tracked set. Queue a fresh funding. It cannot enter until reconciliation completes; its new Built reservation remains protected afterward.
- **Untracking while the funding holds the fence:** remove the tracked entry on another runtime task before a deliberately delayed Built tap. The waiting snapshot stays fenced, then removes the now-untracked reservation after the tap releases the fence; the next ordinary start succeeds.
- **Ordinary start as first snapshot:** remove the old tracked entry while its host reservation remains, then call ordinary `start` directly. The gate itself clears the stale reservation and inserts the row.

## Validation and limits

- Exact `git archive 8bff032` source, compiled independently with the captured Cargo rustc invocation; current dependency artifacts are read-only inputs. Compiler outputs and incremental state are scratch-owned; no shared `CARGO_TARGET_DIR`. Both builds succeeded without diagnostics.
- Original r3 ordinary-gate probe **1/0**; baseline **48/0**; unchanged r3 closure probes **11/0**; fresh gate/race probes **6/0**; same-coin send preservation **0/1**, reproduced in two separate isolated runs at the same final assertion. The supplied whole gate **906/0** was not rerun in full.
- Tracked-lock insertion/removal is simulated in the real wallet map, as in the accepted r3 lifecycle probe. Send selection, signing, prepared handle, host reservation, coin snapshots, coin fence, facade, actor and SQL are real. The acquisition/send pause hooks only place interleavings. This proves the cleanup/ownership boundary offline; it does not exercise a network’s rejection response. Nothing was broadcast.
- [Review-only probe patch](/work/scratch/dp1-02-r4-sol/review-probes.patch) applies cleanly to the reviewed head. Source, compiler recipe, both test binaries, patch and logs remain in `/work/scratch/dp1-02-r4-sol/`. Temporary test data and incremental cache were cleaned after all test processes exited.
- Reviewed branch is unchanged and clean at `8bff032`; branch diff whitespace checks pass. This report remains uncommitted. Accepted residuals 1–3 are not reopened.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
