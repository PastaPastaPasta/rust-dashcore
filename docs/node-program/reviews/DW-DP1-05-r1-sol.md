# DW-DP1-05 — independent review, round 1 (Sol)

Reviewed `dashwallet-desktop` head `4ee3f74e6df60e520c85fe1259e93c48627ee8a3` against `c19e441262ee2149dc788ba2ed664263ff6d97cb`, including both commits `152503e` and `4ee3f74`. Read `REVIEW-COMMON.md` first. Implementation worktree stayed untouched; review and experiments used a detached checkout under `/work/scratch/dp1-05-r1-sol/`. No commits, pushes, or GitHub comments.

## Contract and scope

`docs/design/ROADMAP.md:152` requires: “Restoring into a fresh datadir brings back the identity, names and main name within 2 passes, and the contact payment history after SPV.” The m4 facade contract (`docs/contracts/m4-dashpay-engine.md`, §1 and §2.1) additionally requires an in-memory identity read, a persisted main-identity choice, IdentityScan grants for discovery, and background bring-up/name recovery for discoveries. E0-04 §3.5 and its P1 follow-ups explicitly require own-key IdentityScan tokens to issue scan keys through `hold_key` / `scan_key_held`.

The manager-approved startup duplication of the initial `dpns_sync` pass is accepted here and is not a finding.

## Findings

### M1 — major: valid passphrase-authorized discovery is refused on a locked vault

**Location:** `rust/crates/dw-engine/src/platform/recovery.rs:468`.

Discovery redeems an IdentityScan grant and always calls `vault.scan_key`. A passphrase-authorized grant on a locked or mixing-only vault carries its own key. The vault deliberately rejects direct scan-key issuance from that token with `InvalidArgument`; it requires a `KeyHold` and `scan_key_held`. Thus a user who confirms “Find identities” with the correct passphrase cannot discover identities unless they separately unlock the entire vault first. This also consumes the valid one-shot grant before failing.

**Reproduction:** Import a wallet into an unencrypted regtest session, encrypt it (leaving the vault locked), authorize `GrantPurpose::IdentityScan` for that wallet with `Credential::Passphrase`, then call `DashPay::discover_identities` with the resulting id. The grant authorization succeeds, but discovery returns `invalid_argument` with “an own-key token of this purpose issues signers through a hold.” Independent scratch probe: `reviewer_locked_passphrase_identity_scan_grant_is_rejected`.

**Fix:** In the counted blocking key-work operation, redeem into a mutable token set, call `hold_key`, and choose `scan_key_held` when a hold exists, otherwise `scan_key`. Return and retain the hold with the scan key through master-key resolution and discovery; drop the master, scan key, and hold on cancellation. Add locked and mixing-only credential-grant regression coverage, alongside the current unencrypted grant tests.

### M2 — major: locking after an identity is stored loses its remaining recovery work

**Location:** `rust/crates/dw-engine/src/platform/recovery.rs:503` (also `platform/bringup.rs:662`).

The library stores discovered identities before its DPNS enrichment finishes. If the vault locks during that tail, discovery correctly drops its keys and returns `platform.cancelled`, but `ended?` exits before checking which identities were stored or scheduling the Readmit/names work. The existing startup status is unchanged. After unlock, `after_unlock` re-brings-up only `IdentityUnsettled` wallets; a wallet whose previous status was `NoIdentity` stays at that status even though `identities()` now exposes an identity. No recovery names pass is queued. While SPV is already running, names can then wait for the periodic DPNS loop (normally ten minutes), and the promised follow-up bring-up is absent.

