# DW E0-04.P2b — Sol review, round 1

Two major findings are independently reproduced. R1-01 loses proved funding evidence and permits a false NotSent from Repair after row loss; R1-02 reports a resumed funded Mode B flow as Cancelled. No additional blocker was found in call-permit/signature counting, coin-fence release or DEC-179 owner isolation. BL-76 passed independently at the exact head.

## Scope, authority and method

Target: `dw/e0-04-p2b-mode-b`, `9c9ed46ef38dd605de34c18bb144dd2c6326699a`, worktree `~/workspace/dw-wt/e0-04-p2b`, based on main `d50de65`. Main's later `85ab16e` is outside this review; the merge-time rebase requires its own validation.

Authorities: REVIEW-COMMON.md; approved E0-04 grants/leases design (DEC-73), especially §2a/§12; m4 contract; DEC-154/160/163/171/173/179; the five P2a Sol rounds and Opus-high review; memory dw-e0-04-design and dw-e0-04-p2a-impl. PLAN §7 used codenames only; no ~/private/ reads. Pinned platform-wallet source was checked at `bc41f1b` for track-before-broadcast and raw broadcast semantics.

The branch was clean at the start and remains untouched. No branch edit, commit, push, GitHub comment, or testnet transaction. Adversarial test additions exist only in a copied crate under `/work/scratch/p2b-r1-sol/desktop/rust/crates/dw-engine`; they exercise the unchanged production logic at this head, with its compiled dependency artifacts. A separate rustc test build writes only to scratch, without sharing a CARGO_TARGET_DIR between worktrees.

All review builds/tests used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and `TMPDIR=/work/scratch/p2b-r1-sol/tmp` (0700). The task's disk scratch instruction overrides the older /dev/shm advice in REVIEW-COMMON. Review logs/probe sources are retained under `/work/scratch/p2b-r1-sol`; no owned dashd remains running.

## Findings

### R1-01 — major: proved funding Sent is not journaled, and row loss lets Repair falsely declare it NotSent

Location: `rust/crates/dw-engine/src/platform/lease/mode_b.rs:637` (`rederive`, especially its event-only resolution at 660–667); `:370` (`derive_funding`); `repair.rs:119` (no recovery-completeness gate).

A funding call releases a signature and fails, leaving its durable funding marker. The lock subsequently reaches ChainLocked: the snapshot derives Sent and announces DispatchResolved(Sent), but never writes a DEC-163 Sent bundle for the synthetic funding-step artifact. `note_seen` receives the lock's real txid, not this synthetic id, so BL-76 does not preserve that step evidence either. After wallet row loss/restore with dispatch.sqlite intact, the step is only Marked and becomes MaybeSent. Repair admits it with no pending rows, records a self-spend of the remaining coins, and once that unrelated spend is ChainLocked returns NotSent for a funding that already confirmed. A host/future discard-and-register-again consumer can then discard/re-fund an already-paid operation. This violates H15's durable definite resolutions and the no-double-pay invariant.

The design §2a.6 already acknowledges the restored-wallet Repair residual as **not fail closed**, assigning the scan guard to DP1-05. The current public repair method has no such guard, and the earlier positive Sent is also being lost; documenting that handoff is insufficient for this branch's money-safety gate.

Independent proof: `review_sent_sighting_must_survive_row_loss_before_repair` added only to a copied crate under `/work/scratch/p2b-r1-sol/desktop/rust/crates/dw-engine`. Exact head production logic printed `Some(Sent)` before loss, `Some(MaybeSent)` after reload, Repair admission `Ok([])`, and `Some(NotSent)` after the self-spend. The invariant assertion failed. Probe source: `/work/scratch/p2b-r1-sol/repair-row-loss-probe.rs`; output: `/work/scratch/p2b-r1-sol/adversarial-probes-exact.log`. The fake source supplies the explicitly documented row-loss state; it does not pretend to execute a chain transaction.

Suggested fix: preserve observed proved funding Sent with the full funding marker in a DEC-163 journal operation outside J, retaining an owed/ambiguous obligation on write error, cancellation or panic. Proved Sent must not regress when tracked rows disappear, including across restart. Separately, make Repair fail closed for a restored/incompletely-scanned wallet when there is no durable original-funding proof: require positive recovery-completeness evidence or a proven conflicting input. Until DP1-05 supplies that evidence, refuse that Repair case. Add both successful-write/row-loss and failed-or-panicking-write/reopen regressions.

### R1-02 — major: Lock reports a resumed, already-funded Mode B flow as Cancelled

Location: `rust/crates/dw-engine/src/platform/lease/lock.rs:346`–`:358`; `table.rs:922`–`:929`.

A resumed flow binds its registration funding step through `Lease::bind_funding_step`; the tracked lock is ChainLocked, `funding_status` is Sent and `LeaseView.funds_committed` is true. Its new lease has no calls_done or Mode A artifact history. `Freeze::finish` considers history, calls_done and running calls, but never the bound funding_steps, so its empty artifact list defaults to Cancelled. The same omission leaves a previously failed funding call MaybeSent even after the row proves Sent, and reports Cancelled for a resumed WillBeSent/MaybeSent funding with no local call history. This contradicts §8.4's history-derived outcomes and §12's already-funded registration Lock acceptance; a host receives cancellation for money already committed.

