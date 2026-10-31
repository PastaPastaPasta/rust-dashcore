# MG-04b slice 1b, review r1 (Opus-h: engine correctness, funds safety)

Branch `dw/mg-04b-1b` at 8dc2809a (7 signed commits on desktop main 87eec7a2). Worktree read-only; every build and
run was in the reflink copy `/work/scratch/mg04b-1b-rv-opus` (target seeded from `/work/scratch/dw-mg-04b/target`),
left in place for the manager. Paths below are relative to `rust/crates/` unless noted.

## Verdict: CHANGES-REQUIRED (small)

At the head no path I found loses funds or double-reserves a coin, but one guard is wrong in §12. §12 calls
mutant 10's held-coin filter defense in depth that "no reachable state exercises". It isn't. That filter is the only
thing that stops an MG-04 registration from naming a shared session's reserved coin as its collateral, and no test
kills its removal (M-1, reproduced). The other changes required are a regression in MG-04's
`refresh_external_registration` (m-1, reproduced) and a wrong stage for a registration mined under another txid
(m-2, reproduced). Each fix is a few lines plus a test.

Counts: **1 M, 2 m, 6 n.**

## Findings

### M-1: mutant 10 guards a reachable state, and no test kills it (the §12 claim is wrong)

- **Where:** `dw-engine/src/masternodes/protx/build.rs:572` (`used.collateral.extend(held…)` in `protx_context`);
  `protx/build.rs:895-923` (`existing_coin`); `docs/design/MG-04b-shared-masternodes.md:963-966` (§12).
- **Why it's reachable:** `existing_coin` accepts a coin the user locked ("A coin the user locked may be a
  collateral, as in Core"). Its only `reserved` filter is the in-memory `spends` set, not the persistent locks. A
  session's reserved coin is locked, and the lock is all §12 relies on. So for `CollateralChoice::ExistingUtxo`, the
  held set in `ctx.used.collateral` (`build.rs:653`) is the only guard.
- **Scenario (reproduced):** wallet 0 holds a single coin X of exactly 1,000 DASH. `start_shared` with shares 500/500
  reserves X (largest first, change 500 − fee) and locks it. Then `register(ExistingUtxo{X})`.
  - At the head, `preview_registration` and `register` both return `collateral_unavailable`, which is correct.
  - With mutant 10 (`A6MUT=10`, the filter removed), `register` returns `Ok`, state `Sent`. An MG-04 ProRegTx naming X
    as its collateral is handed off. The shared registration (`sign_shared_funding` or `send_shared_registration`)
    then spends X, and the MG-04 masternode loses its collateral. The op row also records `locked_collateral = false`,
    because X was already locked, so once the session releases X nothing keeps it locked.
  - Probe: `review_probe_a_reserved_coin_is_no_existing_collateral` in the copy's `shared/tests.rs`. It passes at the
    head and fails under `A6MUT=10` (`probe-m10.log`).
- **Fix:**
  - Commit this probe, or an equivalent test, as the killer of mutant 10, plus the `External` twin (`build.rs:664`).
  - Correct §12: three mutants (6, 15 and 16) are defense in depth; 10 has a test.
  - Better still, make `existing_coin` refuse a held coin itself, so the rule doesn't rest on a set built far away.
    Today `held` is folded into `used.collateral`, which also mixes "another row's collateral" with "another row's
    fee or reservation coin".

### m-1: `refresh_external_registration` can no longer reuse the row's own unspent inputs (MG-04 regression)

- **Where:** `dw-engine/src/masternodes/protx.rs:438-456`, then `prepared_row` → `protx_candidates`
  (`protx/build.rs:828-845`, the held filter added at `:831-843`).
- **What happens:** the refresh releases the old inputs' locks (`Holder::Operation(id)`), but the row's `fee_inputs`
  column still lists them until `reprepare_masternode_op`. `protx_candidates` filters
  `held_outpoints(Holder::None)`, which includes this row's own inputs, so the new plan can't take them. The comment
  at `protx.rs:440-442` ("so the new plan may take the ones still unspent") is now false.
- **Scenario (reproduced):** a wallet with one 1-DASH coin. `prepare_external_registration` gives `NeedsSignature`
  with that coin as its fee input. `refresh_external_registration` then fails with
  `send.amount_with_fee_exceeds_balance {fee: 445, available: 0}`. Before 1b the unlocked coin was a candidate and the
  refresh succeeded.
  - The error path re-locks the coin, so nothing is lost. It's a functional regression: a refresh in
    `NeedsSignature`, or in `InputsSpent` with an old input still unspent, needs extra coins.
  - Probe: `review_probe_a_refresh_reuses_its_own_unspent_input` in the copy's `protx/tests.rs`.
  - This is reasoned against the base and reproduced at the head; I didn't build 87eec7a2.
- **Fix:** have `protx_candidates` (or `prepared_row`) take the caller's `Holder`, so the refresh excludes coins held
  by rows other than this operation (`held_outpoints(Holder::Operation(id))`). Commit the probe.

### m-2: a registration mined under another txid reads `conflicted` for the coordinator

