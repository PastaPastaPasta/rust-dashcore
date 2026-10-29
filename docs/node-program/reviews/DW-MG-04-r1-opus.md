# DW-MG-04 r1 — independent Opus-h review

- Branch: `dw/mg-04-protx-owner` @ `6fd8f21f`, against desktop main `e8d88078`
- Counterpart: Dash Core rc.3 (`evo/providertx.{h,cpp}`, `evo/deterministicmns.cpp` CheckProRegTx/CheckProUpRegTx, `txmempool.cpp`, `wallet/rpc` `protx register*` / `update_registrar`, `messagesigner.cpp`, `key.cpp` SignCompact)
- Method: read-only. A detached worktree under `/work/scratch/mg04-review/` with its own target and TMPDIR, at most 12 jobs. Two dashd vectors per tx type re-derived by an independent Python implementation. Ten env-gated mutants (`MG4MUT=mN`) from one build. One throwaway demonstration test. Everything except this file and the scripts has been deleted.

## Verdict: **CHANGES REQUIRED**

There is one Major (the D38 scope guarantee doesn't hold for raw-digest signing) and three Minors. The ProTx bytes and signatures, the Core rules, the charge-before-sign order and persistence are correct, and I verified them independently.

---

## Findings

### Major

**M-1. D38 is not enforced: a `Full`-scope signer (any Spend grant) can produce a valid ProUpRegTx owner signature via `KeyUse::Sign`.**
`rust/crates/dw-vault/src/signer.rs:137`: `SignerScope::Full => key_use != KeyUse::OwnerPayload`, pinned by the test `full_scope_allows_everything_but_owner_payloads` (`signer.rs:557`), which asserts `Sign` is allowed at `m/9'/1'/3'/2'/0`.

- Owner payloads are refused, but `KeyUse::Sign` is not. `Signer::sign_ecdsa` signs an arbitrary 32-byte digest at owner paths (`m/9'/coin'/3'/2'/i`).
- An owner signature in Core is just `CHashSigner::SignHash(SerializeHash(CProUpRegTx))`, a compact recoverable ECDSA over that digest. So (r,s) plus the returned pubkey converts to the 65-byte `vchSig` by trying the four recids.
- I demonstrated this with a throwaway test, now reverted:
  - It used `GrantPurpose::Spend { max_duffs: 1 }`, a `Full` signer, and `sign_ecdsa(m/9'/1'/3'/2'/0, base_payload_hash)`.
  - The returned pubkey equals keychain owner key 0.
  - One recid yields a signature that `owner_of(sig, payload)` recovers to the owner key id. That is a valid ProUpRegTx owner signature.
  - It passed (1 passed, 0.44 s).
- This is the same digest-at-owner-key exposure that D11/D12 removed from `MasternodeOwner` (`signer.rs:~170`: `Sign | Message => address` only). The design states that only a `MasternodeOp` grant can produce an owner signature, and that is not true.
- Exploiting it needs a Spend-grant holder to call `sign_ecdsa` at a provider path, which the engine does not do today. So it is defence-in-depth, but it is a stated, tested guarantee that is false.
- Fix:
  - In the `Full` arm, refuse `Sign` and `Message` when `paths::is_owner_key(path, network)`. No funding account lives at provider-key paths, so nothing legitimate breaks. Do the same for voting paths in MG-05 (`KeyUse::Vote` is separate, but a raw `Sign` at a voting path is the analogous hole).
  - Flip the pinning test to assert that `Full` refuses `Sign`, `Message` and `OwnerPayload` at owner paths.
  - Add a test like the demonstration: a Spend grant cannot produce a signature that `owner_of` accepts.

### Minor

**m-1. A FundNew collateral is not locked when the first hand-off is a definite NotSent and a later resend succeeds, nor after a crash between the hand-off and the record.**

- Where:
  - `rust/crates/dw-engine/src/masternodes/protx.rs:252` (`lock_after: fund_new`, first send only);
  - `protx.rs:722` (`lock_after: None` on `resend_operation`);
  - `protx/ops.rs:411` (`let lock = d.lock_after.filter(|_| out);`).
- After `send.no_peers`, for example, a resend never sets the persistent lock on the new 1,000/4,000 DASH output. The lock then depends on the follower (`follow.rs` POLL 5 s after `local_change`, skipped while the list is unsynced or the fence is busy).
- In that window, once the ProRegTx is IS-locked or confirmed (its change and collateral outputs become trusted), the largest-first `plan_funding` can select the collateral output for an ordinary payment. Core would then evict or invalidate the ProRegTx via `removeProTxSpentCollateralConflicts` / `bad-protx-collateral`, the registration fee is lost, and the user sees a confusing state.
- Core locks the collateral at AddToWallet time (`CWallet::LockProTxCoins` / `AddToWallet` → `LockCoin` for ProRegTx collaterals), independent of relay.
- Fix: lock the FundNew outpoint in the same transaction that saves the row, recorded as the row's `locked_collateral` so `discard` → `row_collateral` unlocks it. Alternatively, pass `lock_after: terms.collateral_outpoint` on resend for FundNew rows. Add a test: NotSent, then resend Sent, then the collateral is locked before any follower pass.

**m-2. Conflicts with the wallet's own unlisted operations are left to Core's mempool, which refuses them silently (from an SPV client's view).**

- Where:
  - `rust/crates/dw-engine/src/masternodes/protx/keys.rs:261-291` (`pick_operator`) and `:294-310` (`pick_platform_node`): Given and Keychain choices bypass `used`; only NextUnused consults it.
  - `protx/build.rs:670`: `check_registration_list` consults `ctx.listed` only.
- If an operator key, platform node id or service address is shared with one of the wallet's own *sent-but-unlisted* operations, Core's `existsProviderTxConflict` refuses it via `mapProTxBlsPubKeyHashes` / `mapProTxPlatformNodeIDs` / `mapProTxAddresses`. Core's on-chain `bad-protx-dup-*` applies once the first one lands.
- The wallet can't see that refusal, so the second row sits `sent` with its inputs locked until a manual abandon.
- D5 covers only the owner key and the collateral. Either check `used` (and the rows' terms) for Given and Keychain too, refusing with the Core reason as the owner key already does at `keys.rs:193-197`, or record the gap as a D-row.

**m-3. Surviving mutant m4: no test pins that an *unconfirmed* spend of an operation's input does not read `Conflicted`.**
`rust/crates/dw-engine/src/masternodes/protx/ops.rs:161-168`: removing `e.context.block_info().is_some()` from the conflicted predicate made 0 tests fail across dw-protx, dw-vault, dw-engine `masternodes` and `proposals`. D49's exit and D25's lock release both depend on "spent in a block" being the trigger, so a regression here would release a `sent` row's input locks on any mempool double-spend sighting. Add a test with an unconfirmed history entry that spends a row input and assert the row stays `Sent`.

### Nits

- **n-1.** There is no explicit fee ceiling (Core's `-maxtxfee` / `MAX_TX_FEE` check in `CWallet::CreateTransaction`) in `build.rs` `finish_registration` / `finish_update`. The fee is structurally 1000 duffs/kB (`plan_funding`) and capped by the grant's `max_duffs`, so this is cosmetic, but one assert would document the bound.
- **n-2.** `dw-appdb/src/lib.rs:228`: `masternode_ops` is in `WALLET_TABLES`, so `delete_wallet` drops *sent* rows. That falls in the same class as the "unloaded wallet" window. List it in Follow-ups next to D43/D49.
- **n-3.** `keys.rs:222` `pick_voting`: `VotingChoice::Address` equal to the owner key's address is not refused. Core allows it, but the design says voting is never shared with the owner, so either refuse it or relax the sentence.
- **n-4.** `protx.rs:133-135`: `resolve_registration(…, true)` issues the NewAddress payout and the FundNew collateral address *before* `begin_lease`. A refused or expired grant burns two addresses, which is harmless within the gap limit but out of order.
- **n-5.** The Follow-up "one-call abandon" should also name registrations stuck `sent` for the m-2 reasons and for an External signature/collateral mismatch Core refuses.

---

## 1. Payload and signature correctness (line by line, vectors re-derived)

`/work/scratch/mg04-review/scripts/rederive.py` is written from Core rc.3 only, with its own secp256k1, RFC 6979, base58 and serializer, and no wallet code. All four vectors are **byte-equal**:

| vector | checked |
|---|---|
| ProRegTx `prepared_regular` | v2 payload; inputsHash = sha256d(prevouts) (`CalcTxInputsHash`); `MakeSignString` = `payout\|reward\|owner\|voting\|payloadHash(reversed hex)` equals the vector's sign_message; "DarkCoin Signed Message:\n" hash; collateral signature recovered **and re-signed RFC 6979 byte-equal** (header 27+recid+4) |
| ProRegTx `existing_evo` | the same plus the evo tail. platformNodeID serializes in reversed (uint160 `SetHex`) order. My first pass forgot the reversal and differed; the wallet's `PlatformNodeId` encodes it correctly, consistent with Core |
| ProUpRegTx `operator_regular` | v2 payload; owner signature over `SerializeHash` excluding vchSig, re-signed byte-equal. Signing with vchSig included, or with inputsHash zeroed, gives a different signature (the brief's two mutants) |
| ProUpRegTx `all_evo` | as above |

The rules (`dw-protx/src/rules.rs`) mirror rc.3:
- `check_registration_trivial` / `check_network_fields` follow `CheckProviderNetworkFields`, the evo ports and the reward ≤ 10000.
- `check_payout` follows `IsPayoutListTriviallyValid` / `IsPayoutListKeySafe`: the payee is not the owner or voting key (m6 killed). The collateral is not reused as a key (`check_collateral_key_safe`).
- `check_inputs_hash` follows `bad-protx-inputs-hash`, and `signed_by` follows `CheckStringSig` / `CheckHashSig`.
- The list checks follow `bad-protx-dup-{addr,key,platformnodeid}` against the confirmed list.
- `owner.rs` hashes `base_payload_hash()` inside the vault. Attached keys are bound to their `pro_tx_hash` (m3 killed), and `attached_owner_signatures_equal_dashds` pins dashd byte-equality.

## 2. Key custody

- `MasternodeOwner` allows `Sign` and `Message` at addresses only, `PublicKey` at address or owner, and `OwnerPayload` at owner. That is correct, D11/D12.
- `Full` is the M-1 gap.
- The attached owner key is used only through `with_attached_key(pro_tx_hash, ROLE_OWNER, OwnerPayload)`, and a voting key can't stand in (m3).
- No owner secret leaves the vault, and dwcli only receives signatures.

## 3. Money safety

- `charge_tx_signer(Masternode, fee)` is called before any signature, and the grant caps it.
  - dwcli authorizes `MasternodeOp{max_duffs}` = the preview's `total_debit`, or the row's fee on resend.
- Funding follows the existing trusted-coin rules: unconfirmed outputs are trusted only at change addresses.
- Reserved coins are refused as ExistingUtxo collateral (m8 killed, the D47 test). `held_by_others` is respected on resend (m7 killed).
- BIP69 ordering, lock time 0, final sequences.
- Gaps: m-1 (the collateral lock on resend) and n-1.

## 4. D43/D49 against txmempool.cpp (rc.3)

- D49's description of `existsProviderTxConflict` matches `txmempool.cpp:1684-1692`:
  - one ProUpRegTx per proTxHash in flight;
  - `mapProTxBlsPubKeyHashes` is keyed by the tx hash, so a second key change for the same MN is refused until the first leaves the mempool.
- `removeProTxSpentCollateralConflicts` matches D43's collateral-spend eviction.
- The guards (`ops_on`, `unlisted_operation`, `resend_conflict`) cover every case the wallet can see.
- The remaining windows are another wallet or Core instance holding the same keys, an unloaded or removed wallet (n-2), a reorg, and clock skew on the expiry. They are inherent to an SPV wallet with no mempool view. Core's own RPC has the same exposure across wallets.
- **None of them should block the merge.** m-2 is the one *self-inflicted* window, and it is cheap to close.

## 5. Persistence

- `masternode_ops` comes from migration `V2026102101__mg_protx_ops.sql`. The insert is durable before the hand-off (`insert_masternode_op_durable`), and so is `begin_masternode_op_attempt`. `end_masternode_op_attempt` is not durable, which is fine because state is derived on read.
- `sent_at` is set before every hand-off. `delete_unsent_masternode_op` deletes only `WHERE sent_at IS NULL`, so a sent row survives every engine path. The exception is wallet removal (n-2).
- `state_of` derives Sent, Confirmed and Conflicted from history (m4 gap, m-3).
- `NOT_EXPORTED` includes the table.
- `lock_manual` persistent locks (D8/D25) are released only through `settle_operation_locks` and `discard`.

## 6. dwcli, contract, Swift T0

- `rust/bin/dwcli/src/masternodes.rs` makes facade calls only, with a grant per call capped at the preview or row fee. It holds no keys or signing.
- `docs/contracts/mn-engine.md` §4 adds no new `engineCodes` for Swift.
- Per `/work/scratch/mgr/briefs/dw-common.md:20`, Swift T0 is required only for m1–m4 tables, so **MG-04 needs no Swift T0**.

## 7. My own mutants (one build, `MG4MUT=mN`, rerun per mutant)

Test binaries: dw_protx, oracle_protx_owner, dw_vault, masternode_signers, mn_masternodes_contract, and dw_engine `masternodes` + `proposals`. The baseline (`none`) had 0 failing.

| # | mutant | site | result |
|---|---|---|---|
| m1 | the owner hash drops inputsHash | `dw-vault/src/owner.rs` | KILLED (11) |
| m2 | owner and voting key ids swapped | `protx/build.rs:654-655` | KILLED (8) |
| m3 | an attached voting key signs the update | `owner.rs` + `signer.rs` | KILLED (4) |
| m4 | an unconfirmed spend reads Conflicted | `protx/ops.rs:163` | **SURVIVED (0)** → m-3 |
| m5 | no FundNew lock after the hand-off | `protx/ops.rs:411` | KILLED (2) |
| m6 | a ProUpRegTx payee may be the owner | `dw-protx/src/rules.rs:428` | KILLED (1) |
| m7 | a resend ignores `held_by_others` | `protx.rs:694` | KILLED (1) |
| m8 | a reserved coin accepted as collateral | `protx/build.rs` `existing_coin` | KILLED (1) |
| m9 | submit skips the signature pre-check | `protx.rs:331` | KILLED (1) |
| m10 | ProRegTx commits to no inputs | `dw-protx/src/payloads.rs:194` | KILLED (25) |

## Evidence check

- `/work/scratch/dw-mg-04/gate.log`: GATE EXIT 0 (Rust 2587 passed / 0 failed, vitest 963, Playwright 1445).
- `merged-regtest-default.log` and `merged-regtest-v24.log`: EXIT 0 at 07:45 and 07:49, after the 07:14 head commit.
- These are consistent with my own runs of the touched crates.

## Required for ACCEPT

1. Fix M-1 (the `Full` scope at owner paths) and flip its pinning test.
2. Fix m-1, or record it in a D-row with the follower-dependence stated.
3. Add the m-3 test.

m-2 and the nits can be follow-ups.
