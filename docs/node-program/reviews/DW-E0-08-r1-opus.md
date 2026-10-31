**Verdict: CHANGES-REQUIRED**

# Review DW-E0-08 r1 (Opus): DashPay facade skeleton and the M4 engine contract

- Branch `dw/e0-08-dashpay-facade` at `cea8baf`, one signed commit on `main` `ffb4f99`.
- Reviewed in a detached worktree at `/work/scratch/dw-e008-review/wt`, with TMPDIR=/dev/shm/dw-e008-rev-tmp.
- Read first: DASHPAY §2.2–§2.7, §3.1–§3.8, §4, §5; the ROADMAP rows E0-05, E0-08, E0-09, E0-10b, E0-13, DP1-*–DP6-*
  and X1a; m1-engine.md §1 and §4; m3-engine.md §4.
- Library checks are at the platform pin `bc41f1b` (`rs-platform-wallet`): `AssetLockFunding`, `claim_invitation`.

The skeleton is careful work. Every stub reports its own name, the code table is tested variant by variant, the
`BearerSecret` type is sound as far as it goes, and the §7 log of decisions makes the contract reviewable. These
problems should still be fixed before merge. They are cheap now. After merge they become signature changes that
four parallel DP tasks, E0-09 and E0-13 would have to absorb:

- registration can't return the `name.*`, `invitation.*` or `avatar.*` codes its own flows raise;
- no code exists for an outcome that is unknown or was cancelled by Lock;
- the registration row doesn't tell the UI why it is parked;
- `DW_BLESS` rubber-stamps any change, and nothing pins the "never serializes" property.

## (e) Gates

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | clean (exit 0) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean (exit 0) |
| `cargo test --workspace --no-fail-fast` | pass: 518 passed, 0 failed, 1 ignored, across 59 test binaries |
| `cargo test -p dw-engine --test m4_dashpay_contract` | 6/6 pass |
| Reviewer probe (untracked file, deleted afterwards) | 5 serde error cases: none echoes the WIF. `AvatarSource::Gravatar` serializes the e-mail in clear. 1 MiB of `File` bytes becomes 4.19 MB of JSON |

The commit carries a `gpgsig` header. This box has no `gpg.ssh.allowedSignersFile`, so `git log --show-signature`
can't verify it here; that is an environment limit, not a finding.

## Findings

Severity: **Major** = fix before merge; **Minor** = fix before merge, or record an owner and a due task in §6;
**Nit** = optional.

### M-1 (Major): registration calls can't return the codes their own flows raise

**Where:** `rust/crates/dw-engine/src/platform/errors.rs:103-119`; contract §2.2, §4, §7.4.

`RegistrationError` wraps only `PlatformError`, but `registration_quote` and `start_registration` run these checks:

- **The label.** It can be invalid, taken, in an open contest or locked: `name.invalid{rules}`, `name.taken`,
  `name.contest_open`, `name.locked`.
- **Invitation funding.** A contested name can't be claimed with an invitation (`name.unavailable_for_invite`). The
  invitation itself can be claimed, expired or invalid, or the wallet may already have an identity (`invitation.*`).
  The claim happens inside `start_registration`; the library's `claim_invitation` is called there.
- **The initial profile's avatar.** Per §5 `InitialProfile`, the engine "uploads a file candidate first" at Draft, so
  `avatar.upload_unconfigured` and `avatar.fetch_failed` can occur.

**Failure scenario.** A user opens an invitation link and picks a contested name. `registration_quote` has to refuse
it. Its only choices are `platform.invalid_argument` with a free-text detail, or `registration.*`, which has no
matching code. `name.unavailable_for_invite`, which §3.6 lists, is unreachable from the one flow that produces it.
The same holds for `invitation.claimed`, `invitation.expired` and `invitation.already_has_identity`: no call returns
`InvitationError` at claim time, and `invitation_status` returns a status, not an error. So the UI either shows a
generic error, or DP1-02 and DP5-02 add variants and every binding's exhaustive match breaks.

**Fix.**

