# ND1 persistence (2b-2 on CH2b b4), Opus-h review round 2

**Head reviewed:** 849ae5f0c on `node/nd1-2b2-persist` (local), a follow-merge of `node/train-21` aef857144
**Reference:** Dash Core v24.0.0-rc.3 (`git show v24.0.0-rc.3:…`)
**Scope:** `2b5bf2a7a..849ae5f0c`:
- a618b8298, the DEC-289 fixes;
- 96656561d, the validator follow-up;
- 849ae5f0c, the merge.

Read-only. Round 1: `docs/node-program/reviews/ND1-persist-r1-opus.md`. Ruling: DEC-289. Implementer notes: `/work/scratch/nd1/plan-2b.md` 2b-D32.

## Method

I read these sources:

- `git diff 2b5bf2a7a 96656561d` (steps.rs, chainstate.rs, sut.rs).
- The merge, three ways:
  - `git show --remerge-diff 849ae5f0c`;
  - `load` at aef857144 against `load` at the head;
  - `git diff aef857144 849ae5f0c --stat`. That net delta is the same six dashd-node files as the branch's own delta over ea790e9bf.
- In dashd-chainstate:
  - `chain/load.rs` (`load_chain_tip`, `check_startup_readiness`);
  - `chain/verify.rs` (`verify_db`);
  - `recovery/{mod,replay}.rs` (`Meta`, `consistent`, the marker leg, `replayed`);
  - `store/meta.rs`;
  - `store/blocks.rs` (`NodeBlockFiles::read_block`).
- In Core:
  - `node/chainstate.cpp:183-277` (LoadBlockIndex → genesis/devnet-genesis checks → LoadGenesisBlock :210 → NeedsUpgrade :244 → ReplayBlocks :251 → InitCoinsCache :256 → LoadChainTip :272-277);
  - `:420-470` (VerifyLoadedChainstate: the future-tip check, then VerifyDB, then CORRUPTED_BLOCK_DB → "Corrupted block database detected");
  - `validation.cpp:4992-5018` (LoadChainTip);
  - `:5030-5090` (VerifyDB, level 0 `ReadBlockFromDisk`);
  - `:5180-5300` (RollforwardBlock / ReplayBlocks);
  - LoadGenesisBlock, a no-op when genesis is already indexed;
  - `init.cpp:2035-2037` (the catch → "Error opening block database").

I ran the tests in a detached worktree (`/work/scratch/nd1-persist-r2-review/wt`, its own target/, since removed):

    cargo test --locked -p dashd-node --features test-support
    #   lib 137 passed (1 ignored), asserted_log_lines 11 passed, index_scan_cache 0 (2 ignored). Exit 0.

The mutants ran on `steps.rs` (lib tests only), and the file was restored after each one:

| Mutant | Result |
|---|---|
| A: `LoadChainTip` skipped (the tip filtered out) | **Killed.** 3 tests fail: restart, INVALID_TIP, readiness. |
| B: devnet guard off | **Killed.** `a_devnet_restart_over_stored_coins_is_refused` fails. |
| C: readiness call replaced by `Ok(())` | **Killed.** `a_stored_tip_the_build_cannot_judge_is_refused_at_startup` fails. |
| D: devnet guard fires even when `open_parts` refused | Survives (n2) |
| E: recovery's replayed blocks not passed to the guard (tip only) | Survives (m2) |
| F: guard keyed on `meta.tip` only, without the `head_blocks` branch | Survives (m1) |

No test was lost in the merge. The `#[test]` names at the head are the union of both parents' (133 ∪ 136 = 137).

## Answers to the review questions

### 1. Are the r1 findings closed as DEC-289 asks?

