# BL-451b r1 review (Opus)

Target: rust-dashcore `node/bl451b` at **036ca96c1** (base f707f473b, tree cb3f9699210776ab90f158866a34fd3683a547bb); Core oracle shim **6959e3b906ff** (one commit on c3e388c24ce).

## Verdict: ACCEPT-WITH-NITS (M 0 / m 2 / n 4)

The BL-477 Core fix is correct and masks nothing the differentials compare. The four targets drive the same comparisons as `l1_mnsync`, are registered consistently (`fuzzctl check` ok), and refuse or filter nothing silently. The seeds are deterministic, and `public_seeds_are_current` is a real check. The pin move is clean: no consumer of the old pin is left, and the SB4 re-record changed only provenance plus the two bench counts.

The two minors are about seed and test strength, not correctness. m-1 is the only one that leaves a validator r0 fix unprotected; both are small fixes to fold in before READY.

## Findings

### m-1: nothing checks that the r0 "live request" fix still holds, except seed freshness

- `node-l1/src/mnsync.rs:984` (`flatdb_seed_inputs`, `FlatFile::NetFulfilled => &[1, 0, 2, 1, 7, 2, 200]`).
- `node-l1/tests/l1_mnsync.rs:690-700` (`seeds_compare`).

**What r0 fixed.** Commit 036ca96c1 fixed this validator r0 finding: every generated netfulfilled.dat was emptied by Core's `LoadCache(true)` → `CheckAndRemove()` (netfulfilledman.cpp:21-28). The fix changed the generator's store bytes. But no test checks the property the fix established, that a generated store keeps a request through the load. `seeds_compare` asserts only "no divergence, no refusal, and diff_flatdb_load reaches both verdicts". Both hold with an expired-only store.

**Failing scenario (run in my copy).**
1. Revert line 984 to the pre-r0 `&[1, 0, 1, 1, 7]`.
2. Regenerate the seeds with `cargo run -p node-l1 --features oracle --example mnsync_seeds`, as anyone changing the generator would.
3. Run `cargo test -p node-l1 --features oracle --test l1_mnsync`.

Result: **17/17 pass. The mutant survives.**

The implementer's M5 was "killed" only by `public_seeds_are_current` (log `mut451b-M5-old-expiry.log`: a missing seed file `f3b08a4f444c14cb`). That is staleness, not behaviour. So the "6/6 killed" is accurate as run, but M5 does not protect the r0 fix.

**Fix.** In `seeds_compare` (or a small test of its own), for each network take the generated NetFulfilled seed with edit 0, i.e. `[sel, 0 | 2<<1, 0, 0] ++ store ++ [0, 5, 0, 7]`. Assert that Core's loaded store holds exactly one request. Either:
- call `Oracle::flatdb_load` and decode `core.store` with `decode_with_magic`'s body decoder; or
- check that `core.store` differs from the empty store's `core_body`.

Asserting "some flat seed loads a non-empty netfulfilled store" would do too.

### m-2: the netfulfilled "walk across the expiry" seed never lands at the expiry or one past it

- `node-l1/src/mnsync.rs:924-931`.
- `node-fuzz/public-seeds/PROVENANCE.md:247` ("the fulfilled requests step across the expiry").
- The comment at :924 says "step to the expiry less one, at it and past it".

**The bug.** `fulfilled_script`'s expiry step is cumulative: `now = now + expire + (b % 3) - 1` (mnsync.rs:353). So the three edges are taken from the previous time, not from the add. My probe (`node-l1/tests/rv_probe.rs` in my copy) printed each time minus the add's expiry:
- mainnet: `[-1, +3599, +7200]`;
- testnet, regtest and devnet: `[-1, +299, +600]`.

Core's two comparisons differ exactly at the boundary:
- `HasFulfilledRequest` is `expiry > GetTime()` (netfulfilledman.cpp:44);
- `CheckAndRemove` prunes on `now > expiry` (:67).

So `now == expiry` is the one state where *has = false but the entry is kept*, and `expiry + 1` is where it is pruned. No seed reaches either. The per-peer seeds also step only to `expire - 1` (args `[1, 0]`).

`l1_mnsync`'s D4 tests do cover these corners, so this is seed quality, not a hole in the differential. The fuzzer is one byte away from them. But the seed and PROVENANCE claim coverage they do not give.

**Fix.** After the first `expire - 1` step, add two `next_time` steps of 1 s each instead of two more `expire` steps. Op bytes `[0, 0, 0, 0, 1, 1]`: mode 0, then `u8 % 4 = 1` → `pick(TIME_STEPS)` index 1 = +1 s. Regenerate the seeds. Alternatively, re-add before each edge.

### n-1: every seeded sync clock op steps 0 s

`node-l1/src/mnsync.rs:868`: `0 => b.extend([1, flags * 3])`. Code 0 is seeded only with `flags = 0` (:445-449), so `flags * 3` is dead and every `SyncOp::Time` in the 76 `diff_mnsync` seeds advances the clock by `TIME_STEPS[0] = 0`. The probe printed `sync time step 0` for every one.

