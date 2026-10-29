# O9b S1 (`-consensusxcheck`) — code review r1 (Opus)

- Branch `node/o9b`, worktree `/work/home/agent/workspace/rdc-wt/o9b` (head a2a57f4d8, frozen; read-only).
- Range reviewed: `a47d4b5b6~1..cbe746e8c` (a47d4b5b6, 71d457a6f, cbe746e8c; all three `%G?` = G).
- Sources: PLAN-O9b.md (S1 row, §6, Q9), DEC-697, O9b-notes.md D1–D5, CH3a D189, Core v24.0.0-rc.4 `src/init.cpp`, `src/util/system.cpp`.
- No build. Box load ~170; everything below comes from reading the code. No reflink copy was made.

## Verdict: **ACCEPT-WITH-NITS** (0 M / 3 m / 4 n)

The abort path is sound. A memo mismatch is a `Fatal` that reaches `AbortNode` before `UpdateCoins`, and nothing maps it to `Invalid`. The flag is off by default everywhere and registered exactly as Core registers `-checkblockindex`. `planted-bugs` can't leak into a product build or into other crates' tests.

What's wrong is the evidence:
- The positive control's "nothing committed" assertion is vacuous (m-1).
- The node-cache half of the wiring has no test, although the plan asked for one (m-2).
- The plan's doc-update deliverable was skipped (m-3).

None of these is a correctness defect in the shipped code, so they are minors, but m-1 and m-2 should be fixed before READY: the slice's whole point is evidence that the flag reaches every check it claims to turn on.

---

## Minor

### m-1. The positive control's "nothing committed" assertion can't fail
`dashd-node/src/sut.rs:1141`

```rust
assert_eq!(sut.coins_tip().unwrap_or(tip.hash()), tip.hash(), "nothing committed");
```

`NodeSut::coins_tip` → `ChainSteps::coins_tip` (`steps.rs:467`) → `Chainstate::tip` (`chainstate.rs:497`), which returns `self.recovered.tip`. That is the tip **recovery found when this open started**, and it never moves afterwards. `sut.rs:1225` relies on exactly that meaning ("the coins' tip" after a restart).

In the test, the session opens a fresh datadir, restarts once before mining, then mines 102 blocks, so the recovered tip is genesis or `None`:
- If it were `Some(genesis)`, the assertion would compare genesis with block 102 and fail. The implementer reports it passing.
- So it is `None`, and the assertion reduces to `tip.hash() == tip.hash()`.

Either way it says nothing about block 103.

**Surviving mutant:** in `CoreConnector`'s Fatal path, commit the block's overlay into the coins cache before returning the `SystemError`/`Fatal`, so the next flush would persist block 103's coins. `tip_watch` still doesn't move, the `*** consensusxcheck` line still prints, and line 1141 still passes. Only the `dashd-chainstate` connector matrix (`chain/tests/connector.rs:1402`) would catch it; the node-level control, which is the thing S1 adds, wouldn't.

