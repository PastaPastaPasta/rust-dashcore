# MG-04b slice 2a (dissolve now), review r1: Opus

- Branch `dw/mg-04b-2`, head `b55948f9` on desktop main `190d5b62`.
- Reviewer: Opus.
- 2026-10-30, from 04:19Z.

## Verdict

**ACCEPT-WITH-NITS** (0 M, 2 m, 4 n).

No funds path can go wrong.

- The bytes the wallet signs and sends are Core's `BuildDissolution` output for the previewed request. The outputs, amounts, order and penalty split all come from the frozen share table, which is the registration's because the txids match.
- The grant capped at the preview's `total_debit` binds the send to the preview's penalty.
- A resend charges the penalty and the fee once per lease, and never sends other bytes.
- D23 holds both ways for rows created without a reorg.

The two minors are a transient wrong `confirmed` (m-1) and a missing cheap test on the send-path dust refusal (m-2). The probe for m-2 is below; it's a ~25-line test, and I'd take it in this slice.

## Findings

### m-1: D81's scan gate can pass before the dissolution that emptied the list is in the history

`rust/crates/dw-engine/src/masternodes/shared/dissolve.rs:367-377` (`dissolution_list`) and `:424` (the gate), `rust/crates/dw-engine/src/masternodes/follow.rs:57-71` (`read_list`), `rust/crates/dw-engine/src/events.rs:319-321` (`WalletState::tip`).

D81 rests on one premise: once the wallet has scanned to the list's height, any other party's dissolution that removed the masternode is in the history. Two things break that premise.

1. **The height and the entries come from two different reads.** `read_list` takes the height from the tracker's progress reading (`follow.rs:58`, `sync.rs:386-390`, fed by `on_progress` events). It then takes the entries from the SPV engine's `latest_masternode_list()` (`follow.rs:60`). Nothing ties the two to one list. The engine applies a block's diff before the progress event that reports it reaches the tracker, so the entries can be the list at H+1 while the height still reads H.
2. **`scanned` is `tip()`, which is `max(processed_height, synced_height)`.** Only `synced_height` certifies a contiguous scan. dash-spv at the pinned rust-dashcore 626c91d (`dash-spv/src/sync/filters/manager.rs:590-634`) advances it per committed batch, and only when the wallet was already certified up to the batch's start ("every matched block below `end` is already applied"). `processed_height` follows each `BlockProcessed` and can run ahead of a batch that hasn't been committed yet; the contiguity guard there exists because batches commit with gaps.

**Failing sequence (1).**
- A's share 0 sent dissolve `D_A` with penalty + fee equal to the share, so its actor output is omitted and the row is "unseen".
- B's unilateral dissolution `D_B` is mined at H+1. It pays A's refund script `amount + bonus`.
- The SPV engine applies H+1's diff, so the masternode is gone from the list.
- The tracker's masternode reading is still H, and the wallet's filter for H+1 isn't committed, so `scanned == H`.
- `operations()` reads height H, entries without the masternode, and scanned H. `D_A` reads `confirmed{H}`.
- After the H+1 scan it reads `conflicted`.

**Failing sequence (2)** is the same with the masternode list caught up during a catch-up sync while a lower filter batch, the one holding `D_B`, is still uncommitted and a later matched block has already been processed.

**Wrong state.** The wallet reports that A's unilateral exit happened, with A paying the penalty, when B's did. No funds go to the wrong place (A actually received more), but the operation reports the wrong outcome. The same read feeds the D23 check (`check_no_pending_maintenance`) and `resend_operation`'s state gate. For (1) the window is narrow but falls exactly on the block that dissolves the masternode.

**Fix.**
- Gate on `synced_height`, not `tip()`.
- Take the gate's height from the same list as the entries. Either use the engine list's own height, or re-read the tracker height after the summaries and require `scanned ≥ max(before, after)`.

The unit test (`tests/dissolve.rs`, `scanned` helper) sets `synced_height == processed_height` and gives the list a single height, so it can't see either cause. Add a case with `processed_height > synced_height`.

### m-2: E11 is a cheap survivor on a sending path, and the guard matters more than D83 says

`rust/crates/dw-engine/src/masternodes/shared/dissolve.rs:279-283`.

D83 says a dust row "would hold the masternode's maintenance (D23) for nothing". It's worse than that.

Without the refusal, `deliver` hands the dust transaction to peers. Unless a reject reaches the wallet, the attempt ends `Out`, so the row reads `sent`. A `sent` row can't be discarded (MG-04 D6), and it's never mined or conflicted. So `check_no_pending_maintenance` refuses every later `update_share` and `dissolve` of the masternode, including the penalty-free dissolution after the early period, until the masternode leaves the list some other way. The actor's principal then has no exit through this wallet.

