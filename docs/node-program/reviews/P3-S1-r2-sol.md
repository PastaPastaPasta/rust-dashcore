# P3 S1 review — Sol round 2

**Head:** `147f09b89` against `f1a8a890d`  
**Verdict: ACCEPT**

## Findings

None in the round-two delta.

## F1 and publication order

- All simulation status writes use `Inner::st_mut` (`dashd-p2p/src/sim/chain.rs:143`, `:176`, `:543`, `:550`, `:766`, `:800`), so `Arc::make_mut` separates an in-progress write from every copy held by a published view. `publish_with` captures the status `Arc` while holding `inner` (`:623-626`), then stores the snapshot, sequence, and status under one `view` lock (`:639-644`). One load of that view cannot pair the new tip with old status, or the old tip with new status. `status` clones that published copy before reading immutable entry ancestry (`:1145-1152`), and `valid_scripts` reads the same view cell (`:1067-1069`). Neither reads an unpublished status set.
- A caller can retain an older `ChainSnapshot` across two publications. Its active-chain operations still use that snapshot, while `status` and `valid_scripts` read the *current last-published* status. This is the explicit P3 §2.4 / `ChainAccess::status` contract (`processing/seams.rs:172-174`): status is not versioned by snapshot. The second publication cannot mutate the first publication's status `Arc` because its next write takes the copy-on-write path. The distinction matters: the original F1 was a status observed **before its first publication**.
- `store_blocks` first publishes its headers through `add`, then publishes the stored data and transaction validity (`chain.rs:759-770`). That second view is Core-reachable with the prior tip: Core's `ProcessNewBlock` exits the `AcceptBlock` `cs_main` section before `ActivateBestChain` (`src/validation.cpp:4869-4893`, `:4897-4900`), and import can run `AcceptBlock` before later activation (`:5503-5506`, `:5530-5541`). The helper does not publish a connected status without the corresponding tip.

## Production follow-up and other reads

D73 accurately describes the CH1-node split. `ConnectTip` calls `connect_block` (`dashd-chainstate/src/chain/abc.rs:533-537`); `BlockStatusWriter::raise_validity` writes through `NodeIndex::modify` (`chain/seams.rs:332-343`; `dash-chain/src/chain.rs:2130-2136`). `ChainViewHandle::entry` reads the arena's mutable group live (`chain/view.rs:141-144`; `dash-chain/src/shared.rs:447-460`), before `after_release` or `scope_end` stores the later `ChainView` (`chain/abc.rs:703`; `chain/actor.rs:1264-1288`). This is a real CH1-node/S6 interface obligation, not a defect added by this S1 delta; S1 has no production `ChainAccess` adapter yet.

I found no second S1 mutable-status read that bypasses the published copy. The simulation's live `lookup`, `entry`, and `ancestor` paths return index identity or immutable ancestry; `snapshot` is the separately documented tip cell, and the live loading/IBD inputs follow their own R4 contract. In production, the live mutable fields exposed by `ChainViewHandle::entry` are the D73 issue that S6 must avoid or CH1-node must fix.

## Checks

| Check | Result |
|---|---|
| `cargo test -p dashd-p2p --test sim_p3_s1` | 15 passed |
| `cargo test -p dashd-p2p --lib` | 33 passed |
| R1 implementation mutant: replace `chain.rs` with its `f1a8a890d` version, keeping the strengthened test | **Caught** at `sim_p3_s1.rs:447`: old view returned `Scripts` for B. |
| Second implementation mutant: read `valid_scripts` from live `Inner::st` instead of `View.status` | **Caught** at `sim_p3_s1.rs:452`: B was valid before the view store. |

The implementation mutations were temporary in the detached review worktree and restored. `git diff --exit-code` was clean afterward. All Cargo runs used `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, and the private `/work/scratch/p3-s1-r2-sol/target`.

**Summary:** ACCEPT. Findings: none; no severity to assign.
