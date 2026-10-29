# Review DW-E0-03 (vault signer glue), round 1 (Opus, security)

- **Branch:** `dw/e0-03-signer-glue` (dashwallet-desktop), `git diff main...HEAD`, commits 5577aee, d144cdc, 58f8374.
- **Counterparts:** platform `bc321362b9`:
  - `rs-sdk-ffi/src/mnemonic_resolver_core_signer.rs:330-600`
  - `rs-platform-wallet-ffi/src/dashpay.rs:713-893`
  - `rs-platform-wallet-ffi/src/sign_with_mnemonic_resolver.rs:155-330`
  - `rs-platform-wallet/src/wallet/identity/network/contact_requests.rs` (the trait at `:35-150`, call sites at `:520-600` and `:3640-3670`, the drain at `:2235-2480`)
  - `rs-platform-wallet/src/manager/startup.rs:1-200`
  - `asset_lock/build.rs:590-640`
  - key-wallet at the locked rust-dashcore rev `e4208c9`.

## What I ran

| Check | Result |
|---|---|
| `umask 022; cargo test -p dw-vault -p dw-engine` | **All green.** dw-vault: unit tests 44 passed, 1 ignored; `platform_scopes` 9/9, `quick_unlock` 11/11, `race` 3/3, `vault` 20/20, `masternode_keys` 2/2. dw-engine: unit tests 98/98, `e0_03_signers` 4/4, and the other offline suites pass. |
| Regenerated the fixture: scratch `git worktree` of platform `bc321362b9`, applied `e0_03_signer_vectors.generator.patch`, ran the README recipe | **Byte-identical** to the committed `e0_03_signer_vectors.json` (sha256 `cadabfca…7634` for both). The worktree and target dir are removed and the main platform tree is untouched. |
| Scratch attack test on `paths.rs` and the scopes (in `dw-vault/tests/`, deleted after the run, not committed) | No scope escape. Details in §1. |
| Scratch race test: 8 threads running `ecdh_shared_secret` in a loop while `lock()` runs | No call that **started** after `lock()` returned succeeded. **2–3 calls per run that were already in flight returned results after `lock()` returned** (§3, m1). |
| Scratch test: Full unlock → `unlock(MixingOnly)` without a lock | The background signer and `ScanKey` both go to `Locked` (the epoch bumps on the scope change). OK. |
| Grep of dw-ffi and Swift for `scan_key`, `dashpay_crypto_signer`, `platform_signer`, `ScanKey` | No references. dw-ffi holds a raw `dw_vault::Vault` (`dw-engine/src/keys.rs:114`, `dw-ffi/src/api/vault.rs:458` `vault_op` closures), so only convention keeps it away (M1). |

The branch is unmodified (`git status` clean at 58f8374).

## Findings

### Blockers

None.

### Major

**M1. `ScanKey` exports the wallet's master xpriv with no grant, and the docs say nothing but the auto-accept key leaves the vault.**

- **Where:**
  - `rust/crates/dw-vault/src/platform.rs:293-296` (`master_key`) and `:304-308` (`Vault::scan_key`)
  - `rust/crates/dw-engine/src/platform/signers.rs:310` (`VaultScanKey::resolve`)
  - the claims in `docs/contracts/m1-engine.md:123-124` ("The derived scalars never leave dw-vault; the one exception is the auto-accept key"), `dw-vault/src/lib.rs:12-13,19-20` ("a buggy view cannot sign, reveal or wipe without [a grant]"; "derived scalars stay in this crate"), `dw-vault/src/platform.rs:1-6` and `dw-engine/src/platform/signers.rs:18-21` ("The derived scalars never reach this module").
- **What is wrong:**
  - `scan_key` returns a `ScanKey` whose `master_key()` hands the depth-0 `ExtendedPrivKey`, worth the whole wallet, to the caller.
  - It needs no grant whenever the vault is `Unencrypted`, or `Unlocked` with scope Full.
  - Before this branch, nothing grant-less on an unlocked vault yielded raw key material. `RevealSecret` deliberately requires the passphrase even while unlocked (`types.rs` `requires_credential`).
  - The library API forces this (`startup.rs:100-104`: `ScanKeyResolver` returns `ExtendedPrivKey`), so this is a design consequence, not a coding slip. But it is the most sensitive export in the change, the four places above document the opposite, and nothing but a comment stops dw-ffi from calling it.
