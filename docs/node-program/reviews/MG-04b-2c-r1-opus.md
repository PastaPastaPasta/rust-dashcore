# MG-04b slice 2c review, round 1 (Opus)

Branch `dw/mg-04b-2c`, head `a126d989` on desktop main `e521c935` (23 files, +2699/−50). I read the whole diff, Core rc.3 and dash-qt rc.3. I also ran the oracle test, the `dw-protx` unit tests and the engine `together` tests in a reflink copy (see "What I checked and ran").

## Verdict: ACCEPT-WITH-NITS

There is no M finding. On every funds path (start, import, approve, send) the request is checked byte for byte against `BuildDissolution(mn, actor, 0, fee)`. Each signature is verified against its share's owner key over `MakeSignHash(tx, n)` before it is stored. Each signature the vault produces is verified the same way before it is written. Signatures are combined in share order. Core's rules then run on the combined transaction with signatures checked, right before the hand-off. So the wallet can't sign, store or send a dissolution whose outputs, amounts, order, actor, fee, lock time, version or sequence differ from what it checked. A request or signature set dashd would refuse can't reach `send_maintenance`.

The actor fee is charged only in the call that signs the actor share. A non-actor is never charged. The fee is capped by `check_fee`, and the relay floor is checked at all four points. D23 holds in every direction among sends, because a together send is a `dissolve` row.

There are two m findings:
- m-1: an unsent session reads `dead{delisted}` while its own dissolution is mined but not yet scanned. D81 guards against exactly this for operations, and D94 lacks that guard.
- m-2: test gaps on the stage and send paths. Four cheap mutants would survive.

Counts: M 0, m 2, n 6.

## Findings

### m-1: D94 calls a session `dead{delisted}` from the list alone, before the wallet's filter scan has reached the list's height

- **Where:** `rust/crates/dw-engine/src/masternodes/shared/together.rs:296-300` (`delisted`). The view comes from `together_read`, `:357-369`, which reads only the `ListView`.
- **The rule elsewhere:** D81's `list_settled` (`dissolve.rs:573-595`) settles a row by the list only once `view.scanned >= view.list_bound`. Its reason (D81, design doc row D81): "Until the scan reaches the list's height, the dissolution that removed the masternode may be another's not yet in the history." The together stage has the mirror case: the dissolution that removed the masternode may be this session's own, not yet in the history.
- **Sequence:**
  1. A prepares, B approves, C approves and sends. A and B never import C's final reply.
  2. The dissolution is mined at height H. A's wallet was offline. On restart its masternode list syncs to the tip within seconds, but its filter scan (`synced_height`) is still below H.
  3. A's session: `combined(mn)` is `None` (A lacks C's signature), and `out_at` is `None`. `collateral_spend` finds nothing, because the history doesn't hold block H yet. `delisted` is true because the list is synced, above the registration, and without the masternode. So the stage is `dead{delisted}` and `next` is `nothing`.
  4. Once the scan passes H, the same session reads `confirmed{H}` through `signs_request`.
- **Wrong state:** for the whole scan lag (minutes for a wallet that was offline for days), the session says another transaction dissolved the masternode, when it was this request. `discard_shared_session` accepts it then (`out_at` is `None`, `shared.rs:856`). A user who trusts `dead` deletes the record of the dissolution they signed. Approve and send refuse `shared_stage{dead}`, which is harmless here. No funds move wrongly, because every output pays the refund scripts. Only the state is wrong. The regtest's last assertion (`t1` reads `dead`) runs after `wait_mined` on B, so it can't see this.
- **Fix:** in `together_read`, read D81's `list_bound` and `scanned` the same way `dissolution_view` does (`dissolve.rs:433-447`). Require `scanned >= list_bound` before calling the session `dead`. Until then it reads `combined` or `signing`. Add D81's sentence to D94. Test: `delisted(&f, 105)` with `scanned(&f, 104, 105)` reads `signing`, and then `scanned(&f, 105, 105)` reads `dead`. In `test_QT_127_a_delisted_masternode_kills_the_session`, call `scanned` first, so the test still says what it says now.

### m-2: four cheap mutants on the stage and send paths survive the engine tests

