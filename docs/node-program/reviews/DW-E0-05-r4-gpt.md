# DW E0-05 round 4 — a2fdfcb

Verdict: **REQUIRES CHANGES**. The round-3 overlap defect and envelope-depth limitation are closed. The readmission fix introduces a reproducible duplicate bring-up after a single successful restore (M4-R4 below).

Narrow independent confirmation of `79aa815..a2fdfcb`, fixes `0c43f60` and `a2fdfcb`. Exact head: `a2fdfcb6bec680c62a7be4ae2696326ae6fb65db`. Reviewed the round-3 report and actual diffs; used detached worktree `/work/scratch/e0-05-r4-gpt/repo`. The implementer's worktree was untouched. The reviewer worktree was removed after validation. This report is not committed.

## Item statuses

| Item | Status | Evidence |
|---|---|---|
| M4-R3: overlapping restores lose committed-wallet bring-up | **CLOSED** | Reran the original R1=`[a]`, R2=`[b,a]` reproduction, including its sentinel and unlock, with fixed-behavior assertions. R1 commits while R2 still marks `a`; R1's signal is consumed and refused. R2 fails with `Backup(AlreadyExists(a))`; its final release now brings surviving `a` to `Starting`. Rolled-back `b` has no startup record or task. The implementation's overlap regression also passes. |
| N2: recursive SDK/persister envelopes overflow the stack | **CLOSED** | Both implementation tests pass at 1,000 envelopes, including their combination and insufficient-credit figures. Independent tests retain SDK `unavailable` and restore `storage` when the leaf has no recognized code; a combined restore/retry unknown maps to the meaningful inner `unavailable`. Bare unknown SDK, wallet and token errors still reach `Internal` with diagnostic detail. Restore fallback detail contains the unknown leaf and stays below 200 characters. |
| N1 / round-2 mapping rule | **Still CLOSED** | Independent token-wrapped timeout remains `platform.timeout`; wrapped proof and availability errors retain their codes. Unknown leaves preserve a meaningful wrapper fallback, while an unwrapped unknown retains `Internal`. |

## Readmit probes

- **Later restore re-marks the wallet:** a real supervisor is held on another wallet's existing after-key-acquisition pause. Release the first wallet's last guard, queuing `Readmit`, then install a fresh restore guard before delivery. The old signal produces no startup record under that new mark. Releasing the fresh guard requeues it successfully. This is a guard-level setup exercising actual supervisor delivery and admission, not a second full backup transaction.
- **Every refusing entry point records the flag:** the only three admission sites are the start listing (`platform/bringup.rs:126`), bring-up (`:384`), and unlock (`:602`). Each uses `PlatformRuntime::admit` (`platform/runtime.rs:376`), which sets `refused` under the mark mutex. Load and both import paths send `WalletAdded` into that same bring-up path (`multiwallet.rs:317`, `keys.rs:377`, `keys.rs:435`). Independent startup, unlock and wallet-added probes pass. A `Readmit` refused under a fresh mark also records the refusal.
- **Refusal races the final release:** 1,000 synchronized thread races against the actual admission/guard code and a captured supervisor channel. In 971 cases admission returned `None` and exactly one matching `Readmit` was queued; in 29 it returned `Some(true)` after release and no readmission was necessary. No missing or duplicate release signal was observed. Both transitions use the same mutex, closing the lost-flag interval.
- **Rollback:** the original overlap probe and the implementation's failed-restore test leave removed wallets without bring-up. The new supervisor registry check skips their queued `Readmit`.
- **Duplicate bring-up:** **yes**, sequential passes; see M4-R4. The supervisor awaits each pass before processing the next signal, so this is not simultaneous bring-up for one wallet.

## New finding

### M4-R4 — Medium: a successful restore schedules its bring-up twice

`rust/crates/dw-engine/src/platform/runtime.rs:216`, `backup.rs:591–596`, `platform/bringup.rs:263–277`.

The last refused restore mark sends `Readmit` when dropped. A successful restore immediately also sends `WalletAdded` for the same wallet. Both become `Job::BringUp`. The existing duplicate check only covers wallets admitted in the initial startup list; a wallet excluded because it was marked is absent from that set. Nothing coalesces these two signals.

Reproduction with one valid backup `[a]`, no overlapping restore:

1. Hold the destination's `WalletCreated(a)` event after registration and before commit.
2. Start SPV while `a` is marked; wait for `Running`. The startup listing refuses `a` and records the flag.
3. Arm the existing after-key-acquisition pause and release the restore. It succeeds. Its final guard queues `Readmit(a)`, followed by its commit's `WalletAdded(a)`.
4. Observe the first bring-up at the pause. Arm a second pause before releasing the first.
5. Without any unlock or explicit retry, observe the second bring-up at the second pause.

The accelerated variant locks the vault during the first pause, making its first pass end promptly without sending an unlock signal; duplicate admission reproduced **3/3**. The independent unlocked variant lets both passes finish against the refusing loopback DAPI endpoint. The first took **11.040 s** before the second admission. After releasing the second pause, a queued seedless sentinel waited another **11.024 s**; the two passes took **22.063 s** together. This proves repeated work and additional supervisor queue delay, not merely two queued messages.

Coalesce the successful restore's commit signal with its deferred readmission while retaining final-release requeue for the original overlapping-failure scenario. Add a single-successful-restore regression counting admissions or subsystem starts; the current tests only assert that bring-up eventually starts.

## Validation and reproduction artifacts

All Cargo runs used `TMPDIR=/dev/shm/e0-05-r4` (0700), `CARGO_BUILD_JOBS=6`, Rust 1.98.1, and the worktree's own `rust/target`. Final runs used `RUSTC_WRAPPER=` after shared-wrapper stalls. An incompatible dependency-artifact copy was discarded with a complete private-target clean before the final build. No public-testnet fixture or real OS secret store was used.

- **Unmodified a2fdfcb:** `cargo test --workspace` — **567 passed, 0 failed, 2 ignored**, exit 0. Ignored: public-testnet identity fixture and OS-keyring round trip. Log: `/work/scratch/e0-05-r4-gpt/workspace.log`.
- The first complete-build test attempt stopped at the unchanged earlier-round test `a_held_key_read_does_not_hold_spv_past_the_budget`: unexpected `UncleanShutdown` notice, after SPV started within budget at 3.045 s. It passed in a workspace-filtered isolated rerun and the full default-concurrency rerun passed. Retained logs: `workspace-first-test-failure.log`, `key-read-rerun.log`. No causal connection to these fixes was found; this remains a test-health observation, not a confirmed new code finding.
- Before reviewer edits: `cargo fmt --all -- --check` and `git diff --check 79aa815..HEAD` — passed.
- Independent probes: `cargo test --workspace r4_ -- --nocapture --test-threads=1` — **10 passed**, 0 failed, 29.11 s. Two tests intentionally assert the duplicate defect, so their passing does not mean acceptance. Log: `probes.log`; repeated duplicate confirmations: `duplicate-repeat-2.log`, `duplicate-repeat-3.log`.

Sources and patch are preserved outside the removed worktree in `/work/scratch/e0-05-r4-gpt/`: `confirmation_probe.rs`, `readmit_probe.rs`, `reviewer-probes.patch`, and `apply_probe.py`. To reproduce, recreate the specified detached worktree at a2fdfcb, run `python3 /work/scratch/e0-05-r4-gpt/apply_probe.py`, then run the probe command from its `rust/` directory with the environment above. Filter `r4_single_successful_restore_enqueues_two_bringups_defect_probe` for the accelerated defect, or `r4_duplicate_bringup_without_a_lock_defect_probe` for both completed unlocked passes.

