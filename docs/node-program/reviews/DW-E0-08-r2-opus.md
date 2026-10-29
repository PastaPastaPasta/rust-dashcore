**Verdict: CHANGES-REQUIRED** (one Major, a one-sentence text fix; every r1 finding is closed)

# Review DW-E0-08 r2 (Opus): closure check of the DashPay facade skeleton and the M4 contract

- Branch `dw/e0-08-dashpay-facade` at `c3c9bbd`: `cea8baf` → `cf9ba8e` (r1 fixes) → `c3c9bbd` (E0-04 additions,
  Contract-Version 2). All three commits are on `main` `ffb4f99`.
- Reviewed in a fresh detached worktree at `/work/scratch/dw-e008-r2/wt`, with TMPDIR=/dev/shm/dw-e008-r2-tmp (0700).
  The r1 worktree was removed with `git worktree remove`.
- Cross-checked against E0-04 design rev1, `docs/design/E0-04-grants-leases.md` at `dw/e0-04-design` `d2a21c6`: §16,
  plus §4.1–§4.7, §5.5, §6.3, §7.2–§7.6, §8.4 and H11/H12.

## Gates

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | clean (exit 0) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean (exit 0) |
| `cargo test --workspace --no-fail-fast` | pass: 520 passed, 0 failed, 1 ignored, across 59 test binaries |
| `cargo test -p dw-engine --test m4_dashpay_contract` | 8/8 pass |
| Bless probe (`PaymentLock.since` u64 → i64; reverted) | fails loudly and requires a version bump (below) |
| Secrets probe (untracked test file, deleted) | `AvatarSource` can't serialize; `Debug` hides the e-mail; 8 of 10 serde error cases don't echo it, and the 2 that do need the e-mail in the wrong field (below) |

### Bless probe, step by step

| Step | Outcome |
|---|---|
| A. Edit only, no `DW_BLESS` | FAILED. The run printed the diff `- pub since: u64,` / `+ pub since: i64,` and "§3 is out of date … raise Contract-Version". |
| B. `DW_BLESS=1`, version still 2 | FAILED. It printed the diff, then "the surface changed: raise Contract-Version (now 2) above the recorded version 2 before blessing". The contract file was left untouched. |
| C. `DW_BLESS=1 CI=1` | FAILED: "refusing DW_BLESS under CI". |
| D. Header raised to 3, `DW_BLESS=1` | FAILED by design, with "blessed §3 as version 3 (36c97e…); review the diff above and re-run". It rewrote the fingerprint and the version mark. |
| E. Re-run without `DW_BLESS` | passed |

Both files were reverted with `git checkout`, and the worktree was clean afterwards.

### Secrets probe

`AvatarSource` doesn't implement `Serialize` (a compile-time assertion), and `Debug` prints `Gravatar { .. }`. I tried
10 serde error cases for `AvatarSource`: wrong-typed e-mail, a misspelled tag, a missing tag, truncation, trailing
data, the e-mail placed in `bytes`, and others. 8 of them don't echo the e-mail. The 2 that do put the e-mail where
it doesn't belong:

- `"crop":"alice@…"` gives `invalid type: string "alice@…", expected struct CropRect`;
- `"kind":"alice@…"` gives `unknown variant "alice@…"`.

That is serde's normal behaviour and not a realistic leak (n-3 below).

I also tried 5 cases for `BearerSecret` in a struct: a wrong-typed sibling, trailing data, truncation, and a number
where the secret should be. None echoes the link. `from_utf8` on invalid bytes returns
`InvalidArgument{"bearer secret is not UTF-8"}`.

## 1. Closure of r1

