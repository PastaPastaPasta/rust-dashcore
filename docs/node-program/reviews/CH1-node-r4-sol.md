# CH1-node round 4 — independent Sol review

Reviewed only `bf90eef84..73a22f6e0` (seven N4 commits) in a detached worktree at `73a22f6e0`. The approved `bf90eef84` baseline was excluded. Core reference: Dash Core v24.0.0-rc.3 (`0364ed626e`), especially `src/validation.cpp` (`v`), `src/node/chainstate.cpp` (`cs`), and `src/rpc/blockchain.cpp`. No implementer worktree or branch was edited.

## Findings

1. **Moderate — D39's option (b) ruling is not recorded or tracked.** `docs/node-program/designs/CH1-node-notes.md:33`; `docs/node-program/BACKLOG.md:121` (end of table). The N4 ledger still says **Asked**, and no backlog row assigns CH5 the post-VerifyDB `ResetBlockFailureFlags(nullptr)` step. In a restart with a failed loaded tip T and tied siblings that all fail to connect, Core at its default `-checklevel=3` un-fails T before startup ABC and can reconnect it (`cs:463-465`, `v:4155-4159`). V intentionally retains the `-checklevel <= 2` outcome. That deferral is the manager's option (b), but the current ledger leaves the default-mode difference unowned. Mark D39 **Ruled: option (b)** and add a CH5 backlog row for `-checklevel`/VerifyDB and the conditional init reset, with a default-mode regression.

2. **Minor — the release-point part of D38 lacks a retained regression.** `dashd-chainstate/src/chain/actor.rs:763`; `dashd-chainstate/src/chain/tests/failure.rs:748`. The current startup test detects a missing hold, but does not detect removal of the loop that drains `held` at a later release point. A class C command deferred during the hold can run before the older held lane once startup ABC ends; its release point must serve held commands before newer lane commands. I wrote and ran the focused scratch test below. It passes the reviewed code and catches the missed mutant with order `["lane"]` instead of `["held", "lane"]`. Land the test with a test-only staging accessor (or equivalent) so this D38 ordering rule stays covered. This is a coverage finding; the production loop is correct.

No new production correctness finding in the reviewed range.

## Claims

| Claim | Judgment and evidence |
|---|---|
| `InvalidateBlock` | **Pass.** `failure.rs:35-186` follows Core `v:3889-4046`: precomputed non-active candidates, one hold per disconnect, mempool update after the disconnect, immediate `FAILED_VALID`, parent and equal-work candidate re-addition, final rescan, `InvalidChainFound`, and the RPC's ABC even after Core's valid-state `false` (`actor.rs:663-678`; `rpc/blockchain.cpp:2010-2027`). The mid-walk read, reorg, failed-disconnect, shutdown, signal, and final-rescan tests cover the distinctive branches. |
| `ReconsiderBlock` / `ResetBlockFailureFlags` | **Pass.** SB4's reset report is applied at `failure.rs:189-218`: cleared `m_best_invalid`, promoted failed children removed from candidates, and each eligible reconsidered entry offered through `TryAddBlockIndexCandidate`. `actor.rs:680-695` runs the ABC tail. This matches Core `v:4152-4230` and `rpc/blockchain.cpp:2048-2067` within the single active chainstate seam. |
| `PreciousBlock` | **Pass.** `failure.rs:220-239` and `index.rs:291-304` use the tip's work, reset the reverse counter only after work increases, saturate at `i32::MIN`, re-key `nSequenceId`, and reinsert/prune only an eligible candidate before ABC. Core is `v:3858-3887`. |
| DEC-196 (1), (2), (3), plus ordinary valid-tip control | **Pass within the stated model.** `load.rs:79-100` refuses an invalid tip above all usable candidates, retains all equal-work candidates and arms D38 only if a retained key ranks below the failed tip, and uses Core's ordinary comparator prune when a candidate has more work. A valid candidate tip follows ordinary prune; an already loaded tip returns. DEC-196 itself numbers only (1)–(3). |
| Case (2) Core reachability | **Pass with D36's sanctioned addition.** The extra intermediate members are precisely the tied keys Core's `LoadChainTip` prune would discard (`v:3583-3592`). After successful ABC the candidate set is pruned against the new tip as Core does in `ConnectTip`; the resulting active-tip/candidate form is also reachable through Core's runtime `InvalidateBlock` re-addition (`v:3990-3999`). If every tied choice fails, V's no-candidate form is reachable in Core at `-checklevel <= 2`; the default-level exception is D39. |
| D38 startup hold | **Pass in code.** `index.rs:254-262` arms only after keeping a candidate below the failed tip. `command.rs:189-198` exempts every `ActivateBestChain`, `SetLoading`, and Control command. `actor.rs:565-617,646-654,760-790` preserves FIFO per lane and drains held commands before newer lane work at release points; `actor.rs:583-592` clears them on abort. A driver that posts startup ABC without awaiting another V command has a path through the hold. The missing retained test is finding 2. |
| D39 and D35 | **D39 code pass, ledger fail.** V has no init reset and its `-checkblockindex` on/off test checks the `-checklevel <= 2` outcomes: next header aborts with the check on; with it off a child of the last connected tip activates. Core's source has those paths and the default `>= 3` reset. **D35 pass:** `failure.rs:730-741` makes a higher-work header-only fork and still refuses the failed loaded tip. |

