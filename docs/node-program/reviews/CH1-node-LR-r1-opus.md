# CH1-node-LR VerifyLoadedChainstate: Opus review, round 1 (after Sol r1)

**Target:** node/ch1-node-lr at d94c015c2. The commits reviewed are 8babac6eb..d94c015c2, first-parent. The follow-merge d759aebd9 and the BL-206/D63a dash-chain changes are out of scope.

## Verdict

**CHANGES REQUIRED.** There is one medium finding. Its fix is one condition, one test, and one sentence in D64. There are also two low findings, which are nits.

I disagree with Sol r1 on one point. Sol confirmed that "the DEC-272 prune happens only after a reset that ran". That is what the code does, but it is broader than DEC-272. A reset can run, reaching the tip in the ancestor sense, without making the tip valid again. In that case the code applies Core's prune against a tip that is not a candidate. That is the state where Core's own prune asserts.

## Findings

### F1 (Medium): the post-reset prune runs when the reset leaves a ChainLock-conflicting tip still unusable, wiping DEC-196 (2)'s fallback

**Where:**
- `dashd-chainstate/src/chain/verify.rs:164-171`
- D64 in `docs/node-program/designs/CH1-node-notes.md:59` ("There the reset makes T valid again")

**What's wrong:**
- The prune is gated on `reset_failure_flags_null()` returning true, which means the best invalid block's ancestor at the tip's height is the tip.
- It is not gated on the reset having brought the tip back.
- DEC-196 (2) and `load_chain_tip` cover a loaded tip that is FAILED *or* `CONFLICT_CHAINLOCK` (`load.rs:283-297`).
- The load-time reset runs with `ignore_chainlocks = false`. Core's ancestor walk then clears only `BLOCK_FAILED_MASK` and keeps `BLOCK_CONFLICT_CHAINLOCK` (`validation.cpp:4196-4200`).
- `TryAddBlockIndexCandidate` refuses a conflicting block (`:4241`, `index.rs` `try_add_candidate`).
- So the reset "runs", T stays a non-candidate, and `prune_block_index_candidates(T)` drops every tied valid block that D36 kept below T.
- DEC-272 says Core's reset clears the flag first, then T stays the tip and Core's prune applies, and that a reset that misses the tip prunes nothing. For a conflicting T, the flag is never cleared, so the ruling's precondition does not hold. The code still prunes.

**Scenario (reproduced):**
1. Take forks a/b/c from `three_forks(2)`, so b2 outranks a2, which outranks c2 after the load, as in the existing D64 tests. b2 is the tip.
2. d3 is a header-only child of b2 and is FAILED_VALID. It is the best invalid block. In practice this is a block on our tip that failed `CheckBlock`/`ContextualCheckBlock` in AcceptBlock (no data), or an `invalidateblock` of a header.
3. b2 is then ChainLock-conflicting in the index, and the coins are still at b2. This is DEC-196's "index written after a ChainLock reorg, the coins before it".
4. On restart, DEC-196 (2) keeps a2 and c2 below b2 (D36) and arms D38's hold.

Results:

| `-checklevel` | Outcome |
|---|---|
| 2 | VerifyLoaded returns Ok, and startup's ABC moves to **a2**. This is DEC-196 (2)'s intent. |
| 3 (the default) | The reset reaches b2 through d3 but leaves b2 conflicting and d3 data-less. The prune empties the candidate set. |

At level 3, V's thread panics:

```
thread 'dashd-v' panicked at dashd-chainstate/src/chain/index.rs:258:9:
validation.cpp:3592 assert
... init's ABC: Err(Disconnected)
```

- In a release build the `debug_assert!` is compiled out. The set stays empty and V sits on the ChainLock-conflicting b2 after startup instead of moving to a2. I worked this out by reading the code; I did not run a release build.
- If d3 has data but is genuinely invalid, the reset makes it the only candidate. ABC fails it again, and V ends in the same stuck state, because a2 and c2 are already gone.
- Core cannot reach this state: its `LoadChainTip` prune would already have asserted. So this is only about V's extension, which is exactly where DEC-272 says what to do.

**Fix:** prune only when the reset left the tip a candidate:

```rust
if options.check_level >= 3
    && self.reset_failure_flags_null()?
    && let Some(tip) = self.tip()
    && self.idx.is_candidate(tip)
{
    self.idx.prune_block_index_candidates(&self.reader, tip);
}
```

- I applied this in a scratch copy, not a worktree. With it, level 3 gives tip a2 and no fatal, the same as level 2.
- All 11 existing `chain::tests::verify` tests still pass (13/13 with the two probes).
- For a FAILED-only T it changes nothing: the reset's `try_add_candidate(T, tip = T)` always inserts T, so `is_candidate(T)` holds and the prune runs as before.
- Add the probe below as a permanent test. It also kills the mutant that drops the new condition.
- In D64, qualify "the reset makes T valid again": a ChainLock-conflicting T keeps its mark (ignore_chainlocks is false), stays a non-candidate, and keeps D36's fallback.

