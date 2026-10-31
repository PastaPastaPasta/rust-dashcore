**Verdicts: E0-04 design (692fd20): APPROVE-WITH-NITS · E0-08 (369f8f0): REWORK (narrow: one contract-text round to adopt the E0-04 amendment)**

# DW-E0-04 / DW-E0-08 r3 (Opus): narrow closure check and cross-consistency

- **E0-04:** `dw/e0-04-design` at **692fd20** (rev2 70cbeee + the E0-08 alignment amendment),
  `docs/design/E0-04-grants-leases.md` and `docs/design/checks/e0_04_design_model.py`.
- **E0-08:** `dw/e0-08-dashpay-facade` at **369f8f0** (Contract-Version 3), diff-only against c3c9bbd.
- **Against:** my r2 reviews (`DW-E0-04-design-r2-opus.md`, `DW-E0-08-r2-opus.md`), DEC-57, DEC-65, DEC-67, and
  Platform pin `bc41f1bc23` for the nonce rule.
- **Timing:** this explains most of the mismatches. 369f8f0 (22:48 UTC) conforms to E0-04 **rev2** (70cbeee, 22:39).
  The amendment (692fd20, 23:00) came 12 minutes later and changed three things E0-08 had just copied: the asset-lock
  `None`, `funds_committed` and `Discovery`. E0-08 has not been asked to follow the amendment yet. Its §15 list is the
  to-do for that round.
- **Scratch:** `/work/scratch/e0-r3-opus/`.

## Gates

| Gate | Result |
|---|---|
| `python3 -I docs/design/checks/e0_04_design_model.py` (E0-04 @ 692fd20) | exit 0. **77 PASS, 0 FAIL**, 2 m 07 s wall (the doc says about 80 s; the box was loaded). Part 5's new checks (`restart_retry`, `rowless_tombstone`, `asset_lock_absent`) pass, and their weaker rules fail as claimed: `forget`, `no-override`, `entry-only`, `skip-consumed`. Output: `model-run.txt` |
| `cargo fmt --all --check` (E0-08 @ 369f8f0) | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | **520 passed, 0 failed, 1 ignored**, across 59 test binaries |
| `cargo test -p dw-engine --test m4_dashpay_contract` | 8/8. The Version 3 fingerprint is consistent |
| Worktree after the gates | clean |

`TMPDIR` was `/dev/shm/e0-r3-opus-tmp` (mode 0700). It has since been removed.

---

## A. E0-04 (692fd20)

### A.1 My r2 majors

| r2 # | Status | Evidence |
|---|---|---|
| **1: the contract fails open on a missing entry; reconcile §16 with c3c9bbd** | **CLOSED** (design side) | H11 (`:1261`) and §16.6 (`:2350-2393`) fail closed. Only `Some(NotSent)` allows a host retry, and NotSent always rests on positive evidence. `None` is unknown for a state transition or a `TxDraft` send. The asset-lock reading of `None` is confined to the engine's funding gates and needs **no entry, no marker and no tracked row of any status**. `finish_asset_locks` is ungated. The restart-after-withdrawal test is in §2a.4 item 7 and the model (`restart_retry`). Every name in my 1b table is adopted: `will_be_sent`, `GrantAction(identity, …)`, the four values in `Option`, `resolution`, no `needs_grant` field, `storage`, `lease_revoked`/`lease_expired`, `WalletClosed`. The reaper skips running flow tasks. `LeaseView` is owned by m4 §2.11, and Repair is engine-internal until DP1-02. The Ambiguous trigger of `DispatchResolved` is dropped. One line stays open: `FlowKind::Discovery` went from "drop it or define it" to defined but incompletely (F-3). The remaining disagreement with E0-08 is on E0-08's side (§C). |
| **2: Mode B status from tracked rows** | **CLOSED** | §2a.5 (`:488-521`) derives the status from three sources: `list_tracked_locks`, the funding marker (FULL, keyed by flow and step, carrying the funding key, with its definite resolutions written alongside), and how the call ended, including the released-signature count. `WillBeSent` = a `Built`/`Broadcast` row. The `DispatchResolved` sources are named. Orphans stay `MaybeSent` until DP1-05 or Repair's ChainLocked self-spend. Part 5 shows rev1's double funding and no double funding in rev2, with power loss and a withholding peer. There is one gap, for rows with no marker (F-2, minor). |
| **3: DEC-67 not applied** | **CLOSED** | Rebind prompts when "require authentication for every payment" is on (`:247`, `:955`, `:1983`, `:2313`). Q22 is marked rejected (`:2228`). Q21 is applied with one combined cap (`:257`, `:531`), and the stale "`QuickUnlock` keeps issuing…" line is gone. The DEC-67 wording is C2. |

