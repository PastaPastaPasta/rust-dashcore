# DW-MG-04 r3: independent Opus-h review (narrow: delta 481532bd..36af6e7b)

- Branch: `dw/mg-04-protx-owner` @ `36af6e7b`. Round commits:
  - `b504fc84`: r2 fixes (D54, D55, n-2, n-3, n-5, n-6).
  - `0b15effc`: D55 widened to every keys-only account.
  - `d27886bd`: docs.
  - `36af6e7b`: rustfmt.
- Counterpart: Dash Core v24.0.0-rc.3. Read from the tag in `~/workspace/dash`:
  - `wallet/wallet.cpp:1088` (`AddToWallet` → `LockProTxCoins`) and `:2856-2872` (`ListProTxCoins`/`LockProTxCoins`);
  - `node/interfaces.cpp:1450-1462` (`listMNCollaterials`);
  - `txmempool.cpp:706-748` (`removeUnchecked` → `removeUncheckedProTx`).
- Method: read-only. A detached worktree `/work/scratch/mg04-review/r3/wt` with a reflinked target and its own TMPDIR, 12 jobs.
  - One build with 11 env-gated mutants (`MG4MUT=…`, `scripts/mutants_r3.py`, `scripts/run_mutants_r3.sh`).
  - One throwaway behaviour probe (`scripts/r3_probe.diff`, log `probe.log`).
  - The four dashd vectors re-derived with the r1 Python script.
  - The worktree and target are deleted. Kept: the scripts, `mut-logs-r3/`, `r3-mut-part{1,2}.out`, `build-r3.log`, `probe.log`.

## Verdict: **ACCEPT WITH NITS**

Every r2 finding the round claims is closed, and I verified each one.

- **D55:** I found no value-bearing path that is wrong. The cap now binds a payment to every keys-only account. I probed all 9 types, through Send, custom change and PSBT. A self-transfer to Standard or CoinJoin is still uncapped.
- **D54:** it matches Core's locking rule, and its fence claim holds.
- **n-2:** the row+locks insert is one SQLite transaction on every insert path.
- **Contract v5:** consistent.
- **Bytes:** the vectors are byte-equal.

The one Minor is a test gap. The behaviour is right; my probe shows it. The rest are nits.

---

## Findings

### Minor

**m-1. D55's widening and its funds side are untested. Mutants r3a and r3e survive.**

