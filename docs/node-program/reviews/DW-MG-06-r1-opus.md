# DW MG-06 own proposals: Opus-h review, round 1

- **Branch:** `dw/mg-06-proposals` at `1af1515d` (off main `06d2be9b`); scope `git diff 06d2be9b 1af1515d`.
- **Reviewer worktree:** detached at `1af1515d` in `/work/scratch/mg06-review/wt`, target reflink-seeded from the
  implementer's, every `.rs`/`Cargo.toml` touched. Core reference: `v24.0.0-rc.3`, read with `git show`.
- **Date:** 2026-10-20.

## Verdict: CHANGES REQUIRED

Two Majors, both in the resend path (`submit_proposal` → `resend_fee`). That path is unfenced and unreserved, and the
rest of the money flow assumes nothing else can send the saved bytes. Everything else the implementer claims holds up:
the fee shape, save-before-broadcast, the durability of the insert, D21's announcement gate, and the run-keyed waits.
So does the Core parity of the hash, the collateral and the timing. The H1/H2/M1–M3 fixes and the validator-r2 fixes
are in place. The rest are test gaps (9 of my 14 mutants survive) and nits.

## Tests run

| Suite | Result |
|---|---|
| `cargo test -p dw-governance -p dw-appdb` | 348 passed, 0 failed |
| `cargo test -p dw-engine -p dw-app -p dwcli` | 1491 passed, 1 failed |

The one failure was `platform::registration_tests::a_run_held_on_a_partial_bring_up_has_it_run_again_and_registers`
("timed out: the flow stops", at load ~150). It is not MG-06 code, and it passed 3/3 when rerun alone. That includes
`opus_probe_b_pre_e0_10b…`, the recovery probe that 1af1515d fixed: it passes.

**Review race test (kept out of the branch).** `review_r1_submit_during_the_create_hand_off_resends_then_the_row_is_deleted`
is in `/work/scratch/mg06-review/race_test.rs`. It **passes on 1af1515d**, which demonstrates finding M-1.

**Hand mutants.** All 14 are gated by `MG6MUT=<id>`, in one build; the script is
`/work/scratch/mg06-review/mutants.py`. Unset, they are inert: the governance and send suites stay green.

| id | Mutant | Result |
|---|---|---|
| m1 | a `MaybeSent` hand-off deletes the row | caught (`a_fee_that_may_be_out_keeps_its_proposal_unsent`) |
| m2 | `insert_proposal` not under `durably` (H2 regression) | **survived** (dw-appdb, all 121) |
| m3 | an unconfirmed (mempool) conflicting spend counts as `fee_failed` | **survived** |
| m4 | "public" on any store copy, `confirmed` or not (one peer) | **survived** |
| m5 | a lost fee still reads `submitted` | caught (2 reorg tests) |
| m6 | the fetch wait counts while the run is not synced | **survived** |
| m7 | the echo wait counts while the run is not synced | **survived** |
| m8 | a resend does not record `fee_sent_at` | **survived** |
| m9 | a `getdata` of an own object is not noted as a fetch | caught (dw-governance `own_objects_are_announced_once_per_round…`) |
| m10 | every announce pass re-sends every item to every peer (no per-round dedupe) | **survived** |
| m11 | `withdraw` keeps the fetchers | caught (same test) |
| m12 | `expired` at exactly `end_epoch` reads live | **survived** (boundary only) |
| m13 | change at a fixed position | **survived** |
| m14 | the `OP_RETURN` value not charged to the lease | caught (`the_fee_needs_a_spend_grant_covering_it`) |

---

## Major

### M-1. A [Submit] during `create_proposal`'s hand-off resends the fee; the fenced attempt's `NotSent` then deletes the row of a fee that went out

- **Where:** `dw-engine/src/governance/proposals.rs`
  - `:409`: the fenced broadcast.
  - `:419`: `(_, Outcome::NotSent) => db.delete_proposal(..)`.
  - `:546-566`: Submit reads the state, then resends.
  - `:586-591`: `resend_fee`, unfenced.
