# DW-MG-04 r2 — independent Opus-h review (narrow: the round's diff plus the r1 findings)

- Branch: `dw/mg-04-protx-owner` @ `481532bd` (r1 was `6fd8f21f`). Round commits: `6927c727` (r1 fixes), `23a31c05` (merge of main `8b0772ec`, MG-09 UI only), `66ad2ba9` (validator r1 fixes), `481532bd` (comments and docs only; verified, the 4 changed Rust lines are comments).
- Counterpart: Dash Core rc.3 (`txmempool.cpp` addUnchecked `:620-700` / `existsProviderTxConflict` `:1590-1700`, `evo/providertx_service.cpp:520-521,944-950`, `wallet/wallet.cpp` `LockProTxCoins`).
- Method: read-only. Detached worktree `/work/scratch/mg04-review/r2/wt` with a reflinked target and its own TMPDIR, 12 jobs. One build with 16 env-gated mutants (`MG4MUT=mN`): the 10 from r1, re-sited, plus 6 on this round's code. One throwaway vault probe test (reverted). Two dashd vectors per tx type re-derived with the r1 Python script. The worktree and target are deleted. Kept: `scripts/mutants_r2.py`, `scripts/run_mutants_r2.sh`, `mut-logs-r2/`, `build-r2.log`.

## Verdict: **ACCEPT WITH NITS**

All four r1 findings are closed, and the signing, payload and vector bytes are unchanged. The new Minors below are either pre-existing in the branch or are follow-up material; none puts funds at risk from a third party. There is one process condition: this round edits the m1 and m2 contracts (the FFI `sign_message` and `sign_psbt` behaviour), and dw-common requires the Swift T0 suite for that. I found no T0 run in the evidence. Run it, or get a manager waiver, before merge (m-4).

---

## r1 findings: status

| r1 | status | evidence |
|---|---|---|
| **M-1** Full signer forges an owner signature | **Closed** on the vault's network | `dw-vault/src/signer.rs:141` refuses `Sign`/`Message` at `is_owner_key`. My r1 proof re-run as a probe (Spend grant, `sign_ecdsa(m/9'/1'/3'/2'/0, base_payload_hash)`) is now `PathNotAllowed`. Every entry point ends in `VaultSigner::check`. The engine pre-checks keep the grant: `keys.rs:776` (`sign_message`: app, dw-console `signmessage`, FFI) and `send/psbt.rs` `ownership` (PSBT). Mutants m13/m14/m15 are killed. See n-1 for the other network's coin type. |
| **m-1** FundNew collateral unlocked after resend or crash | **Closed** (D53) | `protx.rs:221` adds the outpoint to the row's locks. `ops.rs:343-356` writes the locks and then the durable row on one connection. Under WAL, the row's `synchronous=FULL` commit makes the earlier lock commits durable too, so a durable row implies a durable lock. Resend, restart and `not_sent`→resend find the lock already there, and `discard` releases it via `row_collateral` (terms now carry the outpoint). m5 is killed (4 tests). See n-2 for the order. |
| **m-2** clashes with own unlisted ops | **Closed** (D51) | See §4. m12 is killed. |
| **m-3** m4 mutant survived | **Closed** | m4 is now killed by `mg04_a_conflicted_update_lets_the_next_one_through`. |
| n-1 fee ceiling | done (`plan_funding`) | |
| n-2, n-5 | recorded as Follow-ups | |
| n-3 voting == owner | done (D52) | m16 is killed. |
| n-4 | declined with a written reason | acceptable |

---

## Findings

### Minor

**m-1. A Send (or PSBT) to one of the wallet's owner or voting addresses counts as "back to the wallet": it isn't bounded by the grant cap, and the coin is then invisible and, after D50, unspendable from the app.**

- Where:
  - `dw-engine/src/send/mod.rs:1194` (`outflow_of`: `wallet_owns` → recipient "mine" → excluded from `external_sent`, the figure the `Spend` grant caps);
  - `send/psbt.rs:207` (`outputs_mine`, same);
  - `coins.rs:104` (`own_address_info` searches every managed account, the provider-keys accounts included).