- Where: `dw-engine/src/coins.rs:118` (`.filter(|account| account.as_funds().is_some())`). The only test is `send/flow_tests.rs:1385` `a_payment_to_an_own_provider_key_leaves_the_wallet`, and it exercises the owner and voting keys only.
- **r3a** (keys-only accounts that aren't provider accounts, i.e. identity and asset-lock, taken as funds): 0 of 391 tests fail.
  - That is the r2-round validator's widening in `0b15effc`, and nothing pins it.
  - A regression would reopen r2 m-1 for an identity-registration or asset-lock address: a quick-unlock grant could move the whole balance there.
- **r3e** (CoinJoin and DashPay receiving dropped from "funds"): 0 of 391 tests fail.
  - Nothing pins that a self-transfer to the wallet's CoinJoin or DashPay address stays uncapped and shows as `is_mine`.
- The behaviour is correct today. My probe (`scripts/r3_probe.diff`) walks every non-contact account of the fixture wallet:
  - It asserts `wallet_owns` for each account's first address, and that `wallet_funds_address` is true exactly when the account is `as_funds()`.
  - It then pays 0.1 DASH to each address with a fee-only `Spend` grant.
  - All nine keys-only types refuse with `GrantExceeded{max_duffs: 226, outflow: 10000226}`: IdentityRegistration, IdentityTopUpNotBound, IdentityInvitation, AssetLockAddressTopUp, AssetLockShieldedAddressTopUp, and Provider Voting/Owner/Operator/Platform.
  - The two Standard addresses and the CoinJoin address prepare under the fee-only grant.
  - (The fixture has no DashPay receiving account. key-wallet maps it to `ManagedAccountRef::Funds`, `managed_account_collection.rs:934`.)
- Fix: fold a version of the probe into `flow_tests.rs`. It is about 60 lines and kills both mutants. **Not blocking.**

### Nits

- **n-1. The r2 m11 mutant, re-sited at the engine, survives.**
  - The mutant has `save_operation` (`ops.rs:349-352`) call `insert_masternode_op_durable(&row, &[])` and then `lock_manual` per lock, as separate commits.
  - It fails no test: 0 of 391, including the appdb tests.
  - The appdb-level mutant **r3c** (locks committed in their own transaction before the row's) **is killed** by `a_row_and_its_locks_are_written_together`.
  - So the API is pinned but its use isn't. The API shape (locks are a parameter) makes the regression unlikely. Accept as is.
- **n-2. `refresh_external_registration` still writes locks and its row update separately** (`protx.rs:440-451` unlocks the old inputs *before* the fence; `:479-490` runs `lock_manual` per new input, then `reprepare_masternode_op`, as separate commits).
  - A crash between them leaves manual locks on the new inputs while the row still lists the old, already-unlocked ones.
  - `discard` then unlocks the old set, so the new coins stay user-locked until the user unlocks them in coin control.
  - No funds are at risk: the row is unsigned and nothing is out. This predates the round and is outside D53's wording, which covers the insert only.
  - A transactional `reprepare` taking the lock delta would make D53's "neither without the other" hold on this path too.
- **n-3. `address_no_key` for an owner address: the wording doesn't match D-row text.**
  - The engine side is right (`keys.rs:775` → `EngineError::AddressNoKey`; `session_lifecycle.rs:518` pins it).
  - The FFI doc comment (`dw-ffi/src/api/message.rs:14`, and the generated Swift `DashWalletCore.swift:17521`) still reads "valid but not P2PKH".
  - Both UIs show dash-qt's P2SH line for it: "The entered address does not refer to a key. Please check the address and try again." (`apps/desktop/src/tools/signVerify.ts:28-29`; Swift `SignVerifyViewModel.swift:158`).
  - The MG-04 design row says Core's "Private key for address … is not available". Both UIs already have that text, mapped from `address_not_mine`.
  - It's harmless and far better than the old `vault_locked` loop. Update the doc comment, and either relax the D-row wording or map the owner case to the "private key not available" text. Changing the FFI doc comment regenerates Swift bindings, so it falls under T0.

---

## 1. D55 money semantics

**Which accounts count as funds.**
- key-wallet `d3d50520` `ManagedAccountCollection::all_accounts` (`managed_account_collection.rs:898-938`) maps:
  - BIP44, BIP32, CoinJoin, DashPay receiving and DashPay external → `Funds`;
  - identity (4 kinds), asset-lock (2) and provider (4) → `Keys`.
- `own_accounts` drops `is_contact_owned` (DashPay external), so the funds set is exactly BIP44, BIP32, CoinJoin and DashPay receiving.
- Platform payment accounts aren't in `all_accounts`.
- The engine's history view (`history_ops::owned_scripts`) was already built from funds accounts only. D55 makes Send agree with what history shows.

**Can a funds address be misclassified as external?** No.
- Auto change is `next_change_address_for_account(0)` (BIP44 internal).
- The ProTx change and the FundNew collateral/payout come from `issued()` (BIP44, r2 §2).
- `get_address_info` on a funds account searches the same pools as before. For every funds address the result equals `wallet_owns`, which the probe confirms for Standard and CoinJoin.

**Is the cap enforced?** Yes, on every path:

| path | where | evidence |
|---|---|---|
| Send recipients | `send/mod.rs:1195` (`outflow_of`) | probe: all 9 keys-only types; r3h-adjacent test |
| custom change | `send/mod.rs:1199` | r3h KILLED; the D55 test's owner-change leg |
| contact send | `send/contact.rs:182` (same `outflow_of`) | by reading |
| `sign_psbt` / `analyze_psbt` outputs | `send/psbt.rs:207` | r3i KILLED |

- The PSBT input side is unchanged: `ownership.paths` still uses `own_address_info` minus D50's owner paths.
- So a keys-only input (voting, identity) still counts in "spent" while its outputs back to keys-only count as leaving. That direction is conservative: it can only over-count the outflow.

**`create_unsigned` derivations** (`psbt.rs:225`).
- Inputs come from `plan_funding` over the wallet's funds coins, so every input address is a funds address and keeps its record.
- Only an *output* to a keys-only address loses its record. That is what a hardware or external signer should see: it must not treat that output as change.
- r3g (a record for a keys-only output) is KILLED by the D55 test.

**The caller split, every caller by grep.**

| helper | callers | right? |
|---|---|---|
| `wallet_funds_address` / `own_funds_address_info` | `outflow_of`, `record_send_metadata` (`send/mod.rs:2166`), PSBT `ownership` outputs, `derivation_of`, the ProTx `summary_of` (`protx.rs:937`), labels `owns` (`labels.rs:95`) | yes: each is a "does the money stay" decision |
| `own_address_info` / `wallet_owns` | signing-path lookups (`keys.rs:769` sign_message, `coinjoin.rs:667`, `psbt.rs:189` inputs, `protx.rs:900`, `build.rs:822,896`), External-collateral refusal (`build.rs:655`) | yes: these are "is it the wallet's key" decisions |

- One stricter-than-Core corner: a coin sitting at one of the wallet's own provider or identity addresses can't be an `ExistingUtxo` collateral, because it isn't a funds coin. It can't be an `External` one either, because it is `wallet_owns`. Core would allow it unless it equals this registration's owner or voting key.
- It is reachable only after a user pays such an address, which is r2 m-1's recovery follow-up (the dwcli sweep). It needs no action now.
- The ProTx `summary_of` change matters for display only. A ProTx grant charges the fee (`charge_fee`), so the summary's `external_sent` doesn't feed a cap.

## 2. D54 against Core

**The Core rule, verified at the rc.3 tag.**
- `AddToWallet` → `LockProTxCoins(candidates)` (`wallet.cpp:1088`). Here `candidates` are the tx's own new wallet UTXOs.
- `ListProTxCoins` → `listMNCollaterials` (`interfaces.cpp:1450-1462`) keeps an outpoint only if `IsProTxWithCollateral(tx, n)` (a FundNew output) or `mnList.HasMNByCollateral` at the tip.
- So Core **never** locks an existing coin that a pending ProRegTx names, and its mempool frees every claim on removal (`txmempool.cpp:706-748`).
- D54's citations are accurate. The wallet's lock on a pending `ExistingUtxo` collateral is stricter than Core, and D54 releasing it on conflict returns to Core's state.

**Can the release unlock a coin a live registration still needs?**
- *A live row of this wallet*: no. The `named` set (`ops.rs:467`) is every non-conflicted row's `row_collateral`, built from terms whatever `locked_collateral` says, so it is conservative. r3b (ignore `named`) is KILLED, and so is r3k (count conflicted rows in `named`: nothing ever released).
- *A listed masternode*: released, then relocked in the same pass, under the same fence. The follower's `wallet_masternodes` relock runs right after `release_conflicted_collaterals` (`ops.rs:561-600`), so no send can interleave.
- *A winner not yet listed* (another device on the same seed won with the same coin, or a malleated copy of the row's own ProRegTx mined under a new txid): the coin is unlocked until the list reaches that block.
  - Core's wallet has the same window, since `listMNCollaterials` needs the list.
  - It is recorded (Follow-ups, m-3 + validator n-3).
  - Not a regression against Core.
- *Reorg*: the conflicting block is undone, so the row reads `sent` again. Its keys may have been reused, and its collateral stays unlocked until listed. This is documented in D54, and Core judges the clash. Same as Core.

**The fence claim.**
- `release_conflicted_collaterals` runs after `try_coin_fence` and reads the rows under it (`ops.rs:558-561`).
- `register` takes the fence (`protx.rs:142`) before `existing_coin` decides "already locked → not ours", and holds it through `save_operation` (`:242`). `prepare_external_registration` and `update_registrar` save under the fence too.
- So the "is it locked" decision and the row save happen atomically against the release.
- `protx_context` (`used`) is still read before the fence. That is the pre-existing n-7 class (two concurrent registrations of one coin), and D54 doesn't widen it.
- D54's "the new row doesn't own the lock" path: a conflicted R1 holds the lock, R2 takes the coin with `locked_collateral = false`, and R1's flag stays while R2 is live. That is consistent:
  - if R2 conflicts or is discarded, the next pass releases it;
  - if R2 is listed, the relock keeps it.
  - The test `mg04_a_conflicted_registration_frees_its_collateral_and_keys` (`tests.rs:2369`) walks it.
- `protx_context` skipping conflicted rows (`build.rs:586`): r3f is KILLED (2 tests).

## 3. n-2 atomicity

- `insert_masternode_op_durable` (`protx_ops.rs:87-127`) opens `unchecked_transaction` inside `durably` (FULL sync), writes each lock with `lock_manual_on(&tx, …)`, then inserts the row and commits once.
- Callers:
  - `register` (FundNew output + ExistingUtxo collateral + inputs), `protx.rs:242`;
  - `prepare_external_registration`, `:294`;
  - `update_registrar`, `:598`.
  - All go through `save_operation`, so it is one transaction.
- Resend writes no locks (they are already there, r2 §3).
- `release_masternode_op_collateral` (unlock + clear the flag) is one transaction too.
- The remaining split path is n-2 above (External refresh), which predates this round.
- r3c is KILLED by `a_row_and_its_locks_are_written_together`. The re-sited m11 survives (n-1).

## 4. n-6 contract

- `mn-engine.md` is Contract-Version 5, and the generated surface block and its sha are re-blessed: `mn_masternodes_contract` passes in my build.
- The §4 row text, the error-code bullet and the D51/D-error table name `operation`.
- `MasternodesError` reaches no IPC or FFI type:
  - consumers by grep are dwcli (`Failure { code, message: e.to_string() }`, so the operation id shows in the message) and the contract test;
  - there are no generated `apps/desktop/src/api/generated` types for it and no Swift decode.
- So **Swift T0 does not, and cannot, exercise the `ProtxRefused` decode**. None exists, and none is needed.
- n-5 (an update claims only its operator key, `keys.rs:91-106`) agrees with rc.3 `txmempool.cpp:656-659`.

**Swift T0 evidence (r2 m-4).**
- `/work/scratch/dw-mg-04/swift-t0-r2.log` (finished 13:45 UTC): "Test run with 568 tests in 73 suites passed … SWIFT EXIT 0". The bindings check reads "build-core: committed bindings match".
- It started after `0b15effc` (13:33). `d27886bd` and `36af6e7b` are docs and test formatting only.
- It covers this round's m1/m2 contract edits: the D55 Spend-cap and `create_unsigned` wording, and `sign_message` → `address_no_key`, a code Swift already maps (`DashKitError.swift:299`).
- **m-4 is closed.**

## 5. Mutants (one build; logs in `mut-logs-r3/`)

Binaries:
- dw_appdb lib, mn_masternodes_contract, session_lifecycle;
- dw_engine lib filtered `masternodes send labels coins address psbt money_gate history`: 391 tests in all.

The full dw_engine lib baseline was 1366 passed, 0 failed (7m52s). Filtered baseline `none`: 0 failing.

| # | mutant | site | result |
|---|---|---|---|
| r3a | D55 filter takes identity/asset-lock accounts as funds | `coins.rs:118` | **SURVIVED** → m-1 |
| r3e | D55 filter drops CoinJoin/DashPay receiving | `coins.rs:118` | **SURVIVED** → m-1 |
| r3b | D54 releases despite another live row naming the coin | `ops.rs:472` | KILLED (1) |
| r3k | D54 `named` counts conflicted rows (never releases) | `ops.rs:467` | KILLED (1) |
| r3f | conflicted rows count in `used` again (r2 m-2) | `build.rs:586` | KILLED (2) |
| r3c | locks committed outside the row's transaction | `protx_ops.rs:96` | KILLED (1) |
| m11 | r2 m11 re-sited: row then locks, separate commits, in the engine | `ops.rs:352` | **SURVIVED** → n-1 |
| r3g | `derivation_of` records a keys-only output | `psbt.rs:225` | KILLED (1) |
| r3h | custom change to a keys-only address counts as mine | `send/mod.rs:1199` | KILLED (1) |
| r3i | PSBT outputs to keys-only count as mine | `psbt.rs:207` | KILLED (1) |
| r3j | address book files a keys-only address as Receive | `labels.rs:95` | KILLED (1) |

## 6. Regression: vectors

- `scripts/rederive.py` against `36af6e7b` `testdata/protx_registration.json` and `protx_update_registrar.json`:
  - `prepared_regular` and `existing_evo` ProRegTx: payload (208/232 B), inputsHash, `MakeSignString` and signature are byte-equal.
  - `operator_regular` and `all_evo` ProUpRegTx: payload (162 B) and the owner signature over sha256d(payload \ sig) are byte-equal.
- `git diff 481532bd 36af6e7b -- testdata rust/crates/dw-protx rust/crates/dw-vault` is empty.

## Evidence check

- `/work/scratch/dw-mg-04/final-r2.out`: "head 36af6e7b", gate 13:36:46 → "GATE-SCRIPT EXIT 0" at 14:10:35, then "REGTEST default EXIT 0" at 14:14:40.
- `gate-r2.log`:
  - "tests passed 2601 failed 0";
  - vitest "996 passed (996)";
  - Playwright "1519 passed (6.2m)";
  - "GATE EXIT 0".
- `r2-regtest-default.log`: "ProTx suite passed … Tests successful", EXIT 0.
- Swift T0: §4.
- My targeted runs agree: baseline 0 failing, the full engine lib 1366/0.

## Required before merge

None.

## Recommended

- m-1: add the keys-only/funds probe as a test (it kills r3a and r3e).
- n-2: a transactional `reprepare` with its lock delta (follow-up is fine).
- n-3: fix the FFI doc comment, and align the D-row wording or the UI text.
- n-1: no action needed.