- **Failure scenario:** A later FFI task adds a convenience vault method (or a debug or export command) that goes through `session.vault()` / `vault_op(|v| …)`. A call to `v.scan_key(w)?.master_key()` compiles and silently gives the UI layer the master xpriv on an unlocked vault, with no passphrase prompt. Every reviewer reading the contract believes only the auto-accept key can leave.
- **Fix (all small):**
  1. Correct the docs in the four places. The master xpriv leaves dw-vault, unprompted, to platform-wallet's identity scan, which wraps it in `ScanKeyGuard` (`startup.rs:134-157`). List it as a deviation from "derived keys never leave" next to the auto-accept key.
  2. Add a mechanical guard instead of a comment. Either:
     - `rust/crates/dw-ffi/clippy.toml` with `disallowed-methods = ["dw_vault::Vault::scan_key", "dw_vault::Vault::dashpay_crypto_signer", "dw_vault::ScanKey::master_key"]`, which the existing `clippy -D warnings` gate already enforces; or
     - a dw-ffi test that fails if those symbols appear under `dw-ffi/src`.
  3. Recommended: mark `scan_key` `#[doc(hidden)]`, or rename it (`scan_key_for_bringup`), so a misuse stands out in review.
  4. Follow-up (not for this PR): ask upstream to let `ScanKeyResolver` take a per-key derive-and-validate callback instead of the master xpriv. Discovery already has the public-key form of the check (`discovery.rs:1011-1033`), so a `PlatformIdentity`-scoped signer could serve it.

### Minor

**m1. Lock does not stop an operation that is already in flight.**

- **Where:** `rust/crates/dw-vault/src/vault.rs:1433-1450` (`signing_seed`) and `rust/crates/dw-vault/src/signer.rs:230-241` (`with_key`).
- **What is wrong:** `signing_seed` checks the epoch and copies the DEK under the inner lock, then releases it. The record decrypt, the derivation and the operation run unlocked, so a `lock()` landing in that window returns first and the result still comes out afterwards. Reproduced: 2–3 ECDH results per run returned after `lock()` had returned.
- **Scope of the race:** Nothing that *starts* after the lock succeeds, so the brief's literal question ("a signer created before the lock can still sign after it") holds for new calls. DASHPAY §2.6 promises "Lock always wins", though. The window is the same for the existing Spend and mixing signers, so this predates the branch.
- **Failure scenario:** A user presses Lock to cancel a registration ("Registration in progress — Lock to cancel"). A state-transition signature or asset-lock input signature already being computed is still returned to the flow, which can broadcast it.
- **Fix:** In `with_key`, re-check `vault.inner().epoch == self.epoch` after `f` runs and before returning. On a mismatch, drop the output (erase it, if it is secret) and return `SignerError::Locked`. This is cheap, makes "a result returned ⇒ the vault was unlocked when it was produced" true, and fixes the old signers too. Add a race test in the style of `tests/race.rs`.

**m2. The `PlatformFunding` cap is advisory, and the contract states engine enforcement that doesn't exist yet.**

- **Where:** `docs/contracts/m1-engine.md:115`, `rust/crates/dw-vault/src/vault.rs:1283-1303`.
- **What is wrong:**
  - The vault never enforces `max_duffs`; it signs sighashes and can't. `platform_signer` accepts any cap the caller passes.
  - One `PlatformOp` token mints any number of signers of any scope until the next lock. On an unencrypted vault, that means forever.
  - The contract says "The engine checks the cap before signing, as for `Spend`", but no engine code builds a funding transaction yet, so nothing checks it.
  - A `PlatformFunding` signer signs every BIP44 account, BIP32 and DIP-15 receiving address. Under a no-credential `PlatformOp` on an unlocked vault, it is as strong as a Spend grant with a cap the engine chooses itself.
  - This is the documented E0-04 deviation, and it is strictly better than the old `PlatformOp` → Full signer, so it is not a blocker.
