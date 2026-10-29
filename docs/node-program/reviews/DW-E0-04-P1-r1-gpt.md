# DW E0-04.P1 review — round 1

Target: `3dc95c6`, base `746c395`; detached worktree `/work/scratch/e0-04-p1-gpt/repo`. Approved design matches desktop main byte-for-byte.

**Verdict: CHANGES-REQUIRED.** One high-severity admission race and one medium-severity ownership/conformance finding.

## Findings by severity

### High — weak upgrade is not a hold-liveness check

`rust/crates/dw-vault/src/types.rs:233`, `rust/crates/dw-vault/src/vault.rs:1979`.

A running operation temporarily upgrades the held key to a strong `Arc`. If it is preempted before dropping that reference, dropping `KeyHold` does not make subsequent `Weak::upgrade` calls fail: they can upgrade the operation's remaining strong reference. A new call begun after the hold drop can therefore succeed, contrary to §3.5's “Every later call fails Locked.” The short duration of the key copy reduces likelihood but supplies no ordering guarantee. There is another temporary upgrade during signer construction at `vault.rs:1849`, outside the gate. A separate hold-liveness/revocation state, synchronized with operation admission and cleared when the hold drops, is needed; existing operations may finish with their own copies.

### Medium — direct signers and partial holds depart from §3.5

`rust/crates/dw-vault/src/vault.rs:1783`, `rust/crates/dw-vault/src/vault.rs:1859`, `rust/crates/dw-vault/src/types.rs:218`.

Direct own-key signers clone `KeySource::Own` strongly. `hold_key` then skips their tokens and can return a hold for only the remainder of the same grant set. Dropping that hold leaves the skipped key and signer usable. The new test `tests/grants.rs:592` explicitly exercises this behavior (and asserts continued use at line 616). §3.5 instead says own-key signers become weak and every token copy in the set is erased. Changing m1-engine to document the exception does not amend DEC-73's approved design. Make the ownership transfer cover the complete set and make all own-key signers weak, or obtain an explicit design amendment; do not silently interpret a partial hold as the set's hold.

## Interpretation judgments

1. **Direct signers keep strong references:** reject as a conformance exception, for the second finding above. The `*_held` path works in the ordinary case, but §3.5 does not specify an opt-in weak-signing subset.
2. **Round credits per grant, then sum:** accept. This follows §3.7 literally. The model describes rounding after summing credits, but models only one PlatformOp plus one Spend; those shapes agree. With multiple PlatformOp grants the implementation is conservative, by at most one duff per extra credit-bearing grant, and cannot over-authorize. Align the model's general wording when it is extended.
3. **QuickUnlock PlatformOp{0,0} at limit 0; contact crypto from any PlatformOp:** accept. Design §4.2 explicitly gives Crypto to any PlatformOp; §3.4 refuses both signature scopes at zero. Freshness and valid slot-B credentials still apply.
4. **Frozen FFI maps PlatformOp to {0,0}:** accept as a safe interim E0-13 boundary. The host cannot request caps or IdentityScan yet. The reverse enum alias does not let a host redeem a scan id as a PlatformOp; redemption checks the stored grant kind.

## Other scope conclusions

- **Scope/caps:** PlatformOp cannot obtain a full signer or `scan_key`; IdentityScan cannot obtain the public signing APIs. Funding requests above the token cap, and zero-cap funding/identity requests, fail before key access. Contact crypto's limited exports are existing, explicit design exceptions. Repeated signatures and repeated signer creation do not decrement vault budgets: §3.4 explicitly delegates transaction debit and transition-cost accounting to the P2 engine. P1 alone is not a cumulative spending-limit enforcement boundary.
- **Atomicity:** every purpose and wallet binding is checked before issuance; the complete id vector is generated before insertion, and all insertions occur under `inner`. Concurrent redemption, lock, or authorization cannot observe a partially inserted set. Passphrase issuance holds `writer` through its one check and issuance; QuickUnlock validates and issues under `inner`. A lock during the passphrase check can precede subsequent issuance in the new epoch; that is a newly authorized locked-vault grant, not revival of an old token. P2's epoch check around lease creation remains necessary.
- **Throttle:** the passphrase arm calls `check_passphrase` once per set, and its writer serialization prevents concurrent checks bypassing the persisted throttle. Binding/empty-set rejection precedes any attempt. QuickUnlock failures do not increment the passphrase throttle, consistent with the existing policy.
- **QuickUnlock math:** ceiling conversion avoids addition overflow in rounding; individual values and the set sum saturate. Every configured spend limit is far below u64::MAX, so saturation refuses overflow cases. Spend values are included, SignMessage contributes zero, disallowed purposes reject the entire set, and the passphrase-age boundary is tested.
- **Epoch/zeroization:** gated signer calls check their issuance epoch and release under the vault mutex. No missing production epoch-change path was found. Lock waits for gated operations and prevents later old-epoch signatures even for the strongly owned direct signer; it does **not** physically erase that signer's retained Arc. Held operations' DEK/seed copies use Zeroizing; the pinned key-wallet `40268cc` actually implements ExtendedPrivKey's zeroizing Drop, including chain code. A previously exported scan master remains caller-owned until its Drop, an acknowledged design limitation rather than a new cap bypass. Live holds must be dropped by P2 on lock.
- **Race rigor:** the 16-worker/300-drop test has bounded coordination and checks every worker eventually sees Locked. It exercises the normal post-drop path, but is probabilistic at admission. The deterministic test pauses at `signer.rs:240`, after `signing_seed` and master derivation; its zero strong-count assertion therefore excludes the exact window in the first finding. Add a deterministic pause after weak upgrade but before the strong reference drops, drop the hold, then start a second operation and signer construction. Both new requests must fail while the first admitted operation may finish. `Weak::strong_count() == 0` proves destruction of the managed Zeroizing value by source inspection, not inspection of erased bytes; the allocator erasure check is listed separately under P2 in §12.

## Evidence and validation

- Standalone read-only-review reproducer: `/work/scratch/e0-04-p1-gpt/weak-upgrade.rs`. It deterministically pauses an upgraded reference, drops the designated hold, and demonstrates a **new** weak upgrade succeeds. This verifies the Arc/Weak mechanism used by the vault; it is not an instrumented end-to-end vault regression test.
- `cargo fmt --all -- --check`: passed.
- `git diff --check 746c395..HEAD`: passed.
- `python3 -I docs/design/checks/e0_04_design_model.py`: passed (`ok`); log at `/work/scratch/e0-04-p1-gpt/design-model.log`. The model does not cover the transient Arc ownership race.
- `cargo test -p dw-vault --locked`: passed; **136 passed, 0 failed, 1 ignored** (opt-in OS keyring test), including both new hold-drop race tests and the partial-hold reproduction. Fresh local build completed in 8m 12s. The reported broader 551-test/clippy gate was not independently rerun.
- All commands use `TMPDIR=/dev/shm/e0-04-p1-gpt-tmp` (mode 700), with at most 8 Cargo build jobs and a worktree-local target. No repository source edits, commits, pushes, or GitHub actions.