| r1 | Ruling | State at 849ae5f0c |
|---|---|---|
| B1 | BL-271; label the b1 assertion | **Closed.** `sut.rs:803` (doc: "persistence through V's own flush only … BL-271"). `:827-831` cites init.cpp:383/:453 and labels both b1 asserts "BL-271: Core's b2". |
| M1 | fail loudly on a devnet restart | **Closed.** `steps.rs:182`, `:191`, `:439`. Mutant B is killed. See Q2. |
| m1 | call `check_startup_readiness` | **Closed.** `steps.rs:216-236`, with `NodeEvoHooks` shared with the connector (`chainstate.rs`). Mutant C is killed. See Q3. |
| m3 | test the INVALID_TIP row | **Closed.** `an_invalid_stored_tip_fails_load_chain_tip_after_the_coins` (`sut.rs:844`) checks the reindex question, `INVALID_TIP` logged and 2 flushes. Mutant A kills it too. |
| N1 | read b1 back | **Closed.** It is decoded from the stored record's blk file at `data_pos`, and its hash is compared. |
| N2 | LEFT_IBD is ours | **Closed.** In `left_ibd_at_b1`'s doc. |
| N3 | cite :272-277 | **Closed.** In `load`'s Core line. |
| M2, m2 | BL-272, BL-273 | Not re-raised. |

### 2. M1's refusal: precedence, fresh datadirs, misses

