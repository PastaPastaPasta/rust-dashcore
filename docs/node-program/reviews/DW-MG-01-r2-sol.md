# MG-01 Sol r2 delta review — 072d43b6

Scope: the first-parent, non-merge commits after `cbbcea1e`, with the three main-branch merges and `Cargo.lock` excluded. I reviewed the r1 fixes against the prior findings, MG-01 D21–D24, the vault gate, and Dash Core's unknown-parent vote path. The requested branch was clean at `072d43b6`; I did not edit it.

## Findings

No actionable finding in this delta. The two major and one minor r1 findings are resolved.

1. **Operator signer (r1 Major 1): resolved.** The deterministic signer is now in `dw-coinjoin/tests/support/operator_key.rs`, included from the library only under `#[cfg(test)]`; `dw-coinjoin` has no features and Cargo metadata lists `dash-pkc` only as a dev-dependency. Another crate's dev-dependency can unify dependency features, but it cannot enable `cfg(test)` in a production library build or make `dash_pkc` a direct dependency of `dw-coinjoin`. The guard blanks only the exact test modules in `bls.rs` and `queue.rs`, the exact gated include in `lib.rs`, and two test files. Production code appended after either module remains scanned. The `clippy.toml` rules cover the named BLS secret types and signing methods through aliases and re-exports. I found no current production path to the test signer or another BLS secret signer.

2. **Vault lock race (r1 Major 2): resolved.** `store_masternode_key` and `delete_masternode_keys` now take `writer`, drain the operation gate, and then re-read the full-scope key and starting epoch before computing IDs or changing records. A lock that wins the gate makes an encrypted-vault change fail `Locked`; a lock that loses waits for the change to finish. `masternode_key_roles` runs through `with_loaded_key`, including its gated epoch check and release. For an unencrypted vault, `lock()` clears only the cached key, and the documented retry can load the OS key in a new epoch. The synchronous `checked`-hook test forces the old vulnerable ordering for store, delete, and roles without sleeps and would fail with the old implementation. The separate lock-wait test starts its 200 ms observation only after `LockCalled`; that bounded observation is scheduling-sensitive, but the gate ordering and the synchronous refusal test provide the substantive proof.

3. **Orphan votes (r1 Minor 3): resolved.** Before queueing an unknown-parent funding vote, `add_vote` now requires a known voting key, a time no more than one hour ahead, and a matching recovered signature. `has_vote` includes queued hashes, so repeats do not consume more entries. On object arrival it removes the orphan hash and applies the vote through the normal path again. This matches the relevant checks in Dash Core `governance.cpp:820–835` and `vote.cpp:211–250` for the funding votes this store handles. Core also accepts operator-signed votes for other object types; MG-01 intentionally does not tally those. Expiry, eviction and the per-peer bound remain the documented MG-02 work.

## Evidence and limits

- I compiled `no_operator_signing.rs` directly as its standalone Rust test binary in an isolated detached worktree: all five tests passed. Seven isolated mutations were caught: an appended helper, a re-export, a `use` alias, a feature-gated re-export, a second `path` attribute, a normal `dash-pkc` dependency, and a rustfmt skip. The scratch worktree and binary were removed.
- Cargo metadata reports 20 workspace members, no `dw-coinjoin` features, and one `dw-coinjoin` `dash-pkc` dependency of kind `dev`. The source layout and Rust's `cfg(test)` rules establish the production-build boundary; I did not rebuild the whole wallet.
- I relied on the owner's reported final-head gate (Rust 2010 passed; two wall-clock failures in main's code passed alone; Vitest timeout cases passed alone; Playwright 910/910) for broader build and runtime coverage. I did not rerun the vault or governance Cargo suites under the reported high machine load.

VERDICT: ACCEPT
