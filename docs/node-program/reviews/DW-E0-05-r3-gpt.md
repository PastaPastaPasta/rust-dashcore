# DW E0-05 round 3 — 79aa815

Verdict: **CHANGES-REQUIRED**.

Independent, narrow review of `cfb84db..79aa815` (fixes `296a5de` and `79aa815`). Reviewed the round-2 report and actual diffs, reran its two defect reproductions with fixed-behavior assertions, and independently probed restore overlap, failure, unlock, guard cleanup, and nested error mapping. Exact revision: `79aa815f88db5a86739c513e44086fe60f7f39c8`. All work used the detached reviewer worktree `/work/scratch/e0-05-r3-gpt/repo`; the implementer's worktree was untouched. The reviewer worktree was removed after validation. This report is not committed.

## Dispositions

| Item | Status | Evidence |
|---|---|---|
| M4: uncommitted restore admitted by startup | **Original reproduction CLOSED; restore admission still needs changes** | The original WalletCreated barrier now yields `NotRun`, rather than `Starting`, when SPV starts before commit; the duplicate bundle subsequently fails and rolls back. The implementation's pending-restore regression also passes. However, overlapping restores can consume the successful restore's only bring-up signal while its wallet remains marked, leaving a committed wallet idle (M4-R3 below). |
| N1: token-wrapped SDK errors lose their stable code | **CLOSED** | The original wrapped `TimeoutReached` probe now produces `platform.timeout`. The implementation mapping test passes; independent tests also preserve wrapped proof/availability codes, wrapper fallbacks, and unknown-error diagnostic detail. No mapping cycle was found. Arbitrarily deep nesting is not stack-safe; see the low-priority limitation below. |

## Findings

### M4-R3 — Medium: overlapping restores lose bring-up for a successfully committed wallet

`rust/crates/dw-engine/src/backup.rs:591–596`, `platform/bringup.rs:376–382`, and `platform/runtime.rs:187–198`.

The reference count correctly keeps a wallet marked until **all** overlapping restores release it. But a successful restore sends its `WalletAdded` immediately after dropping **its own** guard, even if another restore still marks the wallet. Bring-up consumes that signal and returns on `admit == None`. The final guard only removes the mark; it does not retry a skipped signal. A failed overlapping restore sends no replacement signal for a wallet it did not itself register.

Concrete offline reproduction, `r3_overlapping_restore_loses_committed_wallet_bringup`:

1. Prepare valid backups R1 = `[a]`, R2 = `[b, a]`.
2. Hold R1's `WalletCreated(a)` event after registration, before commit.
3. Start R2 and hold its `WalletCreated(b)` event. R2 has already marked both `b` and `a`.
4. Start SPV. Both pending wallets are excluded; SPV reaches `Running`.
5. Release R1. It succeeds with `[a]`, but R2 still holds a mark on `a`.
6. Queue a seedless sentinel wallet's signal after R1 completes. Wait for its startup record, proving that the supervisor has processed R1's earlier signal. Wallet `a` still has no startup record.
7. Release R2. It fails with `Backup(AlreadyExists(a))`, rolls back `b`, and clears its remaining marks.
8. After completion, admission for `a` is `Some(true)`, its seed exists, and its manager registration survives, but its startup record is `None` and its task is absent. A fresh SPV stop/start immediately admits it.

Observed output: `committed a has seed: true, startup: None, tasks: []`. The defect assertions passed. This is a lost scheduling event, not a permanently leaked mark. The two restores share wallet `a`; R2's extra wallet supplies a deterministic pause before its duplicate-wallet failure.

Serialize overlapping restores, or retain deferred bring-up requests and requeue them when the final mark clears. Ensure rollback only requeues surviving committed wallets, not wallets it removed. Add this overlap regression; the existing single-restore test does not exercise the reference-counted transition to zero after another restore has already signalled.

### N2 — Low, non-blocking hardening: recursive envelopes have no depth limit

`rust/crates/dw-engine/src/platform/errors.rs:421–430, 452–455`.

Every recursive conversion descends into an owned `Box`; safe construction provides a finite tree, so no infinite cycle or self-rewrapping loop was found. There is nevertheless no explicit depth bound or iterative traversal. Two isolated synthetic probes, each constructing 128 envelopes, abort with stack overflow on the default Rust test-thread stack:

- `NoAvailableAddressesToRetry` nested around `TimeoutReached`.
- `PersisterRestore` nested around `WalletLocked`.