- **Failure scenario:** DP1-02 lands before E0-04 and passes `u64::MAX`, or the quote, as the cap. The "funding cap holds" acceptance item is then met only by convention.
- **Fix:**
  - Reword the contract to "the engine **must** check the cap… (not yet implemented; E0-04)".
  - Make DP1-02 depend on E0-04, as the roadmap already says.
  - In E0-04, take the cap from the grant inside `platform_signer`, and refuse a `PlatformFunding` scope whose `max_duffs` exceeds the grant's.

### Nits

- **n1. Out-of-range child indices are accepted at wildcard positions** (`paths.rs:35-44`, `hardened_index` and `is_normal`).
  - `Hardened{index: 2^31|i}` passes the identity, key, account, expiry and contactInfo-index wildcards. key-wallet ORs the top bit (`bip32.rs:1590`), so it derives exactly the same key as `i'`. Verified: the `PlatformIdentity` public key at `…/(2^31+3)'/1'` equals the one at `…/3'/1'`.
  - `Normal{index ≥ 2^31}` passes the leaf wildcards and derives a non-standard key that is distinct from the hardened one. Verified: the credit-key leaf `Normal{2^31}` is not the top-up node `0'`.
  - `Hardened256` passes `is_bip44_path`'s `p[2].is_hardened()`.
  - None of these escape a scope: every fixed step uses exact equality, so `Hardened{2^31|9}` at the purpose step is refused.
  - They do make the same key reachable under two path strings, which is bad for logs and audits.
  - **Fix:** have `hardened_index`/`is_normal` return `None`/`false` for `index ≥ 2^31`, and have `is_bip44_path` require `Hardened { index < 2^31 }`. Add those cases to the `platform_scopes` matrix.
- **n2. `ContactInfoOpened` keeps decrypted contact data in derived `Debug`, `Clone` and an unzeroized `Vec`** (`dw-vault/src/platform.rs:42-46`). Its `private_data` is the decrypted contactInfo (alias, note), and any `{:?}` on a result logs it. Implement a redacting `Debug`; the library's mirror type has the same issue.
- **n3. Upstream temporaries are not zeroized (known; the implementer listed it):**
  - `platform_encryption::derive_shared_key_ecdh` returns `[u8; 32]` by value before it is wrapped;
  - the auto-accept `SecretKey` (`signers.rs:216-226`) has no erasing drop;
  - `derive_priv` intermediates aren't erased.
  These are inherent to the trait and library. Leave a one-line note under "Residual risks" in `lib.rs:26-29`.
