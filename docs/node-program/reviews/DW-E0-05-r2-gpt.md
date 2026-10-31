# DW E0-05 round 2 — cfb84db

Verdict: **CHANGES-REQUIRED**.

Narrow closure check of M1–M5 and the new facade error mapping. Reviewed `d53877a..cfb84db`, the round-one report/probe, the supplied before/after logs, m1 §2.1a and m4 §6 (contract v5). Detached HEAD `cfb84db17379f1c426393cf7a4f0d69a3238d65a`, based on `746c395ed08f3783d3877061136c179973718761`, in `/work/scratch/e0-05-r2-gpt/repo`. Source was not modified; no testnet fixture was used. This report was written provisionally before validation, then finalized.

## Dispositions

| Finding | Disposition | Closure evidence |
|---|---|---|
| M1 loop drains | **CLOSED** | `stop_platform` collects undrained loop names and key-work failure; `stop_spv` stops SPV and returns `EngineError::Sdk` naming unfinished work. Restart drains/retries in the supervisor rather than synchronously in `start_spv`. The admitted, blackholed-pass regression passed. |
| M2 detached key acquisition | **CLOSED** | Both bring-up and unlock provider construction use counted `KeyWork`; cancellation changes its generation, and stop/close wait up to 5 s. Late results from a teardown generation are discarded; work finishing after close locks the vault again. Clean-wait and timeout/unclean-close regressions passed, as did the independent barrier probe. This closes the original session stop/close finding; wallet-specific restore teardown still has the M4 gap below. |
| M3 startup budgets | **CLOSED** | Prelude reads have an admission-relative timeout; key construction uses `since + budget`, and startup receives only the remaining time, with the existing 500 ms slack. The held key read no longer leaves SPV `Starting` beyond the budget. Both 3 s and restored-wallet 20 s tests passed. |
| M4 restore rollback | **PARTIAL** | Restore's own `WalletAdded` signals now occur after all bundles/app rows commit, and rollback forgets startup state; `read_only` is sampled from the vault. The new sequential restore regression passed. Concurrent startup can nevertheless admit the registered, uncommitted wallet, reproduced independently below. |
| M5 lock/unlock retry race | **CLOSED** | The biased select polls the lock condition before the startup future; the final lock-counter check converts an unsettled answer to `IdentityUnsettled`. The eight-round barrier test locks after provider construction and verifies discovery retries after unlock; it passed. |

## Findings

### M4 — Medium, PARTIAL: a concurrent startup still admits an uncommitted restore wallet

`rust/crates/dw-engine/src/backup.rs:588–589, 663–664` (changed code), with admission at `platform/bringup.rs:120–124` and task abortion in `platform/runtime.rs`'s `forget`.

Deferring `WalletAdded` fixes restore's direct admission path. It does not exclude the registered wallet and its vault secret from `start_spv`'s snapshot. Restore does not hold the Platform lifecycle mutex or mark its wallets pending across registration through commit/rollback. Starting SPV between the first bundle's registration and a later bundle's failure therefore still admits bring-up for a wallet that will be rolled back. The new rollback comment, “No bring-up was signalled for it,” is insufficient to establish that no bring-up exists.

Independent offline probe: a barrier in the destination's `WalletCreated` event holds the first restore import after registration, before restore can finish. Calling `start_spv` while that barrier is held produces startup **`Starting`** for the uncommitted wallet. Releasing the barrier lets the duplicate second bundle fail with **`Backup(AlreadyExists(...))`**, rolling the wallet back. The probe passed its defect assertions. It proves admission before commit; it does not independently measure secret lifetime after rollback.

Rollback's `forget` only requests abortion; it neither awaits actual wallet-task disposal nor invalidates/waits for wallet-specific blocking key work. Thus the alternative of allowing early admission but completing teardown before destructive rollback is also not established.

Exclude pending restore wallets from every admission path until commit, or serialize admission against restore and await wallet-specific teardown before deleting keys/state. Add a barrier regression that starts SPV during an uncommitted import. Evidence: `/work/scratch/e0-05-r2-gpt/closure_probe.rs`, `closure_probe.log`.

### N1 — Medium: token-wrapped SDK errors bypass the new shared mapping

`rust/crates/dw-engine/src/platform/errors.rs:437–440`.

`TokenOperationFailed { source, .. }` promotes insufficient balance, then calls `internal(source)` directly. A known `TimeoutReached` inside this wrapper maps to **`internal`**, whereas the identical SDK error directly or inside `PlatformWalletError::Sdk` maps to **`platform.timeout`**. Known proof, context-provider and availability errors are likewise bypassed. This does not match m4 §6's stable facade mapping for these failures; `Internal{detail}` should be the fallback for unmapped failures.

The independent scratch probe constructs a token operation with `dash_sdk::Error::TimeoutReached` and confirms `mapped.code() == "internal"`. Route the source through the shared SDK conversion (`source.into()`), which already promotes insufficient balance and preserves its figures. Add wrapped-source cases alongside the direct SDK cases. Evidence: `/work/scratch/e0-05-r2-gpt/closure_probe.rs`, `closure_probe.log`.

## Error mapping assessment

The four requested `From` impls exist. Outside N1, source inspection agrees with m4 §6: locked/mixing-only/missing-credential/watch-only vault failures become `SignerUnavailable`; timeout, DAPI availability, proof and context-provider errors get their facade variants; persister/vault-store failures get `Storage`; engine insufficient credits preserves `needed`/`available`; unmapped errors retain diagnostic detail in `Internal`. All four mapping unit tests passed. The direct tests do not exercise the token wrapper defect.

## Validation

All Cargo commands used `TMPDIR=/dev/shm/e0-05-r2-gpt-tmp` (verified 0700), `CARGO_BUILD_JOBS=8`, and the detached worktree's own `rust/target`. No shared target directory, production OS secret store, or testnet fixture was used.

- `cargo fmt --all -- --check`: passed.
- `git diff --check d53877a..HEAD`: passed; final source status clean.
- `cargo test --offline -p dw-engine --test e0_05_bringup -- --test-threads=4`: **13 passed**, 29.05 s. Log: `/work/scratch/e0-05-r2-gpt/e0_05.log`.
- `cargo test --offline -p dw-engine --lib platform:: -- --test-threads=4`: **23 passed, 1 ignored**, 33.76 s. The ignored test is the public testnet fixture. Log: `/work/scratch/e0-05-r2-gpt/platform.log`.
- `python3 /work/scratch/e0-05-r2-gpt/run_probe.py`: **1 passed**, 9.62 s. Reused the original round-one OS-read barrier scenario, reversing its defect assertions to require the fixed behavior. At 4.2 s with the read still held, SPV was **Running**. Close waited **5.021 s**, returned with the read still paused, and **retained `.session-open`**; releasing the barrier completed the read. This confirms bounded startup and explicitly unclean close. Source/log: `blocking_probe.rs`, `probe.log` in that scratch directory.
- `python3 /work/scratch/e0-05-r2-gpt/run_closure_probe.py`: **2 passed**, 15.43 s. These are defect-reproduction assertions for M4 and N1, not acceptance tests. Source/log: `closure_probe.rs`, `closure_probe.log` in that scratch directory.

No full workspace, clippy, or unrelated subsystem validation was performed. No remaining uncertainty changes the two reproduced findings above.
