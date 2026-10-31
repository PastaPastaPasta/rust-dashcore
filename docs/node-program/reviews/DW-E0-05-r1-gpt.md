# DW E0-05 review — df32d19

Completed review. Scope: concurrency/lifecycle, m1 compatibility, key handling, and test rigour. Read-only detached checkout; no network fixture tests.

Reviewed the full 27-file diff `ffb4f99..df32d19`, the relevant pinned library implementations, DASHPAY §3.2, ROADMAP E0-05, E0-04 design at `723d9d3`, and E0-08's m4 contract.

## Findings

### M1 — Medium: a failed loop drain is discarded; stop reports success with Platform work still running

`rust/crates/dw-engine/src/platform/bringup.rs:162–164, 596–610`.

`quiesce_loops` logs false results and returns `()`. `stop_spv` therefore stops SPV and returns success even if a library pass is still in flight. The pinned library explicitly documents false as “the pass did NOT drain; the caller must fail closed.” A blackholed pass for a wallet with an identity can continue its network/persistence work after stop. Moreover, the next `start_spv` synchronously calls `stop_platform` before scheduling the supervisor (`:99`), so that still-running pass makes the nominally immediate start wait another full coordinator drain budget. Propagate the drain outcome, make an unsuccessful stop visible, and keep bounded loop recovery inside the scheduled start task. Add a test with a real admitted loop pass held across the drain deadline; empty-wallet loops do not exercise this case.


### M2 — Medium: cancelling the wallet task detaches its blocking key acquisition

`rust/crates/dw-engine/src/platform/bringup.rs:463–471` (also unlock crypto at `:520–522`), with cancellation at `:287–288` and `runtime.rs:209–213`.

The supervisor awaits its JoinSet, but a child aborted while awaiting `spawn_blocking` drops only the JoinHandle: the blocking closure keeps running with a cloned vault. A slow OS-store read in `background_contact_crypto` is a concrete path, not just runtime starvation. Close/stop can thus return while bring-up still accesses the key store and may subsequently install the vault's cached data key or issue providers. Track and join blocking key work across cancellation, with a cancellation/generation check that prevents publication after teardown. Confirmed by a standalone barrier-controlled OS-store probe against the unmodified reviewed libraries: close returned in **3.955 ms** and removed `.session-open` while the bring-up OS read was still paused; releasing the barrier made that read complete after close. See `/work/scratch/e0-05-gpt/blocking_probe.rs` and `probe.log`.

### M3 — Medium: the startup budget does not bound the prelude

`rust/crates/dw-engine/src/platform/bringup.rs:325–347`.

The code awaits wallet-manager access, marker reads and key construction before entering its timeout. Subtracting `since.elapsed()` afterwards cannot recover time already spent: a held key-store read leaves SPV `Starting` beyond 3 s + 1 s (or 20 s + 1 s). The same offline probe confirmed a created wallet was still `Starting` after the OS read had been held for **4.2 s**, beyond its 3 s budget + 1 s acceptance slack. Bound the entire bring-up from admission, including key acquisition, and solve this together with M2 so timing out does not detach secret work.

### M4 — Medium: restore rollback does not cancel the bring-up it now starts

`rust/crates/dw-engine/src/backup.rs:659–690`, reached after `keys.rs:431` signals `WalletAdded` during an import.

While SPV is running, each bundle's import queues bring-up before the entire backup restore succeeds. If a later bundle or app-row import fails, `roll_back_restore` removes earlier wallets and wipes their keys without `platform.forget`. A bring-up that has already resolved its scan master can therefore remain in discovery until its 20 s budget after the wallet was rolled back; the final `manager.get_wallet` check only suppresses its status publication, not the intervening work or key lifetime. The keys-attached-to-watch-only rollback also needs cancellation and startup/read-only cache invalidation. Route every rollback through the Platform cancellation boundary, await actual task disposal before destructive teardown, or defer `WalletAdded` until the restore commits. Add a failed multi-bundle restore test while SPV runs, with bring-up paused after master resolution.

### M5 — Medium: a lock racing completed key construction can lose the unlock retry

`rust/crates/dw-engine/src/platform/bringup.rs:453–458, 381–391`.

A concrete interleaving: (1) bring-up samples lock count L and successfully builds its providers; (2) the vault locks before the awaiting wallet task resumes; (3) `run_subsystems` sees both the changed-lock arm and the startup call ready. The `select!` is unbiased. If it polls the call first, the now-stale scan provider returns `ScanKeyError::Unavailable`; the pinned library immediately returns `PartialNoIdentity` without a network await. (4) `had_keys` is true, so the final `locked_since` check does not override the outcome: it is only used inside `if !had_keys`. The wallet is therefore recorded as `PartialNoIdentity`, rather than `IdentityUnsettled`. The next unlock takes the crypto-drain branch instead of re-running discovery, leaving the identity undiscovered until another SPV start.

