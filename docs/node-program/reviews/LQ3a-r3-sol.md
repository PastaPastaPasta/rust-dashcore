# LQ3a lock verification: independent review, round 3

**Range:** `dabe7b65a..49355f5ab` (one signed commit)  
**Core reference:** Dash Core v24.0.0-rc.3 (`0364ed626e`)  
**Verdict: ACCEPT WITH NITS**

## Findings

1. **Minor — clarify the InstantSend pass sampling boundary.** `dash-evo/src/locks/mod.rs:469-470` says Core reads the flag after building its batch. Core's eighth `PushMessage` can verify a sub-batch *during* `BuildVerificationBatch` (`src/instantsend/net_instantsend.cpp:33,57-59,160,316-320`; `src/bls/bls_batchverifier.h:47-57,177-178`; `src/bls/bls.cpp:335`). **Schedule:** eight current-set legacy locks trigger a legacy verification at the eighth push; V19 connects; a ninth current-set lock, already read in legacy encoding but signed under basic, is verified under basic at the final `Verify`. The flag can also flip between building a one-message batch and its `Verify`. Sampling at pass start is a Core-reachable timing when the flag stays stable until verification, but it does not reproduce every same-timeline Core interleaving. This is a wording/forward-integration nit under D2's individual-message boundary, not a defect in `verify_islock`'s per-message API; LK1 will own the batch schedule.

2. **Minor — distinguish the check flag from the visible tip.** `dash-evo/src/bls.rs:15` calls the check's context the “tip context,” although the new D22 text itself relies on a window in which they differ. Core stores the basic flag while connecting the activation block (`src/evo/specialtxman.cpp:983-986`) before advancing the active tip (`src/validation.cpp:3473`), and the worker can read that flag (`src/bls/bls.cpp:335`). **Schedule:** with the old tip still visible, V19 flips the flag, then an InstantSend pass verifies under basic. `bls_ctx_after(deployments, old_tip)` yields legacy. Say “scheme sampled for the check” here so LK1 does not derive `passes` solely from that old tip.

## Scope answers

- **D22:** The previous Major is closed for the stated single-message, one-view contract: `verify_islock` uses `passes.current` and `passes.previous` in the same order as Core's two calls (`dash-evo/src/locks/mod.rs:570-572`; `src/instantsend/net_instantsend.cpp:339-353`). A single old quorum view with legacy/current and basic/previous is Core-reachable: build both passes against the old tip, let V19 flip after the second batch is built, then run its verification before or after the tip moves. A tip/reorg change between the two quorum scans can instead give Core two views; D6 explicitly chooses a single snapshot. Finding 1 identifies a Core interleaving the *start-sampling rule* cannot mirror at the same event times.
- **D23:** The cross-encoded assertions at `dash-evo/src/locks/tests.rs:559-569` require the read scheme to differ from the verify scheme. Replacing `sig.get(read)` with `sig.get(verify)` in `dash-evo/src/locks/mod.rs:461` would fail the accepted cases. Core eagerly reads at `src/chainlock/chainlock.h:41-44` via `src/bls/bls.h:183-209` and checks later at `src/chainlock/handler.cpp:154-161`.
- **D24:** The `clsig_hash` write context at `dash-evo/src/bls.rs:19-20` is correctly placed at `SerializeHash`, immediately after deserialization and before the handler (`src/net_processing.cpp:5797-5798,5814-5815`; `src/bls/bls.h:170-180`). Finding 2 concerns the neighboring check-context wording.
- **Regression:** No new code regression found in the touched helper or tests.

## Validation

- `git diff --check dabe7b65a..49355f5ab`: passed.
- `CARGO_TARGET_DIR=/work/scratch/lq3a-r3-sol/target CARGO_BUILD_JOBS=12 cargo test -p dash-evo`: exit 0; 133 unit, 7 `cbtx`, 31 `cbtx_root_history`, and 2 `sml_core_vectors` tests passed; one history test ignored.
