# DW-E0-03, round 4 — independent security review

Reviewed `dw/e0-03-signer-glue` at `da66db01a1b0f005af69173d886feda84dc3a5d6`: commits `4c96532`, `4ea92d6`, and `da66db0`, on top of `d36acdb`. Worktree: `/work/home/agent/workspace/dw-wt/e0-03-signer-glue`. Paths below are relative to it unless otherwise identified. Read REVIEW-COMMON, the original brief, all four prior reviews, CLAUDE.md, the changed code/contracts, DASHPAY §2.6, and ROADMAP E0-04. Checked the relevant platform-wallet code at the actual pin `bc41f1bc233dec4607d387101c1d9c2f111019b2` and dash-spv at `40268cc0402a8933ec539f16b2d634c4e25876ad`.

The vault fixes pass the security probes. One **major design finding** remains: the lease-free `Resend` exception needs dispatch provenance and coordination with asset-lock recovery. The first-dispatch ordering rule itself fixes round 3's check-then-act problem.

No reviewed branch source was modified, and no commit, push, or GitHub comment was made. Scratch sources, binaries, command records, and logs are in `/work/scratch/dw-e0-03-r4-gpt/`. Tests used `umask 022` and `TMPDIR=/dev/shm/e0-03-r4-tmp`, mode 0700; that temporary directory was removed after testing.

## Findings

### M1 — major: a resumed, never-dispatched `Built` transaction can take the lease-free resend path

**Locations:** `docs/design/DASHPAY.md:483`, `:499`, `:517`, `:548`, `:572`; `docs/design/ROADMAP.md:66`.

The table includes asset-lock resume among re-dispatches; the hook receives a wallet and `First`/`Resend`; and `Resend` is admitted without a lease because it purportedly commits nothing new. The later qualification “bytes that may already be out” is the right intended restriction, but the design gives no state or classification rule that distinguishes those bytes from a signed transaction stored **before its first hand-off**. `Built`, identical signed bytes, and execution of a resume call are insufficient evidence. The unconditional claim that a refused first hand-off removes the `Built` row also does not hold against the pinned library's concurrent-resume exclusion.

**Concrete interleaving:**

1. A registration signs an asset lock, tracks/queues it as `Built`, and pauses before its first lease check. No transport has been called.
2. Recovery snapshots that same `Built` row, takes a resume-dispatch claim, and waits for transport readiness.
3. `lock_vault` revokes the flow lease and returns; neither task holds its dispatch permit.
4. The original first hand-off resumes and is refused. Its cleanup cannot remove the row while the resume claim exists, so the row and inputs remain reserved.
5. The recovery task becomes ready. If the listed resume call is tagged `Resend`, the specified exception admits it without the revoked lease. This is the **first actual hand-off**, after lock, with no earlier dispatch or deadline expiry to justify `MaybeSent`.

**Evidence at the pin:** `PW/wallet/asset_lock/build.rs:1100` tracks the signed transaction before `:1142` broadcasts it. `PW/wallet/asset_lock/sync/recovery.rs:960` snapshots it, `:1004` claims its dispatch window, and the `Built` arm reaches `:1176`'s broadcast without a previous-send requirement. `PW/wallet/asset_lock/sync/tracking.rs:172` refuses removal under a resume claim. The initial build consequently checks whether removal actually happened (`build.rs:1155`) and reports an unknown outcome when it did not, rather than guaranteeing cleanup. The pinned regression `a_rejection_cleanup_cannot_release_inputs_under_a_parked_resume` (`build.rs:3093`) explicitly exercises this exclusion. Exact pinned source copies are retained as `pin-{build,recovery,tracking}.rs`.

`resend_model.py` / `resend-model.log` reproduce the counterexample as an executable state model of those pinned decisions and the proposed exception. **This is a specification counterexample, not a runtime test of the future dispatch hook, which does not exist in this branch.** It exposes the classification/coordination the upstream hook must add; it does not claim the new vault code broadcasts after lock.

**Suggested fix:** specify an artifact-bound dispatch state, tied to its original flow/lease, that is separate from `Built`. Only a recorded possible/actual dispatch of those exact bytes permits a lease-free resend. A never-dispatched artifact remains `First` even when reached through recovery, and needs its original live lease. Define the crash boundary and conservative `MaybeSent` state explicitly. Coordinate all resume claims and cancellation cleanup so that a provably unsent refusal leaves no future recovery task able to dispatch the artifact. Do not remove reservations for genuinely sent/ambiguous transactions. Extend E0-04's tests with the interleaving above, an unsent persisted `Built` row, and a genuine ambiguous prior dispatch; require only the last case to resend under a revoked lease. The current “a `Resend` passes” test does not distinguish them.

