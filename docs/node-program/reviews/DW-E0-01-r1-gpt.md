# DW E0-01 — independent code review, round 1

**Verdict: APPROVE**

No blocker, major, or other actionable correctness finding in the reviewed diff.

## Scope and revisions

- Worktree: `/work/home/agent/workspace/dw-wt/e0-01-pin-bump`
- Branch: `dw/e0-01-pin-bump`
- Reviewed `git diff main...HEAD`, with `main` at `8be2069fd2fc238d5a7f0e03feaba287b0131bab` and HEAD at `3b98e6fb572a2f2c856ffb2d2167a7cfd5bcf60e`.
- Two commits: `b407dbb949d141ceb99cdc719839f911725ec388` (dependency pins) and `3b98e6fb572a2f2c856ffb2d2167a7cfd5bcf60e` (typed insufficient-credit mapping).
- Read ROADMAP E0-01, DASHPAY §3.6 and §3.9, and DESIGN R3.
- Inspected `git log bc321362b9..bc41f1bc23` and relevant upstream diffs using explicit revisions in `/work/home/agent/workspace/platform`. That checkout is currently on `6499c680c6`; it was not changed or treated as the reviewed pin.
- No branch edits, staging, commits, or pushes. Review output and scratch evidence are outside the worktree.

## Findings

None requiring changes.

### 1. Pins and dependency graph: consistent

Checked the workspace manifest and every crate manifest, including the independently declared dependencies in `dw-compat`, `dw-message` (including its dev dependency), and `dw-uri`.

| Dependency family | Resolved revision | Lockfile packages | Multiple revisions or duplicate package names within family |
|---|---|---:|---|
| dashpay/platform | `bc41f1bc233dec4607d387101c1d9c2f111019b2` | 31 | None |
| dashpay/rust-dashcore | `40268cc0402a8933ec539f16b2d634c4e25876ad` | 11 | None |
| dashpay/grovedb | `9791d277207998b7789c85b0745e99d37040a12e` | 12 | None |

No manifest retains `bc321362b9`, `e4208c90`, or the old GroveDB revision `dce8252f`. The rust-dashcore and GroveDB sources match platform's `Cargo.lock` at the new pin exactly. GroveDB follows through platform's dependencies; no additional local override is needed.

`cargo metadata --locked --format-version 1` succeeded and confirmed the resolved sources. `cargo tree -d` showed no unexpected family duplication. Existing registry version duplication and same-version host/build-feature entries remain; the lockfile's complete duplicate-name/version groups are identical to `main`. Comparing the lockfiles also showed no package additions/removals outside the three pin families and no dependency-list changes for packages whose name/version stayed the same. The platform package version strings move to `5.0.0-beta.2`.

### 2. Upstream API, behavior, and persisted formats: compatible with this engine

The relevant upstream changes are:

- **#5206:** withdrawals and transfers to addresses promote a structured identity-balance refusal to `PlatformWalletError::InsufficientIdentityCredits`, retaining the required and available credits. Other operation failures retain their existing fallback mapping; signer-key-unavailable preservation remains in place. The local conversion now consumes this correctly.
- **#4978:** DPNS fetches reconcile names through one persisted snapshot rather than emitting an intermediate snapshot per name. Existing labels retain their acquisition timestamps. Wallet-owned identities merge fetched names so the marketplace sweep can classify departures; watched identities may replace their list after a complete fetch. Partial fetches only merge. Marketplace departure detection also consults existing owned rows. These are behavioral fixes using the existing identity/DPNS persistence shapes.
- **#5294:** Drive/SDK verification now refuses GroveDB V0 proof envelopes at every supported protocol version. Upstream documents that V1 is emitted from Grove version 3 / Platform protocol version 12, so this policy includes the live PV13 networks. Older V0 proof fixtures or legacy servers are deliberately rejected. Local code does not call the changed proof-helper signatures or construct the changed upstream proof-error variant.
- **#5305 / GroveDB bump:** proof generation and verification fixes include hiding a limit-cut range bound, proving paths through empty provable trees, and rejecting overlong path segments. Additional canonical storage-flag checks and versioned indexed-tree behavior are in that transitive range. The desktop's wallet database is SQLite, not a persisted GroveDB database, so those GroveDB internal changes do not require a wallet migration here.
- Other changes add or tighten PV14 document constraints, duplicate-key validation, withdrawal limits, masternode handling, shielding validation, and group/token proof behavior. These operate through the upstream protocol/version machinery. The SDK's group-burn result retains its existing optional-balance type; `None` means no proposer balance was supplied. No local engine caller needs adaptation to the changed internal helpers.
- The rust-dashcore range contains five commits, all restricted to `rpc-json/src/lib.rs`: Core v24 nested platform addresses, service/port handling, shared/multi-payout masternodes, partial address diffs, and IPv6 fallback formatting. Core/key-wallet/SPV persisted types do not change in this range.

**No persisted-wallet-format change was found.** This is supported by more than an empty storage schema diff:

- The entire `packages/rs-platform-wallet-storage` tree is identical at both pins: tree object `07642a133a583f56e7c7ac63ba5d300c64b78b58`. This includes migrations, SQLite codecs, secret storage, and rehydration code.
- The entire `packages/rs-platform-wallet/src/changeset` tree is identical: `b5c882d66fe25266e39a87ff9b8f87dafbee3a12`. Persisted `IdentityEntry` and the other changeset record layouts are unchanged.
- `ManagedIdentity` gains only a `DpnsFetch` re-export in its type-definition module; no fields change. `DpnsNameInfo` is unchanged. The new `DpnsFetch` enum has no serialization derive and is fetch-completeness metadata, not a stored field.
- Manager startup and signer code have no diff. The upstream DPP identity/public-key types used by the wallet storage have no diff, and rust-dashcore's changes are confined to RPC JSON.