| r1 | Status | Evidence |
|---|---|---|
| **M-1** registration can't return `name.*`/`invitation.*` | **Closed** | `RegistrationError::{Name, Invitation}` (`errors.rs:160-162`), with a passthrough `code()` and `platform()`. The hand-written `From` impls fold a wrapped `Platform` variant into `RegistrationError::Platform`, so each code has one representation (`errors.rs:193-210`). `wrapped_codes_pass_through` asserts `name.unavailable_for_invite` and `invitation.claimed`. The test's `is_wrapper` drops every one-field `*Error` variant. §2.2 and §4 document the extra codes. `AvatarError` is not wrapped, which is consistent because m-3 removed the upload from registration. |
| **M-2** no code for an unknown or cancelled outcome | **Closed** | `platform.broadcast_unknown{artifact}`, `platform.cancelled`, plus `platform.will_be_sent{artifact}` (`errors.rs:44-57`). §4 lists the writes that can return them and gives the UI rule. See N-1 for one clause added in c3c9bbd. |
| **M-3** the row can't say why it's parked | **Closed** | `waiting: Option<RegistrationWait>` (`Unlock`, `Authorize`, `Sync`, `InstantSend`, `ChainLock`, `Network`), `holds_key` and `funds_committed`. `proof` is dropped. `failure: Option<RegistrationFailure>` carries parameters (`registration.rs:80-151`). §5 says when `grant` is required. |
| **M-4.1** bless rubber-stamps | **Closed** | `Contract-Version` header; `<!-- surface-sha256 … version … -->`; an LCS diff on every mismatch. Bless refuses under `CI`, refuses an unbumped surface change, and always fails (`m4_dashpay_contract.rs:334-380`). The probe confirms each of these. |
| **M-4.2** listing blind to impls, cfg and so on | **Closed** | The listing renders trait-impl headers, `#[cfg]`/`#[serde]`/`#[derive]` on items, variants, fields and methods, and `pub const`/`type`/`use` and item macros (`:204-268`). It still can't see impls written outside the facade files (n-1). |
| **S-1.1** nothing pins "never serializes" | **Closed** | `assert_not_impl_any!(BearerSecret: Serialize, Display, Clone, PartialEq, Into<String>)` and `(AvatarSource: Serialize, Clone)` (`:811-818`). `RegistrationRequest` no longer carries a secret, so it may derive `Serialize`. |
| **S-1.2** `AvatarSource` serializes the e-mail | **Closed** | It now derives `Deserialize` only, with a redacting `Debug` (`profile.rs:46-78`). Confirmed by the probe. |
| **S-1.3** secrets enter as host strings | **Closed** | `BearerSecret::from_utf8(Zeroizing<Vec<u8>>)` takes the buffer without copying it and zeroes it if invalid. §1 says E0-13 binds the secret as bytes and states the IPC residual. |
| **S-1.4** rules for details and argv | **Closed** | §1 "Secrets": no `detail` or `Display` quotes a secret; `dwcli` reads secrets from stdin or a file. |
| **S-1.5** `PartialEq`/`Clone` | **Closed** | Both are removed and asserted absent. |
| m-1 duffs shortfall and quotes | Closed | `credits.funding_insufficient`, `credits.below_minimum`, `top_up_quote`, `withdraw_quote`. |
| m-2 event label and `ContestLocked` | Closed | `reference` is defined per kind; `EventKind::ContestLocked`. |
| m-3 registration uploads | Closed | The candidate must already have a URL, otherwise `invalid_argument`. "Registration never uploads" (§5). |
| m-4 invitations with `NoVault` and after a restart | Closed | The 0600-file path is in §2.9; `pending_invitations` and `forget_invitation` exist. |
| m-5 one big file | Closed | Split into the §3.1 domain files; the largest is `errors.rs` at 394 lines. Re-exports are by name, and `every_public_item_is_re_exported` enforces that. |
| m-6 mapping owner | Closed | E0-05 owns it, in the ROADMAP row and in §6. |
| m-7 bytes as number arrays | Closed | `base64_bytes`; `byte_payloads_are_base64`. |
| m-8 contact refusal reason | Closed | `Ineligible{reason: Eligibility}` and `contact.scan_expired`. |
| n-1 the stub test parses `Display` | Closed | It now goes through `platform()` and matches `NotImplemented{call}`. |
| n-2 parser collisions and Kind check | Closed | Rows are keyed by owner; duplicate rows panic; Kind must match exactly. |
| n-3 faucet | Closed (better than asked) | `prepare_faucet_lock` returns `FaucetLockKey`, and the request carries `FaucetAssetLock{key, proof}`. No private key crosses the facade. |
| n-4 `avatar(identity)` | Closed | §2.7 says "any identity". |
| n-5 static tables | Closed | §1 "Static tables before Platform answers". |
| n-6 failure parameters | Closed | `RegistrationFailure{needed, available}`. |
| n-7 long lines | Closed | Only table rows and the generated listing exceed 120 columns. |
| n-8 `unverified` coverage | Closed | §5 says DP3-04's money-move gate checks `dp_trust_unverified` whatever the record says. |