Both processes exited by `SIGABRT` (`-6`). The SDK retry recursion existed before these fixes; recursive persister mapping is newly added. Sixteen-level chains pass and retain their leaf codes. The persister branch also formats every inner chain before knowing whether its mapped code is recognized, adding unnecessary recursive formatting work.

This does **not** independently block approval: inspected production constructors wrap once and return, and no remotely controlled path building arbitrary nested typed errors was identified. It does prevent a claim of safety at arbitrary depth. Consider iterative envelope traversal or a defensive depth limit, including the SDK insufficient-balance promotion helper and recursive diagnostic formatting.

## Race and lifetime checks

- **Failure after marking, before seed storage:** a correctly sealed backup containing an invalid app-row value reaches `restore_one`'s validation, then fails with `Backup(Corrupt("app row value"))`. Admission afterward is `Some(false)`; no seed remains. A subsequent valid restore of the same wallet succeeds and is admitted after commit.
- **Unlock before commit:** with the valid restore held at registration in an encrypted vault, a real lock/password unlock is processed before a sentinel signal. It produces no pending-wallet startup record. Releasing the restore admits bring-up after commit. Together with the shared admission-lock inspection, this supports the single-restore unlock/commit ordering; it does not fix M4-R3.
- **Reference counts and duplicate IDs:** overlapping guards and a guard containing the same ID twice keep admission blocked until the last corresponding guard drops.
- **Panic/cancellation cleanup:** an independent guard-lifetime test catches panic unwinding and aborts/joins a task holding a guard. Both restore admission to `Some(false)`. No explicit guard leak was found in the actual restore paths. These are guard-level cleanup proofs, not assertions that all restore side effects are transactional under panic/cancellation.
- **Public-caller cancellation:** `on_runtime` spawns the actual restore and awaits its handle. Dropping the public caller detaches the spawned work rather than cancelling it; the mark lasts while that restore is still running. A genuinely stuck live restore can therefore keep admission blocked, but the tested RAII mechanism does not leave a stale mark after its owning task is dropped or unwinds.

## Error fallback assessment

`TokenOperationFailed` delegates to shared SDK conversion; discovery and persister wrappers preserve recognized inner codes and keep their own `unavailable`/`storage` codes when the inner maps only to `Internal`. SDK retry envelopes retain recognized leaf codes, otherwise `unavailable`. Insufficient-credit promotion terminates in the concrete credits variant and preserves figures. Independent unknown SDK/token checks retain `Internal { detail }`; the implementation's generic fallback remains intact. A wrapper's meaningful fallback intentionally supersedes an unknown leaf's `Internal`, as requested.

## Validation and evidence

All Cargo commands used `TMPDIR=/dev/shm/e0-05-r3` (0700), `CARGO_BUILD_JOBS=6`, and the detached worktree's own `rust/target`. No real OS secret store or public testnet fixture was used.

- **Exact, unmodified 79aa815:** `cargo test --workspace` — **564 passed, 0 failed, 2 ignored**, exit 0. Ignored: public-testnet identity fixture and OS-keyring round trip. Log: `/work/scratch/e0-05-r3-gpt/workspace.log`.
- `cargo fmt --all -- --check` and `git diff --check cfb84db..HEAD` — passed before adding reviewer probes.
- Round-2 reproductions against current rlibs, with corrected expectations — **2 passed**, 0.88 s. Source/runner/log: `closure_probe.rs`, `run_closure_probe.py`, `closure_probe.log` in `/work/scratch/e0-05-r3-gpt/`.
- Independent probes appended only to reviewer `platform/runtime_tests.rs`: `cargo test --workspace r3_ -- --nocapture --test-threads=1` — **5 passed**, 3.45 s. Source/log: `race_probe.rs`, `race_probe.log` in that scratch directory. The overlap test passes **defect assertions**, not acceptance assertions. To reproduce, create the same detached worktree, append `race_probe.rs` to `rust/crates/dw-engine/src/platform/runtime_tests.rs`, then run that command from `rust/` with the environment above.
- The initial combined 128-envelope stress test aborted; preserved as `race_probe-depth128.rs` and `depth128-combined.log`. Isolated confirmations: `depth_probe.rs`, `run_depth_probe.py`, `sdk_depth_128.log`, `persister_depth_128.log`. Both isolated probes aborted as documented above.

No independent regtest-harness run, separate CoinJoin harness run, or clippy run was performed; the workspace run includes its standard CoinJoin tests. No remaining uncertainty changes the reproduced M4-R3 finding.