A dust actor output is reachable. Core bounds the penalty below the smallest share (`providertx.cpp:120`), so a 300-DASH share with penalty 300 DASH − 1,100 and fee 1,000 leaves 100 duffs.

**Cheap test (probe run, see "What I ran").**
- Make the fixture's `request()` penalty overridable. In the probe it's a thread-local; a `request_with(penalty)` would do.
- Wallet 1 (B, 300) dissolves share 1 with penalty `300 COIN − 1_100`.
- The preview gives `returns[1].amount == 100` and `[ActorOutputDust]`.
- `dissolve` refuses `invalid_argument` with "dust", and nothing is sent.
- With fee 1,100 the actor output is omitted and the dissolution sends with 2 outputs.

**Fix.** Add this test, which kills E11 and also covers D83's "raise the fee to omit the output". Correct D83's rationale to say the stuck row is `sent`, not discardable.

### n-1: D81 reads `confirmed` when the registration itself is reorged out

`dissolve.rs:415-436`.

`list_settled` checks only that the masternode is absent from the synced list.

**Sequence.**
- Registration R is mined at h and listed.
- A sends an unseen dissolution `D_A`.
- A reorg drops R's block. R isn't re-mined, or a contributor double-spends a funding input.
- The list at the new tip lacks the masternode, and the scan has reached it.
- `D_A` reads `confirmed{H}` although nothing was dissolved.

**Fix.** Apply the rule only while the session is still `Confirmed` (`shared_confirmed(wallet, hash)` finds R in a block). Reorg-only, so a nit.

### n-2: D23 settles at one confirmation, and resends skip it

`rust/crates/dw-engine/src/masternodes/shared/update.rs:322` (any `confirmed` counts as settled) and `rust/crates/dw-engine/src/masternodes/protx.rs:687` (only `update_registrar` resends take the update lock and run `resend_conflict`).

**Sequence.**
- `update_share` U is mined at h, one confirmation, and reads `confirmed`.
- `dissolve` D is allowed and sent.
- A reorg drops U's block, so U reads `sent` and is back out.
- U and D are now both pending against one registration.
- If D mines first, U is invalid forever. U's row stays `sent` with its fee-input locks (D69) and can't be discarded.

The same pattern existed for U against U in 1c, and it needs a reorg.

**Fix.** For D23, count `confirmed` as settled only once `buried()`, as D70 does for locks. Alternatively, note the reorg case in D80.

### n-3: the oracle vectors never put the actor last or omit its output

`rust/crates/dw-protx/tests/oracle_shared.rs:560-591` and `testdata/shared_maintenance.json`.

The four `dissolve_unilateral` vectors use actor 1 of 3 (three times) and actor 0 of 2. None has the last share as the actor, and none omits the actor output. The unit test covers omission against the builder, not against dashd.

**Fix.** Add both shapes when 2b/2c regenerate the file. Not required now.

### n-4: a stale fixture comment, and the production registration-bytes path is tested only end to end

`rust/crates/dw-engine/src/masternodes/shared/tests.rs:1400-1401` still says "The pinned library reads no version-3 payload (BL-187)". `listed()` now depends on the history carrying the payload, and so does production `registration_bytes` (`dissolve.rs:381-392`), which serializes the history entry for the vault. If that entry ever lacks the payload or doesn't re-serialize byte for byte, every dissolve fails in the vault ("not a shared ProRegTx" or "the registration of another masternode"). Only `dwd_shared.py`'s D step covers that path.

**Fix.** Update the comment, and keep D in the gated functional suite.

## What I checked

**Funds, from preview to send to resend.**
- `plan_dissolve` builds the outputs from `decode_frozen(session).payload.shares`. `shared_confirmed` requires `session_hash == pro_tx_hash`, so the shares are the registration's.
- Non-actors come in share order with `amount + SplitAmountByShares(penalty)` and the remainder to the last non-actor. The actor comes last with `amount − penalty − fee`, omitted at 0, using checked subtraction.
- The send re-plans under the update lock and the fence after the D23 check. It charges `penalty + fee` against the grant; dwcli sizes the grant at the preview's `total_debit`, so a penalty that appears after the preview, say after a reorg, is refused (`send.grant_exceeded`, tested).
- The signed copy differs from the planned one only in its payload signatures (`Dissolution::signed`, unit-tested).
- On resend, `row_debit` (protx.rs:714) charges the stored `penalty + fee` and sends the stored bytes. The fee inputs are empty, `input_coins` finds nothing, and the collateral is reserved in memory only (D80).
- D23 runs both ways: `check_no_pending_maintenance` covers both kinds and is called by both senders under `update_lock`. Both directions are tested.