- **Where:** `dw-engine/src/masternodes/shared/store.rs:478` (`session_hash`: `complete_txid(..).or_else(registration)`)
  and `:483-492` (`derive_stage`).
- **What happens:** once the coordinator has sent, `complete_txid` is `Some` (the Final Record's txid), so the
  inputs-and-outputs match in `ChainView::registration` is never consulted. A block holding the same registration with
  a different valid scriptSig on any input has another txid. `confirmed_height(complete)` is `None`, and
  `conflict_height(own, except = complete)` finds the mined copy, so the coordinator reads **`Conflicted`**.
- **Scenario (reproduced):** three-wallet session, coordinator sends, and the sent tx is mined with a participant
  input's scriptSig altered. Wallet 0 reads `Conflicted`; wallets 1 and 2 read `Confirmed {height: 101}`, because they
  fall back to the template match. Probe: `review_probe_a_malleated_registration_reads_confirmed`.
- **Consequences for the coordinator:**
  - The follower releases the session's locks once the "conflict" is buried. Those coins are spent, so this is
    harmless.
  - The masternode is missing from `held_shares` (D74: not `mine`, no `shares`).
  - `update_share` refuses `shared_registration_unknown`, because `shared_confirmed` keys on the txid.
  - The user is told the registration lost, though their share is registered.
- **How likely:** low. Relay policy (low-S, push-only, minimal pushes) stops third-party malleation, so it needs a
  party re-signing an input with another nonce, or a miner. The logic predates 1b (1a's `spent_in_block`), but 1b
  turns on sending, `held_shares` and `update_share`, which depend on it.
- **Fix:**
  - In `derive_stage` and `releasable`, treat a history transaction matching `ChainView::registration` (same inputs
    in order, same outputs) as the registration, whatever `complete_txid` says. Exclude it in `conflict_height`.
  - Key `shared_confirmed` and `held_shares` on the mined txid, which is the ProTx hash the list shows.

### n-1: a doc comment says the resend is charged the fee

`dw-engine/src/masternodes/shared.rs:663` says "At `sent`, hands the saved bytes off again (a resend, charged the
fee)". The code charges `rec.debit()` (`shared/send.rs:183`), as D71 and the contract say. Fix the comment.

### n-2: abandon's "no coin" refusal names `sent` for a participant, and isn't in the contract

`shared/send.rs:229-231` returns `shared_stage{stage: "sent"}` when no reserved coin is unspent and free. For a
participant the stage reads `funding_signed`. The contract row for `abandon_shared_session` lists neither this
refusal nor `send.amount_with_fee_exceeds_balance` (the dust floor, `send.rs:241-250`). Either name the caller's
`out_stage(role)` or document `sent` as the reason, and add both to the contract's error list.

### n-3: no test at exactly 5 blocks deep for D70

`shared/flow.rs:671`: the tests check 1 deep (kept) and 6 deep (released)
(`test_QT_126_an_unreadable_out_session_is_released_by_its_stored_columns`), and a ChainLock. A mutant that changes
`+ 1 >= 6` to `+ 1 >= 5`, releasing one block early, would survive. I reasoned this and didn't run it. Add an
assertion at tip 74 in that test.

### n-4: `test_QT_126_a_new_session_never_takes_a_held_coin` can pass without asserting

`shared/tests.rs:2230`: `if let Ok(b) = start(...)` skips the assertion when `start` fails for any reason. Assert the
outcome you expect: either `start` succeeds and avoids A's coins, or it fails with the expected code.

### n-5: D70's depth rule doesn't cover MG-04 operations, `update_share` included

`protx/ops.rs:479-485` releases a `Conflicted` operation's fee inputs at once. D70's reason (a shallow conflict may be
reorged out) applies the same way to an `update_share`, and `update_share` is now an operation. This isn't a funds
loss, since a later double spend just loses the race, but the two rules diverge. Either share `buried` with
`settle_operation_locks` or record the difference as a D-row.

### n-6: an abandon refused by the network still blocks the resend for good

`shared.rs:691` refuses the resend once `abandon_txid` is set, even when the abandon's hand-off ended in a definite
rejection (for example, the registration is already in peers' mempools), so no abandon bytes are out. The user then
has to abandon again rather than resend. This is a design choice (D72); consider keying "abandoned" on an abandon that
may be out (no definite refusal in `last_error`), or say in D72 that this is intended.

## The four surviving mutants (§12)

| Mutant | What it removes | My verdict |
|---|---|---|
| 6 | `set_shared_fee`'s `!others.contains` on its own `also` coins (`shared/flow.rs`) | **Defense in depth, agreed.** Under 1b every writer of a reservation is fenced, candidates exclude held coins, and `unlock_outpoints` refuses held ones, so a session's own reserved coin can't be held by another row. Only a database written by 1a reaches it: 1a's `unlock_outpoints` could free a reserved coin, which an MG-04 operation could then take. That database isn't in users' hands. |
| **10** | `protx_context`'s held coins in `used.collateral` (`protx/build.rs:572`) | **Real gap: M-1.** Reachable through `ExistingUtxo` (and `External`), because a locked coin is an acceptable collateral. Killed by the probe. |
| 15 | The fence in `sign_shared_funding` | **Defense in depth, agreed.** `claim_loaded` serializes calls per session. The follower uses `try_claim_shared_session` (`flow.rs:655`). Another session's reservation excludes held and locked coins, and `unlock_outpoints` refuses held coins with or without the fence. |
| 16 | The fence in `approve_shared` | **Defense in depth, agreed**, for the same reasons as 15. |

## Areas checked with no finding

- **Holds (D69):**
  - The `HELD` query reads both tables and treats non-array columns as empty.
  - `release_lock` skips other rows' holds in every converted path: the session discard, the reservation replace, the
    follower release, operation settle, discard and refresh, the conflicted-collateral release, and the D9 collateral
    release with `Holder::None`.
  - `unlock_outpoints` refuses held coins under the fence.
  - CoinJoin skips user-locked coins (`coinjoin.rs:438,611`).
  - Settled sessions clear `reserved` to `[]`, and settled operations clear `fee_inputs`, so holds don't outlive
    their rows' lives.
- **Lock order:**
  - `register`, `prepare_external_registration`, `start_shared` and `join_shared` take the fence, then
    `protx_context`.
  - `update_registrar` and `update_share` take `update_lock`, then the fence.
  - The shared send and abandon take the session claim, then the fence, then the lease.
  - I found no path that takes the fence and then `update_lock`, and no nested fence.
- **D70 release:**
  - Only an out session waits for depth or a ChainLock; `confirmed` and `dead` release at once.
  - Released coins are either spent or held by no live spend, since exclusivity means no other row holds them, and a
    pending abandon's coin is kept in `spends`.
  - After a reorg deeper than the threshold, the worst case is a lost race, not lost funds (accepted by D70).
- **Send (D71):**
  - The coordinator signs last (the `unsigned` check) and the D65 consent hash is re-checked.
  - Charge, sign, then `typed()` re-serializes unchanged. `out_at` and `signed_tx` are written durably before the
    hand-off.
  - `SpendCharge` refunds on every early error. `out_at` is never cleared.
  - After a crash at `SharedOut`, `overlay_own_signatures` restores the coordinator's own scriptSigs, and the next
    call resends exactly once (the commit 37618057 test).
  - A resend is charged the debit, with `refund_unadmitted` for bytes that may already be out.
- **Abandon (D72):** one input and one output to a fresh change address, at the relay minimum, refused at dust.
  `abandon_txid` is written durably first. Coins pending in `spends` (the registration's, or an earlier abandon's)
  are skipped. A recorded registration has no unspent coin, so the call is refused (QT-091).
- **`update_share` (D73):**
  - Planned under `update_lock` and the fence. The D23 check runs under the same lock.
  - The payload signature is checked against the share's owner key id before the inputs are signed. The inputs sign
    over the signed payload.
  - The row is saved with its locks before `deliver`.
  - The claim is empty (no operator key), and the `SharedMasternode` flow is used for the resend.
- **D9 debit:** `Recorded::debit` = I − C. The participant is charged exactly that (`shared.rs:942`); the coordinator
  is charged share + fee + any dust it gave up. dwcli caps `sign`/`send` grants at `SharedSession.debit`, `abandon`
  at `--max-fee`, and `update-share` at the preview's `total_debit`.
- **Contract v9 against the code:** the §2.4 rows and the error order for `update_share` (`update_pending` after
  `list_unavailable`, before the registration, too-early and share checks) match the code. `HeldShares`, `debit`,
  `UpdateShare` and the three facade methods are in the surface block. The two gaps are n-1 and n-2.

## What I ran

- Built `cargo test -p dw-engine -p dw-appdb -p dwcli --no-run` in the copy (logs `build.log`, `build2.log`).
- dw-engine lib `masternodes:: coins:: --skip review_probe`: **120 passed, 0 failed** (`eng-suite.log`).
- dw-appdb lib: **148 passed**. `mn_masternodes_contract`: **8 passed**. dwcli `mn_cli`: **3 passed**, and
  `mn_shared_cli`: **5 passed**. Logs: `appdb.log`, `contract.log` and `dwcli.log` in the copy.
- Three review probes, appended to the copy's tests (`probes.log`):
  - `review_probe_a_reserved_coin_is_no_existing_collateral`: passes at the head and fails under `A6MUT=10`
    (`probe-m10.log`). That is M-1.
  - `review_probe_a_refresh_reuses_its_own_unspent_input`: fails, which is m-1.
  - `review_probe_a_malleated_registration_reads_confirmed`: fails (wallet 0 `Conflicted`, wallets 1 and 2
    `Confirmed`), which is m-2.
- Mutant 10 is env-gated in the copy's `protx/build.rs`, with the `mutant()` gate appended to `dw-engine/src/lib.rs`.
  The other mutants are judged from the implementer's `patch_1b.py` and `SUMMARY.txt` and from reading the code; I
  didn't rerun them.
- Not run: the T1 regtest (deferred, as ruled) and the full workspace gate.
