# DW-E0-03, round 1 — independent security review

Reviewed `dw/e0-03-signer-glue` at `58f8374f871d126b51ffbfd70048ead99386acb9`, against merge-base/main `1b080d3be91b1ce99bed976f44a8301a16038062`: commits `5577aee`, `d144cdc`, `58f8374`. Repository paths below are relative to `/work/home/agent/workspace/dw-wt/e0-03-signer-glue`. Platform references were read from `git show bc321362b9:<path>`, not the checkout's current files. Read `CLAUDE.md`, DASHPAY §2.6/§3.3, ROADMAP E0-03, and m1-engine §2.2. No branch files were modified and no commits were made.

## Findings

### B1 — blocker: `lock()` can return before an outstanding signer produces a signature

**Locations:** `rust/crates/dw-vault/src/signer.rs:216`, `rust/crates/dw-vault/src/signer.rs:236`, `rust/crates/dw-vault/src/platform.rs:114`; underlying epoch check at `rust/crates/dw-vault/src/vault.rs:1439` and lock transition at `rust/crates/dw-vault/src/vault.rs:612`.

`signing_seed` checks the signer's epoch while holding `Inner`, copies the DEK, and releases that mutex. Neither subsequent record decryption, derivation nor `with_key`'s secret-using callback checks revocation again or remains synchronized with `lock`. An old signer can therefore complete a signature or crypto operation after lock has returned. In `sign_identity`, the double-SHA256 of caller-supplied data occurs **after** the private key has been derived, making the race straightforward to reproduce without changing production code.

**Failure scenario and evidence:** an encrypted regtest vault issues a `PlatformIdentity` signer. A worker calls `sign_identity` for `m/9'/1'/5'/0'/0'/0'/0'` with its correct key data and a 512 MiB public test buffer. After 150 ms, another thread locks the vault. In the final scratch run, `lock()` returned `Locked` in **52.81 µs**, the worker was still running, and that worker later returned **`Ok(signature)` after 7.91 s**. A later public-key call on a retained clone correctly returned `SignerError::Locked`; that does not revoke the already-running operation. The oversized buffer widens an existing race; the same unsynchronized interval exists for ordinary inputs. The seed-copy pattern predates this branch, but the new Platform operations inherit it and fail the brief's explicit immediate-revocation requirement.

**Fix:** coordinate epoch changes with the entire secret-using operation and publication of its result, using an operation guard/permit or equivalent serialization. A lock must not return while a revoked operation can still publish a signature, shared secret, ciphertext or exported key. Do expensive untrusted hashing before acquiring the final signing permit where possible. A standalone final epoch check with no synchronization still leaves a check/publication race. Add a barrier-controlled regression test that exercises lock during derivation/signing and the grant-owned-key case, rather than testing only calls made after lock.

### M1 — major: outer `Zeroizing` wrappers do not erase the crypto helpers' secret copies

**Locations:** `rust/crates/dw-vault/src/platform.rs:132`, `rust/crates/dw-vault/src/platform.rs:150`, `rust/crates/dw-vault/src/platform.rs:225`, `rust/crates/dw-vault/Cargo.toml:28`.

The vault erases its seed, extended private keys, scalar-byte wrappers and returned ECDH wrapper, but its newly used `platform_encryption` helpers leave other owned secret material unwiped:

- At the pinned Platform revision, `packages/rs-platform-encryption/src/ecdh.rs:21` creates a `secp256k1::ecdh::SharedSecret`, then copies it into a plain array. `SharedSecret` in secp256k1 0.30 is `Copy` and has no erasing `Drop`; the helper never calls `non_secure_erase`. Wrapping the returned array cannot erase the helper's original shared-secret value. The scratch test confirms `needs_drop::<SharedSecret>() == false`.
- `packages/rs-platform-encryption/src/contact_info.rs:26` and `src/aes.rs:28` create AES key schedules. The actual Cargo fingerprints show AES 0.8.4 built with **no features**, hence without its `zeroize` feature. Its AES-NI schedule destructor erases round keys only under that feature. These schedules contain recoverable copies of the contactInfo AES keys, which are derived private scalars.
- `packages/rs-platform-encryption/src/account_reference.rs:16` passes an identity private scalar to HMAC 0.12.1. That crate's `src/optim.rs:153` builds a plain padded key buffer and leaves it XORed with the known OPAD value; there is no erasing destructor or explicit wipe. `account_reference.rs:19` also keeps the full PRF output in a plain array. The outer `secret_bytes(x)` wrapper does not reach those buffers.

**Failure scenario:** a memory disclosure or later process-memory capture can recover crypto material after an operation and lock, despite the wrapper's destruction. In particular, an unwiped HMAC key-pad copy can disclose the identity scalar, and an AES schedule can disclose its AES key. This is established from concrete buffer ownership, dependency features and destructor behavior; I did not attempt a post-drop memory-read exploit or claim that every optimized build necessarily retains each stack copy.

