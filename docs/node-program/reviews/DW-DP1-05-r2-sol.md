# DW-DP1-05 — confirmation review, round 2 (Sol)

Reviewed `dashwallet-desktop` head `c0de6d334185a40bb9b5c97869b414e946e205bd`, the single fix commit after r1 head `4ee3f74e6df60e520c85fe1259e93c48627ee8a3`, against unchanged main `c19e441262ee2149dc788ba2ed664263ff6d97cb`. Read the reviewer contract and r1 report first. Used a detached worktree at `/work/scratch/dp1-05-r2-sol/`. No implementation-worktree edits, commits, pushes, or GitHub comments.

## Finding

### M3-R2 — major: restore rollback reopens discovery before removing the wallet

**Location:** `rust/crates/dw-engine/src/backup.rs:677`, together with `platform/recovery.rs:196` and `platform/recovery.rs:525`.

Rollback now calls `end_discoveries`, but immediately calls `platform.forget` before awaiting `manager.remove_wallet` or deleting the wallet's secret. `forget` deletes the per-wallet discovery channel. While the old wallet is still registered, a new explicit discovery therefore creates a fresh, false channel, passes the wallet check, and resolves its master key. The restore's existing `restoring` mark does not prevent this: explicit discovery does not use the supervisor's `admit` check. Rollback never cancels or drains this newly admitted call.

This is a real scheduling window. The pinned library's `remove_wallet_with_teardown` waits for the wallet generation's exclusive lifecycle gate; a payment operation can hold its read guard. Discovery does not take that gate. Its eventual library fold looks up the manager bucket by wallet id, so a call admitted in the rollback window can survive secret deletion and apply a late response to a subsequent reimport of that id. This retains an already-resolved master across completed rollback, potentially until the ordinary 60-second discovery budget, and violates the claimed refusal of new discoveries once rollback begins.

**Independent reproduction, twice:** `reviewer_rollback_reopens_discovery_before_removing_the_wallet` uses the real rollback sequence and public discovery call. Register a wallet, mark it as restoring, and hold `wallet.generation().payment_guard()`. Start rollback with `(id, true)` and wait until `platform.forget` has cleared its startup status. Start a valid IdentityScan discovery, with the mock network response suspended before the identity fold. Release the payment guard and await rollback. The vault has no wallet secret, but discovery has not ended. Reimport the same phrase, then release the response. Both runs report:

```text
rollback scan outlived=true, returned=Ok(1), replacement=[(ALICE, [], None, true)]
```

`ALICE` abbreviates the deterministic synthetic identity printed by the probe. The probe asserts both the surviving call after secret deletion and the identity's appearance in the replacement wallet. It models the library's late id-based fold; no live DAPI response is claimed. Scratch changes only exposed the existing private rollback method to the test module and extended the mock/test fixture; teardown and discovery behavior were unchanged.

**Suggested fix:** Keep discovery admission closed through the entire rollback's manager removal and secret cleanup. Separate supervisor/cache forgetting from clearing the discovery cancellation channel, or defer the channel reset until cleanup is complete. Cover both newly created wallets and the keys-attached-to-watch-only rollback case, and reopen only when appropriate for the surviving/current wallet. Add a regression that holds the generation payment guard during rollback, verifies a new discovery is refused, and verifies no old call can apply after reimport.

## R1 dispositions and race audit

