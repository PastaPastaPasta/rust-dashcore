# DW-MG-05 masternode authorization and `Governance.vote`: independent review, round 1 (Opus)

Reviewed `dw/mg-05-auth` at `ebf68683` (on `4a2aa94a`, off desktop main `24083ca4`), `git diff 24083ca4..ebf68683`
(56 files). The branch was not edited. Scratch copy: `/work/scratch/mg05-r1-opus` (archive of `ebf68683` plus a
reflinked target). Dash Core read at `v24.0.0-rc.3`.

## Verdict: APPROVE-WITH-NITS (ACCEPT WITH NITS)

No blocker or major. The security properties the row asks for hold, and the tests discriminate them:
- a voting lease cannot sign a transaction;
- a `FundNew` collateral is not charged, and the fee is;
- the cap covers foreign outputs plus the fee;
- a contact's address is foreign;
- no operator-key path is reachable.

All 10 vault mutants and 10 of 14 engine mutants were caught. The survivors are reason strings and refund paths that
fail closed. One new test flakes on a wall-clock second boundary (F1, 1 in 40 under load). It is a one-line fix and
should land before the merge, so it doesn't reach the gate.

The r1 validator fixes are verified, not re-found:
- **H1 (D19):** `coins.rs:102` excludes contact-owned accounts; mutant E3 is caught.
- **M1 (D11):** `pays()` covers both purposes; `ipc_tests.rs` asserts the refusal and the passphrase acceptance.
- **L1, L4, L5, L6:** present.
  - L5: `in_flight` is distinct from `duplicate` in code, but untested (F5).
- **L3, L8, L9, L10:** recorded as D20–D22 and D1.

## Findings

### Minor F1: `vote_signs_and_stores_one_vote_per_masternode` flakes when a second ticks over

`rust/crates/dw-engine/src/governance/voting_tests.rs:218-219`

The re-vote asserts `rejected == "duplicate"`. That holds only if `again` is stamped with the same UNIX second as the
first batch, because the vote hash includes `nTime`. Otherwise the store answers `too_often`
(`store.rs:2262`).

Each `vote` call derives 20 keychain keys, each in its own vault operation, before it signs. So the first batch's
signing and the second's are tens to hundreds of ms apart, and the test fails whenever a second boundary falls
between them. I ran the implementer's binary 40 times at load 23–53: 39 passed and 1 failed with
`left: Some("too_often") right: Some("duplicate")`.

This is new flake surface on the gate. It is distinct from the two r1 flakes.

Fix: accept `duplicate | too_often` while asserting that the store still holds exactly one vote for A. Better, give
`vote` an injectable clock (the test module already has `NOW`) and pin the time, so the `duplicate` case is tested
deterministically.

### Minor F2: D19 narrowed `wallet_owns`, but the PSBT derivation lookups still treat contact accounts as the wallet's

`rust/crates/dw-engine/src/send/psbt.rs:185-191` (`path_of`), `:216-224` (`derivation_of`, used at `:286` and `:299`)

Both search `all_managed_accounts()` without the `is_contact_owned` filter. `outputs_mine` (the PSBT cap and the
`is_mine` display) is correct, as m4 item 59 says. The rest of the PSBT code is not:
- `create_psbt` writes a BIP32 derivation record on a contact output: this wallet's fingerprint, a DIP-15 path and
  the contact's public key. A co-signer or hardware wallet that classifies change by "has my fingerprint" may hide
  the contact payment from its confirmation screen. A signer that re-derives the key sees the mismatch.
