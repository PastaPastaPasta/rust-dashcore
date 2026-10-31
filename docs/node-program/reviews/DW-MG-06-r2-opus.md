# DW MG-06 own proposals: Opus-h review, round 2 (the fix delta)

- **Branch:** `dw/mg-06-proposals` at `4eaf3035`; scope `git diff 1af1515d 4eaf3035` (`e1042a59` D20 backoff,
  `111ecb90` r1 fix round, `4eaf3035` gate-r3 keep-step fix). Round 1: `~/workspace/node-program/reviews/DW-MG-06-r1-opus.md`.
- **Reviewer worktree:** detached at `4eaf3035` in `/work/scratch/mg06-review/r2/wt`, target reflink-seeded from the
  implementer's, every `.rs`/`Cargo.toml` touched; deleted after the review.
- **Date:** 2026-10-21. Line numbers below are at `4eaf3035`.

## Verdict: ACCEPT WITH NITS

Both r1 Majors are fixed, and each fix has a test that a mutant kills. The new D23 keep-and-restore path cannot
resurrect a cancelled fee's row or leak another wallet's rows, and a failed restore fails the import closed. I found no
path in the delta that loses a paid proposal, double-spends a reserved coin, or sends a fee twice with different bytes.
What is left: three Minors (a stale `last_error`, a crash window in which an unsent fee reads "paid", and two test
gaps my mutants found) and a few nits. None of them blocks the merge.

## Tests run

| Suite (one build, the review mutants inert) | Result |
|---|---|
| `dw_appdb` lib, all | 125 passed, 0 failed |
| `dw_engine` lib, `governance::` | 66 passed, 0 failed |
| `dw_engine` lib, `send:: wallets multiwallet remov` | 142 passed, 0 failed, 2 ignored |

I did not re-run the gate, as the brief allows. The implementer's mutant logs
(`/work/scratch/mg06/mut/r1/result.txt`, `result-r4c-n4-n8.txt`) match their claims: r1–r14 killed apart from r4/r4b,
r4c killed, n1–n13 and v1–v9 killed except v2 (see m-3). n4 survived the first pass and was killed by the later
`a_resend_after_a_restart_claims_the_fees_coins`.

**My mutants.** All three are gated by `MG6R2=<id>` in one build; the script is
`/work/scratch/mg06-review/r2/tools/mutants.py`.

| id | Mutant | Result |
|---|---|---|
| k1 | `restore_kept` copies back every wallet's kept rows (the `wallet_id` scope dropped, `dw-appdb/src/proposals.rs:249`) | **survived** (appdb all 125, engine governance + removal 108) |
| k2 | a removal also drops `fee_unsent` rows (`proposals.rs:618`) | **survived** (same suites) |
| k4 | the tick's early filter of settled rows withdraws nothing (`proposals.rs:790`) | survived; **equivalent**: `retain_paces` (`:912`) withdraws every relayed key missing from `live_keys`, and only a key with a pace was ever relayed |

k1 and k2 only change code that MG-06's own tests reach (only they create proposal rows), so the governance and
removal filters are the complete set of tests that could kill them.

---

## Brief items

### 1. r1 findings fixed, each with a killing test

| r1 | Fix | Pinned by |
|---|---|---|
| M-1 | `InFlight` mark (`sync.rs` `InFlight`); create holds it from before the save until the record step (`proposals.rs:441`, dropped when `pay_fee` returns); [Submit] holds it for the whole call (`:643`). The `NotSent` delete is `fee_txid`-scoped and `fee_sent_at IS NULL`-guarded, in one transaction with the kept copy. | `a_submit_during_the_create_hand_off_is_refused_in_flight` (n1, n13); `a_proposal_whose_fee_went_out_is_not_deleted` (n2); `a_cancelled_hand_off_deletes_only_its_own_row_and_its_kept_copy` (v5, v6). Reading the race test: on 1af1515d's logic the [Submit] is admitted, so `fee_sent_at.is_none()` fails or the row survives. It reproduces the bug. |
| M-2 | `resend_fee` (`:695-748`): under the coin fence, unless history has the fee, every input must be in the snapshot and neither `lock_input` nor `unresolved_lock`; then `reserve_unless_held(Tx(fee_txid))`. | `a_resend_refuses_a_coin_another_payment_holds` (n3, v3), `…an_asset_lock_holds` (v1), `…an_unresolved_lock_signed`, `a_resend_after_a_restart_claims_the_fees_coins` (n4) |
| m-1 | test gaps | m2 → `a_saved_proposal_survives_power_loss`; m3, m4, m6/m7, m8, m10, m12, m13 each have a test that the matching r-mutant kills; MasternodeOp grant refused (`proposals_tests.rs:563`); coin control (`the_fee_spends_the_chosen_coins`); backup round trip with restored state |
| m-2 | `fee_statuses` once per wallet per tick; settled rows skip it | `a_tick_reads_the_history_once_and_never_for_settled_proposals` (n6, n7). The rewrite is equivalent to the old per-fee scan: depth set for any depth, conflicts only from `depth > 0` transactions with a different txid. |
| m-3 | `MyProposal.last_error`, mg-engine 7, D24 | vectors + contract test |
| nits | n-1 (`reads.rs` D13), n-2 → D24, n-3 → r12/r13 tests, n-4 → D23, n-5 → D25 | — |