- key-wallet's provider-key accounts are `ManagedCoreKeysAccount`, "without `balance`, `utxos`" (`key-wallet/src/managed_account/managed_core_keys_account.rs:30-36`). Their tx match records `received: 0` (`transaction_checking/account_checker.rs:972-1000`).
- So a coin paid to an owner address never shows in the balance and is never selected. D50 removed the last in-app route, the PSBT signer.
- A quick-unlock grant capped at X can therefore send the whole balance to the wallet's own owner address. The outflow is fee only, and afterwards the money is gone from the UI.
- Answers to the brief's question 2:
  - **The wallet itself never pays an owner script.** Receive and change come from BIP44. `issued()` gives the FundNew collateral and the NewAddress payout (`build.rs:684-693`). A given payout equal to the owner key is refused by `check_payout`, and the External collateral may not be the wallet's (`build.rs:645`).
  - **A user can.** The owner address is in the API: `ProTxTerms.owner_address` and MG-03's record `owner_address`. A user can paste it into Send, and a third party can pay it.
  - **This predates MG-04's D50.** Before it, only an externally built PSBT could move such a coin.
  - **Core** treats an owner key as an ordinary wallet key (`protx register` takes any wallet address). Its wallet lists and spends such coins.
- **Not a merge blocker.** Nothing is lost to anyone else, and the follow-up is recorded. Two changes are recommended:
  - (a) Make the follow-up text say the coin is also invisible in the balance, and that a Send to it bypasses the cap.
  - (b) Cheap now or soon: treat provider-key addresses as not-mine in `outflow_of` / `outputs_mine`, or refuse them as a Send recipient with a clear message. Keep the dwcli owner-grant sweep as the recovery follow-up.
- It needs no UI statement until the owner address is displayed in the UI. Today it is in the API and dwcli only.

**m-2. A `conflicted` registration keeps its collateral (and owner index) "in use" forever, so the same coin can't be registered again.**

- Where:
  - `masternodes/protx/build.rs:578` calls `used.add_terms` for **every** row, before the `unlisted` filter that D51 applies;
  - `:629` and `:640` refuse `ExistingUtxo` and `External` with `collateral_unavailable` when `used.collateral` holds the outpoint.
- A sent row is never deleted: `discard` refuses `conflicted` (`ops.rs:465-478`), and `delete_unsent_masternode_op` only deletes `sent_at IS NULL` rows.
- `settle_operation_locks` releases only the fee inputs, so the collateral lock stays as well.
- Scenario:
  1. A fee input of an ExistingUtxo ProRegTx is spent in a block by another device.
  2. The row reads `conflicted`, but the 1,000 DASH coin is still unspent.
  3. Every later registration with it is `collateral_unavailable`.
  4. The user can't recover this in the app. Each conflicted attempt also burns one of the 20 owner indexes.
- D23 says "an *open* operation". Core frees everything once the ProRegTx leaves the mempool.
- This predates this round; I missed it in r1. Found by reading, not reproduced.
- Fix (one line): skip `add_terms` for `Conflicted` rows, or at least their collateral. A row conflicted in a block can never confirm. Also release a conflicted row's `locked_collateral` in `settle_operation_locks`.

**m-3. Restore on another machine: until the follower runs, the restored collateral is an ordinary, selectable coin, and a wallet-history change doesn't trigger the follower.**

