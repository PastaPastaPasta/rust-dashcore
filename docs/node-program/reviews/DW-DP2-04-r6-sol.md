# DW DP2-04 — independent Sol review, round 6

Scope: `5d736fd5..dc5dcf55` only, in a detached mode 0700 copy. The implementer's worktree was read only. No commit or push.

## Findings by severity

- **High:** possible security note: see manager.
- No other merge-blocking finding from this range.

## Round 5 closure

| Item | Judgment |
|---|---|
| F1 — failed watch-only import | Closed for the reported pre-registration failure. The failed import records the removal again while the wallet is unregistered. |
| (a) — overlapping processes | Closed for ordinary, stable database paths and supported local locks. **Not closed for all accepted path forms:** possible security note: see manager. |
| (b) — concurrent imports of one seed | Closed for one live network session per database in a process: registration, failure cleanup, and removal share the lifecycle lock. |
| (c) — readmission versus removal | Closed for one live network session per database in a process. The lock spans the readmission through registration or failure cleanup, and removal through the tombstone transaction. |
| (d) — non-UTF-8 paths | Closed: lock naming preserves path bytes, with a focused Linux test. |

## Locking audit

| Question | Judgment |
|---|---|
| Exclusive then shared gap | Safe when both opens use the same sidecar. The opening instance has performed no wallet write before acquiring its shared lock; an overlapping opener can only repeat a safe clear before either instance returns to callers. |
| Disk-backed `AppDb` without shared lock | No successful disk-backed `AppDb::open` returns without its shared lock. Whether the lock protects the *same physical database* for every accepted path is the high finding above. |
| Detached jobs after close | Their captured `Arc<AppDb>` retains the shared lock until the job finishes. Checked the direct appdb operation, event pump, journal tap and private-details paths. |
| Linux `flock` | A separate-open probe confirmed same-process shared/exclusive conflict and lifetime through a duplicated descriptor. |
| Windows | Rust standard library's Windows implementation uses overlapping `LockFileEx` ranges for shared and exclusive modes; the separate-handle order is plausible. Windows execution was unavailable here. |
| Lifecycle scope and await order | Reviewed seed/Core-file and watch-only imports, restore and rollback, removal, load/unload, startup open, close, backup scheduling, and the gate. No additional deadlock or missed registration path was found. |

## Residual judgments

| Residual | Judgment |
|---|---|
| Cross-process lifecycle interleaving relies on tombstones | Acceptable only with a stable shared lock identity; currently blocked by the high finding. |
| NFS or locks that are not per open file | Acceptable documented boundary for local SQLite WAL use; not validated here. |
| Long restore delays lifecycle operations and close | Acceptable. Its bounded input is opened before admission, and close waits for admitted operations. |
| Prior (e), skipped-insert diagnostics | Accepted diagnostic limitation. |
| DEC-210 (5), late same-wallet write after successful re-import | Manager-accepted residual, unchanged. |

## Reviewer mutations

| Mutation | Focused test | Result |
|---|---|---|
| Process-specific sidecars (distinct from implementer mutations) | Cross-process `dw-appdb` test | Killed: expected assertion failure; source restored with `cp -p`, `touch`, `cmp`. |
| Shared-read lifecycle lock (distinct from per-entry unlock mutations) | Import-during-removal `dw-engine` test | Killed: import ran during held removal; source restored with `cp -p`, `touch`, `cmp`. |

## Validation and limits

- `cargo test --locked -p dw-appdb --lib`: 83 passed, including cross-process and Linux non-UTF-8 cases.
- `cargo test --locked -p dw-engine --lib removal`: 25 passed.
- Individual failed watch-only and seed import, registered-after-error, clean reopen, overlapping restore, and gate tests: 6 passed.
- `cargo test --locked -p dw-engine --test r1_tools_offline imported_again_after`: 1 passed.
- `cargo fmt --all -- --check` and `git diff --check 5d736fd5..dc5dcf55`: passed.
- A standalone local-file alias probe reproduced the high finding. Its details are in the chat reply only.
- `cargo clippy --locked -p dw-appdb -p dw-engine -p dw-app -p dwcli --all-targets -- -D warnings`: passed.
- Both reviewer mutants were killed by the focused tests; their sources were restored byte for byte. The detached checkout is clean.
- The new tests were checked against the parent by source comparison and independent mutants, not replayed on the parent revision.
- Full gate was not rerun. Windows and NFS were not executed.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