My r2 minors 4a–d, 5a–f, 6 and 7a–f: spot-checked and **closed**.
- The stale strings are gone outside the changelog: `cancelled_by_lock`, "Funds locked", "may still go out",
  `broadcast_pending`, `needs_grant: bool` and `platform.storage`.
- P4 is L.
- P2a's events stay engine-side.
- The §12 `payment_guard` line follows L2.

### A.2 The amendment's three self-declared extensions

1. **Asset-lock `None` → never registered, applied only by the engine's funding gates: ACCEPT.**
   - It is sound in Mode A:
     - I1 (`register` precedes tracking and every transport);
     - an entry outlives its row (§6.2);
     - the §16.6 table checks rows before `None` ("no entry, but a tracked row of any status → `MaybeSent`").
   - `asset_lock_absent` shows that both weaker readings fund twice after a journal rollback or deletion: entry-only,
     and skipping `Consumed` rows.
   - Keeping this reading away from the host is right. A txid and a transition hash look alike, and the host only
     ever holds artifacts whose record existed when the engine reported them.
   - The Mode B form has one gap (F-2).
2. **Process-lifetime tombstone: ACCEPT.**
   - It turns rev2's fail-closed "forgotten" result into a retry-enabling `NotSent`. That is safe only because
     `DefinitelyUnsent` for a transition means that every attempt was definitely refused and none is `possibly_out`
     (§5.5). The engine is then the only holder of the bytes.
   - The replacement happens in the J step that admits a later First of the same bytes, before any transport. So a
     re-signed identical transition reads `MaybeSent` from then on (`rowless_tombstone`: `no-override` fails).
   - After a restart the set is gone, and the answer is `None`, which for a transition is unknown: fail closed.
   - Unbounded per-process growth is negligible.
3. **Narrowed H16: ACCEPT the narrowing.**
   - rev2 implied that a post-restart nonce re-query could settle a forgotten transition. Nothing records the hash and
     nonce across a restart, so it could not.
   - Now only an in-process entry, or a resumable step's marker, can settle the transition.
   - The in-process nonce rule that remains is underspecified against Platform's nonce window (F-1).

### A.3 New findings (E0-04)

**F-1 (minor; fix before P2a): H16's "another transition consumed the nonce ⇒ `NotSent`" needs the exact evidence.**
`:1266` (H16), `:521`, `:860`, `:2368`.
- **What the pin does.** Platform nonces are not strictly sequential. `rs-dpp/src/identity/identity_nonce.rs`
  (`bc41f1bc23`, `:15-18`, `:99-170`) keeps a 40-bit value plus a 24-bit *missing-revisions* bitset
  (`MAX_MISSING_IDENTITY_REVISIONS = 24`). A transition whose nonce n is below the current value is still accepted
  while n's bit is set as missing and `current − n ≤ 24`.
- **Two easy implementations that are wrong:**
  - "the fetched nonce is past n", so n was consumed, so `NotSent`;
  - "n is consumed, and the result query for our hash found nothing", so `NotSent`.