- Where:
  - `follow.rs:426` (the tick returns when the list height and generation are unchanged);
  - `:432` (and when the list isn't synced);
  - `protx/ops.rs:513` (`protx_follow_pass`, the only relock on a machine without the row).
- The generation is bumped by local changes and wallet-set changes (D28), not by new wallet transactions.
- If filter/block sync brings the ProRegTx and its collateral into history *after* the list reached the tip, the relock waits for the next block. That is about 2.5 minutes on mainnet.
- In that window, a largest-first payment would pick the 1,000/4,000 DASH output. Core would then drop the masternode (`bad-protx-collateral` / collateral spent).
- Core locks at `AddToWallet` (`LockProTxCoins`), with no list involved.
- This predates this round (D9/D24 rely on the follower). Found by reading, not reproduced.
- Recommendation: a follow-up row, or run the collateral relock on a wallet-history change. It reads only history and appdb, not the list.

**m-4. Swift T0 is not in the evidence, but this round changes the m1 and m2 contracts.**

- The rows changed are `docs/contracts/m1-engine.md` §2.10 `sign_message` (owner addresses refused) and `docs/contracts/m2-engine.md` `sign_psbt` (owner inputs left unsigned). Both behaviours reach Swift through dw-ffi.
- `/work/scratch/mgr/briefs/dw-common.md:20`: "A change to an m1–m4 §4 table or contract needs the Swift T0 suite".
- No new error code was added, so Swift `engineCodes` needs nothing.
- Run `scripts/linux-docker-test.sh`, or record a manager waiver. (My r1 statement "MG-04 needs no Swift T0" was true at 6fd8f21f, which didn't touch m1/m2.)

### Nits

- **n-1. D50 is scoped to the vault's network's coin type.**
  - `paths.rs:108` `is_provider_key` requires `coin_type(network)`.
  - Probe on a regtest vault: a Spend grant's signer signs at `m/9'/5'/3'/2'/0`, and the key equals the same seed's **mainnet** owner key 0.
  - This is the general breadth of `Full`, which signs `m/44'/5'/…` on a testnet vault too, so it isn't specific to MG-04.
  - For the vault, consider matching any coin type in the owner refusal, or having `Full` refuse foreign coin types outright. Raise it with the manager. Not blocking.
- **n-2. Mutant m11 survives: writing the row before its locks fails no test.**
  - At `ops.rs:350-356` the closure commits each `lock_manual` and then the durable insert, separately.
  - The order is right today, but nothing pins it, and a crash between them leaves stray manual locks on the fee inputs with no row to settle them. That part predates this round.
  - Wrapping both in one SQLite transaction makes D53's "in the write that saves its row" literally true and the order moot.
- **n-3. FFI `message.vault_locked` for an owner address** (`dw-ffi/src/api/message.rs:76`).
  - It's misleading: unlocking doesn't help, and a Swift unlock-then-retry flow would loop.
  - A zero-cost fix: have the engine refuse an owner address with the existing `EngineError::AddressNoKey` → `message.address_no_key` (Core's wording is "Private key for address … is not available").
  - That is no new code, so it needs no `engineCodes` change, but it is still an m1 contract row (m-4).
  - Fine as the recorded follow-up.
- **n-4. Voting paths: `Full` still signs any digest there** (`signer.rs:141` covers owner paths only).
  - A governance vote signature is a hash signature by the voting key, so a Spend grant's signer can make a valid vote past the `MasternodeVoting` grant.
  - It's low value (no funds, and the vote can be re-cast), but it is D50's class. MG-07 is unaffected: votes use `KeyUse::Vote`.
  - Record it as an MG-05/MG-07 follow-up, as r1 suggested.
- **n-5. D51's claim of an update row includes its `service`** (`keys.rs:90`, from the listed entry's service in the update's terms).
  - Core indexes only `pubKeyOperator` for a ProUpRegTx (`txmempool.cpp:656-659`).
  - The listed service is refused anyway by `bad-protx-dup-netinfo-entry`. The one stricter-than-Core case is an operator's ProUpServTx moving the service away while the wallet's update row is still unlisted.
  - Set `service: None` for update rows.
- **n-6. A `protx-dup` refusal caused by one of the wallet's own *unsent* rows reads like Core's mempool refusal.**
  - Naming the row, as `update_pending{operation}` does, would let the user discard it.
- **n-7. The D51 race** (it runs at `protx.rs:132` → resolve, before the lease at `:135` and the fence).
  - It needs two concurrent calls with the same given key in one wallet. The outcome is a stuck `sent` row with locked inputs and no funds lost.
  - **No lock is needed before merge.** Re-running `check_pending` against freshly read rows once the coin fence is held would close it cheaply.

---

## 1. M-1 closure, entry by entry

- The vault (`signer.rs:134-142`):
  - `Full` refuses `OwnerPayload` everywhere, and `Sign`/`Message` at owner paths.
  - `Export` stays path-gated outside the scope check (`platform.rs:258`: auto-accept only).
  - `Agreement`/`ContactInfo` require identity paths.
  - The other scopes never reached owner paths.
- The engine `sign_message` (`keys.rs:776`) refuses before redeeming. The app, dw-console `signmessage` and FFI `sign_message` all go through it, as does dwcli (facade only).
  - Test: `session_lifecycle::imported_wallet_signs_after_restart_and_unlock` (killed m15).
- PSBT (`psbt.rs` `ownership`): an owner input is not in `paths`, so it is unsignable and left unsigned, and its wallet's fence isn't taken.
  - Test: `psbt_an_owner_keys_coin_is_not_signed` (killed m14).
- The D50 caller audit, by grep: every signing call is `sign_p2pkh_inputs` (coinjoin, protx build/update: funds paths and `MasternodeOwner`), the platform signers (identity/funding paths), `sign_message` and PSBT. None asks for an owner path. I found no caller missing from D50.
- **The IdentityScan master-key exception** (`bringup.rs:985`, `platform.rs:299`):
  - The master xpriv leaves the vault for platform-wallet's identity scan. Unattended, it is `Credential::None` while the vault is prompt-free.
  - Its holder can derive every key, the owner keys and all spending keys included. So it is strictly broader than a Spend grant, and D38/D50 are claims about vault *signers*, not about an exported master key.
  - The exception is sound as recorded: it is pre-existing, separately granted, engine-only, and forbidden to dw-ffi by clippy.
  - Longer term, a resolver that takes only the identity branch xpriv would remove it, if platform-wallet allows that.
- **Voting paths** still sign under `Full` (pinned by `full_scope_allows_everything_but_owner_signatures` at `m/9'/1'/3'/1'/0`). MG-07's votes use `MasternodeVoting` + `KeyUse::Vote` (`masternode_voting_votes_with_voting_keys_only` passes). See n-4.

## 2. The "owner-address coin" follow-up

See m-1. Summary:
- The wallet never creates such a coin itself. The payout ≠ owner rule, the BIP44-issued addresses and the External-not-mine check cover that.
- A user or a third party can create one.
- key-wallet never tracked such coins as funds. D50 only closed the PSBT escape.
- Core spends them.
- **It doesn't block.** Fix the Send-side accounting soon, and keep the sweep as the recovery follow-up.

## 3. D53 trace

| path | lock state |
|---|---|
| first send | the lock and then the durable row in one `appdb_op`, before `deliver` |
| `not_sent` → resend | the lock is already there (`mg04_a_new_collateral_is_locked_with_its_row_before_any_hand_off`) |
| crash before the hand-off | the row is `signed`, the lock persists, and `discard` unlocks it |
| crash after the hand-off | the same, and the row is `sent` |
| restart | the lock is persistent in `utxo_locks` |
| `conflicted` | a stale lock on an output that never existed, harmless (D53 says so) |
| restore on another machine | no row and no lock; follower-dependent, see m-3 |

- Can an ordinary send pick a FundNew collateral on the original machine? No. The lock precedes the output's existence in the wallet, and `max_spendable` excludes it (the test asserts this).

## 4. D51 against rc.3

- ProRegTx (`txmempool.cpp:1601-1622`) is refused on:
  - a netInfo entry;
  - the owner key id (`mapProTxPubKeyIDs`);
  - the operator key hash (`mapProTxBlsPubKeyHashes`, fed by ProRegTx **and** ProUpRegTx, `:659`);
  - the platform node id (`mapProTxPlatformNodeIDs`);
  - the collateral (`mapProTxCollaterals` / `mapNextTx`);
  - the replacement conflict.
- ProUpRegTx (`:1680-1692`) is refused on:
  - the collateral replacement;
  - one key change per MN;
  - the operator key held by another tx whose map value ≠ this proTxHash. That means any pending ProUpRegTx, the same MN's included, which D51 states correctly.
- The wallet covers them as follows:
  - the operator key, node id and service in D51;
  - the owner key and collateral in D5/D13/D23 (`used`);
  - one key change per MN, and the same-MN rule, in D49 (`update_pending`).
  - The refusal keys match Core's set. Rows' operator keys are stored in basic form, so a single comparison is right.
- Stricter than Core:
  - unsent rows count (a deliberate choice; `discard` frees them);
  - mined rows above the list count (equivalent to the block's `bad-protx-dup-*`, so it refuses nothing Core would accept);
  - n-5 (the update service);
  - m-2, which is D23's and not D51's: conflicted rows hold their collateral.
- No valid user action is refused apart from n-5's edge case and m-2.
- The race: n-7. It is not required before merge.
- Cross-wallet: recorded as a follow-up. It's the same exposure Core's RPC has across wallets.

## 5. D52

- **Core allows voting == owner** and defaults to it (`providertx_service.cpp:521`). Refusing it for a *new* registration (`build.rs:661`), and for a *new* voting key in an update (`update.rs:252`), is a defensible product call. It keeps the owner key off voting machines, and UX-SPEC §4.25.5 wants separate keys.
- **Restore and import are unaffected.** The refusal sits only in those two resolve paths. MG-03's tracking and display, and `KeepOr::Keep` in an update, keep a shared listed voting key.
- One edge: "setting" the voting key to the same shared address is refused where Keep would succeed. The message could point to Keep.

## 6. FFI mapping

- See n-3. This is a pre-existing mapping (`S::PathNotAllowed` was the mixing-only case).
- It's misleading for the owner case. The fix needs no new code.
- Swift T0 is needed for the round anyway (m-4).

## 7. Mutants (one build, `MG4MUT=mN`; logs in `mut-logs-r2/`)

Binaries:
- dw_protx lib, oracle_protx_owner, no_operator_signing;
- dw_vault lib, masternode_signers, platform_scopes;
- mn_masternodes_contract, session_lifecycle;
- dw_engine lib filtered `masternodes proposals psbt`.

Baseline `none`: 0 failing (304 tests).

| # | mutant | site | result |
|---|---|---|---|
| m1 | the owner hash drops inputsHash | `dw-vault/src/owner.rs` | KILLED (11) |
| m2 | owner/voting key ids swapped | `protx/build.rs:714` | KILLED (10) |
| m3 | an attached voting key signs an update | `owner.rs` + `signer.rs` | KILLED (4) |
| m4 | an unconfirmed spend reads Conflicted | `protx/ops.rs` | **KILLED (1)**, was SURVIVED in r1 |
| m5 | FundNew not locked with its row (D53) | `protx.rs:221` | KILLED (4) |
| m6 | a ProUpRegTx payee may be the owner | `dw-protx/src/rules.rs` | KILLED (1) |
| m7 | a resend ignores `held_by_others` | `protx.rs` | KILLED (1) |
| m8 | a reserved coin accepted as collateral | `protx/build.rs` | KILLED (1) |
| m9 | submit skips the signature pre-check | `protx.rs` | KILLED (1) |
| m10 | ProRegTx commits to no inputs | `dw-protx/src/payloads.rs` | KILLED (30) |
| m11 | the row is written before its locks (D53 order) | `protx/ops.rs:350-356` | **SURVIVED** → n-2 |
| m12 | D51 skips the platform node id | `protx/build.rs:732` | KILLED (1) |
| m13 | `Full` signs at owner paths again (M-1) | `signer.rs:141` | KILLED (3) |
| m14 | a PSBT owner input counts as signable | `send/psbt.rs` `ownership` | KILLED (1) |
| m15 | `sign_message`'s owner refusal dropped | `keys.rs:776` | KILLED (1); the vault refuses anyway, and the test sees the grant spent |
| m16 | D52 registration refusal dropped | `protx/build.rs:660` | KILLED (1) |

## 8. Regression: vectors

- `rederive.py` (independent; Core rc.3 only) against `481532bd` `testdata/`:
  - `prepared_regular` and `existing_evo` ProRegTx: payload, inputsHash, `MakeSignString` and signature are byte-equal.
  - `operator_regular` and `all_evo` ProUpRegTx: payload and the owner signature over sha256d(payload without the signature) are byte-equal.
- `testdata/`, `dw-protx` and `dw-vault/src/owner.rs` are unchanged since r1, and `oracle_protx_owner` passes at head.

## Evidence check

- `/work/scratch/dw-mg-04/gate-r1b.log` (11:28):
  - Rust 2595 passed, 1 failed: `e0_05_bringup::a_restored_wallet_gets_the_full_budget`, which passed alone on rerun try 1 (BL-100).
  - Playwright 1519, then GATE EXIT 0.
- `r1b-regtest-{default,v24}.log` (11:34, 11:39): "Tests successful", EXIT 0.
- `481532bd` (11:40) changes comments and docs only on top of `66ad2ba9`, so the evidence carries.
- My targeted runs agree: baseline 0 failing.

## Required before merge

1. **Swift T0** (`scripts/linux-docker-test.sh`) for the m1/m2 contract changes, or a manager waiver (m-4).

## Recommended (before merge if cheap, otherwise as follow-ups with D-rows)

- m-2: the one-line fix for conflicted rows.
- m-1: Send/PSBT outflow for provider-key addresses, and correct the follow-up text.
- m-3: relock on a wallet-history change, or a follow-up row.
- Nits n-1 to n-7.