- Add `Name(NameError)` and `Invitation(InvitationError)` to `RegistrationError`, with `#[error(transparent)]`,
  `#[from]` and a `code()` passthrough. Do the same for `Avatar(AvatarError)`, or see m-3.
- Extend the test's `error_enums()` filter, which drops only `Platform` and `Identity`, so that it drops wrapper
  variants generally.
- Document in §4 that a registration refusal may carry `name.*` and `invitation.*` codes.
- Add a test asserting that `RegistrationError::from(NameError::UnavailableForInvite).code()` is
  `name.unavailable_for_invite`.

### M-2 (Major): no code for an unknown or lock-cancelled outcome

**Where:** `errors.rs:11-53` (`PlatformError`); contract §4. The contract never mentions `MaybeSent` or `Cancelled`.

E0-04 (DASHPAY §2.6, "Commit points") makes every flow report one of three outcomes:

- `Sent`;
- `MaybeSent`, when a hand-off runs past H or the transport can't tell. The UI must say "may have been sent";
- `Cancelled`, when Lock wins the permit. This is definite: nothing was sent.

m1 separates these with `send.no_peers` and `send.broadcast_unknown`. This contract has no code for either of the
last two. The candidates are `platform.timeout` (ambiguous) and `platform.signer_unavailable` (wrong).

**Failure scenario.** A `top_up` asset lock or a contact request is handed off. The user presses Lock, or DAPI stalls
past H. The engine has to return something:

- If it returns `platform.timeout`, the UI shows "failed, try again". The user retries and pays twice: a second top-up
  asset lock, or a second request fee in credits.
- If it returns `platform.signer_unavailable` for a definite `Cancelled`, the UI shows "unlock to continue" for a
  flow that is in fact finished.

**Fix.** Add `platform.broadcast_unknown` (or `platform.maybe_sent`) and `platform.cancelled` to `PlatformError` now,
and list them in §4 under "Refusals §3.6 names no code for". State there which calls can return each:

- every write that hands off a signed artifact;
- for registration, the state machine records it in the row (`Failed{retryable}` or the parked phase).

E0-04 owns the semantics; this contract only needs the codes to exist.

### M-3 (Major): `RegistrationStatus` doesn't say why a flow is parked or what it waits for

**Where:** `records.rs:276-295`; contract §5 `RegistrationStatus`.

The UX in DASHPAY §2.6, §3.4 and §3.7 shows, per row:

- "Unlock to finish", when the flow parked keyless on the ChainLock wait;
- "Finish registering @alice", when it asks again at identity creation;
- "Registration in progress — Lock to cancel", while a lease holds a key on a locked vault;
- "Waiting for the network to sync", when a restored row is held until SPV sync (§3.4 Restore rule 1, §2.2 rule 5);
- "Funds locked — finishing" (F2).

`resume_registration(draft, grant: Option<String>)` makes the host decide whether to prompt for a grant. Nothing in
the record says which of these states applies. `phase` plus `proof` can't distinguish "ProofWaiting, lease alive"
from "ProofWaiting, parked keyless".

**Failure scenario.** DP1-09's wizard and the Home card can't render the resume UX without a new field. Adding it
after E0-13 has generated the Swift and TypeScript types breaks the `WalletDemo` and Playwright fixture constructors.
The alternative is that the host prompts for a grant on every resume, which §2.6's one-prompt goal rules out.

**Fix.** Add to `RegistrationStatus`:

- `waiting: Option<RegistrationWait>`, where `RegistrationWait` is one of `Unlock` (a grant is needed), `Sync`,
  `InstantSend`, `ChainLock` and `Network`;
- `holds_key: bool`, for "Lock to cancel";
- optionally `funds_committed: bool`, true from `FundingSent` on (the "Funds locked" row).

`waiting` would subsume `proof` (`InstantSend`/`ChainLock`), so drop `proof`. Document in §5 that `grant` is required
exactly when `waiting == Some(Unlock)`.

### M-4 (Major): `DW_BLESS` rubber-stamps breaking changes, and the listing can't see trait impls