- **n4. `can_sign_with` decrypts the seed and derives once per identity index on every call** (`signers.rs:145-154`), and maps a locked vault to `false` instead of an error. That's acceptable for dpp's preflight, but a short comment would help.
- **n5. One stale doc against the pinned library:** `startup.rs:136` says "`ExtendedPrivKey` has no erasing `Drop`". At the locked key-wallet `e4208c9` it does (`bip32.rs:373-395`), so the branch's "erases itself when dropped" claims (`platform.rs:291-292`, `signers.rs:294-296`) are **correct**. Pin that dependency in a comment: an E0-01 pin bump to a key-wallet without the `Drop` would make `ScanKey::master_key`'s clone a leak.
- **n6. Forward note for X3 (invitations), not this PR:**
  - `paths.rs:152-173` lets funding sign only a **non-hardened** leaf under `5'/3'`. The library's trait and resolver document the invitation key as `m/9'/coin'/5'/3'/funding_index'` (**hardened**; `contact_requests.rs:66-71`, `mnemonic_resolver_core_signer.rs:395-430`).
  - If key-wallet's invitation pool is non-hardened, which the branch assumes (consistent with top-up at `build.rs:590-620`), the wallet holds the `5'/3'` account xpub. Exporting a non-hardened child private key together with that xpub reveals the account xpriv, and so **every** invitation voucher.
  - X3 must settle this first: hardened voucher keys, or no account xpub in the wallet.

## Answers to the brief's questions

1. **Scope bypass.** None found.
   - Every predicate fixes the length and checks each fixed step by exact `ChildNumber` equality, coin type included (`paths.rs:47-52`).
   - Prefix and extension tricks fail on length.
   - DIP-14 256-bit ids are accepted only as `Normal256` at DIP-15's positions; `Hardened256` and 31-bit ids are refused there.
   - Overflowed indices only alias inside a wildcard the scope already grants (n1).
   - `KeyUse` separates sign, public key, chain code, agreement, contactInfo and export, so no scope gets a chain code over a key another scope signs or exports.
   - I checked the xpub-plus-child-private-key attack: the only exports are the auto-accept key (a hardened leaf, and its parent xpub isn't offered) and the master key (M1).
   - ECDH and HMAC oracles on identity keys leak nothing, because the peer can compute the ECDH value itself and HMAC is a PRF.
   - The scope check runs before the seed is read (`signer.rs:213`).
2. **Secret handling.** Good:
   - seed in `Zeroizing<[u8; 64]>`; master and derived xprivs erased explicitly and by key-wallet's `Drop`;
   - AES keys and HMAC keys in `Zeroizing`, scoped to one statement;
   - no secrets in `Debug` (`VaultSigner`, `ScanKey` and `GrantToken` all redact);
   - errors carry only paths;
   - no secret is held across an `.await`, since every vault operation is synchronous inside the async adapters.
   Gaps: M1 (scope of the master export) and n2/n3.
3. **Lock semantics.** Lock, scope change (Full → MixingOnly, verified) and grant-key signers all revoke through the epoch, and nothing issued or started after a lock works. The one gap is in-flight completion (m1).
4. **Byte-equivalence.** Genuine: I regenerated the fixture from the patch and got a byte-identical file.
   - The patch drives the library's own `SeedCryptoProvider` for every `ContactCryptoProvider` operation.
   - Identity signatures come from `dashcore::signer::sign`, a different implementation from the vault's `sign_ecdsa_recoverable`, so that comparison is independent.
   - Both sides call the same `platform_encryption` functions. The vectors therefore prove derivation and wiring, not AES/ECDH correctness, and that is the right thing to prove here.
   - Covered: receiving xpub, BIP44 account-0 xpub, auto-accept public and secret key, ECDH, account reference and unmask, contactInfo seal and open (including opening the library's ciphertext), identity signatures for keys 0–5 of two identities, and the scan master (identity keys re-derived from it match).
   - Not covered: the funding scope's outputs (credit-key signatures, the top-up account xpub), which are outside `SeedCryptoProvider`. Optional: one vector for the top-up xpub against key-wallet `add_account(.., None)`.
5. **The four deviations.** All four are justified and safe.
   1. `DashPayCrypto` ECDH and mask with identity keys, plus the BIP44 xpub: the library needs them (`contact_requests.rs:530-545, 3640-3662`; `seed_binding.rs`). The outputs are hashed or public. The vault accepts any identity key id; the library checks key purpose upstream (`validate_recipient_key`).
   2. `PlatformOp` gives scoped signers only: a strict improvement, and grep confirms there are no remaining callers.
   3. Locked maps to `WalletLocked`: no library code matches on it, and the drain logs and leaves the entry queued for any provider error (`:2235-2280`), so behaviour is unchanged.
   4. Invitation export: the trait's default fails closed with an error. See n6 before X3 enables it.

   The two documented scope widenings (the top-up xpub, the engine-picked cap) are covered by m2.
6. **Reachability of the grant-less entry points.** They are not reachable from Swift: nothing is exported over uniffi. They are reachable from dw-ffi's Rust code by construction, since it holds a `dw_vault::Vault` through the session. Today no code calls them. M1's guard closes this.

## Verdict

**CHANGES-REQUIRED.** One major (M1: correct the master-key documentation and add a mechanical dw-ffi guard), plus m1 (the cheap in-flight lock re-check) and m2 (contract wording). The path logic, scopes, zeroization and fixture provenance are sound. I would approve once M1 is fixed and m1 and m2 are either fixed or explicitly deferred to E0-04.