- **The path:**
  1. `pay_fee` saves the row with `fee_sent_at = NULL` *before* the fenced broadcast (D4).
  2. While that broadcast is in flight, the row reads `fee_unsent`.
  3. `submit_proposal` takes only a shared `OpGate` read (`gate.rs:78`, an `RwLock::read`), the same as the fenced
     broadcast. So it is admitted concurrently, sees `FeeUnsent`, and calls `resend_fee`. That hands the same signed
     bytes straight to the library, with no fence and no reservation check.
  4. If the fenced attempt then ends as a definite `NotSent`, `pay_fee` deletes the row. `NotSent` here covers:
     - a lock or the permit deadline (`Cancelled`);
     - `NoPeers`;
     - `BroadcastRejected`, for example "txn-already-in-mempool" because the resend got there first;
     - `PreparedTxSpent`, the library's `StaleReservation` once the resend marked the inputs spent.

     In every case the fee that the resend handed out has no row.
- **Consequence:** the 1 DASH is burned and the object data is gone. Nothing can submit the proposal again, which is
  exactly what D4/MG-00 D18 exist to prevent. `create_proposal` also returns the send error, which invites the user to
  create the proposal again and pay a second fee.
- **Demonstrated:** the review race test parks the fake library inside the fenced attempt. It then checks three things:
  - the row reads `FeeUnsent`;
  - `submit_proposal` reaches the library's own broadcast (the resend), and the fixture's library refuses only because
    it has no SPV client;
  - after release, the fenced attempt's rejection leaves `rows()` empty.
- **Why it isn't covered:** D4 and D22 both assume the fenced attempt is the only sender of those bytes.
  `a_hand_off_cut_before_the_library_deletes_its_proposal` covers a lone create only.
- **Fix (pick one, or both):**
  - **(a)** A per-proposal in-flight mark: a set of `(wallet, hash)` in `GovernanceRuntime`, held by `pay_fee` from
    the save until the outcome is recorded. `submit_proposal` refuses or waits while it is held (a `send.*`-style
    `in_flight`).
  - **(b)** Never delete a row whose bytes another path may have sent:
    - `resend_fee` marks `fee_sent_at` (or a `resent_at`) *before* it calls the library;
    - the `NotSent` delete becomes `DELETE … WHERE fee_sent_at IS NULL`, with the record step reading back whether it
      deleted.

  (b) also covers a resend that lands between the broadcast's return and the record step, which (a) only covers if the
  mark spans the record step. Add the race test (or its equivalent) to the suite.

### M-2. `resend_fee` ignores coin reservations: after a restart, a `fee_unsent` resend can double-spend coins another lease or prepared payment holds