- **Failure.**
  1. W1 (nonce n) times out and returns `broadcast_unknown`.
  2. A later transition from this identity or another device uses n+1, which leaves n marked missing.
  3. The engine reads "nonce advanced" as `NotSent` and offers a retry. W1 is still valid and executes, and the retry
     executes as well: a double withdrawal.
  - In the second reading, W1 itself consumed n and the result lookup simply missed it.
- **Fix.** State the predicate. `NotSent` needs **both**:
  - n can never execute again: its bit is not in the missing set, or `current − n > 24`;
  - positive evidence that this hash did not execute: the engine itself signed another transition with nonce n for
    the same nonce space and saw it `Sent`, or a proved read shows that the transition's own effect is absent.
  - Otherwise the answer stays `MaybeSent`.
  - Name both nonce spaces: identity nonces (withdraw, transfer, key updates) and identity-contract nonces (DashPay
    and DPNS documents). §2a.5 still says "the identity's nonce".

**F-2 (minor): in Mode B, a tracked row with no marker is not matched to its flow, so the funding gate can read `None`
while a lock is out.**
- **Where:** §2a.5 (`:493`, `:504`) matches rows to a flow "by the funding key in its funding marker". "No marker →
  `None`" then allows a second funding.
- **Failure.** Mode A covers this case: no entry plus a tracked row reads `MaybeSent`. In Mode B, the same journal loss
  or rollback that the Mode A model covers works like this:
  1. `dispatch.sqlite` is deleted or restored from an older copy, and the marker goes with it.
  2. The draft's row is still `Broadcast`, waiting for IS, and the catch-up has resumed it. So §2a.3's "no funding
     build over a possibly-sent row" no longer blocks.
  3. "Register again" passes the gate and builds a second lock.
- **Fix.** Make the gate match tracked rows by the **draft's own** funding key (funding type, identity index and
  account, all held in `app.sqlite`), not only by the marker's. Treat "no marker, but a matching row" as `MaybeSent`,
  and extend `asset_lock_absent` with a Mode B variant.

**F-3 (minor): `FlowKind::Discovery` is defined but not implementable as written, and E0-04 should drop it again.**
- **Where:**
  - §16.1 (`:2287-2290`) and the amendment table (`:51`);
  - §4.2's purpose table has no `IdentityScan`, and neither does `BudgetPurpose`;
  - §4.1 drops the tokens once the signers exist, while `Vault::scan_key` accepts only an `IdentityScan` **token**
    (§3.2);
  - `needs_grant{purpose: BudgetPurpose}` cannot name the purpose a `discover_identities` call lacks.
- **Why it adds nothing.** `IdentityScan` is "not `requires_credential`" (§3.2), so a "one prompt, several scans" lease
  saves no prompt.
- **E0-04 contradicts itself.** §15's list of E0-08 changes does not ask E0-08 to restore `Discovery`. rev2's own
  §16.1 said "there is no `Discovery`: `IdentityScan` is a grant, not a purpose", and 369f8f0 follows it.
- **Fix.** Revert to rev2's §16.1 text. Add to §16.1 and to m4 §2.2: "`discover_identities` accepts only an
  `IdentityScan` grant id; a lease id there is `platform.grant_invalid`".

**Nits.**
- n-1: §16's preamble (`:2273`) and the amendment cite "c3c9bbd, Contract-Version 2". The contract is now at
  369f8f0, Version 3. Re-point both.
- n-2: §4.6 (`:845`) gives `dispatch_status(artifact) -> Option<DispatchState>` without the `Result`; §16.6 has it.
- n-3: the model's `restart_retry` hard-codes `status = None` and so checks the rule, not a derivation. That is
  acceptable, but §10 should say so.

---

## B. E0-08 (c3c9bbd..369f8f0, diff only)

