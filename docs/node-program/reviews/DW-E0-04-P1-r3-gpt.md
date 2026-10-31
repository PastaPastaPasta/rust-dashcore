# DW E0-04.P1 review — round 3

**Verdict: APPROVE.** Both remaining round-2 gaps are CLOSED. No new findings in this narrow confirmation review.

Target: `2ee635b0b83a24c5d791dab140695abb049af3ef` (`dw/e0-04-p1-vault`), exactly one commit above `e67cf8564ba5f0417c4cc94903f86a7261300aae`. Reviewed the five-file delta, its documentation, and the relevant issuance/redemption/hold paths. The report was created before investigation, as requested.

## Closure evidence

1. **Foreign/stale vault-key tokens: CLOSED.** `hold_key` invokes `token_current` for every supplied token before counting key sources or returning `Ok(None)`. The saved round-2 public-API reproduction now expects and obtains `GrantInvalid` for a foreign vault-key token and `Locked` for an ended-epoch vault-key token. The new in-tree test additionally covers valid all-vault-key sets, foreign/stale members in sets, and stale tokens after re-unlock.
2. **Previously issued signer: CLOSED.** Successful grant-based signer issuance sets the token's private `AtomicBool` to true; no path clears it. `hold_key` refuses an issued own-key token even after every signer is dropped. The saved reproduction now verifies refusal both before and after dropping the direct signer. The new in-tree test covers Spend and SignMessage, alone and in a set.

## Mark bypass and regression checks

- **Cloning:** an external compile-negative check gets E0277 because `GrantToken: Clone` is not implemented. Its fields are private outside the crate and it has no public constructor or deserializer. Cloning a signer cannot change its token's mark: the external runtime test checks refusal with the clone alive and after dropping it.
- **Re-issuing:** issuing another signer from the same token succeeds without resetting its mark; a subsequent hold still fails. Redeeming the same grant ID again gives `GrantInvalid`: `redeem_grant` removes the grant before constructing its sole token. Cloning the public AuthGrant descriptor does not recreate that entry. A new authorization can legitimately create a new token, but cannot reset the used token or transfer the old signer's key into a hold.
- **Sets:** the external harness places an issued token first and last alongside a fresh token. Both calls fail without changing either key source. Removing the used token leaves the fresh token holdable; the used token remains refused. All mark/key/source checks precede mutation.
- **Issuance coverage/concurrency:** Spend/SignMessage, Platform, and scan-key grant paths all reach `token_signer`. The mark is set only after successful signer construction and before return. `hold_key` requires exclusive `&mut` access to the tokens, while issuance borrows `&GrantToken`; safe Rust cannot run both against the same token concurrently. Relaxed atomics suffice for this boolean because ownership/borrowing synchronization orders access, and issuance only ever writes true.
- **Vault-key semantics:** a valid all-vault-key set still returns `Ok(None)` and does not transfer any key. Historical issuance refusal governs admission into an actual own-key hold; vault-key signers remain direct and epoch-bound.
- **Earlier High stays CLOSED:** the round-2 public-API post-drop test still passes unchanged. Spend signer clones, Platform crypto signer clones, scan-key clones, and the held Wipe token refuse after the hold drops; direct issuance and a different hold cannot bypass the hold binding. Both in-tree hold-drop race tests pass. The HeldKey/drop/erasure implementation was untouched by this commit; optimized assembly inspection from round 2 was not repeated.

The earlier mixed-source fixture now reports `Locked` for its stale vault-key member before inspecting the mixture, consistent with the requested binding-first error precedence. The complete vault suite found no regressions.

## Validation

All new execution used `TMPDIR=/dev/shm/e0-04-p1-r3-gpt-tmp` (0700) and at most 8 build jobs. Source was exported with `git archive 2ee635b` into `/work/scratch/e0-04-p1-r3-gpt/repo`; the round-2 build cache was copied into its separate local target directory, not shared. Cargo rebuilt dw-vault from the reviewed source.

- Saved round-2 executable against its original library: **3 passed**, confirming the original acceptance gaps remain reproducible on the old target (`baseline-reproductions.log`).
- Round-2 external harness rebuilt against round 3, changing only the two gap assertions to require refusal, plus the extra bypass test: **4 passed** (`closure-check.rs`, `closure.log`, `run-closure.py`).
- External GrantToken clone compile-negative test: expected **E0277** (`token-clone-fail.rs`, `token-clone-fail.log`).
- `cargo test -p dw-vault --locked`: **139 passed, 0 failed, 1 ignored** (opt-in OS-keyring test), including both new tests and both hold-drop race tests (`test.log`).
- `cargo fmt --all -- --check`: passed (`fmt.log`).
- `git diff --check e67cf85..2ee635b` and `git diff --check 746c395..2ee635b`: passed.
- `cargo clippy -p dw-vault --all-targets --locked -- -D warnings`: passed (`clippy.log`).

Logs and scratch harnesses: `/work/scratch/e0-04-p1-r3-gpt/`. Full workspace tests, desktop builds, and release assembly were outside this tiny closure check. No source edits, commits, pushes, or GitHub actions; the existing feature worktree remained clean.

**Final verdict: APPROVE. High CLOSED; Medium CLOSED.**