None of the engine tests reach these:
- `together.rs:277`: `Some(ProTxOperationState::Conflicted) => return SharedStage::Conflicted`, mutated to `return SharedStage::Sent`. No engine test reaches `conflicted`. Only `test_QT_127_dissolve_together_stages_decode` names it, as JSON.
- `together.rs:293`: `Some(_) => return dead` (the collateral spent in a block by a transaction other than the request), mutated to `Some(_) => {}`. The only `dead` test (`:589`) empties the list, which gives `dead` through `:300` anyway.
- `together.rs:854`: `foreign: owner_index.is_none()`, mutated to `foreign: true`. The three-wallet test asserts `foreign: true` for C, a non-actor sender. No test sends from the actor's wallet, so `false` is never checked. On a together row `foreign` no longer changes the charge (`together` already makes `debit()` 0), but it still makes the row an `unseen_dissolution`. That moves a row from the actor's wallet onto D81's list rule.
- `together.rs:929-933`, the second-signature arms of `absorb`:
  - The "different, valid second signature" arm (`Some(_)`) is never reached. The test's second signature for share 0 is `[8; 65]`, which fails `verifies` first, at `:923`. So `Some(_) => { sigs.insert(index, *sig); taken += 1 }` (replace the held signature) survives.
  - The byte-identical-repeat arm (`:929`) can be made to warn without any test noticing. `"held already"` at `tests/together.rs:252` asserts only `changed`, not `warnings`.