### m1 — minor: the single-H lock bound needs a rule for multiple leases

**Locations:** `docs/design/DASHPAY.md:460`, `:530`, `:538`; `docs/design/ROADMAP.md:66`.

A deadline on each hand-off does not by itself prove that revoking **every** lease takes at most H. For example, sequentially revoking A then B allows B to hand off an already-released signature just before A's H expires, starting B's own H while its lease is still unrevoked. Total waiting approaches 2H; more leases extend it further. The vault lock prevents new signatures, but cannot prevent these hand-offs of results already held by callers.

**Suggested fix:** freeze new lease/first-dispatch admission across the session before waiting, and start revocation of existing leases together under one overall deadline. State that the H bound applies to the asynchronous dispatch-drain phase, after the synchronous vault gate wait; the latter can include file I/O. Add a two-lease test with staggered hand-offs. This is a timing-specification gap, not a demonstrated vault deadlock or a new post-revocation signing path.

### n1 — nit, carried from round 3: a failing stress worker can hang the barrier

**Location:** `rust/crates/dw-vault/src/lock_race_tests.rs:431`, `:434`, `:446`.

An unexpected signer error panics a worker before it reaches the round-end `Barrier`; the other workers and coordinator then wait indefinitely and never reach `join`. The stress-test documentation now correctly distinguishes gate coverage from the isolated release-check test, but this failure-handling half of r3 n1 remains. Use a timeout/cancellation-aware rendezvous that releases all participants on failure; catching the panic without releasing the barrier is insufficient. All three requested runs completed normally, so this is a harness limitation, not evidence of a production deadlock.

## Round-3 closure and adversarial checks

| Item | Result |
|---|---|
| r3 M1: imprecise E0-04 ordering | **Core ordering fixed.** Shared permits serialize the first hand-off against exclusive revocation; the second barrier and check-then-act mutation distinguish the race. Library-internal commit points and the required upstream hook are now named. The new resend exception remains M1 above. |
| r3 m1: keyring read stalls lock | **Closed.** Re-ran the original `GateStore` probe: `seed_derivation` and `core_mnemonic_check` no longer held lock for the blocked 500 ms OS-store read. The read subsequently succeeds in the new epoch on an unencrypted vault. The stale export token is refused without an OS-store read. |
| r3 m2: ungated wipe/encrypt/enroll/set-limit | **Closed.** `key_for` requires an `OpGuard`, checks token binding and copies the key under the same `inner` acquisition, and never calls the OS store. All four mutations take `writer` before entering the gate and retain it through persistence. The committed tests cover a lock before key use and a lock while each operation is gated, including grant-owned and vault-owned keys. The original wipe hook now demonstrates lock waiting for the gated wipe in both vault modes. |
| r3 m3: passphrase change preserves old authority | **Closed.** Original pending grant, redeemed reveal token, scan token, and grant-owned crypto signer now respectively fail `GrantInvalid`, `Locked`, `GrantInvalid`, and `Locked`. `change_passphrase` persists under `writer`, then takes the gate exclusively and revokes under `inner` before returning. The committed test covers locked and fully unlocked states, preserved lock state, failed changes, and new credentials. An independent barrier test also makes a change wait for an in-flight grant-owned secret read. |
| Own-backup plaintext without authentication | **Closed for the vault-key path.** No grant and no passphrase now fail. Wrong-purpose/wallet grants are refused without consumption; foreign, consumed, and lock-revoked grants fail. A fresh passphrase-authorized reveal grant opens the bundle even on a locked vault without changing its state. Both v1 and v2 use the gated token-key path; existing compatibility tests pass. |
| Bounded key reload | **Verified.** Forced a lock after every preload: each metadata read and `authorize(None)` terminates after exactly three key-load attempts with `Locked`; a subsequent uncontended authorization succeeds. The bound limits retries, not the duration of an individual OS-store call; importantly, that call is outside the gate. |
| Lock order / inversion | **No production inversion found.** Gated bodies use in-memory keys and `inner`; writer mutations enter with an existing writer guard and call `commit_records_under`, which does not reacquire it. No gated path calls `full_dek`, acquires `writer`, or recursively enters the gate. Six concurrent roles ×40 rounds of lock/unlock/change-passphrase/enroll/set-limit/quick authorization finished without deadlock. Caller callbacks from `with_revealed_seed` run after release, as documented. |
| Quick-unlock authority and cap | **Verified.** Every configured limit accepts equality and rejects limit+1 and `u64::MAX`. The wrap key cannot authorize reveal, wipe, credential changes, or Platform operations. Wrong-purpose mutation grants fail; the existing suite covers replaced/removed keys, credential age, sealed policy, and invalid limits. Caps remain the issued grant's authority; lowering the setting is not an epoch revocation. |