- **Where:** `proposals.rs:586-602`, and D10 ("Like QT-091 it checks no reservation: the fee's coins are already spent
  by those same bytes").
- **The gap in D10's premise:** it holds only while the original `PreparedTx` reservation lives. A `fee_unsent` row
  survives a restart, and so does a crash between save and broadcast (D22: "A row left behind reads `fee_unsent`").
  But the `spends` reservation is per session, so after a restart the fee's coins are free. The planner can then
  reserve them for another `TxDraft`, for example:
  - an Accept-and-pay payment prepared under its lease and parked at the fence;
  - a `ContactPayment`;
  - a MasternodeOp.
- **What follows:** [Submit] on the old row broadcasts a conflicting spend of those coins, with no fence and no check.
  - Whichever reaches the network first wins.
  - If the fee wins, the other flow's fenced hand-off fails with `PreparedTxSpent`. For Accept-and-pay, that happens
    after its Platform half may have committed.
  - This is the "spend coins another lease holds" case the brief asks about. No unauthorised value leaves the wallet,
    since both spends were authorised once. But it breaks E0-04's invariant that a reserved coin is spent only by its
    owner.
- **Fix:**
  - Before the resend, check `session.spends` for each fee input. If an owner other than `SpendOwner::Tx(fee_txid)`
    holds one, refuse with a code the UI can show (`send.in_flight` or a new `governance.submit_refused{coins_in_use}`).
  - Then reserve the inputs under `SpendOwner::Tx(fee_txid)` for the hand-off, released on a definite rejection,
    exactly as a fenced hand-off does.
  - Update D10's wording.
  - Test: restart (or drop the reservation), prepare another draft on the same coin, then Submit → refused, and the
    other draft broadcasts.

## Minor

### m-1. Test gaps found by surviving mutants (each a one-test fix)

- **m6/m7: no test pins "a run that is not synced counts no wait".** `the_waits_restart_with_each_synced_run` ticks only
  once while unsynced, and `in_run(None)` resets the timers on that one tick. Two unsynced ticks 60 s or 120 s apart
  would expose it. On mainnet a governance sync takes minutes. A regression of `synced &&` (`proposals.rs:696`, `:706`)
  would resync a run that is still syncing, every backoff step, so it never finishes.
- **m2: H2 has no regression test.** `insert_proposal` could lose `durably` (`dw-appdb/src/proposals.rs:104`) with
  every test green. `durability_sim` exists for exactly this (`durability_sim::skips_barrier`). Assert that the
  insert's commit takes the FULL barrier, as the other durable writers' tests do.
- **m3: no test for an *unconfirmed* rival spend.** `fee_status` correctly requires `depth > 0` (`proposals.rs:218`), as
  Core counts only mined conflicts. A mempool rival must not make the proposal terminal `fee_failed`.
- **m4: no test that a single peer's announcement is not "public".** Public needs `confirmed` (MG-02 D3,
  `proposals.rs:485`); the `echo` helper always uses two peers.
- **m8: no seam for the resend.** No test sees a successful resend record `fee_sent_at` and clear `last_error`;
  `submit_resends_an_unmined_fee` stops at the fixture's missing SPV client. This is the same seam M-1 and M-2 need.
- **m10: "once per round and connection" (D19) is not pinned.**
  `own_objects_are_announced_once_per_round_and_their_fetchers_reported` never triggers a second `announce_outbox` in
  the same round (`relay_object` of a queued item does not notify). A `relay_vote` of another item in between would
  pin it (`dw-governance/src/sync.rs:856`).
- **Test-plan items (§4) not covered:**
  - "a `MasternodeOp` grant is refused" (only `Governance` is tested);
  - "coin control honoured" (no `coins` in any test);
  - "a `.dwbackup` round trip … the restored wallet reports the same state" (export only, no import or restored
    state);
  - "Wallet removal deletes the row" (covered only indirectly, through `delete_wallet` before create).

### m-2. Each 2 s tick scans the whole wallet history for every row, settled ones included

`tick` calls `facts()` (and so `fee_status`, `proposals.rs:481`) before it filters out public, expired and failed rows
(`:641-645`). `fee_status` walks every history entry and every input under the history read lock. With the bait9-class
wallets (10⁵ transactions), every proposal the wallet ever made adds a full history scan every 2 s, forever, since
rows are never deleted (D15).
- **Fix:** check `expired` and the store's `confirmed` first, and skip `fee_status` for a settled row in `tick`.
- **Or:** compute the conflict set once per tick for all rows, by collecting the fee inputs first.

### m-3. `MyProposal` does not carry `last_error`

§2.3's `ready_to_submit` row says the UI shows "the error", and a `MaybeSent` create stores its reason in `last_error`.
But `MyProposal` (mg-engine §2.5) has no field for it, so after a `fee_unsent` create MG-09 cannot show why.
- **Fix:** add `last_error: Option<String>` (or a code), with a mg-engine bump. If it is deliberately left out, write
  that down as a D-row.

