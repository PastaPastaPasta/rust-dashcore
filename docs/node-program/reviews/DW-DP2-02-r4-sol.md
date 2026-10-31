# DW DP2-02 — Sol review, round 4

Reviewed `8bca12e..bdb18a9` in a detached scratch worktree against the stated desktop base. All four commits have good signatures. The implementer's worktree and branch were untouched.

## Findings

1. **Medium — possible security note: see manager.** Locations: `rust/crates/dw-vault/src/vault.rs:1789-1811`, `docs/contracts/m4-dashpay-engine.md:1817-1823`.

## Closure table

| Question | Disposition | Evidence |
|---|---|---|
| Round-3 paused signer versus wipe, delete and recovery | **Closed for these three operations.** A signer paused after reading its seed released before each removal completed; a later signer was refused. | Exact-head `a_removal_waits_for_the_signers_that_read_the_seed` passed, including three extra runs under load. M13–M15 and an independent shared-gate mutant were killed by assertions. |
| Drain and epoch semantics | **Confirmed.** Wipe and delete retain the epoch; recovery replaces it. A wallet-B signer must finish before a wallet-A removal enters its exclusive section, and can be used afterward if its authorization is still live. | `vault.rs:1817-1856,2528-2545`; the shared operation gate is vault-wide. |
| Round-2/3 key-checked redemption under `writer()` | **Still closed.** Removal before the check leaves the grant unused; removal after the check waits for redemption. A concurrent Lock ends the grant. | `redeem_grant_holding_key` at `vault.rs:1438-1462`; `a_removal_waits_for_a_key_checked_redemption` and the focused send, accept and key-upgrade request tests passed. |
| m4 §7 item 39 and the manager-accepted residual | **Possible security note: see manager.** | Locations: `docs/contracts/m4-dashpay-engine.md:1817-1832`, `rust/crates/dw-vault/src/vault.rs:1789-1811`. |

## Lock-order table

| Paths | Order and cycle check |
|---|---|
| `create`, `unlock`, `recover_with_mnemonic`, `destroy` | `writer → ops exclusive → inner`; recovery takes the exclusive gate before the old file is replaced. `create` and `destroy` have no populated live records at their file transition. |
| `wipe_wallet_secret`, `delete_wallet_secret` | `writer → ops exclusive → inner` through `drain_ops` and `delete_records`. `token_key` reads only a token-held or in-memory key inside the exclusive gate; delete loads its OS key before `writer`. |
| `encrypt`, `apply_passphrase_change` | `writer → ops shared → inner`, then, after the shared hold ends, `writer → ops exclusive → inner` for epoch change; passphrase change writes unchanged records before the exclusive hold. |
| `enroll_quick_unlock`, `set_quick_unlock_spend_limit` | `writer → ops shared → inner` for token-authorized metadata writes; no nested gated operation. |
| `check_passphrase_change`, passphrase and quick-unlock grant issue, throttle updates, `remove_quick_unlock`, ordinary record commits | `writer → inner`; OS-store work is outside an exclusive gate. |
| `redeem_grant_holding_key` | `writer → inner` (and OS-store load if needed), then `writer → ops shared → inner` for candidate derivation, then grant redemption under `inner`. Its gated body does not take `writer`. A removal waits on `writer`; Lock never needs `writer`. |
| `lock` | `ops exclusive → inner`, never `writer`; there is no inverse wait edge against removal or redemption. |
| Signer calls; `grant_identity_key_matches`; gated reveal, export, own-backup open, `seed_derivation`, `core_mnemonic_check` | `ops shared → inner`; any needed OS-store load precedes the gate. No gated body takes `writer` or enters another gated operation. |
| `store_wallet_secret` | **Possible security note: see manager.** Location: `rust/crates/dw-vault/src/vault.rs:1789-1811`. |
| Test gate logging | `GateEvent`, `gate_logged` and the `DrainWaiting` recording are `#[cfg(test)]`; no hook remains in production. |

## Coverage table

| Seed or file path | Review result |
|---|---|
| `delete_records` via wipe and delete | Only explicit live-record deletion helper; both callers hold the exclusive drain through the file write and read-back. |
| `recover_with_mnemonic` | Only replacement of a populated live vault; drains before `keep_replaced_copy` and `file::write`, then changes the epoch. The retained old file is explicitly disclosed in m4. |
| `destroy`, `create` | Destroy refuses any record; create refuses an existing vault. Neither deletes a populated live seed. |
| Passphrase change, encrypt, quick-unlock metadata, throttle writes | Rewrap or edit metadata while preserving seed records; no seed replacement. Their epoch and gate behavior is listed above. |
| Backup restore | Engine validates bundle wallet identity and imports through `import_secret_inner`; that path derives the wallet ID from the seed and skips storage if the live vault already has it. `open_backup_bundle` is a read. |
| `store_wallet_secret` | **Possible security note: see manager.** Location: `rust/crates/dw-vault/src/vault.rs:1789-1811`. |

## Mutation table

Each mutant was applied alone in the detached worktree. The source was restored and touched after each run. Every failure below was a test assertion, not a compile error.

| Mutant | Result in `a_removal_waits_for_the_signers_that_read_the_seed` |
|---|---|
| M13: wipe skips `drain_ops` | **Killed:** wipe did not reach `DrainWaiting`. |
| M14: delete skips `drain_ops` | **Killed:** delete did not reach `DrainWaiting`. |
| M15: recovery drains after file replacement | **Killed:** recovery changed the file while the signer was paused. |
| Own: `drain_ops` takes a shared hold | **Killed:** wipe changed the file while the signer was paused. |

## Validation notes

- Exact-head vault unit suite: **78 passed, 1 ignored** (opt-in keyring); `platform_scopes`: **12 passed**. Exact-head request suite: **19 passed, 1 ignored** (public-testnet fixture). The round-3 key-checked redemption race passed.
- The committed removal interleaving passed once on the exact head and three more times under load. Scratch-only probes ran and were removed; their security-relevant result is in the manager note above.
- A standalone `dw-vault` test initially hit the workspace's DPP feature-unification issue; selecting `dw-engine` fixed the exact-head command. A temporary scratch-only dev dependency was used for fast mutation runs and then restored with `Cargo.lock`.
- `cargo fmt --all -- --check`, `git diff --check 8bca12e..bdb18a9`, final scratch source cleanliness, and four commit-signature checks passed. The implementer's reported full gate was not independently rerun.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
