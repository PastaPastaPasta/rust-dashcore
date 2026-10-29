# DW E0-10b — Opus high (final gate)

Reviewed 2026-10-11. Exact head: `e96b44d9d441c1718f58ed44b140032ec8b5b1db`, branch `dw/e0-10b-trust-provider`, base desktop
main `9b70543`. The whole branch is in scope (`git log 9b70543..e96b44d`, 9 commits).

I worked in the detached worktree `/work/scratch/e0-10b-opus/wt`. Probes and mutations were applied there and then
reverted (`git checkout -- rust`, `git status` clean at `e96b44d` afterwards). The patch is kept as
`/work/scratch/e0-10b-opus/review-probes-and-mutations.patch`. The branch was not edited, committed or pushed.

Sources read:
- DEC-183, DEC-168, DEC-125/135/139/143, DEC-18/09.
- `docs/design/E0-10b-trust-provider.md` and DASHPAY §2.2.
- m4 §5 (`unverified`, `QuorumSource`) and item 36.
- REVIEW-COMMON.md.
- Sol r1/r2, the E0-10a spike note.
- The pinned platform sources (`ebe37f8`): rs-sdk `sync::retry`, platform-wallet `SpvRuntime::get_quorum_public_key_with_status`, and the DPNS register flow.

Closed Sol items (R1, R2, R3, N1, L1, L3, balance, network scope) are not reopened. Where they come up below, it is
only as confirmation.

## Findings, by severity

No blocker or major findings.

### Minor M1: data stored before E0-10b reads as verified, and the re-fetch never looks at it

**Location:**
- `rust/crates/dw-engine/src/platform/provenance.rs:11-13` (the premise) and `:191-197` (`is_unverified`).
- `rust/crates/dw-appdb/migrations/V2026101101__trust_digest.sql` (no latch handling).
- m4 §5 (`docs/contracts/m4-dashpay-engine.md:1420-1441`).

**Scenario:**
- Before this branch, desktop main (`9b70543`) gave the SDK `SharedContext(LazyTrustedContext)`, so every Platform proof used the trusted HTTPS service's quorum keys.
- No production path ever set the latch: `git grep fallback_in_use 9b70543` finds only the definition and tests.
- An installation that ran such a build holds identities, names, profiles and contact requests in the library's wallet database, all fetched through the trusted service. Its app.sqlite has no `trust.fallback_used_at`.
- After upgrading to E0-10b, `Provenance::open` reads "no latch", so `is_unverified` is `false` for every entity. `IdentitySummary.unverified` is false for that data.
- `held_unverified` is empty, so the re-fetch never runs for it, and no record is ever written.

This lasts until the first lookup this branch serves through the fallback. In the degraded mode that is usually the
first Platform read before SPV holds the quorum. It does not happen at all while the app runs offline. If SPV is
already serving `Verified` keys when the first read happens, it never happens, and the old data stays "verified"
indefinitely. E0-10a measured 100 % `Verified` on first ask at `dev`, 10–16 s after a restart.

The provenance premise ("Before the latch … every fetch was proof-verified against the provider in use") and m4's
"once the trusted-quorum fallback has served a read in this installation" are false for such an installation. The
fallback served every read; it just didn't record the fact.

**Evidence:** probe `opus_probe_b_pre_latch_data_reads_verified_and_is_never_refetched` restores a wallet, adds an
identity with the name `Alice`, and does not use the fallback (the state main left behind). It prints:

```text
OPUS-PROBE-B unverified=false refetched=0
```

**Impact:** the desktop has no tags or releases (PK-04 has not happened), so only development and test
installations are affected. The pasta testnet DashPay fixture wallets are the obvious ones. The data was still
proof-verified against the trusted service, the same trust level as "unverified", so this is a false "verified"
label, not unproven data. It is the exact question this gate answers, though, and the fix is one statement.

**Fix:** have the (still unshipped) `trust_digest` migration set the latch on databases that already hold wallets,
for example:

```sql
INSERT OR IGNORE INTO settings_kv (scope, key, value)
SELECT '', 'trust.fallback_used_at', CAST(strftime('%s','now') AS TEXT)
WHERE EXISTS (SELECT 1 FROM wallets);
```

A fresh database has no wallets when it migrates, so the provenance and trust tests are unaffected. Setting it
unconditionally also works: the degraded mode latches on the first read anyway. A latch read back at open counts for
classification, and DEC-143 durability is still established at the first fallback use. Then:
- add a migration test (an old-schema database with a wallet row is latched after open);
- turn probe B around (old data reads unverified, and the re-fetch picks it up);
- update the provenance doc and m4 §5 to say that installations from before E0-10b start latched.