**Does any path still return a secret without a grant? Yes, deliberately:** `open_backup_bundle`'s **file passphrase slot** returns phrase/seed/payload with the correct bundle password, including while the vault is locked. It uses the bundle's key, not the in-memory vault DEK, and is expressly ungated. A backup made under an old password still opens its own snapshot with that password after a vault passphrase change. This is independent backup restoration, not a return of current vault secrets using an unauthenticated vault key. Thus the grant requirement must not be described as unconditional. Without either a reveal grant or a valid bundle password, the probes found no phrase/seed return. Previously documented engine-only auto-accept/master-key exports retain their respective scope/grant requirements.

**Throttle accounting:** direct file-slot failures intentionally leave the vault counter unchanged; seven independent wrong-password attempts verified that. For the engine router, I mechanically copied the branch's `open_bundle` helper into the scratch test crate, changing only its `WalletSecret` type qualification. Five typos against a current-slot bundle count zero. Against an old-slot bundle requiring the current vault key, failed fallback credentials count 1, 2, 3; the fourth is `Throttled`. A valid current file-slot password still opens while the vault's own authorization is throttled. The real engine suite also passes its before-encrypt/old-passphrase/current-passphrase restore tests. Locked restore is checked before opening bundles and again before storage. These behaviors match the amended contract.

The new FFI source guard passes against the actual sources. An independent `clippy-driver` misuse probe with the real dw-ffi configuration rejects both a direct `open_backup_bundle` call and a function-pointer alias with the intended two `disallowed_methods` diagnostics. This is a lint boundary, not Rust visibility isolation.

The other r3 nits are addressed where touched: token reuse within an epoch and wipe/re-import are documented, and the `with_revealed_seed` callback exception is explicit. The optional SHA long-message coverage suggestion is unchanged; these three commits do not alter `dip15.rs`.

## Validation and evidence

- `cargo test -p dw-vault -p dw-engine`: **261 passed, 0 failed, 1 ignored** (`suite.log`). Includes the real engine backup tests and all four E0-03 fixture adapter tests.
- `cargo test -p dw-vault --lib lock_race -- --nocapture` **×3**: **11/11 each**. Every run exercises 16 workers ×300 locks in each of three modes. Each mode reports 4,800 `Locked` refusals and **zero releases under an ended epoch**; releases while lock waited range from 3,947 to 4,371 (`stress-{1,2,3}.log`). These prove the vault linearization, not arbitrary caller-side delivery or the future flow fence.
- Original r3 probe recheck: **4/4** (`r3-recheck.log`). The original source is retained at its supplied path. `scratch-changes.patch` records the assertion changes for fixed behavior and moving the wipe's lock call to a second thread with a bounded wait; a synchronous lock from the original probe would deadlock precisely because the gate now works. The hook remains at `key_for`, inside the new gate. Wipe/re-store signer/token survival still reproduces and is now explicitly documented.
- Additional independent scratch vault/router probes: **7/7** (`r4-probes.log`); compilation commands/dependency selections are in `compile_probe.py` and `rustc-command.json`. They link the exact dependency fingerprints used by this worktree's freshly run vault suite, with scratch outputs rather than a shared Cargo target directory.
- FFI source test, compiled standalone: **1/1** (`ffi-source-guard.log`). Direct/aliased misuse: both refused (`ffi-misuse.log`). An optional full dw-ffi Cargo test build was interrupted during dependency recompilation after these focused checks; no full-FFI-suite result is claimed.
- Specification counterexample: `resend_model.py` succeeds and records one first actual dispatch after revocation under the proposed resend classification (`resend-model.log`); its evidence limits are stated in M1.
- `git diff --check d36acdb..da66db0`: clean. Reviewed worktree remained clean at the same HEAD after testing.

Verdict: **CHANGES-REQUIRED**.
