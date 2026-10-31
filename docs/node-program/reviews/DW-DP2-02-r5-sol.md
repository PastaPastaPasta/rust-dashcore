# DW DP2-02 — Sol review, round 5

Reviewed `42bace6..60dc558` in a detached scratch worktree. All 23 commits have good signatures. The implementer's worktree, branch and GitHub state were untouched.

## Finding

1. possible security note: see manager. Locations: `rust/crates/dw-engine/src/platform/signers.rs:119-124`, `rust/crates/dw-engine/src/platform/requests.rs:726-732`.

## Port checks

| Check | Result | Evidence |
|---|---|---|
| Send, accept and key enablement take a Credits quote under a call permit | Pass. Each calls `write_permit`, then uses `permit.identity_signer` narrowed by `flow_keys`. `Lease::charge_credits` has no remaining engine call or definition. Ignore and unignore are local writes without a grant. | `requests.rs:393-424,543-573,606-624,725-744`; `rg` of the three requested names. |
| Allow-list and signature accounting | Pass for the current transition call sites. `allowed` precedes `count.begin()`, which precedes the vault's signing ask. A refused key releases nothing. A signature released before an error makes the call `MaybeSent` and keeps its quote. | `signers.rs:151-202`; `lease/call.rs:339-360,868-926`; refund test, M1 and M2. |
| Refusals after permit acquisition | Pass. `derive_keys`, `flow_signer`, a revoked or ended lease, and scan-proof refusal exit through `?`; `CallPermit::Drop` ends them with `finish(false)` semantics. On those error returns, no transition signature means `Cancelled` and refund. | `requests.rs:417-424,561-572,725-743`; `lease/call.rs:955-960`; scratch early-`?` probe. |
| Reaper, Lock and removal drain | Pass. The permit lives through each awaited library write. Its call id blocks the idle reaper and appears in Lock's and wallet removal's drain snapshot until finish or deadline H3. Read-back occurs after the library write. | `requests.rs:565-573,620-624,737-744`; `lease/table.rs:176-185`; `lease/lock.rs:75-105,231-280`; reaper and M5 probes. |
| m4 numbering and language | Pass at this head: P2b item 39, DP2-02 item 40, Contract-Version 12. The later item 41 / version 15 rebase is outside this review. | `docs/contracts/m4-dashpay-engine.md:3,1780-1829`; m4 contract test. |

## Resolution findings

| Conflict area | Finding |
|---|---|
| `names.rs` | P2b's `NameAuthority::{Grant,Lease}` and Credits lease branch remain; DP2-02's shared visibility and `PlatformError` return are present. The conversion at the name call site preserves the name API. Names tests: 39 passed. |
| `signers.rs` | Both `count` and `keys` fields remain. `can_sign_with` checks supported, allowed, open count and the vault key match; `sign` checks allowed before `count.begin()`. The targeted signer test and M1 assertion passed/killed respectively. |
| `lease/mod.rs`, `lease/session.rs` | P2b's journal, step and funding errors and DP2-02's `KeysNotHeld`/vault mapping are all present. Key-checked `begin_lease_holding` and ordinary rebind coexist. |
| r1–r4 regressions | Request suite 20 passed, 1 public-testnet fixture ignored. Both vault removal races passed; store protection and raw-seed re-store passed. The four replayed conflict commits match the supplied range-diff's described changes; no dropped behavior found. |

## Note rulings

| Note | Ruling |
|---|---|
| (a) Raw signer from a counted signer | possible security note: see manager. Locations: `signers.rs:119-124`, `requests.rs:726-732`. |
| (b) Successful adoption without signing | Accepted under P2b's `finish(ok)` rule: `Ok` reads `Sent`, so the quote remains spent from the lease budget even when no Platform credits were consumed. This can be clarified in item 40's one-quote wording. |
| (d) Early `?` after permit | The `Drop` path closes the count with `ok=false` and refunds if nothing signed. A scratch-only early-`?` probe passed. A committed flow-specific regression test would improve coverage but is not needed to establish this behavior. |

## Mutations

Each production mutation was applied alone in the detached worktree, then restored from a saved copy and touched. All failures below are assertions rather than build failures.

| Mutant | Focused check | Result |
|---|---|---|
| Sign before allow-list check | `each_flow_signs_with_its_own_keys_only` with a scratch assertion on the permit's release count | **Killed:** an excluded key released one signature; expected zero. |
| `finish(true)` on an unsigned send failure | `a_quote_goes_back_only_when_nothing_was_signed` | **Killed:** 150,000,000 spent; expected zero. |
| Skip `drop(signer)` before send's `finish` | `a_quote_goes_back_only_when_nothing_was_signed` | **Survived, equivalent:** the awaited library call has ended and `finish` closes the shared count; keeping this local value alive cannot release another counted signature. |
| Remove key enablement's permit and sign under the lease | `a_quote_goes_back_only_when_nothing_was_signed` | **Killed:** signed update spent zero of the 100,000,000 quote. |
| Drain ignores live call permits | scratch `r5_lock_waits_for_a_live_call_permit` | **Killed:** Lock returned with a live permit. |

## Validation notes

- Exact-head request suite: **20 passed, 1 ignored**. Vault removal races: **2 passed**. Store protection and raw-seed re-store: **1 passed**. Names: **39 passed**. m4 contract: **9 passed**. Reaper, drain and early-`?` scratch probes passed before mutation.
- `cargo fmt --all -- --check`, `git diff --check 42bace6..60dc558`, all commit-signature checks, and final detached-tree cleanliness passed.
- The implementer's reported 1,155/0 full gate and R1–R6 refund mutants were not independently rerun. The public testnet T2 fixture was not run.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: yes