## Nits

- **n-1.** `reads.rs:517`: the test doc says "MG-06 D17" for `cutoff_soon`; it is D13.
- **n-2.** §2.3's table gives `fee_paid` no [Submit], but the contract and code resend at `fee_paid{0}` (L3). MG-09
  needs one statement of when [Submit] shows. A fee that drops out of the mempool otherwise sits at "Fee paid 0 of 1"
  forever with no visible action, because nothing rebroadcasts it automatically.
- **n-3.** m13: Core's `RANDOM_CHANGE_POSITION` (`wallet/spend.cpp:1307`) is implemented but untested. Asserting both
  positions over ~40 creates would pin it. The same goes for m12: the expiry boundary `end_epoch <= now` matches Core's
  `ValidateStartEndEpoch` (`governance/validators.cpp:148`) but has no test.
- **n-4.** Wallet removal deletes a paid, not-yet-public proposal (D4, DEC-210). Re-importing from the seed recovers
  the coins, but the 1 DASH fee is lost with the object. The removal confirmation (MG-09/U-04) should say so when such
  a row exists, or the export reminder should cover it.
- **n-5.** dwcli `--propose` authorises exactly `preview.fee + preview.network_fee`. A create whose plan comes out a
  few duffs larger, because of a different change derivation or dust, fails `grant_exceeded`. That is harmless for
  regtest, but a small margin would avoid a flaky T1.

---

## What I checked and found sound

### 1. Money safety

- **Order of operations.** `pay_fee` runs `prepare` (charge, sign, reserve), then the durable insert, then the
  broadcast, then the record step. A failed save abandons the prepared transaction (refund and release) and deletes the
  row as far as it can.
- **The insert.** It runs under `durably`: `synchronous=FULL`, plus `fullfsync` on Apple.
- **Crash points:**
  - before the insert: nothing is sent;
  - after the insert, before the broadcast: the row reads `fee_unsent` and its resend is manual (but see M-2);
  - after the broadcast, before `mark_proposal_sent`: the row reads `fee_unsent` until the fee shows in history, then
    `fee_paid` or `confirming`. The `mark_*` writes are NORMAL-sync, and losing one costs only a re-derivable mark.
- **Outputs.** `check_fee_outputs` runs after signing and before reservation. A failure drops the `SpendCharge`
  unbound, so it is refunded.
- **The data output.** It is refused on `ChangeTarget::Address` and `FullyMixedOnly`. The coin-control `Outpoints`
  path goes through `chosen_selectable`, so a reserved coin is `OutpointUnavailable`.
- **The charge.** The outflow includes the `OP_RETURN` value (m14 caught).
- **A double create in the same second.** It gives the same object hash, so the second insert hits the primary key and
  is abandoned before any broadcast.

### 2. Core parity (v24.0.0-rc.3)

- **Hash.** `GovernanceObject::hash` equals `Object::GetHash` (`governance/common.cpp:23-39`): parent, revision, time,
  `HexStr(vchData)`, outpoint, `0x00`, `0xffffffff`, signature; the collateral is left out. The `OP_RETURN` is built
  from that hash before the collateral is set, and the row key after; both are equal (asserted in
  `the_proposal_is_saved_before_its_fee_is_broadcast`, and by `gobject get` in regtest check 7).
- **Collateral.**
  - `IsCollateralValid` (`object.cpp:790-878`): every output P2PKH or unspendable (`:837`), and
    `OP_RETURN <hash>` ≥ `GOVERNANCE_PROPOSAL_FEE_TX = 1*COIN` (`object.h:87`, `:841`).
  - The wallet sends exactly 1 DASH (D16, `params.rs:68`).
  - Confirmations count only from an active-chain block, and the wallet's `depth` (`history.rs:451`) counts only
    `block_info`, never InstantSend.