**Where:** `rust/crates/dw-engine/tests/m4_dashpay_contract.rs:229-247` and `189-227`.

1. **Bless passes without a trace.** With `DW_BLESS` set, the test rewrites §3 and returns success. It prints no
   diff and asks for no version bump, and nothing stops `DW_BLESS=1` from being set in CI or in an agent's
   environment.
   - **Demonstrated:** in the scratch worktree I changed `PaymentLock.since` from `u64` to `i64`, a breaking change
     for the JSON and the bindings. `DW_BLESS=1 cargo test … surface_listing` passed and printed nothing. The next
     run without `DW_BLESS` passed all 6 tests. The edit was reverted afterwards.
   - A DP task that changes a field type or drops a variant blesses, and the only signal left is a large
   generated block in a docs diff, which reviewers skim. The contract also has no version for hosts or reviewers to
   key on.
2. **The listing skips trait impls, `pub type`, `pub const`, `pub use` and `#[cfg]`.** Only derives are visible.
   - A hand-written `impl Serialize for BearerSecret`, `impl Display for BearerSecret` or `impl From<BearerSecret>
     for String` changes neither the listing nor any test. That is exactly the leak the brief asks about (see S-1).
   - A `#[cfg(feature = "invitation-create")]` call would be listed as unconditional.

**Fix.**

- Add `Contract-Version: 1` to the contract header, and keep a fingerprint next to the generated block:
  `<!-- surface-sha256: <hex> version: 1 -->`.
- On a mismatch, always print a line diff of the old and new listing (the `similar` crate, or a simple LCS).
- Refuse to bless when `CI` is set.
- Refuse to bless unless `version` was raised in the same edit, that is unless the file's version differs from the
  version recorded with the old fingerprint. Additive changes can still require the bump; it costs one line and makes
  every surface change explicit in review.
- Have bless print the new fingerprint and the diff, then fail with "blessed; re-run without DW_BLESS". Otherwise one
  run both rewrites and passes.
- Add trait impls of facade types to the listing, at least `impl <Trait> for <Type>` headers, and render `#[cfg]`
  attributes.

### S-1 (Major, secrets): "never serializes" is unenforced, and `AvatarSource` serializes the Gravatar e-mail

**Where:** `records.rs:237-261` (`BearerSecret`), `records.rs:196-232`, `records.rs:716-730` (`AvatarSource`);
contract §1 "Secrets".

What holds today. I checked each path:

- `BearerSecret` derives no `Serialize`. `Debug` prints `BearerSecret(..)`, and `Display` is not implemented.
- The inner `Zeroizing<String>` clears the full capacity on drop.
- `#[serde(from = "String")]` and `From<String>` move the `String`, so there is no extra copy.
- No record that derives `Serialize` holds one: the derive would not compile. So no output record can leak a secret
  through `serde_json`.
- The probe confirms that serde error messages don't echo the secret for wrong-typed siblings, a misspelled tag,
  trailing data or a wrong-typed `private_key` (5 serde error cases: none echoes the WIF. `AvatarSource::Gravatar` serializes the e-mail in clear. 1 MiB of `File` bytes becomes 4.19 MB of JSON).

What does not hold:

1. **Nothing pins any of this.** A later `#[derive(Serialize)]` on `RegistrationRequest`, a manual `impl Serialize` or
   `impl Display` (invisible to the listing, M-4.2), or `impl From<BearerSecret> for String` passes every test.
   - Fix: add `static_assertions` (or a `trait NotSerialize` trick) to `secrets_stay_out_of_debug_output`:
     `assert_not_impl_any!(BearerSecret: Serialize, Display, Into<String>)` and
     `assert_not_impl_any!(RegistrationRequest: Serialize)`, and the same for `RegistrationFunding`.
2. **`AvatarSource` derives `Serialize`** (`records.rs:717`). `serde_json::to_string(&AvatarSource::Gravatar{..})`
   emits the e-mail in clear (probe). §3.8 says Gravatar e-mails are "hashed in Rust and never stored". The redacting
   `Debug` shows the intent, and `Serialize` defeats it: a `dwcli --json` echo or a Tauri debug log of the command
   arguments prints it.
   - Fix: make it `Deserialize`-only, like `RegistrationRequest`. It is an input; nothing needs to serialize it.