**r4 equivalence holds.** `GovernanceStore::object` already returns only confirmed objects
(`dw-governance/src/store.rs:843`: `self.objects.get(hash).filter(|o| o.confirmed)`), so the engine's
`.is_some_and(|o| o.confirmed)` is redundant. r4 and r4b each leave the other filter in place, and r4c, which drops
both, is killed by `one_announcing_peer_does_not_make_it_public`.

### 2. The in-flight mark, including across a restart

- **It is held over the right span.** Create takes it after `prepare` and before the insert. Before that point no row
  exists, so a [Submit] gets `proposal_not_found`. The binding lives until `pay_fee` returns, so it also covers the
  record step. That closes r1's window between the broadcast's return and the record.
- **[Submit] takes it first,** before the op gate and the row read, so it never acts on a row whose create is still
  deciding.
- **No deadlock.** The `std::Mutex` is held only for the insert and the remove, never across an await.
- **No leak.** The mark is RAII. `begin` builds the guard only when it inserted, so a refused `begin` never unmarks
  someone else's mark.
- **It lives on `NetworkSession.governance`,** so it shares the session's lifetime with `spends`.
- **A death between marking and the hand-off.** The mark lives in memory, so it goes with the process. The row is
  left with `fee_sent_at` NULL and reads `fee_unsent`.
  - After the restart no create is running, so nothing can delete the row.
  - [Submit] then goes through M-2's coin checks. If the original bytes did go out before the death, the resend is
    the same txid: harmless.
- **Same draft created twice in one second.** The second create either gets `in_flight` or hits the primary key.
  Either way it is abandoned, and its `delete_unsent_proposal` is scoped to its own `fee_txid`, so it cannot take the
  first row.
- **The same session after a `MaybeSent` create.** The create's claim is `SpendOwner::Tx(txid)` (`send/mod.rs:1274`),
  the same owner the resend uses, so `reserve_unless_held` admits it (`a_resend_records_the_hand_off_and_its_failure`).

### 3. D23 keep and restore

- **Ordering (`4eaf3035`).** In production `appdb_op` is exactly `spawn_blocking` plus the test-only first-job pause
  (`coins.rs:367-383`), so moving `keep_proposals` to a direct `spawn_blocking` (`wallets.rs:245`) changes nothing
  outside tests. The keep still runs:
  - after `revoke_wallet`'s barrier, so no new lease or permit;
  - before `redeem_grant` and every delete.

  The comment states the reason accurately.
- **A create racing the removal.** Its fenced broadcast is refused once the lease is revoked. The two orders of the
  record step against the keep both work:
  - the delete first: the keep's `INSERT … SELECT` finds no row to copy;
  - the keep first: `delete_unsent_proposal` removes the kept copy in the same transaction as the row.

  A `MaybeSent` record after the keep leaves the copy at `fee_sent_at` NULL, which restores as `fee_unsent`, the
  conservative reading. So no row of a fee that never went out comes back.
- **No cross-wallet leak in the code.**
  - `keep_proposals` copies `WHERE wallet_id = ?1 AND hash = ?2`;
  - `restore_kept` copies `WHERE wallet_id = ?1`;
  - `mg_proposals_kept` is in `NOT_EXPORTED`, and `scoped_tables` skips it on import, so a crafted `.dwbackup` cannot
    inject into it.

  Nothing tests the scope (k1, m-3).