Independent proof: `review_resumed_funded_flow_must_not_report_cancelled` in the scratch copied crate asserts both Sent status and funds_committed=true, then locks. Actual report: `Cancelled, artifacts []`; required Sent assertion failed. Probe source: `/work/scratch/p2b-r1-sol/lock-funded-history-probe.rs`; same exact-filter output log as R1-01.

Suggested fix: include each bound funding step's current history-derived status in FlowReport artifacts/outcome, using its public step id. Map Sent/WillBeSent/MaybeSent/NotSent consistently, retaining the conservative running-call handling. Associate finished funding calls with their step ids so current definite evidence can supersede their stale raw call outcome. Test fresh resumed leases for each funding status, and a failed local call later resolved by network evidence.

### R1-N1 — minor: BL-76's regtest asserts load-sensitive wall time

Location: `rust/crates/dw-engine/tests/e0_04_bl76_regtest.rs:368`, `:376`, `:398`. RPC mining plus SPV block recognition can legitimately outrun the 10 s First permit at high load, and an overdue timer can be polled beyond the fixed 15 s wall-clock ceiling. The test treats those schedules as failures even when the product safely returns BroadcastUnknown and later resolves Sent. The independent isolated regtest passed at exact head. Suggested fix: retain H=10 s in production and deterministic deadline tests; make the real-network test accept Sent or BroadcastUnknown for its first phase, then require eventual positive-evidence resolution exactly once, with load-tolerant eventual waits. This is a test robustness issue, not a reason to expand spending authority.

## Independent validation

- `cargo test -p dw-engine --lib platform::lease -- --test-threads=8`: **171/0**, including the kill matrix, journal failure/cancellation/panic cases, call/signature lifecycle, funding/recovery and release timing, baseline three-seed invariant stress and all built-in stress mutations; 471.06 s.
- `cargo test -p dw-engine --lib send:: -- --test-threads=8`: **40/0**, including coin-fence selection-through-signing, dust locking, held funding coins, owner-specific cleanup, TxDraft exclusions and Repair draft cases.
- Exact `dw-engine` test binary, `coinjoin:: --test-threads=8`: **3/0** (see `/work/scratch/p2b-r1-sol/coinjoin-tests.log`).
- `cargo test -p dw-appdb --lib dispatch -- --test-threads=8`: **20/0**, including schema migration, FULL settings, append-only marker/resolution behavior, Sent bundles, funding/self-spend rows and SQL compaction.
- `cargo test -p dw-engine --test m4_dashpay_contract`: **9/0**. `cargo fmt --all -- --check`: **passed**.
- Four design checks: `e0_04_design_model.py` and `e0_04_dispatch_model.py` print **ok**; split/mutation probes (with dispatch model path argument) reproduce the unsafe variants and leave CAS/baseline clear. The source models remain abstractions; neither covers the two reproduced integration gaps.
- `DW_LEASE_MUTATION=CallBeforeMarker` with `platform::lease::call_tests::a_call_steps_marker_is_durable_before_its_permit_returns`: **caught as intended**, exit 101, `no permit while the marker's write runs`.
- Scratch invariant probes: **0 passed, 2 failed**, demonstrating R1-01/R1-02, exit 101. Reproduce with `/work/scratch/p2b-r1-sol/engine-review-tests platform::lease::mode_b_tests::review_ --nocapture --test-threads=1` under the review env/nice. Log: `/work/scratch/p2b-r1-sol/adversarial-probes-exact.log`. Compilation command: `/work/scratch/p2b-r1-sol/probe-compile-command.json`; compile exit 0.
- `DW_LEASE_STRESS_SEEDS=10`, exact `platform::lease::stress_tests::the_stress_upholds_the_invariants`: **passed all 10 seeds**, 317.77s. Log: `/work/scratch/p2b-r1-sol/stress-10-seeds.log`.
- Full workspace 951/0 is implementer evidence, not independently re-run here; no all-features or testnet claim is added.

## BL-76 classification and recommendation

`DASHD_PATH=$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd cargo test -p dw-engine --test e0_04_bl76_regtest -- --test-threads=1 --nocapture` passed at **9c9ed46: 1/0, 19.26 s**, under the task env/nice constraints. Load had fallen to roughly 65 (from 110 earlier in this review). The fixture used an owned private on-disk datadir and dynamic ports, and stopped its dashd. Log: `/work/scratch/p2b-r1-sol/bl76-regtest.log`.

This supports a test timing classification for the reported high-load failure. It does not prove a universal performance guarantee under load 150–250. The approved H=10 s deadline safely turns an unfinished leased First into BroadcastUnknown/MaybeSent; a later positive network sighting resolves it. A result of MaybeSent under load is allowed product behavior. The first phase assumes RPC mining and SPV recognition finish before H, and the second phase bounds runnable wall time by 15 s. Keep the production deadline. Make this network-evidence test load tolerant as R1-N1 suggests, while deterministic deadline tests continue to prove the authority boundary. Changing H would be a separate design decision, not a CI-flake fix.