## 2. New findings

Severity: **Major** = fix before merge; **Minor** = fix before merge, or record an owner and due task in §6/§7;
**Nit** = optional.

### N-1 (Major): "absent from the journal" is treated as safe to retry, but for state transitions it means "unknown"

**Where:** `docs/contracts/m4-dashpay-engine.md:1170-1171` ("every retry the host offers act[s] only when the
artifact's journal entry is `NotSent` … or absent"). Also `:227`, where `dispatch_status` returns "`None` with no
entry".

**What's wrong.**

- The rule contradicts the contract's own `broadcast_unknown` rule at `:1162`: retry "only once `dispatch_status` says
  `NotSent`".
- It also contradicts E0-04 H11: no retry "unless `dispatch_status` of its artifact is `DefinitelyUnsent`".
- "Absent" is safe only for **registered** artifacts, meaning asset locks. They get a durable entry before any
  hand-off (E0-04 §5.5 `register`, I1), so no entry means never registered.
- **Row-less** artifacts never have a durable journal entry. That covers every state transition (`withdraw`,
  `send_request`, `accept_request`, `update_profile`, `register_name`) and `TxDraft` sends:
  - E0-04 §5.5 keeps them in an in-memory, per-process set ("It is not persisted").
  - On `DefinitelyUnsent` it **forgets** a state transition's id.
  - So `dispatch_status(st_hash)` returns `None` in two cases the host can't tell apart:
    - the transition was definitely not sent and its id was forgotten;
    - it was handed off by an earlier process, and its outcome is unknown.

**Failure scenario.**

1. `withdraw(identity, to, All, grant)` times out after the hand-off and returns
   `platform.broadcast_unknown{artifact: <st hash>}`.
2. The app restarts.
3. The host follows `:1170-1171`: `dispatch_status(<st hash>)` returns `None`, which counts as "absent", so the host
   offers "Withdraw again".
4. The retry fetches a fresh identity nonce. If the first transition landed, the nonce has advanced and the second
   withdrawal is valid as well: the credits are withdrawn twice.

The same path re-sends a contact request, a name registration or a profile update, and pays each one's credit fee
twice. This is exactly the r1 M-2 scenario, reopened by one clause.

**Fix.**

- **E0-08 should move (now; text only):**
  - Make `:1170-1171` say "`NotSent`, or, for a registered artifact (an asset lock) only, absent".
  - In the §2.11 `dispatch_status` row, say that `None` for a state-transition hash or a row-less txid means
    "unknown", which the host treats like `MaybeSent`. That artifact becomes retryable only through
    `DispatchResolved(NotSent)`, or through the flow's own proof wait (E0-04 §7.4).
  - Say what an in-flight entry reports. A live `Unsent` (a lease that hasn't committed yet) or `Committing` must not
    read as `NotSent`; I suggest `MaybeSent`.
- **E0-04 should move too (its closure check):**
  - §5.5 should keep a per-process tombstone that answers `NotSent`, instead of forgetting a settled state
    transition's id.
  - §4.6 should define `dispatch_status` for row-less artifacts. Its `Unknown` value appears there undefined (see the
    table in §3, item 6).

### N-2 (Minor): the §4 refusal list still forbids discard by phase, not by journal

**Where:** `m4-dashpay-engine.md:1199`: "`discard_registration` at or after `FundingSent` … `invalid_argument`".

**What's wrong.** c3c9bbd moved the rule to the journal:

- §2.2 `:128` allows discard "only when the lock's entry is revoked or gone";
- §6 says that a `DispatchResolved(NotSent)` moves a registration row back to retryable.

That row is `Failed` with `failure.phase = FundingSent`, or still `FundingSent`. Read literally, `:1199` refuses to
discard it, even though its lock provably never left.

**Fix.** Write "`discard_registration` while `funds_committed` (§5)".

### N-3 (Minor): `funds_committed`'s definition counts a revoked lock as committed

**Where:** `m4-dashpay-engine.md:1240`: "true once the asset lock is anything but definitely not sent, **that is once
its journal entry left `Unsent`**".

**What's wrong.**

- `Unsent → Revoked` also "leaves `Unsent`", and `Revoked` means definitely not sent (E0-04 §6.3).
- E0-04 §16.5 has the opposite problem. "Any registered artifact … that is not definitely unsent" literally includes a
  live `Unsent` entry, one that is registered but whose lease hasn't committed it. That would turn on "Funds locked —
  finishing" in the window where Lock still cancels cleanly, which contradicts E0-04 §4.6's own "Lock to cancel" copy.

**Fix.** Both documents should say: "its entry is `Committing`, `Dispatching`, `Ambiguous` or `PreFence`" (in Mode B,
the funding call's permit has been taken).

- **E0-08:** fix the parenthetical.
- **E0-04:** adopt this reading in §16.5 and §4.6.

### N-4 (Minor): `RevokeCause` lacks E0-04's `WalletClosed`

**Where:** `flows.rs:51-57`. E0-04 §4.3's state table has `Revoked{Lock | Close | PassphraseChange | WalletRemoved |
WalletClosed}`.

**Failure scenario.** A wallet closed while a flow runs produces `platform.lease_revoked{cause}` with no variant to
carry the cause. The engine must then either pick a wrong cause or add a variant later, which changes a serde enum
that the bindings match exhaustively.

**Fix.**

- **E0-08:** add `WalletClosed` now; it's cheap under the version rule.
- **E0-04:** alternatively, fold `WalletClosed` into `Close` in its closure round.

The two documents must agree either way.

### N-5 (Minor): Lock has two codes, and E0-04 §16 knows only one

**Where:** the `m4` §4 lease table (`:1176-1187`).

**What's wrong.**

- A lock that wins `begin_flow` or a flow's permit is `platform.cancelled`.
- A later call that names a lease revoked by Lock is `platform.lease_revoked{cause: Lock}`.
- The distinction is defensible: both mean "this call sent nothing".
- E0-04 §16.4, however, has neither `lease_revoked` nor `lease_expired`. Its §4.3 `Revoked{cause}` and `Ended`
  states have no mapping there.

**Fix.**

- **E0-04 should move:** adopt `lease_revoked{cause}` and `lease_expired` in §16.4.
- **E0-08:** add a line to §4 giving the UI's reading of `lease_revoked{Lock}`: "nothing sent by this call; unlock and
  start the flow again". That is the "Accepted — payment cancelled. Pay now?" case of E0-04 §4.6, seen from the
  facade.

### N-6 (Minor): which calls accept a lease id is wider than E0-04's rule

**Where:** `m4-dashpay-engine.md:82` and `:224`. Each says any call taking a `grant` accepts a lease id.

**What's wrong.**

- E0-04 §16.1 narrows this to "any call **of its wallet whose purpose it carries**".
- The contract maps the wrong-wallet case to `grant_invalid`, but says nothing about a lease that lacks the purpose
  the call needs. Example: a `ProfileEdit` lease passed to `top_up`.

**Fix (E0-08).** State the restriction, and give the code: `platform.needs_grant{purpose}` matches E0-04 §4.3 ("a
purpose the fresh grants do not cover drops to 0").

While you're there, state E0-04's 10-minute idle-reaper bound for vault-key leases instead of "a few minutes (E0-04
sets N)".

### Nits

- **n-1. The listing can't see trait impls outside the facade files.** `NOT_FACADE` skips `mod.rs`, `signers.rs` and
  `status.rs` (`m4_dashpay_contract.rs:36`), and the rest of the crate is never scanned. An
  `impl Deref<Target = str> for BearerSecret` (or `AsRef<str>`, or `Borrow<str>`) in `signers.rs` would be invisible
  to the listing and pass the static assertions, and then `format!("{}", &*secret)` compiles.
  - Fix: add `Deref<Target = str>`, `AsRef<str>`, `Borrow<str>`, `AsRef<[u8]>` and `ToString` to the
    `assert_not_impl_any!` list, or scan every `src/**/*.rs` for `impl … for BearerSecret|AvatarSource`.
- **n-2. Grant sizing without the payload.** `GrantAction::{UpdateProfile, SendRequest, AcceptRequest}` carry no
  payload, so `grant_request` can only return a worst-case bound for the action.
  - Fix: say so in §5 `GrantRequest`. The contract already promises "charges no more than it quoted", and a worst-case
    quote keeps that promise.
- **n-3. Serde echoes a mis-placed e-mail.** The probe shows that `"crop":"alice@…"` and `"kind":"alice@…"` echo the
  value in the serde error. That's harmless for a well-formed host.
  - Fix, optional: have E0-13 map `AvatarSource` parse errors to a fixed `invalid_argument` detail, in line with
    §1's rule that no detail quotes private input.
- **n-4. `DispatchResolved.wallet_id: String`.** This breaks the §1 rule "Wallet: `WalletId`" and the other
  `EngineEvent` variants, which carry `wallet_id: WalletId` directly. `WalletId` has no serde, which is presumably why
  `String` was used.
  - Fix: let the future variant carry `wallet_id: WalletId` next to `network` and drop the field from the payload, or
    document the field's encoding (lower-case hex).
- **n-5. `send.cancelled` (E0-04 §16.11) has no row in m4 §6.** It is m1's `SendError` for DP3-01, but E0-04 routes it
  through this contract's change list.
  - Fix: add a §6 row (DP3-01) so it isn't lost.
- **n-6. Copy drift.** m4 `:1239` reads "Lock to stop; you'll finish after you unlock", while E0-04 §4.6/§16.10 reads
  "Lock stops it here; you'll finish after you unlock". Pick one.

## 3. Cross-check against E0-04 rev1 §16

| §16 item | E0-08 at c3c9bbd | Match? | Side to move |
|---|---|---|---|
| 1. `begin_flow(wallet_id, flow, grants) -> String`, `end_flow(id)` | Same signatures (`flows.rs:113-127`); `end_flow` is sync and idempotent; a lock during redeem is `cancelled` | ✓ | — |
| 1. A lease id accepted "by any call of its wallet whose purpose it carries"; reaper after 10 min | Any call; "a few minutes" | ✗ wider, vaguer | E0-08 (N-6) |
| 1. `begin_lease(self: &Arc<Self>, …)` | `begin_flow(&self, …)` | ~ an implementation detail, but a later change to `self: &Arc<Self>` is a signature change | E0-08 may want `self: &Arc<Self>` now (nit; the session already uses that receiver for `dashpay`) |
| 2. `grant: GrantRequest{max_duffs, max_credits}` in the three quotes | Present in all three | ✓ | — |
| 2. `grant_request(action: WriteAction)` covering `set_private_details` | `grant_request(identity, action: GrantAction)` with `PublishPrivateDetails`, `RegisterName{label}` | ✗ names and arity | **E0-04**: adopt `GrantAction` and the `identity` argument (the fee depends on the identity's key and the name's label) |
| 3. `RegistrationWait::Authorize` | Present | ✓ | — |
| 3. `RegistrationStatus.needs_grant: bool` (true while `Parked{ProofWaiting}`) | Not added. Instead, the row moves to `Unlock`/`Authorize` once the proof arrives and keys are needed; while parked it reads `ChainLock` with `holds_key = false` | ✗ different encoding of the same rule | **E0-04**: drop `needs_grant`. One field can't disagree with itself; DP1-02 must then perform the transition, which m4 §5 already requires |
| 4. `lease.locked` → `platform.cancelled` | Yes | ✓ | — |
| 4. `Parked`/`NeedsGrant` → `platform.needs_grant{purpose}` | Yes | ✓ | — |
| 4. Budget refusal → `grant_exceeded{purpose, needed, remaining}` | Yes; `BudgetPurpose` = `Funding`/`Credits`/`Spend`/`Crypto`, as in E0-04 §4.2 | ✓ | — |
| 4. Journal unavailable → `platform.storage` | The m4 code is `storage`, without the `platform.` prefix (§7 decision 4, the m1 common codes) | ~ | **E0-04**: write `storage` |
| 4. `platform.broadcast_pending` for `WillBeSent` | `platform.will_be_sent{artifact}` | ✗ name | **E0-04**: its own outcome (§4.6, §8.4) and the m4 `DispatchState` are both `WillBeSent` |
| — `Revoked{cause}`, `Ended` | `lease_revoked{cause}`, `lease_expired` | E0-04 has no mapping | **E0-04** (N-5) |
| — `Revoked` causes: 5 including `WalletClosed` | `RevokeCause` has 4 | ✗ | E0-08 or E0-04 (N-4) |
| 5. `holds_key` = `own_key` ∧ state ∈ {`Active`, `AwaitingProof`} | Same (§5) | ✓ | — |
| 5. `funds_committed` = "not definitely unsent" | "left `Unsent`" | ✗ both imprecise | Both (N-3) |
| 6. Discard only when `DefinitelyUnsent` or no entry | §2.2 matches; the §4 refusal list still says "at or after `FundingSent`" | ✗ internal | E0-08 (N-2) |
| 6. Retry only after `DispatchResolved(NotSent)` or `DefinitelyUnsent` | `:1162` matches; `:1170-1171` also allows "absent" | ✗ | **E0-08** (N-1, Major); E0-04 for row-less semantics |
| 6. `NetworkSession.dispatch_status(wallet, artifact) -> DispatchStatus{DefinitelyUnsent, Committed{will_resend: bool}, Sent, Unknown}` (§4.6) | `DashPay.dispatch_status(artifact) -> Result<Option<DispatchState{WillBeSent, MaybeSent, Sent, NotSent}>>` (async) | ✗ owner, name, values | **E0-04** should adopt m4's four values. They map one to one onto E0-04's own outcome table (§4.6, §8.4). E0-04's `Committed{will_resend: false}` appears nowhere else in rev1 and can't be told apart from `Unknown`. The `DashPay` owner is equivalent (the handle binds the wallet). Async is fine. The `None` semantics need N-1 on both sides. |
| 7. `DispatchResolved{network, wallet_id, artifact, outcome: Sent \| NotSent}` | Payload `{wallet_id: String, artifact, resolution: DispatchResolution}`; the variant `EngineEvent::DispatchResolved{network, resolved}` is deferred to E0-04 P2/E0-13 (§6) | ~ field name `outcome` vs `resolution`; the payload-only approach is fine | **E0-04**: adopt `resolution` (or E0-08 renames; trivial). E0-08: n-4 on `wallet_id` |
| 7. `LeaseChanged`, `LockProgress` | Named in §6's deferral row | ✓ | — |
| 8. Notices `DispatchRecordMissing`, `UnscopedDispatch`, `DispatchJournalUnavailable` | §6 row, E0-04 P2/E0-13 | ✓ | — |
| 9. `Vault.lock()` stays sync | m1 surface; not m4's | n/a | — |
| 10. Copy | Wording drift | ~ | either (n-6) |
| 11. `send.cancelled` | Not in m4 §6 | ~ | E0-08 (n-5) |
| — `FlowKind` (E0-04 §4.1: `Registration, TopUp, ProfileEdit, ContactRequest, Accept, AcceptAndPay, …`) | A superset: adds `Withdraw`, `NameRegistration`, `PrivateDetails`, `EnableDashPayKeys`, `Discovery` | ✓ | E0-04 should list m4's set as the closed list |

**In short.** On names and shapes (`broadcast_pending`, `WriteAction`, `DispatchStatus`, `needs_grant`, `outcome`),
m4 is the better-specified side, and E0-04 rev1's closure should adopt it. Where E0-04 is the more precise
(wallet/purpose scoping of lease ids, `WalletClosed`, retry-only-on-`DefinitelyUnsent`), m4 should move. The
`funds_committed` definition needs fixing in both.

## 4. Summary

Every r1 finding is closed, most of them exactly as proposed, and some better:

- the faucet path now carries no secret at all;
- the contract test is now a real gate: the u64 → i64 change fails three ways (no bless, an unbumped bless, a bless
  under CI) and passes only after a version bump and a second run;
- `BearerSecret` and `AvatarSource` are pinned at compile time, and the probe finds no realistic serde leak.

The E0-04 surface added in c3c9bbd is shaped well. It is clearly marked as pending E0-04 rev1, and it is ahead of the
design on naming. One clause in it breaks the anti-double-spend rule that M-2 established: treating "absent from the
journal" as retry-safe (N-1). It is a one-sentence fix in §4 plus a definition of `None` in §2.11.

- N-2 and N-3 are small wording fixes.
- N-4 is a one-variant addition.
- N-5 and N-6 are a line each.

With N-1 fixed, and N-2–N-6 fixed or given an owner in the E0-04 reconciliation, I expect to approve after a
diff-only check.