The probe I ran was appended to `chain/tests/verify.rs`; the helpers are the file's own:

```rust
fn probe_conflicting_tip(check_level: i64) -> (Option<Result<(), VerifyLoadError>>, Option<BlockId>, Option<String>) {
    let (mut rig, files, _) = chain(0);
    let [main, side, other] = three_forks(&rig, 2);
    assert_eq!(rig.v.precious_block(side[1].hash()).recv(), Ok(Ok(())));
    let d3 = child(&rig.p, &side[1], 2);
    rig.headers(std::slice::from_ref(&d3));
    assert_eq!(rig.v.invalidate_block(d3.hash()).recv(), Ok(Ok(())));
    let (b1, b2) = (rig.id(&side[0]), rig.id(&side[1]));
    let mut exit = rig.stop();
    exit.chain.node_index().expect("node")
        .modify(b2, |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
    exit.chain.init_active_tip(b1).expect("b1"); // index after the ChainLock reorg, coins at b2
    // before_init: v.verify_loaded(level(check_level)); then restart_with(exit, files, b2, cfg)
    // and read the tip after init's ABC: L2 -> a2; L3 -> V panics (unfixed) / a2 (fixed).
}
```

### F2 (Low, latent): nothing ties VerifyLoaded's "non-empty coins ⇒ V's tip is the coins tip" precondition, which is all L12's unreachability rests on

**Where:** `dashd-node/src/steps.rs:178-184` and `dashd-chainstate/src/chain/verify.rs:194-199`

**What's wrong:**
- Core reaches `VerifyLoadedChainstate` only after `LoadChainTip` has set `m_chain.Tip()` to the coins' best block. A miss there is `LoadChainstate`'s failure (`node/chainstate.cpp`, `validation.cpp:4992-5006`).
- `ChainSteps::load` computes `coins_empty` from the coins' own tip. It never checks that V's tip is that block: V's parts come from `open_parts` (genesis only, `MemBlockFiles`) before the coins open.
- Today the two always agree, but only indirectly. rdashd and NodeSut connect nothing (D64), and the coins recover against `NoIndex`.
- When the stored-index load or a connector lands (CH3a a10, O2b-3b), a coins tip that V's index lacks would reach `verify_loaded` with `coins_empty = false`. The future-tip check would read the wrong block (or none). The reset's `None => true` arm (the L12 survivor) would reset whatever `best_invalid` is.

**Fix:** pass the coins tip in `VerifyLoadOptions`, or check it in `steps.rs`, and make a mismatch Core's load failure. At minimum, add `debug_assert!(options.coins_empty || self.tip().is_some())` in `verify_loaded`. This makes L12 provably unreachable instead of argued. It does not need to block this checkpoint if the CH3a/O2b-3b plan carries it.

### F3 (Low, doc): D64 says rdashd reads `-checkblocks`/`-checklevel`, but rdashd still refuses both

**Where:**
- `dashd-node/src/init/support.rs:123` (`Unsupported("CH5")`)
- D64 (`CH1-node-notes.md:59`)

**What's wrong:** `ChainSteps::load` reads both options, but rdashd's support table refuses them when set. So in rdashd only the defaults (6, 3, `require_full_verification = false`) ever reach V, and the level table is exercised only through NodeSut and the V tests. That is reasonable while VerifyDB is CH5's placeholder.

**Fix:** add one clause to D64 ("rdashd refuses both until CH5; NodeSut and the V tests set them"). Or move `-checklevel` to `Implemented` if the manager wants the reset gate user-reachable now.

## Answers to the questions

**1. Core parity.** It matches, apart from F1, which is V-extension-only.
- **Order:** pruned warning, then empty-coins/evodb, then future tip, then VerifyDB mapping, then the reset at ≥3. This is `cs:412-479` and `init:2100-2112` for a single chainstate. Core's `GetAll()` loop and `ActiveChainstate()==chainstate` test reduce to that.
- **Error texts:** the texts are verbatim, including the line-continued FUTURE_TIP. node-l1 also checks Core's text.
- **Narrowing:** the reset compares the raw `i64` (`options.check_level >= 3` on `int64_t`). VerifyDB gets the `int` narrowing, and depth/level are bounded as in `v:5050-5053`. The table tests 2^32+2 and −2^32+3 both ways.
- **Future-tip boundary:** strict `>` on `i64` (u32 nTime promoted, as in C++). 7200 s is accepted and 7201 s fails.
- **Empty coins:** empty coins skip the future check, VerifyDB and the reset. A non-empty evodb gives "Error initializing block database".
- **Pruned warning:** compares `i64` against MIN_BLOCKS_TO_KEEP widened, matching Core's promotion.
- **Omitted:** the BLS-scheme lines, as documented.

