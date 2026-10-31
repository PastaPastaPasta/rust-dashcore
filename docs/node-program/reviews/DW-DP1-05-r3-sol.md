# DW-DP1-05 — narrow confirmation review, round 3 (Sol)

Reviewed `dashwallet-desktop` `dw/dp1-05-recovery` at **`c9a7d130cbe23131a290965e593eea741f966570`**, the single commit after r2 head `c0de6d334185a40bb9b5c97869b414e946e205bd`, based on main `c19e441262ee2149dc788ba2ed664263ff6d97cb`. Followed `briefs/REVIEW-COMMON.md` and read the r2 report. Used a detached worktree at `/work/scratch/dp1-05-r3-sol/wt`. This review confirms M3-R2 and audits the new admission guard, cancellation, overlap, generation handling, and lock ordering; it does not repeat the entire r1/r2 review.

## Finding

### N1-R3 — minor: the recovery acceptance test asserts `Ready` before waiting for it

**Location:** `rust/crates/dw-engine/src/platform/recovery_tests.rs:692` and `:708`.

The first independent workspace run failed `discovery_forgets_the_proven_absence_and_recovers_what_it_finds`: its final startup assertion observed `Starting` instead of `Ready`. Its wait condition checks only the identity's recovered names. The names pass count and bring-up count assertions had already passed.

There is a legitimate scheduling window: the initial supervisor can consume the names work queued by explicit discovery before processing its `Readmit` signal. Those names remain visible when the subsequent bring-up sets startup to `Starting`, and its `Ready` publication comes later. Thus visible names and the counters do not prove the bring-up has finished. This assertion/wait predates the r3 commit. Five isolated reruns passed, and a second unchanged-head workspace run passed all 603 tests. No production recovery failure is established by this test failure.

**Suggested fix:** Include `startup == StartupStatus::Ready` in the wait predicate alongside the expected names, then assert the counters and cleared marker after that wait. Preserve the assertion that recovery reaches `Ready`; wait for its publication.

No blocker or major finding.

## M3-R2 disposition: resolved

`Recovery::forget` (`platform/recovery.rs:244`) no longer changes discovery admission. Rollback holds `_discoveries` (`backup.rs:678`) through manager removal, row deletion, secret deletion, and the end of that wallet's iteration. The guard also spans the secret cleanup for `created == false`, when the manager wallet survives as watch-only. Removal (`wallets.rs:226`) and unload (`multiwallet.rs:274`) retain their guards through their teardown and release them on error returns.

The original scheduling reproduction now passes at the fixed head: hold the real generation's payment guard, start rollback, wait for `forget`, request discovery while removal is blocked, complete rollback, and reimport the same phrase. Discovery is refused with `wallet_not_found`, does not outlive rollback, and leaves no identity in the replacement wallet. It passed in both unchanged-head workspace runs and an additional direct rerun of the workspace-built engine test binary.

For an independent differential, restored the four teardown implementation files to their r2 contents in the scratch checkout, retaining the r3 mock/regression fixture and exposing the existing rollback method to that test. Added only a diagnostic print. The same regression failed, exit 101, with:

```text
r3 baseline: outlived=true, late=Ok(1), replacement=[("US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx", [], None, true)]
a discovery outlived the rollback: Ok(1)
```

The identity is the deterministic mock fixture's public identity. This reproduces the old id-based late fold; no live DAPI response is claimed. Restored all implementation files to r3 before the remaining probes.

## Guard and generation audit

- **No deadlock found.** `end_discoveries` releases its synchronous map mutex before sending cancellation or awaiting receiver drain. The callers drain first and only then call manager removal. They hold neither the generation payment/teardown gate, the manager lock, the choice writer, nor Platform's lifecycle mutex while draining. Checked the pinned Platform library at `bc41f1bc`: its manager removal takes the generation teardown gate before manager locks; identity discovery takes no payment gate and releases manager locks around network calls. The committed reproduction exercises a payment-delayed rollback without a cycle.
- **No admission guard leak found.** The local guard is constructed before `ending.closed().await` (`recovery.rs:227`), so dropping a pending drain releases its closer. The same RAII drop covers normal completion, errors, and unwinding. All three production call sites retain the named binding through cleanup. The map's only deletion is the last closer's drop; `forget` cannot erase another closer's ownership.
- **Overlapping teardowns remain closed.** An independent probe polled two drains with an old receiver alive, cancelled one, called `forget`, and verified admission remained closed through the other drain and its returned guard. Dropping the final guard reopened it. A separate overlap of two already-drained guards also stayed closed until both dropped.
- **No reachable generation reuse after drop/reimport found.** The session counter is independent of the per-wallet map and is never reset by `forget` or teardown. New admission after final guard drop creates both a fresh channel and a fresh generation. A probe checked 1,024 teardown/forget/reopen cycles and rejection of every earlier generation. Cancelling the only pending drain also reopened with a different generation while its retained old receiver remained cancelled. Session close drains admitted operations before its direct manager unloads, so a new session's counter cannot admit an old session's operation.
- **No new old-generation application path found.** The generation check (`recovery.rs:594`) precedes reconciliation, and the receiver remains alive through that reconciliation. The generation check alone is not a fence inside the library's earlier id-based identity fold: safety also comes from cancelling and draining that whole discovery future before proceeding with removal or secret cleanup. The pinned discovery implementation does not detach its fold into another task. If reconciliation has already entered, teardown waits for its receiver before forgetting/removing the wallet. An independent public-API probe suspended discovery before its fold, ran rollback for both `created` values, verified cancellation and secret deletion, then reimported/reattached keys. Neither old scan populated the wallet; a newly authorized scan succeeded, proving admission reopened.

## Independent validation

Commands ran under `rust/`, with **`umask 022`**, **`TMPDIR=/dev/shm/dp1-05-r3-sol-tmp`** (mode 700), `CARGO_BUILD_JOBS=12`, and `RUSTC_WRAPPER=`. The detached checkout used its own target directory; no shared `CARGO_TARGET_DIR`.

- `cargo fmt --check`: **PASS** at the unchanged head and after restoring all scratch experiments.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**, exit 0, unchanged head.
- `cargo test --workspace --no-fail-fast`, unchanged head, run 1: **602 passed / 1 failed / 3 ignored**, exit 101; N1-R3 above was the sole failure.
- Same workspace command, unchanged head, run 2: **603 passed / 0 failed / 3 ignored**, exit 0.
- Workspace-built engine binary, rollback regression: **1 passed / 0 failed**, exit 0; five isolated runs of the failing acceptance test: **5 passed / 0 failed**.
- r2 teardown differential, `cargo test --workspace --no-fail-fast a_restore_rollback_admits_no_discovery_until_it_is_done -- --nocapture`: **expected failure**, exit 101, with the surviving scan and replacement identity above.
- r3 plus scratch probes, `cargo test --workspace --no-fail-fast reviewer_ -- --nocapture`: **3 passed / 0 failed**, exit 0. These cover pending-drain cancellation, overlapping guard ownership, stale generation rejection, and actual rollback/reimport for both newly created and surviving watch-only wallets.

The ignored live fixture was not run. The earlier accepted limits on brief counted blocking key work and redundant names refreshes are unchanged. No implementation-branch edits, commits, pushes, or GitHub comments were made. Scratch changes were restored; the detached checkout was clean at the requested head. The worktree, build artifacts, experiment logs, and dedicated temporary directory were removed. This uncommitted report is the only retained artifact.

**Verdict: APPROVE-WITH-NITS.**