- **Fix (tests only):**
  - Send one session from the actor's wallet and assert `foreign: false`.
  - Mine a different dissolution of the masternode (2a's `dissolve_now` from another wallet, mined) under an unsent session, with the list still listing the masternode. Assert `dead`.
  - Mine a conflicting dissolution under a sent session. Assert `conflicted`.
  - Sign share 0 a second time with a different nonce (`secp256k1::sign_ecdsa_recoverable_with_noncedata`, or the vault twice if it isn't deterministic). Assert it's skipped and that the first signature is kept.
  - In the "held already" import, assert `warnings.is_empty()`.

### n-1: `prepared_here` in m-15 is unreachable

- **Where:** `together.rs:783-787`.
- A coordinator's session is created only by `start_together`, which requires the actor share to be the wallet's (`:590`) and signs every unsigned share of the wallet, the actor among them (`:618-635`). So at a coordinator's `approve_maintenance` the actor share is never in `to_sign`, and `!prepared_here` never decides anything. The mutant that drops it is equivalent.
- `mine` can grow when owner keys 0–19 are found later, but the actor share was already in `mine` at start.
- **Fix:** drop the term, or keep it and say in a comment that it is defensive. Mutant runs should list it as equivalent.

### n-2: D23 at approve and start ignores this wallet's own signatures in other together sessions

- **Where:** `together.rs:585` and `:766`. `check_no_pending_maintenance` (`update.rs:310-362`) reads `masternode_ops` rows only.
- **Sequence:** A, the actor, starts R1 at a fee of 100,000 and is charged 100,000. A then starts R2 for the same masternode at 150,000 (another sign hash, so another session) and is charged 150,000. A 2a `dissolve` from A after either is charged again. At most one of them can be mined, and the charges aren't refunded when a session dies.
- No funds move wrongly. Each charge is authorized by its own call's grant, and a together session from another wallet can't be seen anyway. But the comment at `:764-765` says such a signature "is never charged", which holds only for operation rows.
- **Fix:** reword the comment and D95 to say "while an operation of the masternode is pending". Or, if you want the stronger rule, refuse starting or approving the actor share while another live (`signing` or `combined`) together session of the masternode holds this wallet's actor signature. I'd take the rewording.

### n-3: D95's "can't be discarded" residual has a cheap fix

- **Where:** `shared.rs:856`.
- `hand_off_dissolution` saves the operation row durably (`dissolve.rs:393`) before `deliver`, so a together row with `out_at` set and no `dissolve` row carrying its `txid` never handed its bytes to the network.
- **Fix:** for `kind == dissolve_together`, allow discard when no operation row has the session's `txid`. Other wallets may still send the same bytes. That changes nothing here: the session is local bookkeeping, and the outputs pay the refund scripts.
- D94's residual (a wallet the dissolution pays nothing) really is residual. Core's `CCollateralShare::MIN_AMOUNT` is 100 DASH (`providertx.h:58`, enforced at `providertx.cpp:79`) and `MAX_FEE` is 1,000,000 duffs, so a share-holding wallet is always paid. Only a wallet holding no share is affected, and it reads `dead` once the masternode is delisted. The same arithmetic means `check_dust` (`:102-111`) can't fire on a valid registration, so its mutants are equivalent.

### n-4: the contract's `send_maintenance` order differs from the code's

- **Where:** `docs/contracts/mn-engine.md:197`. It lists "the signatures inserted …, Core's rules with them, the relay minimum and D83, `update_pending`".
- The code runs `update_pending` straight after the stage (`together.rs:829`), before the registration, the check, the combine, the rules, the relay floor and dust (`:831-843`). That matches `approve_maintenance`'s contract row, which says "then `update_pending`".
- **Fix:** move `update_pending` to just after `shared_stage` in the send row.

### n-5: two plan items were dropped without being listed as departures

- **Plan "Regtest":** "`testmempoolaccept` on a fee below the floor". Step T has B refuse fees of `1` and `floor − 1`, and it runs `testmempoolaccept` only on the transaction at `floor`. That shows the floor is enough, not that it equals Core's. Analytically the two are equal: at `DEFAULT_MIN_RELAY_TX_FEE` 1,000 duffs/kB, `CFeeRate::GetFee` is `ceil(1000·size/1000) = size` (`policy/feerate.cpp:23-35`, `policy.h:49`), and the engine test pins `size` against `size − 1`. Either run `testmempoolaccept` on dashd's `shared_combine` of B's request at `floor − 1` (prepared by `protx shared_dissolve_prepare … floor-1`) and expect `min relay fee not met`, or record the drop.
- **Plan "approve_maintenance":** "New code `masternodes.shared_actor_fee{fee}`, added to Swift `engineCodes`". Swift's `engineCodes` (`Sources/WalletRuntime/Contracts/ServiceError.swift:249`) holds no `masternodes.*` code, and `engineCodesMatchTheContractTable` reads only the m2 and m3 contracts. So there was nothing to add it to. The decode-test departure has the same cause.
- **Fix:** record both under the departures in the hand-back and the DEC row.

### n-6: after the masternode leaves the list, importing a late reply refuses `shared_too_early`

- **Where:** `together.rs:695`. `registered` applies m-16, so a synced list without the masternode gives `shared_too_early`.
- **Sequence:** after the dissolution is mined, A pastes C's final reply. A gets "too early", while the stored session reads `confirmed`.
- That's cosmetic, and it is the same in 2b's standby import. Note it for MG-10b's wording, or read the stage first, as approve and send do. No change is needed for 2c.

## Answers to the brief

### 1. Funds

- **Prepare → approve → import → combine → send.**
  - `start_together` builds with `together::prepare` and runs `check` on its own output, so a builder drift is an `Internal` error, not a signature.
  - `sign_together` passes the vault the request bytes and `n`, then requires `verifies(&mn, checked.sign_hash, share, sig)` (`:539-541`). A vault that signed anything else, or with the wrong key, writes nothing and gets its charge back.
  - `import_together` runs `check` on the received request (D92 strict equality, `dw-protx/.../together.rs:64-94`). It takes a signature only if `verifies` passes (`:923`).
  - A session id commits to the sign hash. The sign hash covers every byte `check` doesn't fix itself (the empty scriptSig is fixed by equality). So one session holds one request.
  - `send_together` runs `check` again, combines in share order and runs `check_dissolution(…, check_sigs = true)` (`:840`) before `mark_shared_session_out_durable` and the hand-off.
  - Stored signatures only ever grow. `absorb` never replaces one, and `update_shared_session_durable` keeps `signed_tx` and `txid` (`COALESCE`, `shared_sessions.rs:228-231`). So a retry of the send carries the same bytes, and the mark-out's `signed_tx = ?3` guard holds.
- **Refusals dashd would make.** These are covered by `check`, and the oracle shows it:
  - a share paid to the wrong script (`actor_paid_elsewhere`, dashd `bad-prodis-payee`);
  - a short non-actor share (`non_actor_short`, `bad-prodis-penalty-floor`);
  - a fee over the ceiling (`bad-prodis-fee`);
  - and the lock time, which dashd accepts (D16, refused by D92 as intended).
  - Missing or duplicate share signatures can't occur: signatures live in a `BTreeMap<u16, _>`, and `combine` refuses a short count with Core's text (`InsertUnanimousSignatures`, `providertx_service.cpp:705-729`).
  - CombineShared's "duplicate shareIndex" (`:1186-1188`) has no wallet counterpart, because a map can't hold two. Since `absorb` keeps the first, a dash-qt user and this wallet both end with one signature per share.
- **The actor fee.**
  - It is charged once per wallet per session. The charge applies only when the actor share is in that call's `to_sign`. `unsigned()` excludes held signatures, so a second call never re-signs. Across sessions, see n-2.
  - A non-actor is never charged (`to_sign ⊆ mine`).
  - The fee can't exceed `PRODIS_MAX_FEE` (`check_fee` through `build_dissolution`, and `check`'s equality).
  - The relay floor at the combined size is checked at start, import, approve and send (D96).
- **D23.** `send_together` runs `check_no_pending_maintenance` under `update_lock` (`:820`, `:829`). A sent together row is a `dissolve` row, so 2a's `dissolve`, 2b's `send_standby` and `update_share` all see it (`test_QT_127_three_wallets…` checks `dissolve_now` → `update_pending`). The reverse direction is tested at `:355-371`. Approve's D23 check is tested at `:376-386`.
- **Skipped entries.** A skipped entry is never stored. A later valid entry for the same share is taken only if the map has none, so it can't be counted twice. Every stored signature verified over this session's sign hash, and the send re-verifies all of them, so a skip can't lead to a combined transaction Core rejects.

### 2. Core parity (rc.3)

| What | Core / dash-qt | Wallet | Verdict |
|---|---|---|---|
| PrepareSharedDissolution | `GetSharedMn`, then `BuildDissolution(dmn, actor, 0, fee)`: actor range, fee > 0, fee ≤ `MAX_FEE`, actor overdrawn, in that order (`providertx_service.cpp:1281-1296`, `:641-681`) | `prepare` = `build_dissolution(mn, actor, 0, fee)`, the same order (`dissolve.rs:97-112`) | same |
| Sign hash and count | `MakeSignHash(tx, shares.size())` (`:1292-1294`); `CProDisTx::MakeSignHash` (`providertx.cpp:506-533`) | `sign_hash(sig_count(mn))`, fields in Core's order (`dw-protx/src/shared/mod.rs:203-240`); the oracle's `signHash` matches | same |
| CombineShared | size check, then per-index lookup in order, "missing signature for share %d" (`:715-726`); duplicate refused at parse (`:1186-1188`) | `combine`: `Count` with Core's text, then `Missing(i)`; duplicates can't occur | same |
| Fee floor and ceiling | consensus `(0, MAX_FEE]` (`specialtxman.cpp:1762-1763`); relay `GetFee` = `size` at 1,000/kB | `check_fee` and `check_relay_fee_of_size` at the combined size | same (n-5 on evidence) |
| dash-qt reader | `SharedMnReadDissolution` (`sharedmnwidgets.cpp:272-352`): it doesn't check lock time, version, sequence or the actor output's value | D92 equality, stricter as approved (DEC-917) | intended departure |
| dash-qt addSignatures | skips each bad entry; an identical repeat is silent; a second different one is a problem; "does not contain any approvals" for a non-array (`sharedmndialogs.cpp:559-613`, `:572`, `:607-608`) | `absorb`, `SkippedSignatures`, `NoApprovals` | same |
| dash-qt bare array | `importEnvelopeChecked` returns false when nothing was absorbed and something was wrong (`:690`) | `Ok` with `changed: false` and the warning | same information, a different shape; acceptable |
| dash-qt preparer | only its own shares as actor (`:1373-1375`); fee field 1,000–1,000,000 duffs, default 100,000 (`:67-71`) | `share_not_mine`; any fee ≥ the relay floor; dwcli default 100,000 | the wallet goes below dash-qt's 1,000 field floor only for fees dash-qt's reader still accepts; fine |

### 3. State

- The stages are derived on every read from the row, the history, the op row and the list. A restart changes nothing (tested at `:619-635`).
- A reorg that takes the dissolution out of its block takes `confirmed` back to `sent`, `combined` or `signing`. The history is re-read each time.
- `confirmed` before mining: possible only through D81's documented list-lag residual on a sent `foreign` row, or on a row with the actor output omitted. That needs a sender holding no share (D87's residual, which now reaches together rows too). The history paths need a block.
- Pending forever: no path for a share-holding wallet. A wallet holding no share reads `dead` after delisting (D94, a real residual, n-3).
- The masternode leaving the list for another reason:
  - for an unsent session it gives `dead`, never `confirmed`;
  - for a sent row, the `scanned ≥ list_bound` guard lets the history's `conflicted` win first.
  - m-1 is the opposite error: `dead` for this session's own dissolution while the scan lags.