The diff touches the contract (+167/−67), `flows.rs` (`LeaseView`, `LeaseStateView`, `BudgetView`, `ParkReason`,
`leases()`, `WalletClosed`, `Discovery` removed, `self: &Arc<Self>`), the `invitations.rs` receivers, a doc comment
in `registration.rs`, re-exports in `mod.rs`, and the contract test (more `assert_not_impl_any!` traits, a `leases()`
stub row).

| r2 item | Status | Evidence |
|---|---|---|
| **N-1 (major)**: a retry on a missing entry | **PARTIAL** | Row-less artifacts are fixed: `None` is never a retry signal, the host retries only on `Some(NotSent)` or `DispatchResolved(NotSent)`, and an in-flight entry or `Committing` reads `MaybeSent` (`m4:1205-1221`, §7 item 31). **But** the asset-lock half now reads "a missing entry is itself evidence that nothing was handed off. The engine answers `Some(NotSent)` for it, never `None`" (`:1210-1212`). That is the `entry-only` rule, which E0-04's `asset_lock_absent` shows funds twice after a journal rollback or deletion, and it hands the reading to the host. Mismatch C-1. *The reopening traces to my own r2 fix text ("for a registered artifact only, absent"), which lacked the tracked-row condition. The amendment's version is the correct one.* |
| N-2: discard by phase | **CLOSED** | `:128` and `:1252`: the journal decides, not the phase. The wording follows C-1 once that is fixed. |
| N-3: `funds_committed` | **PARTIAL** | 369f8f0 copied rev2's §16.5 word for word: "true once any funding … is not `NotSent`" (`:1298-1302`, `registration.rs:94`). Under both documents a live `Unsent` entry reads `MaybeSent`, so in E0-08 it sets `funds_committed`, which is the N-3 defect again. The amended §16.5 fixes it. Mismatch C-2. |
| N-4: `WalletClosed` | **CLOSED** | `flows.rs:51-57`, `m4:1336-1337`. |
| N-5: the reading of `lease_revoked{Lock}` | **CLOSED** | `m4:1237-1239`, including C8. |
| N-6: lease-id reach and the reaper | **CLOSED** | `m4:82`, `:222`, §7 item 26: same wallet only, only for a purpose the lease carries (`grant_invalid` / `needs_grant{purpose}` otherwise), 10 minutes, and the reaper skips running flows. |
| n-1: listing blind to impls outside the facade files | **CLOSED** (the alternative fix) | `Deref<Target=str>`, `AsRef<str>`, `AsRef<[u8]>`, `Borrow<str>` and `ToString` are asserted absent (`m4_dashpay_contract.rs:811-822`). |
| n-2: grant sizing | **CLOSED** | Worst-case bound (`:225`, `flows.rs:60-62`). |
| n-3: serde echoes a misplaced e-mail | open (optional) | Not addressed. It belongs to E0-13 and is fine to leave there. |
| n-4: `wallet_id` encoding | **CLOSED** | Lower-case hex is documented (`:1337`, `flows.rs`). |
| n-5: `send.cancelled` row | **CLOSED** | §6 row (`:1379`). |
| n-6: copy drift | **CLOSED** | C1/C2/C3 match §16.10 exactly (`:1294-1297`). |
| `begin_flow` receiver | **CLOSED** | `self: &Arc<Self>` on every async `NetworkSession` call. |

### B.1 New findings (E0-08)

**G-1 (minor): `resolve_payment_lock` has its own `LockResolution{Sent, NotSent, Unknown}` with no evidence rule for
`NotSent`.**
- **Where:** `m4:165`, `payments.rs:17-26`.
- **What's wrong.** A contact payment is a `TxDraft` send. Its `broadcast_unknown` sets `dp_payment_lock` (DASHPAY §2.3
  step 8), and `resolve_payment_lock` decides when that lock clears. Nothing says what evidence `NotSent` needs. "Not
  found on chain or in the wallet" is the obvious implementation, and it is exactly the re-query that §4 (`:1198-1202`)
  forbids.