3. **Secrets in at the binding.** m1 §1 "Secrets in" passes secrets as `Vec<u8>` and wraps them in `Zeroizing` on
   entry, because a Swift or JS `String` can't be zeroed. `BearerSecret` can only be built from a `String`.
   - Fix: add `BearerSecret::from_utf8(Zeroizing<Vec<u8>>) -> Result<Self, PlatformError>`. Have §1 say E0-13 binds it
     as bytes (UniFFI custom type over `Vec<u8>`), never as `String`, and that the binding never returns it. Record the
     residual risk as m1 does: the JSON or IPC payload that carried it is not zeroed.
4. **Rules the contract should state now,** because DP5-01, DP2-03 and E0-09 will write the code:
   - no error `detail` (`InvalidArgument`, `Internal`) and no `Display` may quote a `BearerSecret` input or any
     substring of it. A link parser's "invalid link: dashpay://invite?…" is the likely leak;
   - `dwcli` reads bearer inputs from stdin or a file, never argv (argv shows up in `/proc/*/cmdline` and shell
     history).
5. **Low.** `BearerSecret: PartialEq` is not constant-time, and `RegistrationRequest: Clone` multiplies zeroizing
   copies. Both are acceptable in-process. Either drop `PartialEq` from `BearerSecret`, so that the records containing
   it implement `PartialEq` by hand for tests, or document that it is not constant-time.

### m-1 (Minor): `CreditsError` has no code for a Core shortfall, and top-up has no quote (decision 5)

**Where:** `errors.rs:255-268`; contract §4 lines 931-932; §2.8.