- **Restore idempotence.** `INSERT OR IGNORE` from copies that stay. A repeated readmit, or a readmit rolled back by
  `record_removed_again`, finds them again. The next removal's `keep_proposals` replaces the set, including with an
  empty set.
- **A failed restore.** On the removed path the transaction is dropped uncommitted, so `delete_wallet_rows` and the
  `removed_wallets` delete roll back. The import gets a plain storage error (`keys.rs:532` forgets a newly added
  secret), and the wallet stays removed: it fails closed and can be retried. The comment there is wrong (n-1), and
  no test injects the failure.
- **Backups.** `.dwbackup` carries `mg_proposals` and not the kept table. A backup import after a restore is
  `INSERT OR IGNORE`, so neither copy fails the other. At worst a stale mark wins, and marks re-derive.

### 4. Paid proposals, double spends, fees sent twice

- **Losing a paid proposal.** The only deleter is a create's own `NotSent`, and it is now `fee_txid`- and
  `fee_sent_at`-guarded and mark-fenced. Removal keeps every row that is not dead. `kept_on_removal`'s failure fails
  the removal before anything goes (`wallets.rs:238`).
- **Double spend.** A resend needs every coin unspent in the wallet's view, held by no asset lock, and claimed by no
  other owner. The check and the claim happen under the coin fence, and the claim stays after any failure (a repeat's
  failure is always `broadcast_unknown`). `coin_snapshot`'s `retain_unspent` drops the claim once the coins are seen
  spent.
- **A fee sent twice.** Every hand-off of a row sends the same saved bytes, one txid, and there is no path that signs
  a second fee for an existing row.

**Declined n-6 (don't release the claim when the resend's record fails): I agree.** If `mark_proposal_sent` fails, this
resend sent nothing, but an earlier hand-off of the same bytes may have. Releasing would let the planner promise those
coins to another payment that then loses to the fee. Keeping the claim costs at most coins frozen until the next
restart, or until they are seen spent.

### 5. Regtest check 7's stale `last_error`: yes, it should go once the wallet sees the fee (m-1)

---

## Minor

### m-1. `last_error` outlives the hand-off it describes

- **Where:** `proposals.rs:600` (`MyProposal.last_error`) and `:576` (`Facts.sent`).
- **What happens:** a `MaybeSent` create that in fact went out (regtest check 7) keeps its `last_error` for good.
  - The row moves to `fee_paid` → `confirming` → `public`, because `derive_state` reads the depth first.
  - `MyProposal` still reports "the last hand-off of its fee's failure" for a fee that is public.
  - mg-engine 7 says `last_error` is "the failure of its fee's last hand-off if it failed", which a seen fee
    contradicts.
  - D24 tells MG-09 to show `last_error` with the [Submit], so today it would only leak where the UI shows it outside
    the [Submit]. The DTO itself is still wrong for any other consumer.
- **Fix (either one):**
  - in `my_proposal`, report `last_error` only while the fee is unseen (`depth.is_none()`);
  - or have the tick clear it once `depth.is_some()`. That is one write per row, and it also makes `Facts.sent` honest.

  Add a test: a `MaybeSent` create, then the fee seen, then `last_error == None`.

### m-2. A crash between the resend's pre-mark and the library call leaves an unsent fee reading "Fee paid"

- **Where:** `proposals.rs:726`: `mark_proposal_sent` before the broadcast, with `last_error` untouched.
- **When it happens:** the row had `fee_sent_at` NULL and `last_error` NULL. That is:
  - D22's row of a save without a broadcast;
  - a backup restored before the broadcast;
  - a D23 copy taken before the create's record step.
- **What follows:** a [Submit] that dies after the mark and before `broadcast_transaction` leaves `fee_sent_at` set
  and no error.
  - After the restart, `Facts.sent` is true and the state is `fee_paid{0}` ("Fee paid"), but nothing was ever handed
    to a peer.
  - The fee's coins are unclaimed after the restart, so a payment can spend them, and the row then reads `fee_failed`.
  - [Submit] still shows at `fee_paid{0}` (D24), so nothing is stuck. But the wallet tells the user a 1 DASH fee was
    paid when it was not. At 1af1515d the mark came after the broadcast, so this window is new.
- **Fix:** in the same `appdb_op` as the mark, set a provisional `last_error` (for example "handing the fee off"). The
  post-broadcast `set_proposal_error` already overwrites it. A crash then reads `fee_unsent`, which is D10's own rule
  ("reads `fee_unsent` while `last_error` is set"). Update D10's sentence.

