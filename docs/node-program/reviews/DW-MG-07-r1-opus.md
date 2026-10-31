# DW MG-07 contested-name voting: independent review r1 (Opus)

Branch `dw/mg-07-contested-voting` at `dca252a4`, scope `8b0772ec...dca252a4`.
Platform pin `82fb33aff89f85f23cd69eeff71a82d523ac3ffa` (rust/Cargo.toml:32/36, Cargo.lock
`dash-sdk`/`dpp`), read from `~/.cargo/git/checkouts/platform-fff45202c37f3b1f/82fb33a`
(HEAD = 82fb33aff8). Citations below are `packages/...` at that checkout.

## Verdict: ACCEPT WITH NITS

No blocker or major finding. The signed bytes equal Platform's construction. I re-derived them
independently: the poll comes from the DPNS contract itself, the key and identity from rs-sdk's own
helpers, and the signature was checked with Drive's verifier. MG-07 adds no new signing scope or raw
path, and every mutant I tried is killed. There are three minors: m1 is a wording fix in D17, and m2
and m3 are follow-ups.

## What I verified

- **Gate claims.** `/work/scratch/dw-mg-07/gate.log` was written 12:17:53, after dca252a4 (11:55),
  and the implementer's worktree is clean at dca252a4. The log shows `tests passed 2553 failed 0`,
  vitest `Tests 996 passed`, Playwright `1519 passed`, `551 skipped` and `GATE EXIT 0`; mn_cli has 3 tests.
  In my own worktree I re-ran contested (9/9), name_votes (5/5), mg_governance_contract (10/10) and
  mg07_fixtures (2/2). All pass.
- **Independent parity test.** `scripts/mg07_review_parity.rs` was run as a dw-engine integration
  test and passes. It covers 3 labels × 2 keys (keychain index 7, attached) × 3 choices, with nonces
  1, 25 and 2^32+6. It builds:
  - the poll from `load_system_data_contract(DPNS)`: `domain` has exactly one contested index, named
    `parentNameAndLabel`, with properties `[normalizedParentDomainName, normalizedLabel]`. The label
    passes the contract's own contested `field_matches`. The parent is `"dash"`, which is rs-sdk's
    `DPNS_PARENT_DOMAIN` (rs-sdk/src/platform/dpns_usernames/contested_queries.rs:25);
  - the key from rs-sdk's `voter_identity_voting_key(&hash160)`: id 0, `VOTING`, `HIGH`,
    `ECDSA_HASH160`, read-only (rs-sdk/src/platform/transition/masternode_vote_keys.rs:71-83);
  - the identity from `voter_identity_id(ProTxHash, &hash160)`, the typed orientation (ibid. :63-65);
  - the transition from `MasternodeVoteTransition::try_from_vote_with_signer(…, PlatformVersion::latest(), None)`
    (rs-dpp/.../masternode_vote_transition/methods/mod.rs:28-58 → v0/v0_methods.rs:26-54). The signer
    is simple-signer's `SingleKeySigner::sign` body, `dashcore::signer::sign(data, secret)`
    (simple-signer/src/single_key_signer.rs:107-131).

  The test asserts that `serialize_to_bytes`, `transaction_id`, `voter_identity` and
  `signature_public_key_id == 0` are equal, and that the signature verifies with Drive's
  `verify_hash_signature(double_sha(signable_bytes), sig, hash160)` (rs-dpp/src/state_transition/mod.rs:2287-2313).
  The vault's `is_contested_label` also agrees with "contract contested match ∧ DPNS
  `normalizedLabel` ∧ no i/l/o" on a 200k-string sweep.