The 6 s, 30 s and 900 s constants are reached only by mutation. The early-start seed covers the 900 s reset from the start side only.

**Fix.** Seed code 0 under a few step indices, e.g. 3, 6 and 8 (6 s, 31 s, 900 s), or give code 0 its own flag list.

### n-2: the stray-file child does not watch its working directory

`dash-oracle-sys/tests/gov_regtest.rs:748-800`. Core's `GetDataDir()` returns an empty path when `-datadir` names something that is not a directory (Core `util/system.cpp:361-382`). `CFlatDB` then builds `"" / "mncache.dat"` at construction (`flat-database.h:179-180`), a relative path, and a later `Store()` writes it into the process's cwd. Under `cargo test` that cwd is `dash-oracle-sys/`, which the test does not inspect.

No current entry gets there: `DataDirScope` always forces an existing mkdtemp directory, and `ClearArgs` drops forced settings. But it is the one other place a regression would land unseen.

**Fix.** Run the child with `.current_dir(root.join("cwd"))`, create that directory, and include it in `left`.

### n-3: the child covers three of the four GovNode callers and one network

`dash-oracle-sys/tests/gov_regtest.rs:765-795`.

**GovNode callers.**
- GovNode is constructed directly by `oracle_gov_object` (oracle.cpp:5213).
- It is also constructed as `GovManager::m_node` (oracle.cpp:4995-5004), which `oracle_gov_ops` (:5276) and `oracle_block_value_payee` (~:5650, its `std::optional<GovManager>`) both use.
- The child runs `gov_object`, `gov_vote` and `gov_ops`, but not the payments entry.

The fix is in GovNode's destructor, so the payments path is covered by construction today. But a future payments-side change that marks the meta manager valid would not be caught.

**Networks.** The pre-created directories are `""`, `regtest` and `testnet3`. The BL-451 entries run only on `chain()` (regtest), while the fuzz targets run them on all four `mnsync::NETWORKS`, devnet (`devnet-bl451`) included.

**Fix.**
- Add one `block_value_payee` call (the `superblock_payments_under_a_script` fixture) to the child.
- Optionally run the mnsync, mncache, netfulfilled and flatdb calls on a devnet `Chain` too, creating its directory.

### n-4: diff_flatdb_load needs no `min_certify_execs`, but its rate should be written down

`node-fuzz/targets.toml:1381`.

**Answering the brief.** Certify floors count executions per CPU window (`fuzzctl.py` `cpu_floor_seconds`). The soak measured 1,824,766 executions in 1.0011 CPU-h, about 506 per CPU-s, which is above the 1,000,000 default for a 3,600 CPU-s window. So no floor is needed.