**Fix:** after `wait_for("AbortNode ran", …)`:
1. Run the node's shutdown (`sut.restart(Teardown::Clean)`, or `sut.steps.shutdown()`), so whatever flush the abort-then-shutdown path does has happened.
2. Then assert both:
   - `stored_tip(&sut) != Some(block.hash())` (the store's meta `tip`, `sut.rs:1176`);
   - on the reopened session, `sut.coins_tip() != Some(block.hash())` and `tip_watch() != Some(block.hash())`.

Drop the `unwrap_or`. Under BL-271 the shutdown flush is still the WAL-sync stub (`sut.rs:1221-1224`), so add a one-line comment saying that the check becomes load-bearing once BL-271 lands.

### m-2. `cache.set_xcheck(setup.xcheck)` has no test; the plan's prefetch-hit test was dropped without a D-row
`dashd-node/src/chainstate.rs:432`

PLAN-O9b's S1 row lists "a prefetch-hit xcheck test through the node's cache" among S1's tests. O9b-notes D2 explains that the node has no prefetch producer until CH4, but it doesn't record that the test was dropped. Nothing in the tree reads the cache's flag through the node.

**Surviving mutants (all three survive every test in the reported evidence):**
- delete line 432;
- `cache.set_xcheck(false)`;
- `cache.set_xcheck(true)`.

That is exactly the wiring CH4 is meant to inherit (DEC-694 Q13).

**A producer isn't needed for the test.** `CoinsCache::admit_prefetch` (`coins/cache.rs:586`) and `PrefetchBatch { fetch_gen, epoch, entries }` (`coins/prefetch.rs:31`) are both `pub`, and `ChainSteps::coins_cell()` is available under `test-support`.

**Fix:** add a `sut::tests` case that, on V, as `tip_coins` does (`sut.rs:442-447`):
1. takes the cell;
2. admits a batch with the current `released_gen()` and `write_epoch` holding a **wrong** `PackedCoin` for a mature coinbase outpoint;
3. calls `fetch`.

Expected results:
- default (flag soft-set on): `Err`, with the `consensusxcheck: the prefetched coin differs from the database's` corruption;
- `-noconsensusxcheck`: the wrong coin comes back.

That kills all three mutants. If the manager prefers to defer the test to CH4, add a D6 row that says so and names the surviving mutant, and give CH4's plan an acceptance item for it.

### m-3. The plan's "update CH3a D189's 'until O9b' text" deliverable wasn't done
- `docs/node-program/designs/CH3a-notes.md:130` (D189) still says "It is off in the node until O9b wires the flag (PLAN O9b)".
- `dashd-chainstate/src/connect/connector.rs:67-68` still says "on in the L2 replay and `NodeSut`; O9b wires the node's flag (CH3a D189)".

The S1 row names this doc update explicitly.

**Fix:** reword both to the present state:
- D189: "off by default in the node; `-consensusxcheck` (O9b S1, O9b-notes D1–D3) turns it on through `CoinsSetup::xcheck`."
- `connector.rs`: "the node's `-consensusxcheck`, via `CoinsSetup::xcheck`."

---

## Nits

### n-1. The help-category assertion lets most wrong categories through
`dashd-node/src/init/support.rs:309`

Only `at > "Debugging/Testing options:"` is checked. Core prints categories in enum order (`util/system.cpp:661-715` at rc.4): `DEBUG_TEST` is followed by `NODE_RELAY`, `BLOCK_CREATION`, `RPC`, and so on.

**Surviving mutant:** `OptionsCategory::DebugTest` → `OptionsCategory::NodeRelay` (or `Rpc`). `-consensusxcheck` would then be listed under "Node relay options:", after the Debugging/Testing header, and the test still passes.

**Fix:** also assert `at < debug.find("Node relay options:").unwrap()`, or slice the Debugging/Testing section out and search within it.

### n-2. The help text promises more than S1 checks
`dashd-node/src/init/support.rs:66`

The text reads "Check every optimized consensus path against its reference". Today the flag covers:
- the connect memo (L1);
- the prefetch-hit read, which has no producer in the node (D2).

The DML twin (EV2 §6.7) isn't wired until CH7a. An operator reading `-help-debug` would assume the evo path is cross-checked.

**Fix:** either word it to match ("Cross-check the connect loop's coins reads against direct reads while validating, and stop the node on any difference (default: 0)") and widen it when CH7a wires the twin, or keep the text and add a D-row saying the text is the end state.

### n-3. `CoinsSetup::planted` / `NodeSut::planted` accept any of the 68 `Planted` variants, but only two do anything
`dashd-node/src/chainstate.rs:390`, `:453`

`connector.plant` only has an effect for L1 and T12 (`connect/serial.rs:1207-1211`). Cache bugs such as X10 (the natural prefetch-xcheck control for m-2) are armed through `CoinsCache::plant`, which this path never calls.

**Failing input:** a later test sets `sut.planted = Some(Planted::X10)`, expects xcheck to fire, and sees nothing. That reads as "xcheck misses X10" rather than "the bug was never armed".

**Fix:** either
- `debug_assert!(matches!(setup.planted, None | Some(Planted::L1 | Planted::T12)), …)` and change the doc comment from "a `Planted` bug" to "a connector bug (L1, T12)"; or
- also call `cache.plant(bug)` for cache-side variants.

### n-4. The FT harness isn't covered by S1, and nothing tracks it
PLAN Q9 / DEC-697 (9) say "L2, NodeSut and the FT harness pass it explicitly". L2 (`conformance/src/digest/replay_core.rs:346`) and NodeSut do. `contrib/node/func-compat/` has no `-consensusxcheck`.

The S1 row doesn't list the FT harness, so this isn't an S1 defect, but no D-row or BL row owns it either.

**Fix:** a one-line D-row in O9b-notes naming the owner (O14 / func-compat), or a BL row.

---

## What I checked

### 1. Abort path (focus 1)

**The flag reaches the memo.**
1. `coins_configuration` reads `get_bool_arg("-consensusxcheck", false)` (`node.rs:605`).
2. `CoinsSetup.xcheck` → `ConnectorParts.xcheck` (`chainstate.rs:446`) → `SerialCore::set_xcheck` (`connector.rs:91`).
3. `let xcheck = self.xcheck` (`serial.rs:756`), copied into the control closure → `TxCoins::resolve(…, xcheck)` (`serial.rs:853`).

There is one `CoreConnector::new` in the node (`chainstate.rs:450`), and both Node (`node.rs:251`) and NodeSut (`sut.rs:339`) build their setup through the same `coins_configuration`. There is no second node connect path that bypasses it.

**Every memo read is compared.** `TxCoins::read` (`tx_coins.rs:117-140`) compares after the index lookup, whether the cursor hit (`next`) or it fell back to `position`. It covers every consumer:
- `check_tx_inputs` (`dash-consensus/src/tx/inputs.rs:72`, `view.coin(…).map_err(ViewOr::View)?`);
- the `prev_heights` `access` (`serial.rs:895`);
- `transaction_sigop_count` (`serial.rs:908-916`);
- `input_scripts` → `spent.copy` (inline and queued; the copy runs on V before queueing).

The planted L1 skew is applied before the compare, so the compare sees the skewed coin. The exec-cache-hit fast path (`exec_cache_probe` returning `None`) skips the script copy only; `check_tx_inputs` has already read and compared every input.

**A mismatch can't become `Invalid`.**
- `ViewOr::View(e) => return Err(e)` at `serial.rs:869` and `:911`.
- The `ViewOr::Reject` arm of the sigop count is itself a `Fatal` (`:912-916`).
- `step?` (`serial.rs:999`) propagates it after the control waits.
- No `.ok()` or `unwrap_or` on the memo path.
- The cache's prefetch xcheck goes through `db.corrupt` → `CoinsReadError` → `read_failed` → `Fatal::reported` (latch tripped, reported by the store), not through `Invalid`.

**Ordering.** All compares happen before `UpdateCoins` (`serial.rs:959-982`). `Step::Stop`/`Err` returns before the `all_ok` verdict and before step 18, so a parallel-check block can't connect on a Fatal either. Inline and parallel controls (`with_control`) both hand the closure's `Err` back unchanged.

**Shutdown during the check.** The compare is synchronous on V inside one `connect_at`; S1 adds no new await or cancellation point. A shutdown that arrives mid-block finishes or drops the attempt exactly as before. A Fatal raised then still goes through `Report::UnlessLatched`: if a store had already tripped the latch, that store's report stands and the node stops anyway (seams.rs:51-61). Nothing in S1 can turn the xcheck Fatal into a silent return.

The flush side is unverified at node level (m-1). The chainstate matrix (`connector.rs:1402`) does verify "nothing committed" at its level.

### 2. Default off (focus 2)
- Registered `ALLOW_ANY | DEBUG_ONLY`, `OptionsCategory::DebugTest`. That matches rc.4 `init.cpp:801` `-checkblockindex` and `:805` `-checkmempool` (verified with `git show v24.0.0-rc.4:src/init.cpp`).
- Core defaults those two from `DefaultConsistencyChecks()` (regtest on). rdashd deliberately uses 0 everywhere (D1, DEC-697 Q9). That's a stricter, documented choice, not a parity bug.
- `get_bool_arg` follows Core's `InterpretBool` (`strencodings.rs:37`): `=0`, `-no…` and `=abc` are off, and `=2` is on.
- Command line, `dash.conf`, `[main]` sections and `settings.json` can all turn it on, exactly as they can turn on Core's `-checkblockindex` on mainnet. Nothing ties it to the network or to `debug_assertions`.
- The only automatic enabler is `NodeSut`'s `soft_set_bool_arg` (`sut.rs:249`). `sut` is `#[cfg(feature = "test-support")]` (`lib.rs:32-33`), and `test-support` fails to compile in an optimized build (`lib.rs:12-13`). No other code sets it.
- The soft-set happens after `parse_parameters` + `init_config` (config files read), so command-line or config `-noconsensusxcheck` wins, as Core's `SoftSet*` does.
- When the flag is on, the effect is only extra reads and an abort on a real mismatch. No consensus result changes when the memo is correct.

### 3. `planted-bugs` in the gate (focus 3)
- `dashd-node/planted-bugs = ["test-support", "dashd-chainstate/planted-bugs"]`. Release builds are blocked twice: dashd-node's `test-support` canary (`lib.rs:12`) and dashd-chainstate's `planted-bugs` canary (`dashd-chainstate/src/lib.rs:56`, which has no `clippy` exemption, same as before S1).
- Even a debug rdashd built with the feature has no CLI path that arms a bug: `coins_configuration` always sets `planted: None` (`node.rs:617`), and only `NodeSut`'s private field, set from its own tests, feeds it.
- No `Cargo.toml` in the workspace enables `dashd-node/planted-bugs` or `dashd-chainstate/planted-bugs` through a dependency (only dashd-node's and dashd-chainstate's own `[features]` and gate metadata mention it). The main `cargo test --workspace` step therefore can't unify it into other crates.
- The gate's feature step (`/work/scratch/gate-feature-tests.py`) runs one `cargo test -p <pkg> --features …` per package, so the new feature affects dashd-node's run only.
- Cost: dashd-chainstate is now built with `{rocksdb, test-support, planted-bugs}` for that run instead of `{…, test-support}`. That may add one dashd-chainstate lib build per gate if no other run shared the old combination. Tier 1 was green on a2a57f4d8, so this is a wall-clock note only, not a finding.
- `CoinsSetup` is only constructed inside dashd-node, so the cfg-gated `pub planted` field can't break another crate's struct literal.

### 4. Tests and mutants (focus 4)

Killed by the existing tests (reasoned from the code, not run):

| Mutant | Killed by |
|---|---|
| default `true` in `get_bool_arg` | `node::tests::consensusxcheck_is_read_at_step_7`, unset case |
| log line inverted or unconditional | same test |
| `xcheck: true` constant in `CoinsSetup` | sut control, leg 2 (`-noconsensusxcheck` + L1 connects) |
| `xcheck: false` (M1) | sut control, leg 3 |
| `connector.plant` dropped (M2) | sut control |
| soft-set dropped (M3) | sut control |
| soft-set → `force_set` | sut control, leg 2 aborts |
| `DEBUG_ONLY` dropped | help test, plain `-help` |
| support entry → `Unsupported` | help test (`refusal`) |
| `planted: self.planted` override dropped | sut control, legs 2/3 |

Surviving: m-1 (Fatal-path commit), m-2 (`set_xcheck` on the cache) and n-1 (category). The other legs of the control are sound:
- leg 1 runs unplanted with xcheck on and gets `bad-txns-in-belowout` through `BlockChecked`, with no abort;
- the 102-block prefix connects with xcheck on, which shows there are no false positives on valid blocks;
- the log text pins the memo compare as the cause of the abort (not the cache's, nor a flush).

### Not checked
- PF0: the plan's §6 advisory PF0 on the `startup-*` rows isn't in the implementer's evidence. I didn't run it (manager / train).
- I didn't re-run the implementer's test counts, the mutants or tier 1.