- **The parity fields one by one.**
  - The signable bytes are dpp's own `signable_bytes()`: `signature_public_key_id` and `signature` are
    `#[platform_signable(exclude_from_sig_hash)]` (masternode_vote_transition/v0/mod.rs:49-52). The
    digest is sha256d, and the 65-byte compact header is 27+recid+4.
  - `sign_external` sets `signature_public_key_id = key.id()` (state_transition/mod.rs:1958-1960).
    Drive gives every voter identity key id 0 (rs-drive-abci/.../create_voter_identity/v0/mod.rs:21-27).
  - The voter id is `create_voter_identifier(pro_tx_hash, hash160)`, the same as Drive's
    `create_voter_identity_from_masternode_list_item_v0` (ibid. :35-39). It uses
    `masternode.pro_tx_hash.to_byte_array()`, which is Core RPC display order.
  - The nonce comes from `get_identity_nonce(voter, bump=true)`, as rs-sdk `PutVote` gets it (rs-sdk/src/platform/transition/vote.rs:76-78).
  - **`user_fee_increase`:** `MasternodeVoteTransitionV0` has no such field at the pin
    (v0/mod.rs:43-53), so it does not apply.
  - The index values are `[Text("dash"), Text(label)]` in contract property order, as confirmed above.
- **Orientation of the list's proTxHash.**
  - `MasternodeListSummary.pro_tx_hash` is wire order (rs-platform-wallet/src/masternode/list.rs:24-28,
    58-63). It copies `entry.pro_reg_tx_hash`, which is consensus-decoded raw bytes
    (rust-dashcore-dashpay d3d50520 dash/src/sml/masternode_list_entry/mod.rs:220).
  - The engine's `platform_order()` reverses it to display order (contested.rs:346-350).
  - The T2 harness reverses Core's display hex into wire order to build the same convention
    (contested_live.rs, the `protx list` loop).
  - So the shipped orientation is the one Drive accepted live.
- **Domain separation of the voting key.**
  - A name vote's preimage is sha256d of a bincode `StateTransition::MasternodeVote` prefix. A Core
    sighash would need a valid transaction serialization whose prevout bytes are fixed by the voter
    id hash, which is not feasible.
  - The testnet funding digest is a fixed 84-byte preimage, and the mainnet one has the
    "DarkCoin Signed Message" prefix. Both are distinct preimages.
  - So adding `sign_name_vote` under `KeyUse::Vote` creates no cross-protocol signature.

## 1. Signing parity: holds

The parity holds, as shown above. The implementer's test (dw-vault/tests/name_votes.rs:160-194)
builds its poll from literals that mirror the vault's. Mine takes it from the contract, so the two
tests do not share an assumption. Both pass.

## 2. Authorization (MG-05): no new hole

- `crates/dw-vault/src/signer.rs` is unchanged on the branch.
  - `SignerScope::MasternodeVoting` allows only `KeyUse::Vote` on `paths::is_voting_key` (signer.rs:159-161).
  - `allows_attached` allows only `(MasternodeVoting, Vote, "voting")` (signer.rs:180-182).
  - `MasternodeOwner` refuses `Vote` (`_ => false`, signer.rs:162-172), and `DashPayCrypto` refuses
    it explicitly.
- `VoteSigner` is made only by `governance_signer` under a `GrantKind::Governance` token with
  `SignerScope::MasternodeVoting` (vault.rs:2111-2120).
- The new method `sign_name_vote` (vote.rs:184-210) reaches keys only through `with_secret`, which
  takes a voting path or the `ROLE_VOTING` attachment. It signs bytes the vault lays out itself:
  - the caller supplies the label, which is charset-bound and checked before the key is read
    (vote.rs:189-194);
  - the contender id, nonce, proTxHash and choice are typed fields;
  - the poll, contract and prefix are fixed.
- No `Signer`, digest or message method was added. No dwcli, dw-app or FFI path reaches
  `VoteSigner`: dw-app lists both calls as `EXCLUDED` (api/governance.rs) and the Swift host binds
  no governance call.
- Owner keys cannot vote, and voting keys cannot `Sign`/`Message`/PSBT under `MasternodeVoting`.
  Mutants 2 and 4 confirm this.
- `SignerScope::Full` reaches every path, voting keys included. That was already true before MG-07
  (the spend signer) and MG-07 does not change it.