- **M1 resolved.** The grant is redeemed into a mutable token set, `hold_key` is called, and own-key grants use `scan_key_held`. The locked and mixing-only passphrase cases pass through the public discovery API. The resolved master, wrapped scan key, and hold have the required drop order on scan exits.
- **M2 resolved.** The reconciliation runs after lock cancellation and library errors as well as success and timeout. A stored identity refreshes the snapshot and queues names plus `Readmit`, while the original cancellation/error is preserved. Independent probes exercised lock-after-fold through `IdentityUnsettled` and a subsequent unlock to `Ready`, and a non-timeout error after fold while SPV was stopped followed by names recovery at start. The committed mock test alone reports `Ready` while locked; the independent unlock probe also exercised the engine's unsettled/retry path. The pinned library's unfinished contact recovery outcome supports that path.
- **M3 partly resolved.** Ordinary removal and unload cancel and drain admitted discoveries, without occupying the supervisor task slot. The committed remove/unload reproduction passes. Two independently suspended concurrent discoveries also drained before ordinary removal, and their responses did not populate a reimported wallet. Restore rollback still has M3-R2 above.
- **N1 resolved.** The mock recognizes identities on file, and the acceptance test checks `Ready` and the cleared marker after the queued work. Both full workspace runs pass.
- **N2 resolved for completed choice calls.** Choice writes and loads hold the same async lock across the database operation and cache publication. The r1 public concurrent-identity-choice stress reproduction passed 1,000 pairs in each of two runs. A separate concurrent main-name write/load probe passed and verified clearing the preference restores the default selection.

No additional removal/discovery lock cycle or lost wakeup was found. Ordinary removal waits on the watch channel without holding a manager or choice lock. The watch value retains cancellation for receivers that have not yet polled; `closed()` waits for all admitted receivers. A completed scan's reconciliation may finish while removal waits, but it finishes before that removal can proceed to reimport. The concrete exception is rollback resetting admission too early.

## Known limits and DP1-03 adoption

Both stated limits are acceptable. Cancellation can detach the brief counted blocking key step; its result is dropped and cannot deliver a master to the cancelled async scan. This does not justify M3-R2, whose scan has already resolved its master and survives on a network await. Requeuing the whole names batch after stop causes redundant public-data refreshes and preserves owed recovery work; it is not a merge blocker.

The shared choice API is suitable for DP1-03 to adopt. `NetworkSession::set_main_name` writes the existing `dp_prefs/main_name` row and publishes the same value under the shared choice lock; `None` deletes it. `identities()` applies the shared normalized selection rule and returns the currently owned spelling, with first-acquired fallback. The helper is crate-internal: DP1-03 callers must retain their admitted session operation, canonical Base58 identity ids, and ownership/name validation. DP1-03 should route all writes to this preference through the helper, use identity summaries for reads, and retain its separate contest/registration settlement work when removing duplicate choice logic. Any user-facing forwarding setter remains a facade responsibility. No DP1-03 merge or its gate is claimed here.

## Independent validation

All commands ran under `rust/`, with `umask 022`, `TMPDIR=/dev/shm/dp1-05-r2-sol-tmp` (mode 700), `CARGO_BUILD_JOBS=12`, and `RUSTC_WRAPPER=`. The build used this detached worktree's own target directory.

- `cargo fmt --check`: **PASS** at the unchanged head, and again after restoring scratch experiments.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**, exit 0, unchanged head.
- `cargo test --workspace --no-fail-fast`, unchanged head, run 1: **602 passed / 0 failed / 3 ignored**, exit 0.
- Same workspace command, unchanged head, run 2: **602 passed / 0 failed / 3 ignored**, exit 0.
- Scratch `cargo test -p dw-engine --lib reviewer_ -- --nocapture`: **6 passed / 0 failed**, exit 0. One probe asserts the broken rollback behavior described above; its passing result is evidence of the blocker.
- Scratch `cargo test -p dw-engine --lib platform::recovery_tests -- --nocapture`: **18 passed / 0 failed / 1 ignored**, exit 0. This repeats all six probes, including the rollback violation, and the committed recovery regressions covering the r1 scenarios.

The optional live testnet fixture was ignored. The author's historical differential and ten-run claims are not substituted for the independent results above. Scratch source changes were restored, `git diff --exit-code HEAD` and status were clean, and the detached worktree, build artifacts, experiment logs, and dedicated temporary directory were removed. The report is the only retained review artifact and is uncommitted.

**Verdict: CHANGES-REQUIRED.**