- D94 is truly residual. D95 has the cheap fix in n-3.

### 4. Rulings (my input)

- **Serde-JSON decode test instead of a Swift test: acceptable.**
  - There is no Swift `SharedStage` decoder and no Swift `masternodes.*` code table (n-5), so DEC-917's "Swift decode test" had nothing to attach to.
  - `test_QT_127_dissolve_together_stages_decode` pins the wire encoding that a future decoder must read.
  - Condition: MG-10b, which adds the Swift decoder, adds the decode test against the same JSON strings.
- **Skipping bad entries with a warning: right.**
  - dash-qt `continue`s past every bad entry, takes the rest and keeps the tx (`sharedmndialogs.cpp:584-613`, `:720-724`).
  - Refusing the whole message would strand valid replies and would differ from the reference wallet.
  - The plan's "refuses the whole message" was the departure from dash-qt. The code follows dash-qt, D92 records it, and nothing unverified is ever stored.

### 5. Mutants that would survive

These are listed under m-2:
- `:277` Conflicted → Sent;
- `:293` dropped;
- `:854` `foreign: true`;
- `:930` replace-held;
- `:929` repeat-warns.

These are equivalent (n-1 and n-3), and the run should list them as such:
- `!prepared_here` dropped;
- `check_dust` dropped at approve and at send;
- start's `if actor_signing { fee } else { 0 }` → `fee`. At start the actor share is always unsigned when anything is.
- Defensive, and survives because import already verifies: `check_dissolution(…, true, true)` → `…, false)` at `:840`. Worth keeping.