## Mutation and scratch probes

Each production edit was one line and ran alone against its named focused test. For every case I saved the source with `cp -p`, restored it with `cp -p`, ran `touch` after restore, and checked `cmp=0`; each cargo output confirmed a new `dashd-chainstate` build. R1–R4 are my mutations, distinct from the implementer's N1–N26. G4 rechecks the specifically missed release-point mutant.

| Mutant | Focused witness | Result |
|---|---|---|
| R1: reverse precious counter increments instead of decrements (`index.rs:301`) | `precious_switches_between_equal_work_tips` | **Caught:** second precious call leaves the wrong tip. |
| R2: D38 tests only the best retained key rather than the lowest (`index.rs:257`) | `commands_wait_for_startups_abc_after_a_tied_fallback` | **Caught:** init ABC reply disconnects when the early header aborts V. |
| R3: clear `best_invalid` only when it was promoted, instead of reconsidered (`failure.rs:206`) | `reconsider_clears_the_best_invalid_block` | **Caught:** `Some(BlockId(2))` remains instead of `None`. |
| R4: precious re-adds the old tip instead of the target (`failure.rs:234`) | `precious_switches_between_equal_work_tips` | **Caught:** the equal-work switch does not occur. |
| G4: release points skip `held` (`actor.rs:763`) | Scratch `held_commands_run_at_a_deferred_commands_release_point` | **Caught:** `["lane"]` instead of `["held", "lane"]`. The original end-to-end scratch attempt missed it; the controlled release-boundary test below caught it. |

### Release-point regression to retain

I ran this scratch test against both the reviewed code and G4. It needs `V::held` exposed to crate tests (one `pub(crate)` visibility change), or a `#[cfg(test)]` staging method. It stages the state after startup ABC has lifted the hold, while a deferred class C operation reaches a release point:

```rust
#[test]
fn held_commands_precede_lane_commands_at_release_point() {
    use crate::chain::command::ChainCommand;

    let mut rig = Rig::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let expected = seen.clone();
    let v = rig.v.clone();
    rig.on_v(move |vv| {
        let held = seen.clone();
        vv.held.push_back(ChainCommand::Query(Box::new(move |_| {
            held.lock().expect("order").push("held");
        })));
        let newer = seen.clone();
        v.send(ChainCommand::Query(Box::new(move |_| {
            newer.lock().expect("order").push("lane");
        })))
        .expect("queue newer query");
        vv.release_point()?;
        if *seen.lock().expect("order") != ["held", "lane"] {
            return Err(Fatal::new("held command skipped at release point"));
        }
        Ok(())
    });
    assert_eq!(*expected.lock().expect("order"), ["held", "lane"]);
    assert!(rig.stop().fatal.is_none());
}
```

## Validation and limits

- Clean N4 head: `cargo test -p dashd-chainstate --all-features chain::` **85 passed**; default-feature `cargo test -p dashd-chainstate chain::` **85 passed**.
- `cargo clippy -p dashd-chainstate --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check bf90eef84..73a22f6e0` passed.
- The scratch release-point test passed before G4 and failed under G4; its scratch edits were removed before the clean-head suites.
- Core comparison was source-level. I did not rerun the implementer's live rc3 regtest reproduction or a complete SB4 storage reload: `Rig::restart` reuses its `HeaderChain`, rebuilds V tables, and calls `LoadChainTip` and ABC. It does not run VerifyDB, so the default `-checklevel` result is established from Core's source and the supplied rc3 reproduction, not a new differential run.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