- **Scoring.**
  - `MNGOVERNANCEOBJECT` scores 20 when `ProcessObject` returns false (`net_governance.cpp:194-198`).
  - `ProcessObject` returns false only for invalid without `fMissingConfirmations` (`governance.cpp:262-270`). That
    covers 0 confirmations ("Can't find collateral tx" `:805`, txindex has no mempool; "not mined yet" `:811`) and a
    peer lacking the block or its txindex entry.
  - 1–5 confirmations are postponed (`:265`, `object.cpp:867`). An expired proposal is not scored: `IsValidLocally`
    checks with `fCheckExpiration=false` (`object.cpp:736`).
  - So the gate is right: ≥ 1 confirmation, caught up, and 15 s of propagation (D21). The fetch at 1 confirmation and
    the silence until 6 match `ConfirmInventoryRequest` (`governance.cpp:608-640`, no re-request of a postponed object)
    and `CheckPostponedObjects` (`:856-887`).
  - The object is served only on `getdata` (`PeerConsumeObjectRequest`, `net_governance.cpp:187`), and is withdrawn
    before 1 confirmation and after a reorg to 0, so it is never served unrequested or at 0 confirmations.
- **Shape.** `GenBudgetSystemCollateralTx` (`wallet/spend.cpp:1296-1314`): one valued `OP_RETURN`, change at a random
  position. That matches, apart from m13's missing test.

### 3. Resume and reorg

- A reorg to the mempool withdraws the object. The next confirmation queues a new round with the fetchers cleared, and
  a fetch is expected again (`lost`).
- A fee missing from history only withdraws it.
- A confirmed double spend gives a terminal `fee_failed`. Past `end_epoch` it is terminal `expired`. Both are
  withdrawn, and `submit_proposal` refuses public, expired and `fee_failed`.
- The outbox always serves the saved bytes, so no stale object is resubmitted. A row that disappears (its wallet
  removed) is withdrawn through `retain_paces`.

### 4. Concurrency

- **H1.** No op guard is held across `pay_fee`. Each public entry takes its own shared `OpGate` read, and nothing nests
  under a waiting close.
- **The tick.** `proposal_tick` serialises the ticks. The `paces` std mutex is never held across an await or another
  lock. The follower wakes the tick every 2 s (`POLL`), so the 15/60/120 s waits need no other wakeup.
- **Notify.** `relay.notify_one` stores its permit when no run is listening, and the next run announces on
  `announce_open` anyway. No deadlock and no lost wakeup found.
- **The one real race is M-1.**

### 5. Storage and backup

- `mg_proposals` is STRICT with length checks, and listed in `WALLET_TABLES` with the DEC-210 trigger (the generic
  `every_wallet_table_has_a_removal_trigger` covers it).
- The migration checksum is pinned, and the pre-E0-10b probe drops the table (1af1515d; it passes).
- Export carries the row. Import goes through the generic `rows.rs`, which is untested for this table (m-1).

### 6. Contracts and bindings

- **mg-engine 6.**
  - The §2.5 rows, the §3 surface and the two §4 codes match the code.
  - `governance.invalid_proposal` carries `ErrorData::ProposalFields`; `submit_refused` carries `Reason`.
  - dw-app binds exactly the four calls, behind four capabilities in the `governance` set, main window only.
- **m4 33.** It adds `FlowKind::Proposal` (item 60). `kinds_for` gives it `Spend` only, and a `Governance` grant is
  refused as `platform.*` (tested).
- **Gap:** see m-3 (`last_error`).

### 7. Earlier review folds

| Fix | Status |
|---|---|
| H1 | holds |
| H2 | holds; no test, m-1 |
| M1 (run-keyed waits) | holds |
| M2 (D21 15 s + caught up) | holds |
| M3 (missing fee ≠ reorg) | holds |
| Validator-r2 #1, #2, #4, #6 | present |
| Validator-r2 #6 (random change) | present; untested, n-3 |