**The precedence is right.** The guard sits after `opening.open()` (steps 0–3, including Core's NeedsUpgrade refusal) and after `open_parts`:

- LoadBlockIndex, :183;
- the wrong-genesis and wrong-devnet-genesis checks, :191-197;
- BL-127's blk-file check, inside "Error loading block database".

It sits before V starts (V's devnet step is LoadGenesisBlock, :210) and before `recover` (ReplayBlocks, :251). On a stored datadir genesis is already indexed, so Core's LoadGenesisBlock is a no-op and the guard masks nothing there. The refusals Core reports after the guard's point are:

- ReplayBlocks' "Unable to replay blocks";
- LoadChainTip's "Error initializing block database".

Core refuses in both cases too, so only the text differs, and that falls inside the deviation DEC-289 accepted. The guard returns before `self.chainstate` is set, so the teardown flushes nothing. The test asserts that (`flushes() == 0`).

**On a fresh devnet datadir it can't fire.** Both `meta.tip` and `head_blocks` are absent. The test's two `restart(Clean)` loads show it, and so does the load after the tip is erased.

**It misses no stored chainstate.** The guard has to catch every case in which `recover` would leave a tip, because on a devnet `parts` is `None`, so `LoadChainTip` would be skipped silently, which is r1's M1. Recovery yields `tip: Some` only from one of these:

- `consistent()` with `meta.tip`, at `replay.rs:410-416`;
- the marker leg, at `new`.

Both are in the guard's disjunction. Cases that don't fire the guard are caught elsewhere:

- A malformed marker (`read_at_open`'s lenient case) reads as `head_blocks: None`, so the guard doesn't fire. Recovery then refuses with Core's "unknown inconsistent state".
- A non-empty `evo_tip` with no tip is Corruption.

Neither case starts V with an unloaded tip. An index entry without a coins tip is Core's empty-coins path: the ABC runs from genesis, as in Core. The `head_blocks` branch is untested, though (m1).

### 3. m1: the readiness guard after `load_chain_tip`

**The implementer is right that CH3a's sentence can't hold.** `load.rs:336` says "after `load_chain_tip`, before recovery's replay". In Core, ReplayBlocks (chainstate.cpp:251) precedes LoadChainTip (:272-277), and the active tip exists only after :274's `SetTip`. The accepted order (2b-D18) is Core's, so a call that needs the loaded tip can't also precede the replay.

**Running it after the replay also loses nothing.** Core's RollforwardBlock runs `ProcessSpecialTxsInBlock` (validation.cpp:5196-5203), so in Core the replay touches evo state. Here it doesn't, for two reasons:

- The port's replay is coins-only. `recovery/` is generic over the engine and the files and takes no `EvoHooks` at all.
- The flush that wrote the marker had already written the evo state at `new` (`replay.rs:356`).

So no hook the guard protects runs during the replay. A refusal after it leaves `tip = new`, `evo_tip = new` and no marker (`:296-297`), the state a complete build resumes from.

Running it after also keeps Core's message order. A bad block on the transition's forward leg is reported with ReplayBlocks' own text before the guard could report it as a store read. CH3a should fix the doc sentence to "after `load_chain_tip`, over the active tip and recovery's replayed blocks, before init's ABC" (n1).

The order of the call is right: replayed blocks, then the tip once (R2). A replayed hash the index lacks maps to FAILURE. It can't happen, because the replay ran on that index.

### 4. An unreadable stored tip block: "Error opening block database" here, VerifyDB in Core

**This can be a backlog row. It needn't be fixed now.**

- **Outcome.** For a tip above genesis, both nodes refuse at init with FAILURE and the reindex question.
  - Here: `LoadFailure::Store` → `OPEN_BLOCK_DB_FAILED`, the cause logged.
  - Core: VerifyDB level 0 → CORRUPTED_BLOCK_DB → "Corrupted block database detected". Core logs `Verification error: ReadBlockFromDisk failed at %d, hash=%s`.
  - Core's VerifyDB always reads the tip once the tip has a parent: `nCheckDepth <= 0` means all of them (:5052). Under prune, a block without data is skipped in both nodes, since the guard needs `HAVE_DATA`.
  - `NodeBlockFiles::read_block` returns an error and doesn't call `AbortNode`, so there is no double report.
- **Only the text differs, plus two narrow orderings:**
  - **A stored tip at genesis.** Core's VerifyDB returns at once (`pprev == nullptr`), and LoadGenesisBlock doesn't read when genesis is indexed, so Core starts. The guard reads genesis's data (`g1_rule` can't decide on a block with no parent), so an unreadable genesis record refuses here. Corruption of genesis only, and a regtest-ish state today.
  - **Core's future-tip check** (chainstate.cpp:423) comes before VerifyDB, and so before Core's read. Here the read comes first.
- **It shouldn't be "fixed" by deferring the read error to VerifyDB yet.** `verify_db` is still a stub that returns `Success` (`verify.rs:189-195`, CH5). Deferring today would let a node start on a tip it can't read, which is worse than either message. The right fix belongs with CH5's VerifyDB. Once level 0 reads the tip, a guard read error should defer to VerifyDB, which gives Core's text and log line and Core's genesis behaviour. That makes it a BL row on CH5 (or LR), cross-referenced from 2b-D32's R4 (m3).

### 5. The merge

**Nothing is lost.**

- `load` keeps everything from both parents:
  - from ND1: the real attach (`attach_parts(&params, hub, tuning)` → `chain.attach_coins(coins)`, `steps.rs:250-262`), and the NodeFlush/CoreConnector wiring in `chainstate.rs`;
  - from CH1-node-LR: the parsing of `-checkblocks`, `-checklevel` and `require_full_verification` before the load; `coins_empty` from the recovered tip; `ui.init_message("Verifying blocks…")`; `verify_loaded` with its `AbortNode` → `chain.stop()` → Interrupted path; `verify_failure` and its unit test.
- The order is replay → LoadChainTip → readiness → V → attach → VerifyLoaded, which is chainstate.cpp:251/:272 then init.cpp:2100. `MemBlockFiles` is gone, correctly, since ND1 replaced it with `NodeBlockFiles`.
- `ui` is the init `Ui` in both `node.rs` (`&self.ui`) and `sut.rs`. `tuning` is `RocksTuning::default()` in production and `MemoryTuning` in the SUT, matching what each parent passed.
- The `#[expect(clippy::too_many_arguments)]` carries a reason. The test-name union holds.
- The implementer flags stale docs at `chain.rs:13/:262` as a follow-up. They are docs only.

## Findings

### B (blocking)

None.

### M (major)

None.

### m (minor)

**m1. The guard's transition-marker branch has no test (mutant F survives).** `dashd-node/src/steps.rs:182`; the test at `sut.rs:873`.
- The test writes only `meta.tip`. Drop `|| meta.head_blocks.is_some()` and everything passes.
- **Scenario.** A devnet is killed mid-flush, leaving a marker and no tip. Without that clause, recovery replays to `new`, `LoadChainTip` is skipped because a devnet's parts are already V's, and step 11's ABC fails fatally from genesis onto coins at `new`. That is exactly r1's M1.
- The code is correct. Only its test is missing.
- **Fix.** A second case in `a_devnet_restart_over_stored_coins_is_refused` that writes a `head_blocks` value (`new ‖ 0`) instead of a tip.

**m2. The readiness guard over recovery's replayed blocks has no test (mutant E survives).** `steps.rs:219-229`; the test at `sut.rs:905`.
- The only readiness test runs without a marker, so `replayed()` is empty. Checking the tip alone passes everything.
- **Fix.** Plant a `head_blocks` marker with `old = genesis` and `new = b1`, then restart with `-dip3params=1:1`. The refusal must name b1 (the forward leg's block, also the tip) and come after "Rolling forward". Better still, use a two-block leg whose first block alone trips G1, which separates "replayed checked" from "tip checked".

**m3. Record the unreadable-tip difference as a backlog row on CH5's VerifyDB (Q4).** `steps.rs:231` → `load.rs:368`.
- What differs: the message ("Error opening block database" vs "Corrupted block database detected") and Core's `Verification error:` log line. A stored genesis tip with unreadable data refuses here and starts in Core.
- The refusal is the same everywhere else. Don't defer the read error while `verify_db` is a stub.
- **Fix, when CH5 fills `verify_db`.** Make a guard read failure defer to VerifyDB, and add the NodeSut row.

**m4. BL-271 can't land before CH1's devnet path (2b-D8) without breaking every devnet restart.** `steps.rs:182-192`.
- Today a devnet's coins tip is stored only if one of V's own flushes fires (PERIODIC's 24 h coins write, or cache pressure). The shutdown flush is still the stub, so a clean devnet restart loads.
- Once BL-271 wires the shutdown flush, every clean stop stores `tip = devnet genesis` (height 1, V's tip after the first start, as the test notes). Every second start of every devnet would then be refused with "start the devnet in a new data directory".
- **Fix.** Record the ordering: BL-271 depends on 2b-D8, or ships with it. Put the dependency in BL-271's row and in the `devnet_restart` doc. Or flip the restart test's devnet twin when BL-271 lands, so the dependency fails loudly in CI.

### N (nits)

**n1. CH3a's doc sentence** (`dashd-chainstate/src/chain/load.rs:336`, "before recovery's replay") should read "over the active tip and recovery's replayed blocks, after `load_chain_tip` and before init's ABC". Route it to CH3a, as 2b-D32 says.

**n2. R1's precedence has no test (mutant D survives).** A devnet datadir with a stored tip and a wrong devnet genesis, or a missing blk file, should report the index load's refusal ("Incorrect or no devnet genesis block found…" or "Error loading block database"), not the devnet guard's. One case on top of the existing devnet test would pin it.

## Verdict

**ACCEPT.** The head may ride as DEC-289's recorded deviation.

- Every finding DEC-289 required is closed:
  - B1's labelling;
  - M1's loud devnet refusal;
  - the m1 readiness call;
  - the m3 INVALID_TIP test;
  - N1–N3.
- r1's three mutants (skip LoadChainTip, guard off, readiness off) are all killed.
- The guard's precedence matches Core's init order. It can't fire on a fresh devnet datadir and misses no stored chainstate.
- The readiness call's order is Core's, and the CH3a doc sentence is the part that's wrong.
- The unreadable-tip message difference is a backlog row, not a reject-path change, apart from the genesis edge.
- The merge lost nothing from either parent.

m1 and m2 are test gaps on code that is correct. They are cheap, and I'd take them before READY. m3 and m4 are backlog and sequencing records. No possible security note.