### Minor M2: a label the summary hides still makes the identity unverified, possibly for a whole contest

**Location:**
- `rust/crates/dw-engine/src/platform/reverify.rs:350-352`: `held_of` digests every name in `owned_names`.
- `rust/crates/dw-engine/src/platform/recovery.rs:345`: `unverified = o.entities.iter().any(..)`.
- Against `recovery.rs:347-359`, where the shown names are `shown_names(evident_names(..), open_contests)`.
- `names.rs:360-379`.

**Scenario:**
- The user registers a contested name (`bob`). platform-wallet appends it to `dpns_names` as soon as the write returns (`rs-platform-wallet/src/wallet/identity/network/dpns.rs:286-300`) and also lists it as contested.
- The summary hides it: it is in `open_contests`, and pending in prefs.
- `held_of` still includes `Held::label("bob", identity)`. No `domain` document exists until the contest resolves (about two weeks on mainnet), so the re-fetch can never verify it.
- The identity therefore reads `unverified = true` for the whole contest, although everything it shows verified.
- A lost contest, or a pending or refused copy the library's list keeps (the cases `evident_names` documents), keeps it unverified until the library drops the label.

DEC-183 and item 36 bind provenance to the content shown. This binds it to content that is not shown. The error is in
the fail-closed direction: availability and the meaning of the flag, not a false "verified".

**Evidence:** probe `opus_probe_a_hidden_contested_label_keeps_the_identity_unverified`. The library has
`Alice` + `bob`, with `bob` contested. Every entity except the `bob` label is recorded as the re-fetch would record
it. The probe prints:

```text
OPUS-PROBE-A shown names=["Alice"] unverified=true refetched=[Label { label: "b0b", spelling: "bob", .. }]
```

**Fix:** in `summaries`, compute `unverified` from the identity and profile entities plus
`Held::label(spelling, owner)` for exactly the names the summary shows (`names` after `evident_names`/`shown_names`).
When `current == false` no names are shown, so no label entities. The re-fetch walk may keep fetching every library
name; at worst that wastes a query every 10 minutes. Keep probe A, with its assertion as written, as the regression.

### Nit N1: the only production trigger of masternode sync, and so of the re-fetch, is untested

**Location:** `rust/crates/dw-engine/src/events.rs:621-631` (`SessionHub::on_progress` → `trust.note_masternodes_synced()`).

**Evidence:**
- Mutation `hub_no_sync` (never call `note_masternodes_synced`) survives the targeted set (74 tests).
- It also survives the whole dw-engine lib suite: 346 passed, 0 failed.
- The tracker (`sync.rs:608`) and the provider (`trust_tests.rs:760`) are tested separately, but nothing tests the wiring between them. If the wiring broke, the re-fetch would never start: nothing would ever verify after the latch, failing safe. Enforcing would also never leave the fallback.

**Fix:** a hub test: feed `on_progress` headers and masternodes done, and assert `trust.masternodes_synced()`. Also
assert it stays false with only the masternode phase done.

### Nit N2: no test covers which requests the re-fetch walk enumerates

**Location:** `reverify.rs:357-364` (`requests_of`).

**Evidence:** mutation `no_incoming` (drop incoming requests from the walk) survives. The tests build `Held` values
by hand (`trust_tests`) or exercise profiles and labels only (`recovery_tests`). DP2-01 relies on `contact_request`
records for sent, incoming and established requests. Dropping one direction silently leaves those rows unverified
forever (fails safe).

**Fix:** extend `contact_profiles_and_shown_labels_are_refetched_until_verified` (or add a test) so that one sent,
one incoming and one established request each appear in `held_unverified` after the latch.

### Nit N3: the Enforcing re-check after the trusted call is untested

**Location:** `trust.rs:337-339`.