- **dwcli's grant before validation is UX only.**
  - `mn vote-name` authorizes before `vote_name` checks the arguments (masternodes.rs, the `VoteName` arm).
  - `vote_name` redeems the grant only after every whole-call check (contested.rs:646-662). The test
    `whole_call_errors_leave_the_grant_unused` holds this.
  - The grant lives in a dwcli process that exits after this one command. The command line is the
    user's confirmation, and the grant cannot be spent on anything else.
  - The only cost is a passphrase or KDF prompt before an argument error.
- Note for MG-11: a `Governance` grant is per purpose, not per vote (MG-05 D1, MG-07 D1). The UI's
  confirmation must be the only issuer for a `vote_name` call.

## 3. Vote-change rules and refusal codes: correct

- The mapping in contested.rs:138-162 matches Drive's typed errors and their codes at the pin:
  - 40300–40307 (rs-dpp/src/errors/consensus/codes.rs:410-418);
  - raised in rs-drive-abci/.../masternode_vote/transform_into_action/v0/mod.rs:46-120 (same
    choice → 40304 *before* the count check; `previous_vote_count >= votes_allowed_per_masternode`
    → 40303 with `times_already_voted`; then `MasternodeNotFoundError`);
  - and in state/v1/mod.rs:84-135 (Lock not offered → 40307; status must be `Started`; a non-contender → 40307).
- The local refusals are right:
  - `vote_limit` at `used >= allowed` (contested.rs:790-792);
  - the poll's end (contested.rs:540-542), killed by mutant 3;
  - a resolved poll (`winner.is_some()`) and an unknown poll (no contenders).
- An unproved vote is `Sent` in the fence and reported `unconfirmed` with its hash (179039fb, test
  `an_unproved_vote_is_sent_but_unconfirmed`).
- At the deadline: `NotEntered` gives `cancelled`/`NotSent`, and `Cut` gives `unconfirmed`/`MaybeSent`
  (contested.rs:884-898, test `a_slow_broadcast_is_cut_at_the_permit_deadline`).
- **Broadcast vs proof classification.** rs-sdk's `From<DapiClientError>` is in rs-sdk/src/error.rs:189-244.
  - A consensus error that cannot be decoded is `Protocol(e)`. At the broadcast it means DAPI sent a
    consensus refusal, so `refused` with `NotSent` is right. At the proof, `Protocol` without
    consensus is a proof or decoding failure, so `unconfirmed` is right.
  - A `StateTransitionBroadcastError` without a `cause` occurs only on the wait stream, where it is `refused`.
  - `DriveInternalError` and `Generic` are `unconfirmed`, which is conservative.
  - None of these is user-visible and wrong, except the `AlreadyExists` case in m2.

## 4. Nonce follow-up: not a merge blocker

- Drive accepts a nonce up to 24 above the stored one (rs-dpp/src/identity/identity_nonce.rs:17, 99-131).
- The SDK cache bumps under its lock and merges `max(cached, platform)` on re-fetch
  (rs-sdk/src/internal_cache/mod.rs:248-360). Concurrent callers therefore get distinct nonces and
  no two votes collide.
- A local refusal, a cut-off or a CheckTx refusal wastes one nonce. The next vote goes out at +2,
  which is accepted. T2 shows this live: the repeat `already_voted` was followed by an accepted change.
- The edge is m3.

## 5. D17: acceptable as MG-07's T2, with the m1 wording fix

- **What ran live** is the real engine session, the vault and the production `SdkNames` on dashmate
  `local`. It exercised:
  - the vault's signature, for attached keys;
  - the voter identity from wire→display proTxHash;
  - the SDK nonce;
  - the fence admit;
  - the SDK broadcast and proof wait;
  - Drive's real 40304 mapped to `already_voted`;
  - a vote change and `votes_used`;
  - all three reads of `contested_names`.