**Fix:** enable AES's erasing feature in the actual resolved graph and harden the ECDH/HMAC helper implementations, or use equivalent audited, guarded implementations inside the vault. The original shared-secret object, key-pad buffers, keyed state and PRF intermediates need explicit lifetime-bound erasure; wrapping only final outputs is insufficient. Retain the existing byte-equivalence fixtures when changing these helpers, and add feature/destructor checks. These are inherited upstream implementation gaps, not a wire-format mismatch.

### M2 — major: path gates admit children outside the documented BIP32/BIP44 shapes

**Locations:** `rust/crates/dw-vault/src/paths.rs:35`, `rust/crates/dw-vault/src/paths.rs:42`, `rust/crates/dw-vault/src/paths.rs:67`, `rust/crates/dw-vault/src/paths.rs:137`.

`hardened_index` accepts any raw `u32`, and `is_normal` accepts any `Normal` variant, without enforcing the 31-bit index range. Public `ChildNumber` variants can be constructed directly, so string parsing and the safe constructors are not sufficient validation. Separately, `is_bip44_address` delegates its account check to the broad legacy `is_bip44_path`, whose `p[2].is_hardened()` accepts **`Hardened256`**. That admits a 256-bit BIP44 account even though this module explicitly promises that 256-bit children are confined to the two non-hardened DIP-15 identity-id positions.

**Failure scenario and evidence:** scratch calls using the built branch library demonstrated that:

- A `PlatformFunding { max_duffs: 0 }` signer successfully signs an ECDSA digest at `m/44'/1'/0'/0/<Normal { index: 2147483648 }>`. The signature verifies, even though reparsing the displayed path rejects that index. The zero cap here only identifies the tested scope; cap enforcement is documented as an engine responsibility and is not a separate finding.
- The funding signer also successfully signs when the BIP44 account is replaced by `Hardened256 { index: [0x42; 32] }`.
- A crypto signer exports an auto-accept key for raw `Hardened { index: 2147483649 }`. It equals the key for expiry `1`, because upstream hardened derivation ORs in the high bit instead of validating the index. Canonical constructors reject the purported expiry.
- `u32::MAX` is accepted in identity indices/key ids, contactInfo indices, BIP44 accounts/leaves, asset-lock leaves and top-up identity slots.

These cases demonstrate off-shape derivation/signing and index aliasing, not a proven jump into a different purpose/coin subtree. The fixed purpose, coin and feature checks held in the scratch tests.

**Fix:** make the strict Platform predicates validate `Normal { index }` and `Hardened { index }` with `index < 1 << 31`, and require an ordinary 31-bit hardened BIP44 account rather than merely `is_hardened()`. Preserve intentionally broad legacy CoinJoin behavior separately if necessary. Exercise every dynamic slot with direct raw variants at `2^31−1`, `2^31`, `u32::MAX` and the 256-bit variants; the existing out-of-range test only mutates a fixed feature constant.

### N1 — minor: the contract omits the scan master-key export exception

**Locations:** `docs/contracts/m1-engine.md:123`, `rust/crates/dw-vault/src/platform.rs:293`, `rust/crates/dw-engine/src/platform/signers.rs:310`.

The contract says derived scalars never leave dw-vault except for the auto-accept key. `ScanKey::master_key`, however, deliberately clones and returns the full master extended private key, and the engine returns it to platform-wallet's `ScanKeyResolver`. DASHPAY §3.1 already acknowledges this interface. After resolution, the exported key itself is not revocable by the vault epoch; the library's guard erases it on drop, not on a subsequent vault lock.

**Failure scenario:** a future caller reads the contract as proof that bring-up never holds an unrestricted private key or that invalidating the resolver erases a key already returned. The current API and library's await-containing scan do not provide either guarantee.

**Fix:** document the master-key export as a second, tightly bounded exception, state its Rust-only consumer and drop lifetime, and make E0-05's lock/cancellation integration explicitly dispose of an active scan key. This export is required by the pinned scan interface, so I am not asking to remove it in E0-03.

## Checks against the brief