**Evidence:** mutation `enforcing_no_recheck` survives. The design and commit `db7814f` claim this behaviour ("If
masternode sync completes while a lookup waits for the trusted service, the lookup re-checks"). `Enforcing` cannot be
reached in production (`expect(dead_code)`, H-09), so this is a test gap for H-09.

**Fix:** a fake trusted source that calls `note_masternodes_synced()` inside `quorum_key`. Assert that the lookup
then waits and fails closed instead of serving the trusted key. Alternatively, record it as an H-09 test item.

### Nit N4: the migration comment names the old encoding

**Location:** `V2026101101__trust_digest.sql:7` ("encoding `v1`"). The code writes `v2` (`reverify.rs:223`).

**Fix:** since the migration is unshipped, say "the encoding version the digest names (`v2` at this head)". If M1's
fix touches this file, do it in the same edit.

## Simplification opportunities (optional)

- **`Held::Label.label` is derived from `spelling`** (`convert_to_homograph_safe_chars(spelling)`). The field, and
  its bytes in the digest, carry no extra information. It could be computed in `record()` and in the re-fetch query,
  with the digest dropping it at the next version bump. Not worth a `v3` on its own.
- **The `Enforcing` machinery** (`wait_for_verified`, `failed_closed`, `writes_wait`, the rule-3 tests) is about 90
  lines plus 5 tests of code production cannot reach until H-09. DEC-183 accepted it as reported, so this is only an
  option: defer it to H-09, where its first caller and N3's test will live.
- **`Recorded.written`** only feeds a debug log since the R3 fix. Keeping it is fine; it is the R2 metric.
- M2's fix also simplifies `OwnedIdentity`: the summary needs the identity and profile entities only, and the label
  entities come from the names it shows.

## Confirmations

**1. End-to-end trust chain.** SPV status → provider → proof → record → read → flag.
- **Status mapping.**
  - `SpvRuntime::get_quorum_public_key_with_status` (pin `runtime.rs:354-377`) returns the entry's own `verified` status.
  - `SpvAnswer::of` (`trust.rs:94-103`) maps only `Verified` to verified. `Unknown` and every `Skipped` reason, including the `MissedList` and `NotMarkedForVerification` cases E0-10a found at startup, are `Held`: compared, never verified. `Invalid` refuses.
  - The `invalid_as_held` mutation is killed. Any lookup error or timeout is `Absent` (5 s bound).
- **Degraded rules.**
  - `Verified` with a disagreeing trusted key refuses (rule 4). `Verified` with the service unreachable is served as SPV-verified, per DEC-183's accepted decision.
  - `Held` requires the trusted key and refuses if it differs. `Absent` serves the trusted key only behind the latch.
  - `deg_no_mismatch`, `held_no_compare`, `no_latch` and `mismatch_as_context` are all killed.
  - A forged trusted key with SPV holding the real one refuses every lookup and latches nothing.
- **Scope tally.**
  - The pinned SDK's `retry` (`rs-sdk/src/sync.rs:73-140`) awaits each attempt and its proof parsing on the calling task, so `trust::scoped` sees the production lookups of `Identity::fetch` and `Document::fetch_many`.
  - An attempt that fails and is retried adds its lookups to the tally, so verification can only be under-counted.
  - `lookups > 0` and `unverified == 0` are both required.
  - The re-fetch requires both a match and a verified tally. `tally_true`, `tally_nolookups` and `reverify_no_tally` are killed.
  - The fetched contracts are compiled-in system contracts, not trusted-service ones.
- **Write.**
  - `confirmed` compares complete `Held` values with the library's state under the wallet manager's read lock. That lock is held across `spawn_blocking(record_all)`.
  - `record_verified` updates memory only after SQLite succeeds.
  - Even without the write-time check, a reader hashes its own content, so a stale write can only cost availability.
- **Read and flag.**
  - `IdentitySummary.unverified` is the only flag set at this head; Contact, UserHit and ScannedContact are stubs.
  - Every field the summary shows is either in the digest or derived from it: the identity and keys (`has_dashpay_keys`), the five profile fields (`ShownProfile::of` ≡ the shown `Profile`, `bio` fallback included), names and main name (⊆ `owned_names`), and the local choices.
  - Balance is excluded, as documented and accepted.
  - Marketplace rows and contests only hide labels, or reveal a label that is itself digest-bound.
  - Apart from M1, I found no path where unproven, fallback-sourced, stale-beyond-L3, other-network or other-wallet content reads as verified. Records are per network (separate app.sqlite). Within a network they are global across wallets but bound to content, so another wallet's copy verifies only if it is byte-identical.
- **Stale evonodes.**
  - A frozen node citing an old quorum gets `Absent` from SPV, so the trusted key is served behind the latch and the data reads unverified. If SPV still verifies the old quorum, the content is proven as of then.
  - That is DEC-183's historical meaning (L3); freshness is outside provenance and the SDK's staleness checks.

**2. Rules 1–4 and 6.**
- **Rule 1:** the cache of verified keys is keyed by `(type, hash)`, capped at 256 entries and cleared when full. `cache_held` is killed.
- **Rule 2:** the fallback is behind a durable latch, and refused if the latch cannot be written or checkpointed.
- **Rule 3** (Enforcing only):
  - the bounded wait and the 30 s failed-closed window (`failed_closed_off` killed);
  - "rebuild from storage" is not offered, as accepted.
- **Rule 4:**
  - a refusal maps to `platform.trust_mismatch`, with one notice per quorum per session;
  - an unjudged proof maps to `context_unavailable`.
- **Rule 6:** contracts, token configs and the activation height come from the trusted service.

The degraded mode matches DASHPAY §2.2: trusted keys, compared with SPV whenever SPV holds the quorum, rule 5
suspended, and `quorum_source = Trusted`. The trusted-only toggle is global, read at open, applied at once, and still
latches.

**3. Concurrency and lifecycle.**
- **Before `attach`:** lookups refuse.
- **Weak SPV and provenance:** with both held weakly, a closed session serves only cached `Verified` keys and nothing behind the latch.
- **Re-fetch session handling:**
  - the task holds the session only for the snapshot and the record step, never across network fetches;
  - it stops on close (`try_enter` per entity, `enter` before recording);
  - it holds a weak session handle between runs.
- **Close:** waits for the record step's operation guard. A reopen gets a new provider and records.
- **If a session closes before masternode sync,** the task waits on the provider's watch until the provider drops. It holds no strong session or storage handle.
- **No lock-order hazard found** between the wallet manager's read lock and the AppDb mutex in the record step: nothing takes the AppDb connection and then waits for the wallet manager.

**4. Tests.**
- The design §8 table matches the tests.
- Sol r1/r2 found the digest, encoding and record-step paths mutation-strong, and the committed tests still kill those faults (r2's 12 kills; the length-prefix and N1 regressions are now committed).
- My 14 mutations below target the provider, the tally, the walk and the wiring.

## Validation

All runs used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `umask 022`, `TMPDIR=/work/scratch/e0-10b-opus/tmp` (0700),
`--locked --offline`, and my own `CARGO_TARGET_DIR` (`/work/scratch/e0-10b-opus/target`). The target was seeded by
reflink copy from the implementer's target at the same head; only workspace crates rebuilt. Test dirs come from
`dw_testutil::private_tempdir` (0700). No wall-clock flakes occurred, so no reruns were needed.

**At the exact head: 137 passed, 0 failed, 1 ignored** (`logs/baseline.log`):

| Suite | Passed | Ignored |
|---|---|---|
| dw-engine lib: `platform::{trust_tests, provenance, reverify, recovery_tests}`, `sync::tests::masternodes_synced`, `session::tests` | 67 | 1 (the public-testnet fixture) |
| dw-appdb | 50 | 0 |
| m4 contract | 9 | 0 |
| session_lifecycle | 11 | 0 |

**Two reviewer probes**, appended to `recovery_tests`, fail at the head, as findings M2 (probe A) and M1 (probe B).
Their output is in `logs/probes.log`.

**14 mutations**, each guarded by `DW_REVIEW_MUTATION` and compiled once. Each counts only an executed failing
assertion; there were no compile failures.

Killed (11), with the tests that kill each:

| Mutation | Killing tests |
|---|---|
| `deg_no_mismatch` | 3 |
| `held_no_compare` | 1 |
| `no_latch` | 7 |
| `cache_held` | 1 |
| `tally_true` | 4 |
| `tally_nolookups` | 1 |
| `invalid_as_held` | 1 |
| `failed_closed_off` | 1 |
| `reverify_no_tally` | 2 |
| `no_labels` | 1 |
| `mismatch_as_context` | 2 |

Survived (3): `hub_no_sync` (N1; also the whole dw-engine lib suite, 346/0), `no_incoming` (N2) and
`enforcing_no_recheck` (N3). Logs: `logs/mutations.txt` and `logs/mut-*.log`.

**Gate9:** I inspected `/work/scratch/e0-10b/gate9.log` (`GATE HEAD e96b44d`): 896 passed, 0 failed, 4 ignored,
`GATE EXIT 0`. I did not repeat the full gate.

The scratch target was deleted afterwards. Scripts, logs and the patch are kept in `/work/scratch/e0-10b-opus/`.

## Limits

- No live Platform or testnet probes, no GitHub actions, no `~/private`.
- BLS proof verification and Platform's protocol were not re-audited; I traced the SDK call path at the pin only.
- DP2-01's use of `show_label` and `Held::label` cannot be certified from this head, where its facade is a stub.
- The accepted residuals stand: no chain anchoring (DEC-168(4)), L1, L3, balance, and the deferred DP3-04/H-09 gates.
- No security note: nothing found that bears on Platform or Core.

**Verdict:** APPROVE-WITH-NITS. M1 and M2 are cheap and should land before the merge, or as its first follow-up. M1 at least should land before any build reaches an installation that ran an earlier desktop build.
