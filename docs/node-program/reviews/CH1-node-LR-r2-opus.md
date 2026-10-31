# CH1-node-LR: Opus review, round 2 (narrow)

**Target:** node/ch1-node-lr at f136f8738. **Reviewed:** d94c015c2..f136f8738, first-parent:
- 320957698: F1/F2/F3, the misuse latch and the D64 status.
- f136f8738: `a_second_verify_loaded_aborts_the_node`.

The r1-accepted code outside this range was not re-reviewed.

## Verdict

**ACCEPT WITH NITS.**
- F1, F2 and F3 are closed, and so is Sol's latch nit.
- The latch breaks no legitimate call order, and I found no new defect in the fixed code.
- One nit: D64 gives the wrong reason why L21 is equivalent. L21 is equivalent in production for a different reason. In the V test rig it is **not** equivalent, because of a pre-existing rig artifact (N1).

## Findings

### N1 (Nit, doc + rig): D64 says L21 is equivalent because `load_chain_tip` already pruned against a candidate tip. That prune is skipped when V's chain already sits at the coins tip, which is the rig's same-tip restart.

**Where:**
- D64 (`docs/node-program/designs/CH1-node-notes.md:59`): "L21 … is equivalent, since `load_chain_tip` already pruned against a candidate tip (`load.rs:321`)".
- `dashd-chainstate/src/chain/load.rs:303-305`: Core's early return in `LoadChainTip`, taken when the tip already equals the coins' best block.
- `dashd-chainstate/src/chain/tests/support.rs:576-596`: `restart_with` reuses `exit.chain` at its old tip, after `NodeBlockIndex::load`. That load re-adds every valid block with data as a candidate (`index.rs:486-490`, with `tip = None`).

**What's wrong:**
- The prune at `load.rs:321` runs only when the early return does not.
- In production the early return cannot fire at startup. A freshly built or loaded `HeaderChain` has `tip: None` (`dash-chain/src/chain.rs:583`). Only `init_active_tip` and connect/disconnect set it, and Core's `m_chain` is likewise empty at `LoadChainTip`.
- So for rdashd and NodeSut the argument reduces to this, and the L21 verdict stands:
  - the load pruned against a candidate tip;
  - `try_add_candidate` never adds a block worse than the tip (`index.rs:245-249`), and that covers the reset's `after_reset`, the devnet step and everything else between the load and `VerifyLoaded`;
  - so a prune when the reset did not run is a no-op.
- In the V rig, a restart whose coins tip equals the stopped V's tip takes the early return. That leaves the ancestors (and genesis) as candidates worse than the tip, and L21 is observable there.

**Scenario (reproduced; scratch probe, not committed):** `chain(2)` (genesis, a1, a2), then `restart_verified(.., &main[1], level(L), ..)`.

| Level | Before `VerifyLoaded` | After init's ABC | Fatal |
|---|---|---|---|
| 2 | a1 is a candidate | a1 is still a candidate; tip a2 | none |
| 3 | a1 is a candidate | a1 is still a candidate; tip a2 | none |
| 3, with L21 applied | a1 is a candidate | a1 has been pruned | none |

- In the unmutated runs, an explicit `check_block_index()` via `TestOnV` after init returns `CheckBlockIndex: a block worse than the tip, or with an unseen parent, is a candidate at entry 0`.
- No fatal appears because init's ABC connects nothing, and Core's ABC skips `CheckBlockIndex` when no block connected.
- The `chain::tests::verify` suite still passes 16/16 with L21 applied. So no test sees the difference, but the claim as written is false for the rig.

**Impact:**
- None in production.
- In the rig, every same-tip restart (several `verify` and `failure` tests) starts from an index state that Core's `CheckBlockIndex` rejects. The state is cleaned up only once some ABC step connects and prunes.
- No fixed-range test depends on it. The F1 test deliberately avoids it with `init_active_tip(b1)`.

**Fix:**
1. Correct D64's L21 sentence to: "equivalent at startup: a loaded `HeaderChain` has no tip, so `load_chain_tip` pruned against a candidate tip (`load.rs:321`), and `try_add_candidate` adds nothing worse than it since".
2. Separately, as a backlog item outside this range, either:
   - have `Rig::restart_with` present a tip-less chain, as SB4's reload would; or
   - document that the rig's same-tip restart skips the load prune.

## Answers to the questions

### F1: closed

- `verify.rs:172-183` now also requires `self.idx.is_candidate(tip)`, which is what I asked for. It matches DEC-272 as amended at 14:35Z: the prune runs only when the reset leaves the tip a candidate.
- **The test is not vacuous.** `a_conflicting_tip_the_reset_reaches_keeps_its_tied_fallback` is my probe as a permanent test, run at levels 2 and 3: tip a2, `Ok(())`, no fatal.
  - With the gate removed (L23), the test fails at level 3: V panics at `index.rs:258` (`validation.cpp:3592` assert), and the test's reply `expect` panics at `verify.rs:82`.
  - So at level 3 the reset really reaches b2 through d3, and b2 really stays a non-candidate.