- **Failure.**
  1. A payment is withheld by a peer.
  2. After a restart, `resolve_payment_lock` returns `NotSent` because the wallet has not seen it.
  3. The lock clears, and the user pays again.
- **Fix.** State that `NotSent` needs E0-04 §16.6's positive evidence:
  - in-process, `dispatch_status(txid) == Some(NotSent)`;
  - or a ChainLocked conflicting spend of one of its inputs.

  `Unknown` behaves as `MaybeSent`/`None`, so the lock stays. DP3-01 implements it, but the rule belongs in the
  contract now.

**Nits.**
- g-1: "E0-04's `Unknown`" is cited in `flows.rs:83`, `flows.rs:201`, `m4:1216` and `m4:1471`. The amendment removed
  that term; say "no entry; unknown for a row-less artifact" (C-4).
- g-2: the docs of `DispatchResolved.artifact` (`flows.rs:103`) say "a txid or a state-transition hash". They should
  add the funding step id, which §2.11 and E0-04 §16.6 allow. The §6 row and the struct doc list two sources, while
  E0-04 §4.6 lists five plus Mode B's; cite E0-04 §4.6 instead (C-5).

---

## C. Cross-consistency: E0-04 @ 692fd20 against E0-08 @ 369f8f0

### Agrees

| Item | E0-04 | E0-08 |
|---|---|---|
| `dispatch_status` | `DashPay::dispatch_status(artifact: String) -> Result<Option<DispatchState>, PlatformError>`, async, 4 values (§16.6) | `flows.rs:198-205`, same; enum `WillBeSent, MaybeSent, Sent, NotSent` (`flows.rs:85-98`). Neither has an `Unknown` variant in `DispatchState` |
| Accepted `artifact` forms | txid, ST hash, funding step id | same (`:228`) |
| Host retry rule | only `Some(NotSent)` or `DispatchResolved(NotSent)`; `None` never | same (`:1205-1208`) |
| Row-less `None` | unknown, read as `MaybeSent` | same (`:1213-1220`) |
| In-flight entries | live `Unsent` and `Committing` read `MaybeSent` | same (`:1221`) |
| `RevokeCause` | `Lock, Close, PassphraseChange, WalletRemoved, WalletClosed` | same |
| `LeaseError` → code | revoked → `lease_revoked{cause}`; `end_flow`/reaper → `lease_expired`; `Parked`/`NeedsGrant` and **a passed `key_until`** → `needs_grant{purpose}`; budget → `grant_exceeded{…}`; bad or other-wallet id → `grant_invalid`; `lease.locked` → `cancelled`; journal → `storage` (§16.4) | the same table (`:1226-1234`), with "a lease without the call's purpose" added to the `needs_grant` row, which matches §16.1 |
| Lease-id acceptance (N-6) | own wallet only, for a purpose the lease carries; reaper at 10 minutes with no call, no permit and no flow task | same (`:82`) |
| `begin_flow` / `end_flow` / `leases()` / `LeaseView` | §16.1, §4.6 | the same fields, one for one |
| `GrantAction`, `GrantRequest`, "no more than quoted" | §16.2 | same |
| `RegistrationWait`, no `needs_grant` field | §16.3 | same |
| `DispatchResolved{wallet_id, artifact, resolution}` | §16.7 | same |
| `resume_registration` any state; `finish_asset_locks` ungated | §16.6 | same |
| Copy C1/C2/C3 | §16.10 | same strings |

### Mismatches