## Focus-area disposition


- **H13 / call markers:** permit admission, quote charge and Committing ownership are one J step; durable writes are spawned outside J and awaited before returning a step permit. The normal kill matrix, concurrent-call and journal-fault/cancellation regressions passed. SignatureCount uses SeqCst ordering: begin counts an in-progress signature before querying closed/the vault, close publishes closed before reading signing/released, and kept signer clones refuse after close. The dropped-permit/in-signature regression passed. No additional authority defect found here.
- **H15 / funding gates:** fresh tracked rows and the draft's own persisted key are read before funding admission. Unknown rows, standing markers and markerless registrations with unmatched unattributed recovered locks fail closed. The top-up funding refusal is appropriate until top-ups have an independent recovery key. The remaining gaps are R1-01 (durable proved history / Repair) and R1-02 (Lock aggregation).
- **Leased broadcast deadline:** the TxDraft hand-off deadline and real SPV timeout regressions passed, including refusing entry after an already-expired First and settling a hanging hand-off as MaybeSent. H=10 s bounds the hand-off/drain, not proof delivery; Mode B permits signed bytes to emerge after Lock, while the vault gate prevents new signatures.
- **H6.2 / catch-up:** start, reconnect, journal recovery and wallet reopen trigger spawned resume passes. A single coordinator per wallet coalesces triggers, resumes rows concurrently, and ignores completions from a retired pass. No coin fence is acquired in the library resume path. Returned-error resumes stopping the funding block are a documented pin-semantic residual; the pinned library re-reserves its tracked inputs.
- **Coin fence:** verified acquisition before wallet state/manager guards on normal coin reads, manual lock/unlock, TxDraft prepare (plain/leased/mixed), CoinJoin funding and mixed-coin chunks. Prepare reserves before releasing the fence. A funding call transfers the same guard from its policy check into its permit; note_tracked releases it outside J after a signed, non-foreign row matches the key, and call end/refusal also releases it. Library tracking installs its row before queueing the changeset, so a newly awakened reader sees lock_input reservations. Status refresh only tries the guard; vault lock and unload take none. The real session tests exercised callbacks while funding held a fence and release before the proof wait, and passed.
- **Boundaries of that statement:** the design deliberately releases CoinJoin denomination-session reads before reserve/sign, and PSBT signing has no fence/reservation-policy read. They are not covered by a universal claim that every signing path is fenced. The fence has no wait deadline and a caller holding it can stall coin operations; close can also wait on an admitted operation waiting for that guard. No lock-order cycle between the examined production library callbacks, J, vault lock and manager was found.
- **DEC-179:** the production release API has no bare-outpoint removal. release/release_some remove only the named owner. snapshot includes an outpoint while any owner remains. PreparedTx, CoinJoin funding and sweep chunks use Tx(txid); mixing gets a fresh token per session, and held inputs use (outpoint, owner), so two sessions' expiry is isolated. Abandon-from-history releases each abandoned txid, rather than the union of its input coins. retain_unspent (observed spent) and wallet unload are the intentional all-owner removal cases. The shared-owner regression passed. This closes the cross-owner cleanup class; it does not make an erroneous release of an operation's own possibly-sent transaction safe.
- **Funding cap / dust:** dust, user locks and reservations remain counted in held_funding_coins and are distinguishable, as ruled. The upstream excluded-outpoints request belongs to platform-wallet's asset-lock builder. The documented received-coin race between fee_bound_worst and selection remains: the actual fee can exceed the snapshot quote when enough new inputs arrive. A conservative 500-input bound would remove that race at the cost of a larger quote; the present snapshot must not be described as an unconditional exact outflow cap.
- **Phase boundaries:** most Platform writes remain stubs on this head; P2b supplies permits/signers/gates for DP1-02 and later consumers. No testnet registration, credit-task end-to-end result, or Mode A hand-off guarantee is claimed. The actor/generation ownership rulings remain DP1-02's work.

## Remaining boundaries and follow-ups

The design's restored-wallet Repair caveat is not safe to leave as an unenforced downstream assumption: R1-01 requires an actual fail-closed gate. Keeping already-observed Sent durable also removes the demonstrated loss of positive evidence.

One inherited issue remains outside the newly introduced cross-owner reservation class: `coinjoin.rs:2346` releases the current mixed-coin sweep chunk for every raw broadcast error, including TransactionBroadcastUnconfirmed; the pinned plain broadcast API supplies no in-broadcast reservation pin there. The same premature release existed on d50de65. Keep that chunk's owner claim on ambiguity, release only definitely unsent later chunks, and cover it in the raw-send/TxDraft consumer migration. This report does not credit DEC-179 with fixing ambiguous transport settlement.

Approval requires closure of R1-01 and R1-02 and regression coverage. BL-76's isolated pass resolves the presently reported numerical gate failure, but a green baseline cannot override the two reproduced production-logic failures.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