**Reproduction:** Start a wallet whose initial bring-up returns `NoIdentity`; run explicit discovery with a valid vault-key IdentityScan grant; have the discovery mock add an identity to the real manager, then suspend before returning (the real library's post-fold enrichment point). Lock the vault, await `platform.cancelled`, and unlock. Independent probe `reviewer_lock_after_identity_fold_loses_recovery_work` observes one visible identity without names, startup still `NoIdentity`, one total bring-up, and zero recovery names passes.

**Fix:** Reconcile newly persisted identities on every exit after discovery began, including cancellation and library error, rather than only success/timeouts. Queue their remaining recovery work for the live wallet while preserving the caller's cancellation/error result; ensure unlock or the next start consumes that work. Add a controlled lock-after-fold test with a subsequent successful recovery.

### M3 — major: deleting/unloading a wallet does not cancel its explicit discovery or drop its master key

**Location:** `rust/crates/dw-engine/src/platform/recovery.rs:479` and `:496`.

Explicit discovery is not registered in the supervisor's per-wallet task tracking and watches only the vault lock counter. `remove_wallet`/`unload_wallet` call `platform.forget`, which aborts supervisor jobs, but cannot abort this call. After wallet removal has completed and its vault records are wiped, an outstanding network lookup can still retain the resolved master private key and continue the scan until the request ends or its 60-second discovery budget expires. A close/reopen or removal/reimport of the same wallet id can also let that old scan resume against the replacement wallet's manager bucket, since the library resolves it by id rather than a wallet generation.

**Reproduction:** Begin discovery under a valid IdentityScan grant and suspend its mock after entry (master resolution has already occurred). Remove the wallet through the public `remove_wallet` call with a valid Wipe grant. Independent probe `reviewer_wallet_removal_does_not_cancel_discovery` verifies that removal succeeds and `has_wallet_secret` is false while the discovery task remains running. A separate vault lock finally cancels it. The mock models an outstanding library await; in production the remaining lifetime is bounded by `DISCOVERY_BUDGET`, not by deletion.

**Fix:** Give explicit discovery a per-wallet cancellation/generation guard shared with wallet deletion/unload, and watch it alongside vault locks and the deadline. Cancel and drain the call so its resolved master key is dropped before removal completes; prevent results from an old wallet generation being applied to a reopened/reimported wallet. Do not simply overwrite the existing one-abort-handle-per-wallet supervisor slot. Add deletion and unload tests at a suspended discovery await.

### N1 — minor: the discovery acceptance mock recreates the absence marker and makes the workspace gate flaky

**Location:** `rust/crates/dw-engine/src/platform/recovery_tests.rs:235` and `:629`.

The mock bring-up derives its outcome only from `self.identities`. In the discovery test that vector is empty, while `self.discovered` contains the identity that `discover` adds to the real manager. Consequently the supervisor's requested second bring-up returns `NoIdentity` with no identity id even though the manager now owns an identity. Production startup instead recognizes the identity already on file. `finish_bring_up` faithfully persists the mock's false absence again. Whether the immediate `marker().is_none()` assertion sees that write is a scheduling race.

**Reproduction:** The required unchanged-head `cargo test --workspace --no-fail-fast` run failed at `recovery_tests.rs:629`, with aggregate results **597 passed / 1 failed / 3 ignored**. The same test passed when selected on its own. The existing mock's status calculation directly explains the discrepancy; this finding concerns the test fixture, not an independently demonstrated production marker regression.

**Fix:** Have the mock's re-bring-up recognize identities already present in the manager and return the corresponding Ready outcome, rather than fabricating absence from the original configured discovery vector. Assert the absence marker remains cleared after the queued bring-up and names pass finish. Include that assertion in the isolated and workspace runs.

### N2 — minor: concurrent main-identity writes can leave the cache inconsistent with SQLite

**Location:** `rust/crates/dw-engine/src/platform/recovery.rs:367` and `:388`.

The database write and cache publication are separate operations. SQLite can serialize writes A then B, while the async continuations publish B then A. `choice_writes` protects loads against writes; it does not order two writers. Both public calls can succeed, yet `identities()` reports A as main while `dp_main_identity` contains B. The same ordering window exists in `set_main_name`.

**Reproduction:** Run two public `set_main_identity` calls for different owned identities concurrently with `tokio::join!`, await both, then compare the sole `is_main` summary with `db.main_identity(wallet)`. The first 1,000-pair stress run did not hit the race. A second run reproduced it at zero-based iteration 31: SQLite held `YMN9Qj5jPNp7j14VPcML1B6xGgcPWVZUGLFU3Mnyfaf` while the cache exposed `US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx`. These are deterministic synthetic identities, not user identifiers. The scratch probe asserting cache/database equality failed on that mismatch.

**Fix:** Serialize each wallet's database-and-cache choice writes, or attach a publication revision generated inside the serialized database operation and reject older cache updates. Add concurrent-writer coverage alongside load-versus-write tests.

## Validation and claim assessment

All required gates ran under `rust/`, with `umask 022`, `TMPDIR=/dev/shm/dp1-05-r1-sol-tmp` (mode 700), and `CARGO_BUILD_JOBS=12`. The shared compiler-wrapper invocation stalled; this review's own build processes were stopped and the gates restarted with `RUSTC_WRAPPER=`. No other build or cache processes were changed.

- `cargo fmt --check`: **PASS**, exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**, exit 0.
- `cargo test --workspace --no-fail-fast`, exact unchanged head: initial run **597 passed / 1 failed / 3 ignored**, exit 101; failure was the new discovery/marker test described in N1. A full unchanged-head retry after restoring the scratch experiment file passed **598 / 0 / 3**, exit 0. The isolated marker-test rerun also passed.
- Scratch-only `cargo test --workspace --no-fail-fast reviewer_ -- --nocapture`: M1, M2, and M3 each reproduced twice. The final run's three lifecycle probes passed their assertions describing the broken behavior. Its fourth probe, asserting database/cache equality, failed with the concrete N2 mismatch above. The prior exploratory stress run did not reproduce that race in 1,000 pairs.

Only the scratch test module was extended for the probes; production code was unchanged. That module was restored to HEAD before the full workspace retry. The positive retry does not resolve the reproduced lifecycle and cache findings.

The existing offline recovery tests exercise restore before SPV, restore while SPV runs, persisted main identity, selection/normalization, sold/transferred rows, IdentityScan-purpose rejection, and clearing a proven-absence marker. They do not exercise the three major scenarios above. The supervisor's stop path preserves names work, but coarsely requeues the entire batch, including wallets whose names jobs already finished; this is a redundant-pass limitation, not a separate merge blocker here. Names refresh is public-data work and legitimately does not require an unlocked vault.

The marker is wallet-local, has the pre-existing seven-day TTL, is excluded from wallet backup rows, and is removed by `delete_wallet`; explicit discovery clears it once scan-key issuance succeeds. The timestamp-based main-name rule uses normalized preferences and `$transferredAt`/`$createdAt` from marketplace rows, falling back to library acquisition times. The new appdb accessors introduce no columns, tables, or migration changes: `dp_main_identity` and `dp_prefs` already exist at the base revision. Existing migration/export/import/deletion tests and the added accessor round-trip test cover their storage paths. Resolved keys use the pinned key-wallet `ExtendedPrivKey::Drop` zeroization; M3 concerns when that drop happens, not missing zeroization.

The optional live testnet fixture and separate regtest harness were not rerun; their reported results are not independent evidence from this review. The required workspace gate was run independently.

Scratch worktree/build artifacts and `/dev/shm/dp1-05-r1-sol-tmp` were removed after evidence was recorded. The implementation branch was not modified.

**Verdict: CHANGES-REQUIRED.**