**Core parity** against v24.0.0-rc.3.
- `BuildDissolution`, `providertx_service.cpp:642-681`: the input is the collateral with an empty scriptSig and final sequence, lock time 0; the refusal texts and order match; the actor output is omitted at 0.
- `SplitAmountByShares` and `ProRataFloor`, `providertx_util.cpp:88-115`: matches, with a u128 intermediate.
- `DissolveShared`, `:1242-1279`: early is `tip+1 − registered < period`, the penalty applies iff early, there is one signature, `MakeSignHash(tx, 1)`, and it's signed by `shares[actor].keyIDOwner`.
- `CheckProDisTxForList`, `specialtxman.cpp:1676-1786`, and `CheckProDisTx`, `:1788-1812`: the masternode must exist at pindexPrev, and the spend height is pindexPrev + 1.
- `is_early` and `penalty_free_from` equal Core's test and `registered + period` (D79, D82).
- Dust: `IsStandardTx`, `policy.cpp:139-141`, with `DUST_RELAY_TX_FEE` 3000 (`policy.h:47`). The wallet's `dust_threshold` is `3 × (output size + 148)`, which is 546 for P2PKH. Non-actor outputs are at least their share, so only the actor output can be dust.
- Relay floor: `DEFAULT_MIN_RELAY_TX_FEE` 1000 (`policy.h:49`), applied to the exact size with one 65-byte signature. Sigops (at most 8 × 20) never exceed that size.
- The empty scriptSig spend of the template is standard (`AreInputsStandard`, `policy.cpp:185-188`).

Two conservative differences, both documented and not findings:
- The wallet refuses while the registration is in the list's tip block (m-16, one block deep), where Core would take a spend at tip + 1.
- A list lagging the tip can charge a penalty Core's RPC wouldn't. That penalty is valid, shown in the preview, and capped by the grant (D79).

**State (D81).** I read `list_settled`, `state_of` and `conflict_height`, the scan heights in `events.rs:551-576` and dash-spv's filter commit, `read_list`, and the tracker. Findings: m-1 and n-1.
- PoSe bans don't remove a masternode from the SML (`MasternodeListSummary.is_valid`), and a shared collateral is spent only by a ProDisTx. So nothing except a dissolution, or a reorged-out registration (n-1), empties the entry.
- A restart reads `sent` until the masternode phase is synced again.
- A reorg lowers both heights (`lower_heights`).

**E10.** The only signing path is `send_dissolve` → `VaultSigner::sign_dissolution` (`dw-vault/src/shared.rs:143-188`, unchanged here). It reads the actor's key id from the history's registration, whose txid must equal `proTxHash`, and refuses before `sign_compact`. Beyond that, `plan.check(vec![sig], true)` re-verifies the signature with Core's rule against the same key. So E10 is an equivalent mutant twice over, and needs no test.

**Refusal order** in `preview_dissolve` matches the claim: `shared_registration_unknown`, `shared_too_early`, `share_not_mine`, then the builder's and relay `invalid_argument`s, then `protx_refused`. The fee bounds come before the wallet.

## What I ran

- Reflink copy at `/work/scratch/mg04b-2a-r1-opus` (left in place), with `TMPDIR` inside the copy, nice 19 and `CARGO_BUILD_JOBS=8`.
- Probe for m-2: a thread-local penalty override in `tests.rs`'s `request()`, the test `review_probe_dust_actor_output_refused_on_send` in `tests/dissolve.rs`, and the E11 mutant gated by `REVIEW_MUT_E11` in `dissolve.rs`.
- Build: `cargo test -p dw-engine --lib --no-run` as a user unit, 20m56s, exit 0.
- `dissolve` filter: the 9 slice tests and the probe pass (10/10).
- Probe with the guard on: passes (the preview gives an actor output of 100 and `[ActorOutputDust]`, the send is refused, and fee 1,100 sends with 2 outputs).
- Probe with `REVIEW_MUT_E11=1`: fails at `tests/dissolve.rs:483` (`unwrap_err`), because the dust dissolution was signed and sent. So E11 is killed by a ~25-line test.
- I didn't run `dwd_shared.py` or the oracle test. The byte-for-byte oracle claim rests on reading `test_QT_127_build_dissolution_equals_dashds_shared_dissolve` and its four vectors.