- **No change for a FAILED-only tip.** The reset's `after_reset` → `try_add_candidate(T, Some(T))` inserts T, so `is_candidate(T)` holds and the prune runs as before. `a_reset_tip_that_outranks_its_tied_blocks_prunes_them` still passes.
- **D64 text:** it now qualifies "makes a FAILED T valid again" and describes the conflicting case with the right Core citations (`v:4196-4200`, `:4241-4243`).

### F2: closed for this checkpoint

- `debug_assert!(options.coins_empty || self.tip().is_some(), "cs:277 assert")` sits at the top of `verify_loaded`, after the latch and before any branch. That makes the reset's `None => true` arm (L12) unreachable from a load in debug builds.
- D64 hands the real coins-tip/V-tip agreement check to CH3a a10 / O2b-3b. That is acceptable, and it is the fix r1 said may wait for that plan.
- The assert cannot fire on a legitimate path: non-empty coins reach V only through `load_chain_tip`, which sets the tip.

### F3: closed

D64 now says rdashd refuses `-checkblocks`/`-checklevel` until CH5 (`init/support.rs:123`), and that NodeSut and the V tests set them.

### Latch: correct; it breaks no legitimate order

- **Mechanism:** `verify_loadable` is cleared in `composite` (`actor.rs:1375`), exactly where D51's `coins_attachable` is cleared. It is also cleared by the first `VerifyLoaded`.
- **rdashd and NodeSut** (`steps.rs:136-201`): V starts (the devnet step runs in `serve` through `load_devnet_genesis`, not through `composite`), then AttachCoins, then one `VerifyLoaded`, then `import()` (SetLoading plus the startup ABC).
  - Any class C operation before `VerifyLoaded` would already have tripped D51's coins latch at the attach before it. The new latch therefore adds no constraint on what precedes `VerifyLoaded`.
  - `NodeSut::restart` builds a new `ChainSteps` and a new V (`sut.rs:219`), so its restarts never send a second `VerifyLoaded` to one V.
  - `load` is documented as running once, and `-reindex` is refused, so Core's `do { … } while (!fLoaded)` retry has no counterpart here that would re-send it.
- **Tests:**
  - The `TestOnV` save/restore (`actor.rs:1245-1252`) keeps the rig's `before` probes from closing the latch.
  - No probe calls `V::verify_loaded` directly inside `TestOnV`; I checked with grep. A probe that did would have its latch closure undone by the restore. That would be a test-only hazard, not a defect.
  - Both misuse tests check the fatal text and the abort sink: a second `VerifyLoaded` before init, and one after startup.
- **Mutants:** L24–L27 cover the latch check, its closing, the class C clear and the probe restore. The implementer reports all of them caught; I did not rerun them.

### L21: equivalent in production, but not for the stated reason (N1)

- The mutant prunes whenever the tip is a candidate, whether or not the reset ran.
- **At startup:**
  - With no best-invalid block, or one that is not ahead of the tip, the reset changes nothing.
  - The load's prune already ran: a fresh chain has no tip, so the early return at `load.rs:303` cannot fire.
  - `try_add_candidate` never adds a worse block.
  - So the prune is a no-op, and L21 is equivalent for rdashd and NodeSut.
- **In the rig:** same-tip restarts skip the load prune, and L21 does change the candidate set there. The probe shows a1 pruned at level 3. No test observes it.

## Evidence

- **Worktree and target:**
  - Detached worktree of f136f8738 at `/work/scratch/ch1-lr-r2-opus/wt`.
  - Target reflinked from `/work/scratch/ch1-node/lr-wt/target`.
- **`cargo test -p dashd-chainstate --lib chain::tests::verify`**, unmodified apart from scratch probes:
  - 14 branch tests pass, plus my 2 probes.
  - With L23 (gate dropped): `a_conflicting_tip_the_reset_reaches_keeps_its_tied_fallback` FAILED, with V panicking at `index.rs:258`.
  - With L21: 16/16 pass, and probe 2 shows a1 pruned at level 3 only.
- **Probes** (kept with the review as `verify-tests-with-probes.rs`; not committed):
  - `r2probe_early_return_candidates` (`three_forks(2)` restarted at a2): every block is a candidate before `VerifyLoaded`, and init's ABC moves to b2, the tied block that ranks first after the reload.
  - `r2probe_linear_same_tip`: the N1 table, plus the explicit `CheckBlockIndex` failure. This used a scratch-only `pub(crate)` on `V::check_index`, since reverted.
- **Full suites** on clean f136f8738, with the probes reverted:
  - `cargo test -p dashd-chainstate --lib`: 568 passed, 0 failed (659 s at load ~30–110).
  - `cargo test -p dashd-node`: 117 passed with 1 ignored, then 11 passed, then 0; no failures.
- **Not run:** node-l1 (out of range; Core cannot reach F1's state), PF0, and the mutant script itself.
- **Cleanup:** the worktree and target are removed; review.md and the probe file are kept.
