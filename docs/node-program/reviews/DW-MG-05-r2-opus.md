# DW-MG-05 masternode authorization: independent review, round 2 (Opus-h)

Reviewed the fix diff `ebf68683..431a1ce0` of `dw/mg-05-auth`: 21011799 (r1 F1–F6) and 431a1ce0 (validator r2:
`sign_message` under D19, the E11 wait bound). Both are signed (`%G?` = G). The branch and `/work/scratch/mg05` were not
edited. Scratch: `/work/scratch/mg05-r2-review`, which holds a `git archive 431a1ce0` and a reflinked target with every
`.rs` and `Cargo.toml` touched. After the sweep, the mutated files were checked byte-equal to `git show 431a1ce0:…`.
Load during the run was 170–250.

## Verdict: ACCEPT

All six r1 items are closed as claimed. Each mutant I re-applied by hand was caught: the four r1 survivors, the three
F2 lookups, the shared helper, D23, and two forms of D5 (11 of 11). F2's filter touches only DashPay external
(contact) accounts. Change, CoinJoin and DashPay-receiving addresses keep their own-address paths. The one unfiltered
lookup left, in `coinjoin.rs`, is unreachable for a contact address and fails closed regardless. F5 breaks no real
vote path, because the only production hand-off runs under `lease.scope`. I found no new defect. Two optional nits are
below; neither blocks the merge.

## Findings

No blocker, major or minor.

### Nit N1 (optional): `coinjoin.rs` keeps its own unfiltered lookup

`rust/crates/dw-engine/src/coinjoin.rs:665-669` (`path_of`), called at `:874`, `:2643` and `:2703`.

It is safe today, for three independent reasons:
- **Its inputs are never contact addresses.** `:874` (`mix_view`) and `:2703` (`sign_coinjoin_payment`) take
  addresses from coins. `coins::read_coins` (`coins.rs:556-569`) collects only `funding_accounts` (BIP44, BIP32,
  `dashpay_receival_accounts`) and `coinjoin_accounts`, never `dashpay_external_accounts`. `:2643` signs the mixing
  session's own inputs.
- **The mixing signer refuses the path.** The signer at `:2643` is `SignerScope::CoinJoinOnly`
  (`dw-vault/src/signer.rs:131`), which allows only `m/9'/coin'/4'/…`. A DIP-15 path is refused before any key is
  used.
- **Signing would fail closed anyway.** A contact account's addresses come from the contact's xpub. This wallet's
  seed at the recorded path yields its own key, not the contact's, so any signature would fail script verification.
  No funds move and nothing leaks.

Routing it through `coins::own_address_info` would leave D19 with literally one lookup, and the doc's "the PSBT
derivation lookups and `sign_message` use the same helper" would then cover every path lookup. This is consistency,
not a fix.

### Nit N2 (optional): the "no derivation record" claim is tested at the helper, not through `create_unsigned`

`rust/crates/dw-engine/src/send/flow_tests.rs:1292`

`psbt_a_contacts_address_is_not_the_wallets` calls `psbt::derivation_of` directly. That is why `derivation_of`
became `pub(super)` (`psbt.rs:213`). The m4 contract line says "`create_unsigned` writes no derivation record for
it". The production callers are `psbt.rs:280` and `:293`, so the helper-level test does kill F2b. A
`create_unsigned` to a contact address asserting the output's `bip32_derivation` is empty would test the contract
sentence as written and let `derivation_of` stay private.

## r1 items: closed as claimed

| r1 | Change | Evidence |
|---|---|---|
| F1 | `vote` forwards to the private `vote_stamped(…, now)` (`governance/voting.rs:175`). `sign_one` stamps with `stamp()` (`:373`). The fixture pins `time: t`. The test now asserts `duplicate` for the repeat and `too_often` for the change. | `too_often` is Core's answer: in `CGovernanceObject::ProcessVote` (`object.cpp:429-447`), a same-second vote with a higher outcome (No=2 > Yes=1) passes the same-timestamp check and hits the `GOVERNANCE_UPDATE_MIN` rate check. My run: `vote_signs_and_stores_one_vote_per_masternode` passed **20/20** under load ~200. Each run took 4–17 s, so batches crossed second boundaries that flaked before the fix. |
| F2 | `coins::own_address_info` (`coins.rs:104`) filters `is_contact_owned` and serves `wallet_owns`, PSBT `path_of` (`psbt.rs:187`), `derivation_of` (`:219`) and `sign_message` (`keys.rs:769`). | F2a, F2b, F2c and F2h caught (table below). Only `DashpayExternalAccount` is `is_contact_owned` (key-wallet `d3d50520`, `account_type.rs:284-300`, an exhaustive match). |
| F3 | E8–E11 each have a killing test. `choose_key` is pure (`voting.rs:392`) with a unit table (`:685`). The H fixture covers an attached key that is not the list's. The change-to-contact case was added to `mg05_a_contacts_address_is_charged_as_foreign`. | E8–E11 caught. The added change case checks `total_debit` and the Masternode charge at `2 COIN − 1,000,000`, which is change plus fee, so the arithmetic is right. |
| F4 (D23) | An unreadable keychain (not `Locked` or `NoSecret`) with no matching attached key gives `signer_unavailable`. | F4 caught, both my variant (`Err(_) => false`) and the owner's (guard arm removed). The or-pattern guard `Ok(_) \| Err(NoSecret) if unread` applies to both alternatives, as intended. |
| F5 (D5) | `fence.rs:2892-2895` refuses a non-joining unleased `Vote`, as for an asset lock. | F5 (clause removed) and F5b (`unleased = false`) are caught by `the_fence_refuses_an_unleased_vote`. That test also pins that an unleased CoreTx is still `FirstUnleased`. |
| F6 | `check_payment_auth` doc names `masternodeOp`/`governance`. `require_synced`'s doc is moved back onto it. | Read; correct. |