**2. Post-reset prune.**
- It is Core's `PruneBlockIndexCandidates`: the same comparator key and strict-worse removal.
- It runs in the same handle right after the reset, before the ABC, which Core's prune-in-`LoadChainTip` ordering would have done earlier.
- It can remove candidates Core would keep only in V's extension, and there DEC-272 sanctions it, *except* when the reset leaves the tip a non-candidate (F1). In that case it removes D36's whole fallback and can leave the set empty.
- **CheckBlockIndex:** after a reset that restores T, the tied blocks below T are gone, so `v:5755`'s "worse than tip ⇒ not a candidate" holds; `a_reset_tip_that_outranks_its_tied_blocks_prunes_them` covers this. After a reset that misses T (the best invalid block on another branch), nothing is pruned and ABC moves off T. With F1 unfixed, the restored-but-conflicting case breaks Core's `:3592` invariant (empty set). With the fix, the fallback stays and ABC moves as at level ≤2.
- **Later invalidate/reconsider:** InvalidateBlock's whole-index candidate rebuild re-admits any pruned tied block at or above the new tip, so the prune leaves no stale state for later `invalidateblock`/`reconsiderblock`. This is exercised by the existing test's `invalidate_block(other[1])`.

**3. Concurrency.** I agree with Sol.
- VerifyLoaded is class M and on the D38 exempt list beside ABC/AttachCoins/SetLoading. The reset and the prune run in one `handle`, so nothing interleaves between them.
- Between VerifyLoaded's reply and startup's ABC nothing else is sent to V:
  - rdashd's RPC stays in warmup until step 13 (`node.rs:178-179`, `:272-274`);
  - P2P is not started;
  - `-prune`, `-txindex` and the other indexes are refused, so Core's steps 8 and 10 have no V traffic;
  - NodeSut runs the steps alone.
- In the hold case, other commands stay queued (`verify_loaded_is_served_under_the_startup_hold`).
- Nit, not a finding: unlike AttachCoins (`coins_attachable`), `ChainHandle::verify_loaded` has no misuse latch. A second VerifyLoaded after startup's ABC would rerun the reset and prune outside Core's init. Only `steps.rs` sends it today.

**4. dashd-node wiring.**
- **Options:** read before the load (`steps.rs:136-139`), with `IsArgSet` for `require_full_verification`.
- **Order:** V starts, the coins attach, VerifyLoaded is sent only if the attach succeeded (`opened.and_then`), and `import()` (SetLoading plus the startup ABC) comes after `load` returns, which is Core's step 7 before step 11.
- **Status mapping** (`verify_failure`):
  - Failure → `Recoverable`: the reindex question, or SHUTDOWN_REQUESTED if a shutdown is pending, matching `init:2119-2133` with `-reindex` refused.
  - Interrupted → `Interrupted`.
  - Insufficient dbcache → `Incompatible`, a plain InitError (`init:2115-2117`).
- A V that disconnects while serving the command maps to `Interrupted` after `chain.stop()`, the same as the attach and as Core's AbortNode-then-ShutdownRequested.
- F2 is the one gap (latent).

**5. Tests and mutants.**
- The 11 V tests and 6 node-l1 sessions cover the Core-reachable space well: the level table, narrowing, empty coins/evodb, the future boundary, the VerifyDB mapping, the hold exemption, and restore versus miss.
- **Gap:** no test has a ChainLock-conflicting loaded tip under VerifyLoaded (F1). Add the probe.
- **L12:** it is unreachable in today's production and NodeSut paths, so I agree with Sol on the fact. But the reason is the indirect pairing described in F2, not an invariant the code checks. With F2's assert, L12 becomes provably dead, so the survivor can be dropped instead of argued away.
- I did not run node-l1: Core cannot reach F1's state (its `LoadChainTip` asserts first), so a Core session would add no evidence.

## Evidence

- **Scratch copy:** `git archive d94c015c2` into `/work/scratch/ch1-lr-r1-opus/probe`, with the probe test appended. This was not a worktree, and nothing was committed.
- **Unfixed:** `cargo test -p dashd-chainstate --lib chain::tests::verify`: 12 passed and 1 failed. `probe_conflicting_tip_level_2` gives tip a2. `probe_conflicting_tip_level_3` panics V with `validation.cpp:3592 assert`.
- **With the `is_candidate(tip)` gate:** 13 passed. Level 3 gives tip a2 and no fatal.
- **Not rerun:** `cargo test -p dashd-node` and fmt, because Sol r1 ran them and the in-scope node code did not change. I did not run node-l1 and did not touch any live oracle.
- **Cleanup:** the review worktree, the probe copy and the target are removed.