## What I checked and ran

- **Read:**
  - the full diff `e521c935..a126d989`: `dw-protx` `together.rs`; engine `together.rs`, `shared.rs`, `dissolve.rs`, `flow.rs`, `standby.rs`, `store.rs`, `shared_records.rs`, `errors.rs`; the tests; dwcli; regtest step T; the generator; the vectors; the contract; design rows D45 and D92–D98;
  - the 2a/2b code that 2c relies on: `check_no_pending_maintenance`, `list_settled`, `unseen_dissolution`, `dissolution_view`, `hand_off_dissolution`, and the appdb `update_shared_session_durable` and `mark_shared_session_out_durable`.
- **Core rc.3** (`git show v24.0.0-rc.3:`):
  - `evo/providertx_service.cpp:641-729` and `:1171-1296`;
  - `evo/specialtxman.cpp:1580-1790`;
  - `evo/providertx.cpp:506-533`;
  - `evo/providertx.h:58, 134-135`;
  - `policy/feerate.cpp`;
  - `qt/sharedmndialogs.cpp:240-300, 520-730, 1354-1548`;
  - `qt/sharedmnwidgets.cpp:272-352`.
- **Ran** in the reflink copy `/work/scratch/mg04b-2c-r1-opus` (target `tgt/`, TMPDIR inside it, `CARGO_BUILD_JOBS=8`, nice 19; `/work` was 277G and then 246G free, 5-minute load 13–44):
  - `cargo test -p dw-protx --test oracle_shared together`: `test_QT_127_dissolve_together_equals_dashds` ok.
  - `cargo test -p dw-protx --lib together`: 3/3 ok.
  - `cargo test -p dw-engine --lib together`: 11/11 ok. That is the 10 `test_QT_127_*` together tests (three wallets, D23 both ways, the start of an imported request, m-15, import refusals and skips, delisting, restart, the switch, stage decode), plus one unrelated backup test the filter matched.
- **Not run:** the regtest step T (it needs dashd rc.3) and any mutants (the implementer's slot). I didn't touch `/work/scratch/dw-mg-04b-2c`. My copy is left in place, as the brief says.