### m-3. Test gaps from surviving mutants

- **k1:** no test has two wallets' kept copies. The "can't leak another wallet's rows" property of D23 is right in the
  code (`dw-appdb/src/proposals.rs:249`) but unpinned.
  - To pin it, extend `a_removal_keeps_the_chosen_proposals_for_the_readmission`: keep and remove `bb` as well, then
    readmit `W` only, and assert `bb` has no live rows.
- **k2:** D23 keeps a `fee_unsent` row whose fee may be out (a `MaybeSent` create), but
  `a_removal_keeps_a_paid_proposal_for_the_wallets_return` only removes a `fee_paid{0}` row. A filter that also
  dropped `FeeUnsent` would lose exactly the case D23 exists for.
  - To pin it, create with `Mode::Unconfirmed`, remove, import, and assert the row is back as `FeeUnsent`.
- **v2** (the implementer's, survived): `!c.unresolved_lock` (`proposals.rs:712`) is redundant with `!c.lock_input`.
  - `coins.rs:731` makes `lock_input` true whenever `unresolved_lock` is true and the coin is not spend-reserved.
  - A spend-reserved coin is refused by `reserve_unless_held` anyway, unless the reserving owner is the fee itself.
  - So v2 is equivalent in practice. Record it as equivalent, like r4, rather than leave it unexplained.

## Nits

- **n-1. `dw-appdb/src/lib.rs:647`:** the comment "Its own failure: the removal's rows are gone in this transaction"
  says the opposite of what happens.
  - On a restore error the transaction is dropped, so the rows and the `removed_wallets` record come back, and the
    import fails closed with a plain storage error rather than `RemovalUnfinished`. That behaviour is right; the
    comment should say it.
  - No test injects the failure.
- **n-2. `proposals.rs:484`:** `deleted` defaults to `true` when the record step fails, so the variable means "report
  the send error", not "the row was deleted". On a `NotSent` whose delete failed, the row survives as `fee_unsent`
  while the create returns the error. That is the pre-existing behaviour and M-2's checks make a later [Submit] safe,
  but name the variable for what it decides.
- **n-3. D23 retention:**
  - Kept copies are never purged unless the wallet is imported and removed again, including copies that later expire.
  - A kept `fee_unsent` copy holds a signed transaction that was never published, so its addresses and amounts
    survive a Wipe.
  - The design accepts this (logged, DP1-02 precedent). r1 n-4's point still stands, though: the removal confirmation
    (MG-09/U-04) should say that paid proposals are kept for re-import.
- **n-4. `proposals.rs:790`:** the early `withdraw_object` for settled rows is redundant with `retain_paces` (k4 is
  equivalent). That is harmless; leave it for clarity, or note it.
- **n-5. D10:** the resend's coin check ignores user coin-control locks and dust locks (`user_locked`, `dust_locked`).
  That is consistent with dash-qt's Resend of already-signed bytes, but D10 lists what refuses a resend and should say
  locks do not.
- **n-6. The migration was edited in place:** `V2026102001` gained `mg_proposals_kept`, and its checksum was re-pinned.
  That is fine for an unmerged branch, but any dev or regtest data directory created from 1af1515d will fail
  refinery's checksum check and needs to be recreated.

## Also checked and sound

- **D20 backoff.** `echo_retry` is separate from `fetch_retry`:
  - the first look comes at 120 s after the sixth confirmation, then 30 min, doubling to a 24 h cap;
  - it ends when the row settles as expired, through the early filter;
  - `progressed()` resets only the fetch backoff, which is right, because an echo makes the row public and settles it.

  It is pinned by `echo_retries_back_off_to_a_day` (o15, o16).
- **D25.** dwcli caps the grant at `fee + network_fee + max(network_fee, 10 000)`. The lease charges what is spent, so
  the overpayment is bounded at 0.0001 DASH on a dev tool.
- **`broadcast_result` extraction (`send/mod.rs`).** It is a verbatim move. The resend passes `first = false`, so every
  failure is `BroadcastUnknown`, which is correct for a repeat.
- **`reserve_unless_held`.** It checks first and inserts only if nothing is held, all under one lock, so it never
  leaves a partial claim.
- **The tick's `manual_ticks` and `fee_scans` hooks** are `#[cfg(test)]` only.