- **What was replaced:**
  - the list source: Core's `protx list valid true` went into the D14 `name_list` slot. Its byte
    order matches dash-spv's, as verified above;
  - the attachment: `set_key_id` set the recorded key id directly instead of MG-03's attach flow;
  - the vault: unencrypted, with `Credential::None`;
  - the gateway certificates, which were reissued.
- **Not exercised live:**
  - keychain voting keys. They are covered by byte parity for indexes 0, 7 and 19 (both tests);
  - a passphrase grant (MG-05's tests cover it);
  - the dwcli/SPV path, which is the stated blocker.
- **The broadcast code changed after the run.**
  - The run used contested.rs blob `2ce9a103` (e271aa70 … 4cd1e028; live-c0ntest.txt was written at
    10:24, and 0905ed90 committed at 10:28).
  - The head's contested.rs is `67b31a96`. 7225aef4 split it, and 7e2df756 changed the broadcast:
    `retries: Some(0)`, a permit-bounded `hand_off`, and `broadcast_outcome`/`proof_outcome`.
  - vote.rs changed after the run only in a refactor (`key_id_of`), so the signed bytes are the ones
    Drive accepted.
- Every signing-relevant part was exercised. The broadcast-relevant parts were exercised at the
  earlier revision, and the head's delta is narrow and unit-tested.
- Ruling: D17 is acceptable once m1 is fixed.

## 6. BL-232 contract test (dca252a4): no weakening

- `NOT_RETURNED` only removes names from the "must be seen" set. Any record a bound command returns
  is still checked by `expectRecord` (governance.test.ts:104-112, 256-271).
- The six new records are returned only by the two unbound calls.
- `NOT_MIRRORED` drops `NameChoice`/`NameVotingKey`. These are enums with fields, which
  `contractEnums` cannot parse anyway, and only the new records use them.
- The 19→21 call count and the 9→12 code count are exact. The three new codes reach the vectors
  (governance_vectors_tests.rs) with data `none`, and `ERROR_CODES` matches.
- Nothing already bound lost a check. See n3 for hardening.

## 7. Mutants

The patch is in `scripts/mutants.patch`; each mutant is env-gated by `A6MUT`, and everything was built once.

| # | Mutant | Killed by |
|---|---|---|
| 1 | vault poll `index_values` = `[label]` (dropped `"dash"`) | `name_votes_equal_platforms_construction` (name_votes.rs:180). My parity test would also kill it. |
| 2 | `MasternodeVoting` accepts any path for `Vote` (non-voting key) | `signer::tests::masternode_voting_votes_with_voting_keys_only` (signer.rs:640) |
| 4 | `allows_attached` accepts any role (e.g. `owner`) for `Vote` | the same test (signer.rs:668) |
| 3 | skip the poll end-time refusal (`check_open_poll`) | `whole_call_errors_leave_the_grant_unused` (contested_tests.rs:546) |

With A6MUT unset, the baseline was green: dw-vault (11 suites) and dw-engine `governance::` (78 tests).

## Findings

### Blocker
None.

### Major
None.

### Minor

- **m1, D17 wording.** `docs/design/MG-07-contested-voting.md` D17 says "The vote path is the
  shipped code". At dca252a4 that is not exact: the live run used contested.rs `2ce9a103`, from
  before 7225aef4 and 7e2df756 (`retries: Some(0)`, the permit-bounded `hand_off`, the outcome split).
  - Name the commit the run used and the delta since. The delta is covered by
    `a_slow_broadcast_is_cut_at_the_permit_deadline` and `broadcast_and_proof_answers_classify`.
  - Optional, and the manager's call because it broadcasts: if the devnet is still free before
    DP6-01, one re-run of `contested_live.patch` at the head (about 6 min, a single changed vote)
    would close the gap.
- **m2, `AlreadyExists` is read as Accepted and can mask a refusal** (contested.rs:185, then the 60 s wait).
  - Tenderdash keeps a transition that CheckTx refused in its cache and answers a re-send of the same
    bytes as a duplicate. The T2 harness hit exactly this: see the `user_fee_increase` comment in
    contested_live.rs's identity put.
  - Identical vote bytes recur when the nonce does. Example: a vote refused at CheckTx (say
    `already_voted`), then the app restarts. The SDK cache is empty, Platform's nonce has not moved,
    and RFC 6979 gives the same signature.
  - The user then waits 60 s and sees `unconfirmed` instead of `already_voted`. It is benign (D7) but wrong.
  - rs-sdk's `put_to_platform_and_wait_for_response` handles "already exists" by re-fetching the vote
    (rs-sdk/src/platform/transition/vote.rs:140-153). D12 drops that branch.
  - Suggestion: after `AlreadyExists` plus no proof, read `votes_of(pro_tx_hash)`. If the present
    choice is the one cast, report `already_voted`. This can be a follow-up.
- **m3, nonce drift (follow-up BL, not a blocker).**
  - Each vote that leaves no executed transition advances the SDK's cached nonce of that voter
    identity by one, and the cache never moves down in-process: re-fetch keeps `max(cached, platform)`
    (internal_cache/mod.rs:330-347), and `refresh()` preserves the value (:207-246).
  - After 24 such votes for one masternode in one app run, the cache sits more than 24 above
    Platform. Every later vote of that masternode is then refused by Drive with
    `InvalidIdentityNonceError(NonceTooFarInFuture)` (identity_nonce.rs:117-131), which reads
    `refused`, until a restart.
  - The causes are local refusals, `cancelled`/`in_flight`, cut-offs and CheckTx refusals.
  - rs-sdk's own `PutVote` behaves the same way, and votes are rare, so this is a follow-up.
  - Possible fix: for voter identities, fetch the nonce fresh, `bump_first=false` plus one, or use
    the nonce fetch's `IdentityNonce` directly. Or map `InvalidIdentityNonceError` to its own reason.

### Nit

- **n1.** `Verdict::Resend`/`FirstUnleased` broadcasts outside `hand_off`, with no deadline
  (contested.rs:900-904). D16's "nothing leaves after a lock's drain" holds for `First` only.
  - Only identical bytes signed before the lock can go out this way, so it is harmless.
  - D16 should say so.
- **n2.** In live-c0ntest.txt, "platform tally after" shows `lock=Some(0)`, while `contested_names`
  read right after shows `lock_votes 4`. That is probably a DAPI node one block behind; the final
  reads agree. A line in D17 would stop the evidence reading as contradictory.
- **n3.** `NOT_MIRRORED`/`NOT_RETURNED` in governance.test.ts are static lists.
  - Assert that every `NOT_MIRRORED` enum appears only in `NOT_RETURNED` records. Otherwise a later
    bound record carrying `NameChoice` would silently skip the mirror check.
  - MG-11 must empty both lists when it binds the calls.
- **n4.** At `votes_used >= 5` the engine says `vote_limit` even for the same choice. Drive would
  answer `already_voted` first (transform_into_action/v0:62-75). Both are refusals, so nothing changes.
- **n5.** dw-app's three new errors carry the label and identity in `message` (D13), while dwcli
  words them without the arguments (D32). MG-11 should pick one rule when it binds the calls.
- **n6.** The MG-07 §3 table names Platform codes 40300–40306 but leaves
  `VoteChoiceNotAllowedForVotePollError` without its code. It is 40307.

## Artifacts

- `scripts/mg07_review_parity.rs`: the independent re-derivation (a dw-engine integration test, not
  for commit).
- `scripts/mutants.patch`: the four env-gated mutants (`A6MUT=1..4`).
- The worktree `/work/scratch/mg07-review/wt` and target `/work/scratch/mg07-review/target` are removed.
- No devnet query or broadcast was made. `/work/scratch/dw-mg-07` and `t2/owner-keys` were not read
  or touched; I read `contested_live.rs`, `live-c0ntest.txt`, `live-run5.log` and `gate.log` only.