Consequently there is no missing migration or pre-migration backup requirement introduced by these commits. DPP's newly parsed PV14 document-type fields are not a change to the persisted wallet records examined here.

### 3. Error mapping: complete for the requested forms

At `rust/crates/dw-engine/src/error.rs:220` and `:255`, all requested paths retain the structured data:

| Input form | Mapping |
|---|---|
| Typed `PlatformWalletError::InsufficientIdentityCredits` | Base58 identity ID, `required` → `needed`, and unchanged `available` |
| Balance refusal inside `PlatformWalletError::Sdk` or `TokenOperationFailed` | Upstream typed promotion, then the same conversion |
| Direct `dash_sdk::Error` | Upstream typed promotion, then the same conversion |

The upstream promotion helper recognizes CheckTx `Protocol(ConsensusError)` and wait-stream `StateTransitionBroadcastError` causes, and recursively unwraps `NoAvailableAddressesToRetry`. It matches the actual `IdentityInsufficientBalanceError` variant, not an error message or numeric code alone. Its `required_balance()` and `balance()` accessors are used in the correct direction.

Non-balance errors return `None` from promotion. Direct SDK errors still become `EngineError::Sdk`; wrapped wallet/token errors still follow the pre-existing wallet fallback, preserving their diagnostic text. No non-balance error is converted to a balance refusal or swallowed. The recursion terminates at the typed platform-wallet variant.

The four new tests passed. They cover the typed refusal, both direct SDK consensus shapes, both wallet wrapper variants, correct Base58 identity and amounts, and generic SDK/wallet fallbacks. A retry-envelope and non-balance token-wrapper test would be a useful optional coverage extension, but the current matcher and fallback control flow are correct; this is not a change request.

### 4. FFI and bindings: interface unchanged

There are source changes in the FFI conversion code, so “unchanged” should mean the exported interface and bindings:

- `rust/crates/dw-ffi/src/api/error.rs:77` maps the new internal error to the existing M0 `Sdk { detail }` variant.
- `rust/crates/dw-ffi/src/api/wallet.rs:219` includes it in the existing wallet-domain internal-error fallback.
- No exported enum variant, record, method signature, or UniFFI definition changes. No generated binding file changes in the diff.
- Rebuilt `dw-ffi`, regenerated Swift bindings into scratch, and compared them with `Sources/DashWalletCore/Generated/DashWalletCore.swift`: **byte-for-byte identical**. Both SHA-256 hashes are `1fbaeb19c359eb9e7edc62961f0407446fc9efa76609d870c52ec6074e7c5d53`.

The current M0/wallet calls do not perform identity-funded Platform operations. Exposing the eventual `platform.insufficient_credits{needed, available}` domain error belongs to the E0-08 facade contract; the internal typed error here prepares that work without expanding the existing binding API.

### 5. Documentation: matches the scoped change

DESIGN R3 and HANDOFF now name the new platform/rust-dashcore pins; DESIGN also names GroveDB and explains that the Mac reference checkout may still be at the old pin. The pin date matches the upstream commit date. DASHPAY's old-pin references are explicitly tied to its research baseline at repository revision `05161d9`, while §3.9 specifies the intended bump. They do not silently leave a build dependency on the old revision. ROADMAP's acceptance checks remain intact.

## Validation performed

All scratch output is under `/work/scratch/e0-01-review/`; the existing worktree's own `rust/target` was used.

| Check | Result / evidence |
|---|---|
| `cd rust && CARGO_BUILD_JOBS=12 cargo test --workspace` | Exit 0; **436 passed, 0 failed, 1 ignored** across unit/integration/doc-test results. The ignored test is the existing opt-in OS keyring round-trip. Log: `cargo-test.log`. |
| `cargo tree -d` | Exit 0; reviewed existing duplicates. Output: `cargo-tree-d.txt`. |
| `cargo metadata --locked --format-version 1` | Exit 0; checked resolved revisions. Output: `metadata.json`. |
| `cargo fmt --all -- --check` | Exit 0. |
| `git diff --check main...HEAD` | Exit 0. |
| `cargo build -p dw-ffi` | Exit 0 in dev profile. Log: `ffi-build.log`. |
| `cargo run -q -p uniffi-bindgen -- generate --library target/debug/libdashwallet_core.so --language swift --out-dir /work/scratch/e0-01-review/bindings` | Exit 0; Swift comparison succeeded. `swift-format` is absent, so the generator warned that auto-formatting was unavailable; the generated file nevertheless matches exactly. |
| Worktree status after validation | Clean; no tracked or untracked branch changes. |

## Limits and remaining milestone checks

This review does not claim completion of every E0-01 acceptance gate. I did not rerun Linux Swift tests, the complete `build-core.sh --check-bindings` packaging script, or the `l1-sync`, `l1-send`, `l2-tools`, `restore`, and completed CoinJoin regtest suites. The direct FFI rebuild and scratch generation above verify the binding-compatibility claim without rewriting the artifact bundle. `regtest/RESULTS.md` contains D1-era completed agentbox runs; those records alone are not evidence of a new-pin regression run.

The task owner should attach the remaining E0-01 regression evidence before marking the roadmap row complete. No blocker or major was identified in the code reviewed here.