- **A shortfall in duffs is not `insufficient_credits`.** `top_up(identity, duffs, grant)` spends Core duffs. A
  shortfall there is not `platform.insufficient_credits`, which is in credits and has other copy ("top up your
  credits"), and not `platform.grant_exceeded`, which means a cap was hit. Yet the contract says "the credit and
  funding refusals are `platform.insufficient_credits` and `platform.grant_exceeded`".
  - Failure scenario: a user with 0.01 DASH tops up 0.05. The UI says "not enough credits" on the screen for buying
    credits.
- **The UI can't compute the grant's cap.** Registration has `registration_quote`, so the host knows `total_duffs`
  for the `PlatformFunding{max_duffs}` grant. Top-up has no quote, so the host can't know the asset-lock fee. It
  either over-authorizes or meets `grant_exceeded`. Withdraw has the same gap: "≈ duffs you receive" and the fee
  reserve on Max (DP6-02).

**Fix.**

- Add `CreditsError::FundingInsufficient{needed, available}` (duffs), and `credits.below_minimum{min}` for
  `top_up_min_duffs`.
- Add `top_up_quote(identity, duffs) -> TopUpQuote{fee_duffs, total_duffs, credits}` and
  `withdraw_quote(identity, amount) -> WithdrawQuote{credits, fee_credits, expected_duffs}`.

These can stay stubs until DP1-06 and DP6-02.

### m-2 (Minor): the event journal can't name what a name event is about, and lacks a "locked" outcome

**Where:** `records.rs:649-672`; contract §5 `EventKind`.

- **No label.** `DashPayEvent.reference` is documented as "a txid or a request id". `UsernameRegistered`,
  `ContestWon` and `ContestLost` need the label ("You won @alice"). `dp_events` has no label column, and the event's
  `contact` is empty for these kinds.
- **No `ContestLocked`.** DP1-04's acceptance covers "won, lost and locked outcomes". `ContestState` has `Locked`, but
  `EventKind` has no `ContestLocked`.

**Fix.** Define `reference` per kind: the txid for payments, the request id for requests, the label for name events.
Add `EventKind::ContestLocked`.

### m-3 (Minor): `InitialProfile.avatar_candidate` makes `start_registration` upload to Imgur

**Where:** `records.rs:204-213`; contract §5 `InitialProfile`.

Having the engine upload a `File` candidate inside `start_registration` causes three problems:

- it adds a third-party network dependency, and a new error domain (M-1), to the funding call;
- it publishes the image before the user's funds are committed;
- it makes `registration_quote(req)` ambiguous: does the quote upload too?

**Fix.** Require `avatar_candidate` to name a candidate with `url` set, that is one prepared from `Url` or `Gravatar`,
or already uploaded with `upload_avatar`. Otherwise refuse with `invalid_argument`. The engine then only resolves the
URL, hash and fingerprint from memory. Say so in §5.

### m-4 (Minor): invitations before a vault exists, and after a restart

**Where:** `dashpay.rs:37-51`; contract §2.9 line 169.

- **No vault yet.** DASHPAY §2.9 (last row) says that before a vault exists, the link goes to a 0600 file in the vault
  directory and moves into the vault when it is created. The contract says only "stores the link in the network's
  vault". A session can exist in `NoVault`: the stub test opens one on a fresh data root.
- **After a restart.** DP5-01's "replay after onboarding" and "survives onboarding" need the host to find a stashed
  link after a restart. There is no `pending_invitations() -> Vec<String>` call and no `forget_invitation(link_id)`
  (the user dismisses the link, or the claim is finished).

**Fix.** Add both calls as stubs owned by DP5-01, and document the `NoVault` path in §2.9.

### m-5 (Minor): one file for every record, and one `impl` block for every call

**Where:** `records.rs` (846 lines), `dashpay.rs:65-381`.

- **Size.** DASHPAY §3.1 asks for one file per domain, each under about 800 lines. `records.rs` is already 846 lines
  before any DP task extends a record.
- **Merge conflicts.** In W4–W6, DP1-02, DP1-03, DP2-01, DP2-05 and E0-05 run in parallel, and every one of them
  edits the same two files. That is a merge conflict per pair.

**Fix.** Split the records into the §3.1 domain files now: `identity.rs`, `registration.rs`, `names.rs`,
`contacts.rs`, `payments.rs`, `profile.rs`, `credits.rs` and `invitations.rs`, with status in `startup.rs`. Give each
file its own `impl DashPay` stub block. The contract already allows this ("the DP tasks may add `impl DashPay` blocks
in their domain files"), and the listing test picks the files up. Doing it now makes the first DP diffs local.
Replace `pub use records::*` with explicit re-exports, so that a `pub` helper does not silently become API.

### m-6 (Minor): the error-mapping owner is "the first DP task that needs it"

**Where:** contract §6 line 1002.

DP1-02, DP1-03, DP1-06 and DP2-01 start in parallel waves. "Whoever needs it first" means two of them write
`From<PlatformWalletError> for PlatformError` at the same time, with different mappings.

**Fix.** Name one owner that lands first. DP1-01 is in W3 and has no other facade work. Alternatively, add the mapping
skeleton now with `Internal { detail }` as the fallback.

### m-7 (Minor): binary payloads as JSON number arrays

**Where:** `records.rs:720-723` (`AvatarSource::File.bytes`), `records.rs:776-779` (`AvatarImage.png`).

With the default serde form, a `Vec<u8>` becomes a JSON array of numbers. The probe measures 1 MiB of bytes at 4.19 MB of
JSON. A 5 MiB avatar file is about 21 MB over Tauri IPC, and a contacts list of 256 px PNG thumbnails is a
few MB per refresh. `dwcli --json` output gets the same treatment.

**Fix.** Use `#[serde(with = "serde_bytes")]` for MessagePack and UniFFI, or base64 strings for JSON. Alternatively
have §1 say E0-13 carries these over Tauri's raw-bytes IPC. Decide now, because it fixes the JSON shape that the §3
listing and E0-09 promise.

### m-8 (Minor): contact refusals without the reason the flow needs

**Where:** `errors.rs:165-182`.

- **No reason on `contact.ineligible`.** `send_request` refusing with it loses which `Eligibility` failed, for example
  `NoDashPayKeys`, which F9 answers with "Enable DashPay keys".
- **No code for an expired `dapk`.** DP2-03 requires that "an expired `dapk` is refused". With no code, the UI can't
  say "this QR code has expired; ask for a new one".

**Fix.** Use `Ineligible { reason: Eligibility }` and add `contact.scan_expired`. Alternatively state that
`verify_scanned` returns `invalid_argument` for it, and accept the generic copy.

### Nits

- **n-1. The stub test parses `Display` text.** `m4_dashpay_contract.rs:596-602` extracts the call from the `Display`
  string `"not implemented: …"`, but §1 says `Display` is diagnostic and unstable. Match `code() ==
  "platform.not_implemented"` and pull `call` out of the variant instead, for example with an `as_platform()`
  accessor on each domain enum. That accessor is useful to hosts anyway.
- **n-2. Name collisions and lax Kind checks in the §2 parser.** `call_rows` keys rows by the bare method name, so a
  future `NetworkSession.status` and `DashPay.status` collide silently. The Kind check accepts any cell not starting
  with "async" as sync. Key the rows by owner (`DashPay.status`) and require `sync`, `async` or `free, pure` exactly.
- **n-3. The faucet asset lock.** `FaucetAssetLock{outpoint, private_key}` is workable only through a
  `claim_invitation`-style path, which reconstructs the proof and signs with the given key. It does not work through
  `AssetLockFunding::FromExistingAssetLock`, which needs a lock this wallet tracks and re-derives the credit key's
  path (`orchestration.rs:140-162`, `227-240` at the pin). Say so in §5 so DP1-02 doesn't try the tracked path.
  - The faucet's preferred API (`POST /api/asset-lock-proof`) takes a public key and returns `assetLockProof`. The
    engine could therefore derive the credit key itself from `m/9'/c'/5'/1'/…` and never accept a bearer secret,
    which would make this variant `FaucetAssetLock{proof}`. That is optional for a developer-only path.
  - Either way, the `outpoint` needs the canonical text rule of the §3.6 note: the engine normalizes it or refuses
    with `invalid_argument`.
- **n-4. `avatar(identity, size)`.** Say that `identity` may be any identity: a contact, a search hit or the inviter.
  The contact rows have no avatar field, so this call serves them.
- **n-5. The static tables before Platform answers.** `profile_limits` and `cost_table` are sync reads of tables
  fetched from the contract and the fee version. State what they return before the first fetch: the built-in 25/140
  and the PV defaults, or `platform.context_unavailable`.
- **n-6. `RegistrationStatus.error` drops parameters.** It is a bare code string, so
  `funding_insufficient{needed, available}` loses its numbers. Consider `error_params: Option<serde_json::Value>`, or
  a typed failure record.
- **n-7. Over-long prose.** Contract line 10 runs to 163 characters; the prose elsewhere wraps at 120.
- **n-8. `unverified` coverage.** `Counterparty` and `InvitationStatus::Valid.inviter` carry no `unverified` flag.
  §2.2 rule 5 makes it matter for a DPNS resolution that leads to a payment, and `UserHit` already carries it. This
  is fine for 1.0 if DP3-04's gate covers payments regardless; say so in §5.

## The 16 decisions

| # | Decision | Verdict | Comment |
|---|---|---|---|
| 1 | Every call returns `Result` | **Agree** | A stub has to fail, and the finished reads need `network_not_open` and `wallet_not_found`. The m1 sync rule still holds. |
| 2 | `IdentityError` lives inside `PlatformError` | **Agree** | It keeps §3.6's identity signatures, and `identity.keys_missing` reaches the contact calls. Optionally add `From<IdentityError>` for each domain enum, to avoid the double `.into()`. |
| 3 | Each domain error wraps `PlatformError` | **Agree, extend** | Right as far as it goes. `RegistrationError` must also wrap `NameError` and `InvitationError`, and `AvatarError` unless m-3 is taken (M-1). |
| 4 | The m1 common codes go in `PlatformError` without a prefix | **Agree** | This matches m1 §4. Note for the hosts: m1's common `not_implemented` is `platform.not_implemented` here, so `ServiceErrorCode` needs both. |
| 5 | `CreditsError` has no codes | **Change** | Top-up spends duffs, and `platform.insufficient_credits` is the wrong unit and the wrong copy. Add `credits.funding_insufficient{needed, available}` and a below-minimum code, plus the quotes (m-1). |
| 6 | `eligibility` takes `identity` | **Agree** | Without the asking identity, `IsSelf`, `AlreadyContact` and `Pending*` can't be answered. |
| 7 | The `IsSelf` variant | **Agree** | `Self` is a keyword. The test maps `IsSelf` to `contact.self` explicitly. |
| 8 | `FaucetAssetLock{outpoint, private_key: BearerSecret}` | **Agree, with a note** | It is consistent with §3.4. It must take the claim-style path, not `FromExistingAssetLock`, and the outpoint must be canonicalized (n-3). An engine-generated key with `{proof}` would remove the secret altogether. |
| 9 | The invitation calls move to `NetworkSession` | **Agree, extend** | This is correct for F18, since the vault is per network. Document the `NoVault` 0600-file path, and add `pending_invitations` and `forget_invitation` (m-4). |
| 10 | `send_request` gains `scan` | **Agree** | The proof stays in the engine, and the scan id is session-scoped and single-use. Add `contact.scan_expired` for a stale id or `dapk` (m-8). |
| 11 | `events` and `registrations` are async | **Agree** | Both read app.sqlite. `status()` stays sync from the cache, which E0-05 has to keep current. |
| 12 | A new `InitialProfile` | **Agree, change detail** | It is right not to persist a candidate id. Require an already-resolved candidate (url set), so registration never uploads to Imgur (m-3). |
| 13 | `BearerSecret`: never serializes, redacted `Debug`, zeroed on drop | **Agree, harden** | The type is correct. Pin it with `assert_not_impl_any!`; build it from bytes at the binding; state the rule that an error detail never quotes it; make `AvatarSource` `Deserialize`-only (S-1). |
| 14 | `StartupStatus::IdentityUnsettled` | **Agree** | §3.2's locked-vault bring-up needs a value that the library's seven and our `NotRun`/`Starting` don't provide. |
| 15 | `unverified: bool` on records | **Agree** | §2.2 rule 2 requires it. Optionally extend it to `Counterparty` (n-8). |
| 16 | The deferrals to DP3-01, DP3-02, E0-05, E0-10b and E0-13 | **Agree, one change** | `Recipient::Contact`, `counterparty`, the `NoticeCode`s and `EngineEvent::Platform` touch frozen m1 types and bindings, so they belong with their tasks. The error-mapping row (§6, last) needs one named owner now, not "the first DP task that needs it" (m-6). |

The contract's §7 also carries items the brief did not list: 8 (`search_users` and the others keep their arguments),
10 (`keys_missing` typed as `KeyPurpose`), 12 (records defined from §3.2–§5), 13 (notices deferred) and 21 (the
assigned refusal codes). I agree with all of them. The 21 refusal assignments are good and should gain the two codes
from M-2.

## Summary

The skeleton fulfils the E0-08 row: every call of §3.6 is a named stub, the code table is tested, and the
contract is precise about sync and async, ids, grants and ownership. The decisions in §7 are mostly right.

It is not ready to merge because the contract is meant to be stable, and four gaps would force breaking changes:

- `RegistrationError` can't carry the `name.*` and `invitation.*` refusals its own flows raise (M-1);
- there are no codes for E0-04's `MaybeSent` and `Cancelled` outcomes (M-2);
- the registration row can't say why it is parked (M-3);
- the sync test lets `DW_BLESS` approve any surface change silently and can't see hand-written trait impls (M-4).

On secrets, `BearerSecret` itself is sound and no output record can carry one through serde. But nothing pins that,
and `AvatarSource` serializes the Gravatar e-mail (S-1).

Each fix is small: a few variants, three fields, two stub calls, a test hardening and a file split. With M-1–M-4 and
S-1 fixed, and the Minors fixed or given an owner in §6, I would expect to approve in r2.