**What the soak also showed.** The target is fsync-bound (`dashd-masternode/src/flatdb.rs:502, :511`, and Core's `Store`). That one CPU-h took 2.93 h of wall time at `--jobs 4`, about 43 exec/s per job in wall terms, which is where the brief's "~40 exec/s" comes from. It also produced `slow_units=1`.

**Fix.** Add a comment like the ones for other slow targets, e.g. "~506 execs/CPU-s (1.82M in 1 CPU-h, soak at 66f541522); fsync-bound, ~3× wall per CPU-s; the default floor holds". Then the farm and certify owners don't re-derive it.

## Answers to the brief's questions

**1. The BL-477 Core fix**
- **Is it correct?** Yes:
  - `m_meta_valid` (a reference) is declared before `m_meta_valid_before`, so the init order is right.
  - Every throw in the constructor comes before `m_meta_valid = true`. The only work after it is the static `TxIndex` (in memory) and two pointer stores.
  - In `GovManager`, `m_node` is constructed before `m_govman` and destroyed after it, so the flag is still true for the whole manager lifetime. `~CGovernanceManager` stores only if `is_loaded`, which the oracle never sets.
- **Does it mask Core behaviour?** No. The flag is true for the entire call (`ProcessVote()`'s assert), which matches a node after `LoadCache`. Restoring it afterwards changes only `~CMasternodeMetaMan`'s `Store` (meta.cpp:25-29). No comparison reads that file, and no entry calls `LoadCache` on the context's manager.
- **Can anything else write into the default data dir?** I checked:
  - `CNetFulfilledRequestManager` (E2) and `CMasternodeMetaMan` (E3) are never `LoadCache`d, so they are never valid.
  - The E4 managers are built inside `DataDirScope`, so their `pathDB` is the private directory.
  - `CGovernanceManager` has `is_loaded` false.
  - `CSporkManager` exists only in chain sessions, which use their own `-datadir`.
  - The LLMQ and coinjoin `CMasternodeMetaMan`s are never valid.
  - `ChainContext::m_evodb` is a memenv (no `TryCreateDirectories`, dbwrapper.cpp:141-160).
  - Logging is off (`m_print_to_file = false`).

  So nothing else writes into the default data dir. `oracle_select_network` does not `ClearPathCache`, so the cached network subdirectory is whichever network first called `GetDataDirNet()`. That is why the test pre-creates `""` and `testnet3`, and it is correct.
- **Is the test strong enough?** Yes for the regression it targets. The implementer's M1 (old pin) fails with `.dashcore/regtest/mncache.dat`, and I read that log. The sentinel kills the filter-miss and no-sentinel mutants (M2, M3), and the new pin passes in my copy. Gaps: n-2, n-3.

**2. Fuzz targets**
- **Same comparison as the tests?** Yes. Each `TARGETS` entry is the same `compare_*` that `l1_mnsync`'s proptests and seed tests drive, and `registered_on_v24` checks the fn pointers.
- **Refusals and filters:**
  - Only `ErrorKind::Argument` becomes `OutOfDomain`. Rust generates inputs inside the shim's domain (times clamped to 1..2^33, known ops), and the proptest twins assert that the oracle refuses none.
  - Any other oracle error, a node start-up failure, or an `Io` verdict is a divergence, so nothing is hidden.
  - An empty flatdb input is a legitimate `BothAccept` (no file).
- **Bounds:** `MAX_OPS` 64.
- **Registration:** `fuzzctl check` ok (121 targets, 119 smoke and 118 farm legs). The `targets.toml` entries match diff_coins_ops's form (private route, smoke 30, nightly 1, public_seeds).
- **Certify floor:** see n-4.
- **Soak:** at 66f541522, which is on f707f473b and differs from 036ca96c1 only in the tidy and the r0 seed; old fuzz pin c3e388c24ce:
  - diff_mnsync: 9.40M executions;
  - diff_netfulfilled: 8.88M;
  - diff_mncache: 10.21M;
  - diff_flatdb_load: 1.82M.

  All had 0 divergences and 0 crashes. The old pin is acceptable because BL-477 changes only GovNode, which none of the four targets reach.

**3. Seeds**
- **Deterministic?** Yes. I regenerated them in my copy at 036ca96c1 and `diff -r` against the worktree is identical: 76 + 196 + 43 + 88 = 403 files.
- **Is `public_seeds_are_current` a real check?** Yes: exact bytes per name, plus no extra files.
- **The 32 changed diff_flatdb_load seeds:** fully explained by the r0 fix. That is 4 networks × 8 edits of the generated NetFulfilled store (32 A / 32 D in 036ca96c1); the no-file and MnCache seeds are unchanged. Gaps: m-1, m-2, n-1.

**4. Pin move**
- `pins.txt` has one line, both flavor hashes.
- PROVENANCE (oracle-v24) names both pins correctly.
- `git grep` finds no old pin outside that provenance note.
- All 54 SB4 vectors differ only in `core_commit`, `binary_sha256` and `archives_sha256`, except `bench-pnbh-mainnet-{dgw,kgw}`. Their `instructions_u` moved −0.046% and −0.040%; I checked dgw directly and the rest via `sb4b-vecdiff.txt`, which covers all 54 files.
- **Is the missing `archives_sha256` cross-check acceptable?** Yes, as at earlier pin moves. Every one since f21a6d3d6 also moved the unrelated archive hashes, for example `libleveldb.a` across all five previous re-records. Those hashes are per build root and were never reproducible across builds, so a cross-check against an absent build dir would add nothing.

## Checked and ran

- **Decisions read:** DEC-626, DEC-653, DEC-661 and DEC-666.
- **Core (read-only, via `git show`):**
  - the BL-477 diff;
  - `oracle_mnsync.cpp` (E1–E4, `DataDirScope`);
  - the oracle.cpp `ChainContext`, `GovNode` and `GovManager`;
  - meta.cpp, netfulfilledman.cpp, governance.cpp, flat-database.h, `util/system.cpp` `GetDataDir`, dbwrapper.cpp;
  - every `CMasternodeMetaMan` and flat-db owner across `src/test/oracle`.
- **Reflink copy** `/work/scratch/bl451b-rv-opus` (tree cb3f9699 = 036ca96c1^{tree}), with `TMPDIR` in its `.rvtmp`:
  - `cargo test -p dash-oracle-sys --features oracle --test gov_regtest`: 6/6 pass, including `oracle_writes_no_file_outside_its_temp_dir`.
  - `cargo test -p node-l1 --features oracle --test l1_mnsync`: 17/17 pass.
  - Mutant R1 (m-1): the pre-r0 store plus regenerated seeds. **SURVIVED.** I then restored `mnsync.rs` from the worktree and regenerated the seeds, which came out identical to the worktree's.
  - Probe `node-l1/tests/rv_probe.rs` (left in the copy) for m-2 and n-1.
  - `python3 -I node-fuzz/fuzzctl.py check`: ok.
- **Implementer evidence read:**
  - `/work/scratch/bl451/logs/mut451b-*.log`, its run script, and which assertion killed each mutant;
  - the `bl451b-soak` journal (all four summary lines);
  - `sb4b-vecdiff.txt`.
- **Not done:** no oracle rebuild, and I did not re-run the old-pin mutant myself (I relied on the implementer's M1 log).