- `path_of` counts an input at a contact address as signable, so `analyze` reports `CanSign`. It also inflates the
  outflow, which fails closed. `dw_psbt::sign` then fails the whole PSBT at `lib.rs:357` ("the key at … does not pay
  this input"), so no bad signature is produced.

This predates MG-05, but D19's statement now makes the inconsistency visible. Fix: one helper, used by
`wallet_owns`, `path_of` and `derivation_of`, that returns the wallet's own account for an address with contact
accounts skipped.

### Minor F3: the `in_flight`, attached-key-mismatch, D20 and drop-refund paths are untested

Mutants E8, E9, E10 and E11 survived (table below). None of them opens a hole:
- **E8:** `Deferred` reported as `duplicate`. The L5 fix is in place but nothing pins it.
- **E9:** an attached key used without matching the list's key id. The store then rejects the signature
  (`add_own_vote` → `key_ok`), so only the reason changes from `no_voting_key`.
- **E10:** the D20 fallback turned fail-closed.
- **E11:** a dropped Masternode charge refunded to `Spend`. The Masternode budget then leaks the charge, which
  over-charges and fails closed.

Each still deserves a test. For E8 and E9, add a masternode to the `voting()` fixture with an attached key whose id
differs from the list's, and drive a `Deferred` with two `hand_off`s of one vote while the first holds its permit.
For E11, force `sign_and_reserve` to fail after the charge.

`mg05_a_contacts_address_is_charged_as_foreign`'s doc says "or sending its change there", but it tests only a
recipient. Add a `ChangeTarget::Address(contact)` case, or drop the words.

### Nit F4: under D20 the keychain error is reported as `no_voting_key`

`rust/crates/dw-engine/src/governance/voting.rs:364,374`

The keychain read can fail with something other than `Locked` (`Vault` I/O, `Derivation`). When the masternode then
has no attached key, the vote reports `no_voting_key`, which tells the user their wallet lacks the key. Report
`signer_unavailable` when the keychain errored with anything other than `NoSecret` and the attached lookup is
`NoSecret`. This is not a fail-open (ask 5).

### Nit F5: the fence admits a `Vote` under an unleased dispatch scope

`rust/crates/dw-engine/src/platform/lease/fence.rs:2974-2983`

`Origin::Unleased(_)` returns `FirstUnleased` for every `ArtifactKind`, `Vote` included. D5 makes the voting lease
the vote's authority. No caller admits a vote unleased today; `hand_off` always runs under `lease.scope`. Refusing
(or deferring) `ArtifactKind::Vote` there would make D5 structural, as `asset_lock` is.

### Nit F6: two stale doc comments

- `dw-app/src/api/vault.rs:379-385`: `check_payment_auth`'s doc still says the setting covers "`spend` and
  `platformOp` grants". Since D11 it also covers `masternodeOp` and `governance`.
- `dw-engine/src/governance/reads.rs:72-80`: `require_relaying` was inserted between `require_synced`'s doc comment
  and its `fn`. Rustdoc now opens `require_relaying`'s doc with "`governance.not_synced{phase}` until a run reached
  `synced` (MG-00 D21)", and `require_synced` has no doc.

## The implementer's asks

**1. D19 (`wallet_owns` for every caller): correct everywhere. Keep one predicate.** The callers:
- `send/mod.rs:1104` `outflow_of`: the `Spend` and `Masternode` lease charges and the unleased `Spend` grant check
  (`:1035`), plus `recipient_mine` and `change_mine` in the summary.
- `send/mod.rs:1956` `record_send_metadata`: whether a recipient is entered as Receive or Send.
- `labels.rs:93` `owns`: the address book's "mine".
- `psbt.rs:206` `outputs_mine`: the PSBT cap and `is_mine`.

A DIP-15 external-account address is derived from the contact's xpub, and the wallet can't spend or receive at it.
So "foreign" is the truth for each caller, not just the grant charge. A grant-only narrow predicate would leave
quick-unlock `Spend` grants and the PSBT cap open to the same bypass H1 closed, because a contact payment would count
as a self-payment. I found no caller that relies on a contact address being "mine":
- balances and history come from key-wallet, not `wallet_owns`;
- `DashpayReceivingFunds` (the wallet's own receiving account from a contact) is not contact-owned and stays mine;
- DP3-01's placeholder is unaffected (`contact.rs:50-57`).

The one gap is the two PSBT lookups that don't use `wallet_owns` (F2).

**2. D11 (Governance under the payment-auth setting): the right home.** The setting's threat is a renderer obtaining a
signing grant without a prompt (`check_payment_auth`). A vote batch is exactly that, and it is hard to undo:
`too_often` locks a changed vote for an hour, and Core keeps the newest. Grouping by "what a compromised renderer
could do silently", not "moves funds", is the right axis. One grant per batch already gives MG-00's "N votes, one
passphrase". Quick unlock still issues it at value 0 (D2), which the setting accepts as a credential. Only the doc
comment lags (F6).

**3. D1 (the vote digest): correct for every network.** What Core v24 does:
- **Testnet** (`Params().NetworkIDString() == CBaseChainParams::TESTNET`; the comment calls it "Harden Spork6", but
  no spork is consulted):
  - Signing: `CWallet::SignGovernanceVote` (`wallet.cpp` ~2396) signs `vote.GetSignatureHash()` through
    `SignSpecialTxPayload`, a compact hash signature.
  - Checking: `CGovernanceVote::CheckSignature(const CKeyID&)` (`vote.cpp:164-190`) uses `CHashSigner::VerifyHash`.
  - The hash: `GetSignatureHash()` is `SerializeHash(*this)` with `SER_GETHASH`. That is sha256d over
    `masternodeOutpoint` (32-byte txid, then `n` as u32 LE), `nParentHash` (32 bytes), `nVoteOutcome` (int32 LE),
    `nVoteSignal` (int32 LE), `nTime` (int64 LE). That is 84 bytes, and `vchSig` is excluded under `SER_GETHASH`
    (`vote.h:190-206`). Outcome comes before signal here.
- **Mainnet, devnet, regtest:**
  - Signing: `SignMessage(GetSignatureString())`.
  - Checking: `CMessageSigner::VerifyMessage`.
  - The string: `GetSignatureString()` is `ToStringShort() + "|" + nParentHash.ToString() + "|" + nVoteSignal + "|"
    + nVoteOutcome + "|" + nTime` (`vote.h:166-172`). Signal comes before outcome here. `ToStringShort` is
    `"%s-%u"` of the display-order txid.
  - The hash: the signmessage hash is sha256d(compact-size ‖ `"DarkCoin Signed Message:\n"` ‖ compact-size ‖ string).

`FundingVote::digest` (`dw-vault/src/vote.rs:76-99`) matches both layouts, including the two opposite field orders,
and `dw_message::message_hash` uses that magic. The 65-byte signature with header `27 + recid + 4` (compressed)
recovers to the hash160 of the compressed key, which is what both verifiers compare with `keyIDVoting`. A BLS
(operator) signature is only checked for non-funding signals or unknown parents (`IsValidForUnknownParent`), and the
wallet never makes one.

End to end: the store's own recover-and-key check accepts the signed votes (the regtest message path). Testnet is
pinned by the layout test and by the equality test against `GovernanceVote::signing_digest`. Mutants V6 (field
order) and V10 (network switch) are both caught.

**4. D4/D14 (charge, TOCTOU, refunds, `FundNew`): sound.**
- **No TOCTOU.** `check_tx_signer` hands out nothing; it is an early refusal only. `charge_tx_signer` re-reads the
  lease state, the signer and the budget, and charges, in one J step (`table.rs:1050-1080`). The charge is then held
  by `SpendCharge` until `bind_spend` keys it by txid with its purpose. At admission it moves to the row-less entry
  with the same purpose (`fence.rs:3001-3008`). A lease revoked in between fails `FenceState::live` and goes to
  `refuse_rowless`.
- **Refunds.** A definite `NotSent` (`NoPeers`) refunds the `Masternode` budget; the test asserts it, and E6 is
  caught. `abandon` refunds through `bind_spend`'s purpose; E12 is caught. A lock revokes the lease, after which
  `prepare` is `cancelled` (tested) and its budget no longer matters.
- **No double charge.** A retry after a refund charges once more. A second prepare can't bind over a live first,
  because the first's reserved inputs give a different txid.
- **`FundNew` and foreign collateral.** A collateral to the wallet's own address is exempt (charged fee only, the
  exact cap signs, one duff under refuses). A collateral to a foreign or contact address is charged amount plus fee.
  E1, E2, E2b and E3 are all caught.
- **The voting lease** has neither budget. `tx_purpose` is `None`, so `prepare` is `send.grant_invalid`, and E4 is
  caught.
- **Raw grant ids.** The unleased path redeems `GrantKind::Spend` only and `vault.signer` requires `signs()`, so a
  raw `MasternodeOp` or `Governance` grant id passed to `prepare` signs nothing.

D4's documented looseness stands: a `MasternodeOp` lease prepares any `TxDraft` within its cap, not only a ProTx. It
is bounded by the same cap and by D11.

**5. D20/D21/D22:**
- **D20 is not fail-open.** Both keys are authorized by the same `Governance` grant (D3). Both go through the same
  `MasternodeVoting` signer with its scope and epoch checks: `attached_secret` re-checks the op epoch, and V8 is
  caught. The attached key is used only if it hashes to the list's voting key id. `Locked` still cancels. The
  fallback widens no authority; it only picks between two keys the grant already covers. The reason string is a
  nit (F4).
- **D21 is accepted.** `store_masternode_key` needs the full-scope data key and signs nothing. A swapped key can't
  vote for a masternode whose list id it doesn't match, and a swap between `key_id` and `sign_vote` is rejected by
  the store's key check.
- **D22 is accepted.** `Erased` erases the in-gate copy on drop, panics included. The by-value copies through
  secp256k1's constructor and the `Result` move are the same class as the existing `non_secure_erase` handling of
  `ExtendedPrivKey`.

## Also checked

- **Operator signing.** `MasternodeOwner` refuses `3'/3'` (operator) and `3'/4'` (platform node), and so does
  `MasternodeVoting`. `allows_attached` admits only `voting`, and `operator`/`platform` are not storable roles. V9
  (owner scope reaches the operator family) is caught. The `no_operator_signing` guard is meaningful: P1 (a
  `prepare_update_service` fn added to `dw-vault/src/vote.rs`) makes `no_operator_key_signing_path_remains` fail.
- **The fence.** Each vote goes through `hand_off` → `admit(ArtifactKind::Vote)` under `lease.scope`. Authority is
  `e.voting` on a live lease; E5 is caught. `Deferred` becomes `in_flight` (untested, F3). `Refused` becomes
  `cancelled`, tested by `a_lock_before_hand_off_cancels_the_vote`. A duplicate re-admission can't downgrade a sent
  vote, because `dispatch_status` checks `fence.sent` first. Leases end on every early return through
  `LeaseOwner`'s drop.
- **Contracts.**
  - mg-engine is at `Contract-Version: 5`.
  - m4 is at `Contract-Version: 32`. Its blessed marker `surface-sha256: dc38b642… version: 32` matches the
    `FlowKind` and `BudgetPurpose` additions, and item 59 records them. `m4_dashpay_contract` and
    `mg_governance_contract` are in the diff.
  - I did not re-run the contract tests. The baseline targeted suites pass: dw-vault lib 19 and
    `masternode_signers` 8; dw-engine `mg05` and `governance::voting`, 10.
- **The r1 gate flakes.** Neither is plausibly caused by this diff.
  - The `e0_05_bringup` close bound is in `platform/runtime_tests.rs`, and the diff touches no bring-up or close
    code. It adds two fields to the lease `Entry` and two match arms.
  - The dw-governance sync late-delivery broken pipe: the dw-governance diff is `atoi_u32` (moved from the console),
    `parse_display_hex` use and the read-only `resolved_collateral`. No sync or P2P code.
  - The flake this diff does add is F1.

## Mutants

Applied one at a time in the scratch copy, with the targeted tests run each time. The restored files were checked
byte-equal to `ebf68683` at the end.

| ID | Mutation | Result |
|---|---|---|
| V1 | `MasternodeVoting` also allows `Sign` at voting paths | caught: `masternode_voting_votes_with_voting_keys_only` |
| V2 | `MasternodeOwner` `Sign` reaches voting keys | caught: `masternode_owner_signs_…`, `masternode_voting_…` |
| V3 | `allows_attached` ignores the role | caught: `masternode_voting_…` |
| V4 | `vote_signer` accepts any grant purpose | caught: `each_signer_takes_its_own_purpose_only` |
| V5 | `masternode_signer` accepts any grant purpose | caught: `each_signer_takes_its_own_purpose_only` |
| V6 | testnet preimage: signal before outcome | caught: `digest_follows_core_layouts` |
| V7 | provider-key paths accept a hardened leaf | caught: `provider_key_shapes` and 3 more |
| V8 | `attached_secret` skips the epoch check | caught: `a_lock_ends_the_vote_signer` |
| V9 | `MasternodeOwner` reaches `3'/3'` operator keys | caught: `masternode_owner_signs_…` |
| V10 | digest network switch inverted | caught: `digest_follows_core_layouts` |
| E1 | `prepare_leased` charges without the fee | caught: `mg05_a_fund_new_…`, `mg05_a_contacts_…` |
| E2 | every recipient counted foreign | caught: `mg05_a_fund_new_…` |
| E2b | every recipient counted as the wallet's | caught: `mg05_a_fund_new_…`, `mg05_a_contacts_…` |
| E3 | `wallet_owns` includes contact accounts | caught: `mg05_a_contacts_address_is_charged_as_foreign` |
| E4 | voting lease gets the `Masternode` budget | caught: `mg05_a_voting_lease_cannot_sign_a_transaction` |
| E5 | fence admits a `Vote` under any live lease | caught: `a_vote_needs_a_governance_grant_and_a_voting_lease`, `vote_signs_…` |
| E6 | fence moves a Masternode charge as `Spend` | caught: `mg05_a_fund_new_…` (`NoPeers` refund) |
| E7 | `Voting` flow also takes `MasternodeOp` | caught: `mg05_a_voting_lease_…`, `a_vote_needs_…` |
| E8 | `Deferred` reported as `duplicate` | **survived** (F3) |
| E9 | attached key used without matching the list id | **survived** (F3; the store rejects the vote) |
| E10 | D20 fallback made fail-closed | **survived** (F3; no test of a non-`Locked` keychain error) |
| E11 | `SpendCharge` drop refunds to `Spend` | **survived** (F3; fails closed) |
| E12 | `SpendCharge` binds as `Spend` | caught: `mg05_a_fund_new_…` (abandon refund) |
| E13 | keychain voting key ignored | caught: `vote_signs_…`, `whole_call_errors_…` |
| P1 | operator builder name added in dw-vault | caught: `no_operator_key_signing_path_remains` |
