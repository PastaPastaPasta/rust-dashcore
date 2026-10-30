# DW E0-04.P1 review — round 2

Target: `e67cf8564ba5f0417c4cc94903f86a7261300aae`, desktop base `746c395`; fixes reviewed against round-1 target `3dc95c6`. Fresh detached worktree: `/work/scratch/e0-04-p1-r2-gpt/repo`.

**Verdict: CHANGES-REQUIRED.** The post-drop safety defect is closed. The two minor findings below leave the explicitly requested refusal contract incomplete; no unauthorized key use was demonstrated.

## Dispositions

- **High: CLOSED.** Held-key admission uses a mutex-protected `Option<Key32>`, rather than temporary strong-reference counts. Drop clears the same slot. `key_for` and `signing_seed` copy under that mutex inside the gate; signer issuance checks liveness without copying the key. Existing operation copies cannot revive the slot. Public `KeyHold` is not cloneable.
- **Medium: PARTIAL.** DEC-78's M1 exception is correctly confined to Spend/SignMessage; own-key PlatformOp/IdentityScan and every held token require their own hold. Transfers validate the complete own-key set before changing any token. Two requested refusal cases remain incomplete, below.

## Findings in changed code

### Minor — all-vault-key sets skip vault/epoch validation

`rust/crates/dw-vault/src/vault.rs:1833` returns `Ok(None)` before `token_current`. A vault-key token redeemed by another vault, or redeemed before this vault's epoch ended, is therefore accepted by `hold_key`. The new §15 promises refusal of foreign and ended-epoch tokens without limiting that rule to own-key tokens. Subsequent signer APIs still reject these tokens; this is a refusal-contract gap, not demonstrated unauthorized key access. Validate binding for every nonempty set before the no-hold return; retain `Ok(None)` for valid vault-key sets.

### Minor — "issued no signer" is checked only while a signer remains alive

`rust/crates/dw-vault/src/vault.rs:1846` tests `Arc::strong_count == 1`. Create a direct M1 signer, drop it, then call `hold_key`: the token is accepted. The new docs and requested "already used" refusal instead say tokens that issued a signer are refused. `tests/grants.rs` explicitly asserts this acceptance after `drop(direct)`. No surviving strong signer bypass is demonstrated. If historical refusal is the intended requirement, track issuance on the token; otherwise the manager must explicitly narrow the requirement to tokens with a surviving direct signer and align the docs.

## Post-drop paths checked

The external harness exercises the actual vault library: after dropping the hold, a held Spend signer clone (`public_key`, through `signing_seed`), a Platform crypto signer clone, and a scan-key clone (`master_key`, through `signing_seed`) all return `Locked`. A held Wipe token reaches `key_for` and returns `Locked`. Direct signer issuance from held Spend, PlatformOp and IdentityScan tokens returns `InvalidArgument`; issuing via a different live hold also returns `InvalidArgument`. A borrowed reference to the correct hold cannot outlive its drop in safe Rust. An external compile-negative check confirms `KeyHold::clone` is unavailable. A verbatim-source scratch harness additionally retains a strong clone of the internal HeldKey Arc and reconstructs an internal hold: both find the slot dead after the original hold drops. Neither reference-count retention nor signer/scan clones revive it.

The deterministic in-tree interleaving pauses operation A immediately after taking its key copy, drops the hold, and checks B fails while A remains paused; A then succeeds with its admitted copy. It passed, as did the expanded 16-worker/300-drop test.

Mixed-key-source, already-held, foreign own-key, stale own-key, and live-direct-signer sets fail before token mutation. Differing own keys are compared in constant time before constructing the hold or replacing any source; this private-state case was verified by source inspection, not forged through the public API. The M1 exception uses `GrantPurpose::signs()`, exactly Spend and SignMessage; own-key PlatformOp and IdentityScan cannot use it. Vault-key signers remain direct and epoch-bound, as DEC-78/§15 permits.

## Optimized erasure check

A scratch harness includes the HeldKey/KeyHold source verbatim and compiles it with rustc 1.98.1 at `-C opt-level=3`, using the locked zeroize dependency. The emitted Drop path calls Option drop glue with the original heap slot address; that glue emits 32 volatile zero stores to its payload before writing None. There is no move of the old key to a temporary in this path, so the Option::take regression is absent. This proves erasure of the original key, not that an inactive Option's payload is permanently all-zero: assigning None may write unspecified replacement payload bytes. Nor is it a proof that no previously admitted operation has its authorized copy.

The actual vault library was also compiled with `cargo rustc -p dw-vault --lib --features insecure-test-kdf --locked -- -C opt-level=3 --emit=asm`. Its exported KeyHold Drop passes the original heap Option address (`Arc allocation + 21` on this build) directly to drop glue, which emits 32 volatile zero stores at payload offsets 1–32. The old key is not read or moved into the stack replacement. This independently confirms the same erasure path in the real optimized library. Cargo reports the dev profile name, but the explicit `-C opt-level=3` sets optimization for dw-vault. This is an optimized library check, not a full release-profile workspace build.

Artifacts: `/work/scratch/e0-04-p1-r2-gpt/actual-drop-assembly.txt`, `repo/rust/target/debug/deps/dw_vault-6baa01e0b56c6297.s`, `held-exact.rs`, `erase-check.rs`, `erase-check.s`.

## Validation

- `git diff --check 746c395..HEAD`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo test -p dw-vault --locked`: **137 passed, 0 failed, 1 ignored** (OS-keyring test). Both targeted hold-drop race tests passed. Log: `/work/scratch/e0-04-p1-r2-gpt/test.log`.
- External public-API harness: **3 passed**, covering post-drop paths and reproducing both refusal gaps. Reproducer assertions intentionally describe actual behavior; passing them confirms the gaps rather than proving contract compliance. Source/log: `/work/scratch/e0-04-p1-r2-gpt/closure-check.rs`, `closure.log`.
- KeyHold clone compile-negative check: expected E0599; `clone-fail.log`.
- Actual optimized vault library build and Drop assembly inspection: passed; `optimized-build.log`, `actual-drop-assembly.txt`.
- Full workspace tests/clippy and the implementer's reported broader 552-test gate were not independently rerun; this was a narrow closure review.

The scratch harness initially needed dependency/toolchain alignment and a debug library with the test-KDF feature; those setup errors were corrected before the reported successful run. An initial attempt to assert every inactive Option payload byte remains zero was discarded: None may overwrite erased bytes with unspecified replacement payload data. The erasure conclusion uses optimized assembly showing volatile writes at the original location, not reads of inactive storage.

Read-only review: no source changes, commits, pushes, or GitHub actions; the reviewed worktree is clean. Builds used `TMPDIR=/dev/shm/e0-04-p1-r2-gpt-tmp` (0700), at most 8 jobs, and worktree-local targets. Only the requested report and scratch artifacts were written; the review TMPDIR was removed after validation.

**Final verdict: CHANGES-REQUIRED.** High CLOSED; Medium PARTIAL because its requested refusal contract is incomplete. Both remaining findings are minor contract/conformance issues, not demonstrated key-access vulnerabilities.
