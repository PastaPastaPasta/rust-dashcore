# Review brief: dashwallet-desktop E0-03 (vault signer glue). Security-sensitive.

Repo: dashwallet-desktop. Branch `dw/e0-03-signer-glue`, worktree /work/home/agent/workspace/dw-wt/e0-03-signer-glue. Diff: `git diff main...HEAD` (3 commits: 5577aee, d144cdc, 58f8374). Read CLAUDE.md, docs/design/DASHPAY.md §2.6/§3.3 (as amended by this branch), docs/design/ROADMAP.md row E0-03, and docs/contracts/m1-engine.md §2.2.

What it does: the scopes `PlatformIdentity`, `DashPayCrypto` and `PlatformFunding{max_duffs}` in dw-vault; identity signing and DIP-15 crypto (ECDH, account reference, contactInfo seal/open, auto-accept key) running inside dw-vault so derived keys never leave it; `VaultIdentitySigner`, `VaultContactCrypto` and `VaultScanKey` in dw-engine implementing platform-wallet's traits. Counterparts in the platform checkout (/work/home/agent/workspace/platform, pinned rev bc321362b9; read via `git -C ... show bc321362b9:<path>`): packages/rs-platform-wallet (the PWF/dashpay.rs that `mnemonic_resolver_core_signer.rs`, `contact_requests.rs`, `seed_binding.rs` and `sign_with_mnemonic_resolver.rs` belong to).

Check adversarially:
1. Scope bypass: can any caller get a key, signature or ECDH result for a path outside its scope? (path parsing and normalisation, hardened/unhardened confusion, 256-bit DIP-14 indices, coin-type and network confusion, prefix and extension tricks, integer overflow in indices). Try to break `paths.rs` with crafted paths in a scratch test (don't commit).
2. Secret handling: zeroization of seeds, xprivs, ECDH shared secrets and intermediate buffers; secrets in Debug/Display/logs/errors; copies via clone; lifetimes across await points.
3. Lock semantics: a lock revokes every outstanding signer immediately; there is no race where a signer created before the lock can still sign after it.
4. Byte-equivalence with the library's provider: are the fixture vectors genuinely produced by the library (inspect the generator patch and recipe), and do they cover every operation?
5. The four documented deviations: is each one justified and safe?
6. Whether the grant-less entry points (`scan_key`, `dashpay_crypto_signer`) can be reached from dw-ffi or Swift.
Run `cargo test -p dw-vault -p dw-engine` (use `umask 022` until the agentbox-setup branch merges).

Write the review to the path in your task: findings with severity (blocker/major/minor/nit), file:line, failure scenario, fix. End with a verdict: APPROVE / APPROVE-WITH-NITS / CHANGES-REQUIRED. Don't modify the branch and don't commit.