| # | Item | E0-04 @ 692fd20 | E0-08 @ 369f8f0 | Severity | Side to change |
|---|---|---|---|---|---|
| **C-1** | What `None` means for an asset lock or funding step | `None` = no entry, no marker **and no tracked row of any status**. The host reads every `None` as unknown, and only the engine's funding gates apply "never registered" (§16.6, H11, I16) | "a missing entry is itself evidence … The engine answers `Some(NotSent)`, never `None`" (`:1210-1212`, §7 item 31): no row condition, and host-visible | **major** (safety: `entry-only` funds twice after a journal rollback or deletion) | **E0-08** adopts §16.6: return `None`, leave the reading to the gates, and change `:128`/`:1252` to "`NotSent`, or (engine gate only) an asset lock's `None`". *An acceptable alternative, if E0-04 agrees: keep a host-visible `Some(NotSent)` for **funding step ids only**, with no entry, no marker and no tracked row matched by the draft's funding key (F-2), never for a bare txid.* |
| **C-2** | `funds_committed` word for word | §16.5 as amended: Mode A entries `Committing/Dispatching/Ambiguous/PreFence`, or a tracked row with no entry. Not for `Unsent` (live or dead), `Revoked`, or no entry and no row. Mode B: marker and status ≠ `NotSent` | rev2's text: "true once any funding of the flow is not `NotSent`" (`:1298-1302`, `registration.rs:94-95`). That makes a live `Unsent` (`MaybeSent`) committed, so C3 shows while Lock would still cancel cleanly | minor (copy and flag; discard agrees either way) | **E0-08** copies §16.5 verbatim, in the contract and in the `RegistrationStatus` and `LeaseView` doc comments |
| **C-3** | `FlowKind::Discovery` | restored and defined (§16.1, amendment) | dropped (rev2) | minor | **E0-04** reverts to rev2 (F-3). Both documents add "`discover_identities` takes an `IdentityScan` grant id only; a lease id is `grant_invalid`" |
| **C-4** | "`Unknown`" anywhere | gone, except in changelog history | cited as "E0-04's `Unknown`" (`flows.rs:83,201`, `m4:1216,1471`) | nit | **E0-08** |
| **C-5** | `DispatchResolved` sources and artifact forms | §4.6: resend accepted or tx seen; reload cleans `Unsent`; row-less `DefinitelyUnsent`; Mode B status moves; nonce re-query. The artifact may be a funding step id | two sources; artifact "txid or ST hash" (`:1376`, `flows.rs:101-104`) | nit | **E0-08** cites E0-04 §4.6 and adds the step id |
| **C-6** | Contract version cited | "c3c9bbd, Contract-Version 2" (`:2273`, amendment) | Version 3 at 369f8f0 | nit | **E0-04** |
| **C-7** | Payment-lock resolution vocabulary | `NotSent` needs positive evidence (§16.6) | `LockResolution{…, Unknown}` with `NotSent` undefined (G-1) | minor | **E0-08** |

Fixing C-1 and C-2 needs a Contract-Version 4 bump and a re-bless. C-3 needs none on E0-08's side.

---

## Summary

- **E0-04 @ 692fd20: APPROVE-WITH-NITS.**
  - All three of my r2 majors are closed on the design side, and the model passes 77/77.
  - All three amendment extensions are sound and accepted.
  - Fix before P2a starts:
    - F-1: H16's nonce predicate must respect the 24-revision window, and NotSent needs per-hash evidence;
    - F-2: the Mode B gate matches rows by the draft's funding key;
    - F-3: drop `Discovery` again.
  - The nits are n-1 to n-3.
  - None of these touches the journal, the CAS under J, the barrier or the model's core.
- **E0-08 @ 369f8f0: REWORK (narrow).**
  - The gate is green, and N-2, N-4, N-5, N-6 and five of the six nits are closed.
  - It conformed to rev2 a few minutes before the amendment, so it carries rev2's asset-lock and `funds_committed`
    text. Two fixes are needed:
    - **C-1 (major):** the host-visible `Some(NotSent)` for a missing asset-lock or funding entry has no tracked-row
      check, which reopens double funding after a journal rollback or deletion;
    - **C-2:** `funds_committed` must be §16.5 verbatim.
  - G-1 and C-4/C-5 are a few more lines.
  - This is one text round with a Version 4 bump. I expect to approve it on a diff-only check.