- **Scopes and key binding:** fixed purpose/feature/coin constants use typed equality, and identity keys require exactly seven components. Prefixes, extensions, wrong coins, non-hardened identity slots and hardened DIP-14 friendship ids were refused in the inspected tests/scratch checks. High-bit **256-bit** friendship ids remain correctly allowed as `Normal256`. `DashPayCrypto` never permits `Sign` or `Message`; agreement is restricted to identity keys and contactInfo derivation internally appends the two hardened feature children. Identity signatures require the derived compressed key or HASH160 to equal the requested key data. No additional cross-scope signing/export route was found beyond M2.
- **Secret ownership and awaits:** top-level seed/payload/key buffers are guarded. The pinned key-wallet `ExtendedPrivKey` has an erasing `Drop` for its scalar and chain code, including intermediate clones and error/unwind paths. The new signer Debug implementations expose wallet id/scope, not seeds or scalars; signer errors include paths and crypto errors rather than private input bytes. The new crypto/signing adapter bodies do not suspend with a locally derived scalar. The scan's exported master deliberately lives through library awaits under its guard; grant-owned DEKs live in `Arc<Key32>` until the last clone drops, with epoch invalidation controlling use. M1 remains a material gap in the broader zeroization claim.
- **Locks:** issuance correctly checks current state while taking the signer epoch; encrypted `Locked` and `UnlockedMixingOnly` reject both background factories. Stale handles fail subsequent calls, including after another unlock. B1 demonstrates the missing in-flight revocation guarantee.
- **Fixture provenance and coverage:** the generator patch applies cleanly to the exact `bc321362b9` `contact_requests.rs` blob. It adds a scratch-only test and calls that file's actual `SeedCryptoProvider` methods; it does not generate expected bytes by calling dw-vault. Identity expectations use key-wallet's wallet derivation and `dashcore::signer::sign`, matching `PWF/sign_with_mnemonic_resolver.rs:320`. The fixtures cover both seeds on both networks, receiving xpubs, seed-binding xpub, auto-accept public/exported key, ECDH, account reference/unmask and contactInfo seal/open, plus identity keys 0–5 for identities 0/1. The committed tests compare these products and verify both ECDSA key representations. Scan output is checked by deriving the same identity public keys. Funding-specific signatures/top-up xpubs have scope tests rather than independent golden fixtures. I inspected the generator/recipe and checked patch applicability; I did not independently rebuild the entire upstream workspace to regenerate the JSON.
- **Grant-less FFI reachability:** searches and inspection of `rust/crates/dw-ffi/src/api/vault.rs`, the session wrappers and `Sources/DashWalletCore/Generated/DashWalletCore.swift` found no export of `scan_key`, `dashpay_crypto_signer`, `VaultScanKey` or `VaultContactCrypto`, nor a returned vault/signer object exposing those methods. They remain callable by Rust engine code; their `pub` visibility is not itself Swift reachability.

## Documented departures and justifications

The brief mentions four documented deviations but supplies no separate enumerated implementation report. I reviewed the concrete departures present in the branch and their pinned-library reasons:

1. **Identity-key ECDH/account-reference operations in `DashPayCrypto`: justified.** `PW/wallet/identity/network/contact_requests.rs:534` constructs the sender's identity encryption-key path, and `:3659` calls agreement with the recipient's identity decryption-key path. The amended scope returns agreement/PRF products and cannot sign. Its path boundary still needs M2, and its erasure claim needs M1.
2. **BIP44 account-0 xpub in `DashPayCrypto`: justified.** `PW/wallet/identity/network/seed_binding.rs:200` reads the persisted account-0 xpub and compares it with `crypto.receiving_xpub`. Access is restricted to that exact account node; it returns public derivation material. This is necessary to prevent a wrong-seed drain from permanently registering wrong contact accounts.
3. **Per-identity top-up account xpub in `PlatformFunding`: justified.** `PW/wallet/asset_lock/build.rs:600` requires it for externally signable wallets. The allowed node is the exact top-up account, not an identity-auth or wallet-master xpub. The cap remains metadata on a digest signer, with enforcement and grant-bound caps deferred to E0-04 as explicitly documented; no new production funding flow in this diff claims to enforce it. Caps/flow leases must remain an E0-04 acceptance gate.
4. **Invitation private-key export left unsupported until X3: justified and fail-closed.** `PWF/dashpay.rs:781` implements that export upstream, but `PW/contact_requests.rs:86` explicitly provides the unsupported default for hosts that do not create invitations. This implementation keeps that default and tests refusal. Invitation creation is outside E0-03.

Other explicit differences were also checked: stricter coin/path gates than `mnemonic_resolver_core_signer.rs` are appropriate defense in depth, subject to M2; mapping `SignerError::Locked` to `PlatformWalletError::WalletLocked` instead of upstream's generic invalid-data error is a sound retry signal; the grant-less background factories are temporary epoch-bound handles rather than E0-04 flow leases and need B1 corrected before they meet the stated lock rule. The scan master export follows the pinned `manager/startup.rs:104` interface, with the documentation correction in N1.

## Validation

- `umask 022; cargo test -p dw-vault -p dw-engine` in the branch's `rust/`: **passed, exit 0**. This includes the four `e0_03_signers` integration tests and nine `platform_scopes` integration tests. The vault unit suite has one pre-existing ignored test.
- `git diff --check main...HEAD`: **passed**.
- Independent scratch tests linked against the branch's built artifacts: **3 passed**, reproducing the failures above. Source: `/tmp/dw-e0-03-gpt-review/adversarial.rs`; executable: `/tmp/dw-e0-03-gpt-review/adversarial`; final output: `/tmp/dw-e0-03-gpt-review/results.txt`. The tests assert the observed faulty behavior, so their pass status confirms reproductions, not security correctness.
- Exact-pin generator patch: `git apply --check` against a scratch copy of the pinned upstream blob: **passed**.
- I left all tracked branch files unchanged and did not edit the platform checkout. At the final status check an external untracked reviewer file, `rust/crates/dw-vault/tests/zz_review_scratch_opus.rs`, had appeared; I did not read, modify or remove it. No Swift/macOS runtime test was performed on this Linux host.

Verdict: **CHANGES-REQUIRED**
