# P2a-net round 5 narrow review — Sol

Reviewed detached `node/p2a-net` at `098e47ca0`, range `418e0c0b4..098e47ca0`, in `/work/scratch/p2a-net-r5-sol/wt`. The prior Core reference is `v24.0.0-rc.3`; its `src/net_processing.cpp:4292-4304` checks the common version before `fSuccessfullyConnected`, and `src/version.h:25-26` defines `ADDRV2_PROTO_VERSION = 70223`. The scratch worktree was clean after validation; no repository code was edited, committed, or pushed.

## Item rulings

| Item | Status | Evidence |
|---|---|---|
| R4-M1: 70222 late `sendaddrv2` peers must remain live and retain address-path coverage | **CLOSED** | The generator shares the threshold at `node-l1/src/p2p_addr.rs:156-158` and only marks a late-`sendaddrv2` peer gone when `version >= ADDRV2_PROTO_VERSION` at `:676-680`. Live-peer selection therefore retains 70222 peers at `:865-878`, including a manual seeder. `Ends` records the expected ended set at every pass (`:315-320, 590-619, 948-960`), and the Core-only test compares Core's complete ended set after every pass at `node-l1/tests/p2p_addr.rs:211-224`. The fixed manual-seeder regression test at `:262-296` verifies a 70222 manual peer sends its seed after late `sendaddrv2`, remains unended, and produces a `getaddr` reply. Against the pinned Core tag, the strict test passed. |
| R4 diagnostic nit: `AddrFetchDone`/timeout classifier comment | **CLOSED** | `node-l1/tests/p2p_addr.rs:68-72` now documents that unreadable address messages do not end a peer, that the done reason is credited only on the following pass, and why the generator's timeout handling cannot share that pass. The branches at `:91-95` implement that ordering, while the exact per-pass `Ends` check at `:211-224` prevents a misclassification from hiding liveness drift. The strict run covered all 192 generated scripts without an unexplained removal. |
| Changed-code regression scan | **CLOSED** | The `script` API and comparison plumbing are updated consistently at `node-l1/src/p2p_addr.rs:826, 963-980`; unit and integration tests passed. No additional regression was found in the three changed commits or the documentation update. |

## Mutant validation

I applied a scratch-only mutant replacing `self.peers[i].gone = self.peers[i].version >= ADDRV2_PROTO_VERSION` (`node-l1/src/p2p_addr.rs:679`) with unconditional `true`, restoring the R4-M1 behavior. The `p2p_addr` integration suite then failed 2 of 3 tests:

- `late_sendaddrv2_at_70222_keeps_the_manual_seeder` failed because the manual peer had no post-`sendaddrv2` seed.
- `scripts_reach_core_address_paths` failed at step 14: Core had ended `[false, false, false, false]` while the script expected `[false, true, false, false]`.
- `address_targets_run_both_legs` remained passing.

This proves the new strict check detects both the generic lost-live-peer case and the fixed manual-seeder case.

## Validation

- `CARGO_TARGET_DIR=/work/scratch/p2a-net-r5-sol/wt/target TMPDIR=/work/scratch/p2a-net-r5-sol/tmp cargo test --locked -p node-l1 --all-features --test p2p_addr -- --nocapture`: **3 passed**. The strict Core-only test reported relay/reply floors and ended-peer reasons for all three targets.
- `CARGO_TARGET_DIR=/work/scratch/p2a-net-r5-sol/wt/target TMPDIR=/work/scratch/p2a-net-r5-sol/tmp cargo test --locked -p node-l1 --all-features --lib p2p_addr -- --nocapture`: **5 passed**.
- `cargo fmt --all -- --check`: **passed**.
- `CARGO_TARGET_DIR=/work/scratch/p2a-net-r5-sol/wt/target TMPDIR=/work/scratch/p2a-net-r5-sol/tmp cargo clippy --locked -p node-l1 --all-features --all-targets -- -D warnings`: **passed**.
- `git diff --check 418e0c0b4..098e47ca0`: **passed**.

ACCEPT