## Mutants (re-applied by hand, `mut.py`, logs `mut-*.log`)

Clean baseline: 89 targeted tests passed (`base.log`).

| ID | Mutation | Result |
|---|---|---|
| E8 | `Deferred` → `"duplicate"` | caught: `a_vote_another_wallet_is_handing_off_is_in_flight` |
| E9 | attached key used without matching the list's id | caught: `a_vote_uses_the_keychain_key_else_…`, `vote_signs_and_stores_…` (H) |
| E10 | D20 fallback fail-closed (`Err(_) => return Err("signer_unavailable")`) | caught: `a_vote_uses_the_keychain_key_else_…` |
| E11 | `SpendCharge` drop refunds to `Spend` | caught: `mg05_a_failed_prepare_refunds_the_masternode_budget` (spent `Some(1000226)`, expected 0) |
| F2a | PSBT `path_of` unfiltered | caught: `psbt_a_contacts_address_is_not_the_wallets` (`CanSign` vs `NoMatchingKeys`) |
| F2b | `derivation_of` unfiltered | caught: same test (`(true, true)` vs `(false, true)`) |
| F2c | `sign_message` lookup unfiltered | caught: `mg05_a_contacts_address_signs_no_message` |
| F2h | the filter removed from `own_address_info` itself | caught: 3 tests (`…signs_no_message`, `…charged_as_foreign`, `psbt_a_contacts_…`) |
| F4 | an unread keychain treated as read (`Err(_) => false`) | caught: `a_vote_uses_the_keychain_key_else_…` |
| F5 | the unleased-Vote clause removed | caught: `the_fence_refuses_an_unleased_vote` |
| F5b | `unleased` forced false | caught: `the_fence_refuses_an_unleased_vote` |

This agrees with the owner's `/work/scratch/mg05/mut-r2/result.txt` (9 of 9). F2h, F5b and the F4 variant are mine.

## Ask 2: F2 and the legitimate own-address paths

- **Change.** Change goes to the Standard account's internal pool, which is not contact-owned. `wallet_owns` and
  `own_address_info` still find it. `outflow_of` (`send/mod.rs:1104`) and `record_send_metadata` (`:1956`) are
  unchanged in behaviour for it.
- **CoinJoin.** `CoinJoin` accounts are not contact-owned, so they are unaffected through the helper. CoinJoin's own
  `path_of` is unfiltered and safe (N1).
- **DashPay.**
  - The wallet's receiving account from a contact (`DashpayReceivingFunds`) is not contact-owned. It stays "mine",
    and `sign_message` and PSBT signing still work at it.
  - A payment to the contact's external address was already foreign under D19 (r1). This diff only makes the PSBT
    and message paths agree.
  - `platform/money_gate.rs:355-375` (`in_paying_account`) deliberately looks inside `dashpay_external_accounts`. It
    is not an ownership check, and the diff leaves it alone.
- **PSBT behaviour change.** It is correct. A PSBT with both a contact-address input and wallet inputs used to count
  the contact input as signable, then fail the whole `dw_psbt::sign` ("the key at … does not pay this input"). Now
  the contact input is foreign: the wallet's own inputs sign, and that one is left for its owner. That is standard
  partial-signing behaviour. The second half of `psbt_a_contacts_address_is_not_the_wallets` pins it (own coin to a
  contact output → `CanSign`).
- **Regression check.** Gate r3 on `431a1ce0` ran 2465 Rust tests with 0 failures (`gate-r3.log:7`), which includes
  the CoinJoin, DashPay and send suites.

## Ask 3: F5 and real vote paths

The only production `admit(ArtifactKind::Vote)` is `Governance::hand_off` (`voting.rs:258-275`). It runs inside
`lease.scope(...)`, so `DispatchScope::current()` is `Origin::Lease`. A task-local scope nests, so an outer unleased
scope, if one were ever added, is shadowed. Peer votes and `relay_vote` go to the store without the fence.

The refusal applies only when `!joins`. An unleased copy of a vote already admitted under a lease would resend the
same bytes, the asset-lock rule. That adds no authority, and no caller does it.

The voting suite (9 tests) and the 78 fence tests pass on the clean tree.

## Ask 4: new defects

None found. I read every hunk of the 14 files. Points checked:
- **`choose_key` matches the pre-fix logic except D23.** That covers the keychain hit, the attached-id match,
  `Locked` → `cancelled`, an attached-lookup vault error → `refused(e)`, and a seedless keychain (`NoSecret`) still
  allowing the attached key.
- **The keychain is still read once per batch.** This is unchanged; a cached error applies to the whole batch, as
  before.
- **`vote_stamped` takes `impl Fn() -> i64 + Send + 'static`.** It is private, and `vote` passes `now`, so production
  behaviour is unchanged. `hand_off` still gives the store a real `now()` for its rate rules, as Core's
  `GetAdjustedTime` does.
- **The new tests release what they hold.** The E8 test finishes its held permit (`NotSent`). The E11 test's
  120-s `eventually_for` bounds only how long a failure takes to show, and the success path returns at once.
- **`insert_contact_account` is `#[cfg(test)]`.** It asserts its own precondition ("listed, not owned"), so the
  F2 tests cannot pass vacuously.
- **Docs and contracts are consistent with the code.** D5, D19 and D23, mg-engine's `signer_unavailable` wording, and
  the m4 Ownership note all match. N2 is about test placement, not accuracy.