Give the changed-lock arm priority and classify a key-bearing run against the lock counter again before recording its outcome. Add a barrier between provider construction and polling startup, then lock and assert that the next unlock re-runs discovery. This is a source-proven ready-branch race; the existing sleep-based lock test holds discovery on the network, where only the lock arm is ready.


## Test rigour and contract observations

- **Low, missing race coverage — `rust/crates/dw-engine/tests/e0_05_bringup.rs:292, 349, 419, 500`:** the lifecycle tests start first, then sleep 300–500 ms, then perform close/stop/lock/remove. They check cancellation during network discovery, but do not force the registration window, task-tracking window, lock during key construction, simultaneous start/stop, wallet add/remove during the initial snapshot, stalled loop drains, or restore rollback. The round-two fixes need barrier-driven tests at those boundaries. Passing these tests is useful evidence for the common path, not proof of the race fixes.
- `spv_running()` remains `Result<bool>` and is true while starting; `SpvStateChanged{running: true}` remains the actual-start notification. New Rust records match E0-08's `StartupStatus`, `SyncLoop`, and `SyncLoopStatus` names/shapes. The FFI filters the new notice rather than changing frozen generated Swift types. `ensure_spv_running` correctly avoids treating cached scan heights as proof of SPV startup and fails fast when the asynchronous start fails.
- Frozen Swift hosts cannot call the new Rust-only `spv_state`; they must distinguish actual startup through the existing event stream. Existing heights/balances can be known during `Starting`, while rescan/rotation/broadcast can still refuse. That is a deliberate behavioral change now documented in m1, not a new signature break. Avoid using the boolean to enable operations that require actual SPV.
- No identity signer is passed to startup or unlock crypto drains, so these paths cannot perform auto-accept state-transition writes. The prompt-free provider factories use the vault's own scope/epoch checks, and `ScanKeyGuard` in the pinned library erases a resolved master on future drop. The unresolved issue is ensuring the task/worker actually drops at each teardown boundary.
- Close itself has two cancellation passes around the operation gate and leaves a single sealed loop drain to `manager.shutdown`; I found no lock-order cycle in the added short-held mutex/RwLock accesses. The blocking work and rollback findings prevent accepting the stronger claim that every bring-up task has ended at close/removal.
- Reviewed the ignored testnet test's code, without running it. It restores into a fresh data directory, checks `Ready`, identity, zero pending account builds and nonempty receival accounts after actual SPV startup. Given this isolated test's absence of any other signer-present work, the ordering is supported by the implementation. It does not instrument the first filter scan itself.

## Spec ambiguities

The stated list of nine implementer ambiguities was not found in the tracked branch docs or four commit messages; its location has been requested. I cannot claim to have reviewed that absent list. Visible interpretations I agree with: the extra `IdentityUnsettled` value (also in E0-08's m4), no auto-accept signer for unattended work, installation-local markers excluded from backups, and keeping new notices/APIs Rust-only until E0-13. I disagree with treating a failed loop drain merely as a later shutdown-report concern (M1), or measuring a whole-startup budget only after unbounded key/setup awaits (M3).

## Validation

- Detached checkout: `/work/scratch/e0-05-gpt/repo`, HEAD `df32d1969b032186d200f65872f71c9dc467896c`; repository source remains unchanged.
- `TMPDIR=/dev/shm/e0-05-gpt-tmp` (0700), `CARGO_BUILD_JOBS=8`; no shared target directory.
- `cargo fmt --all -- --check`: passed.
- `git diff --check ffb4f99..HEAD`: passed.
- `TMPDIR=/dev/shm/e0-05-gpt-tmp CARGO_BUILD_JOBS=8 cargo test --offline -p dw-engine --test e0_05_bringup -- --test-threads=4`: **9 passed**, 0 failed; test execution 24.59 s.
- `python3 /work/scratch/e0-05-gpt/run_probe.py`: **M2/M3 reproduced** using a fake in-memory OS store with a controlled read barrier. The probe links the detached checkout's compiled libraries and asserts the problematic behavior; its passing result confirms the defects. Runtime 4.42 s. Source and output: `/work/scratch/e0-05-gpt/blocking_probe.rs`, `/work/scratch/e0-05-gpt/probe.log`.
- No testnet fixture, regtest harness, CoinJoin suite, or independent full 526-test/clippy rerun performed. The implementer's wider validation is not independently certified by this review.

Verdict: CHANGES-REQUIRED
