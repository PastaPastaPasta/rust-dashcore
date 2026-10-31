# RP1a-2c-2 review, round 1 (Opus)

- Branch `node/rp1a-2c2`, head 50b596caf; base c40c11e38. Reviewed `git diff 4d7a9b564 50b596caf` (7 commits, 11 files) and glanced at db1155e00 and 4d7a9b564 (the 2c-1 r2 nits).
- Core reference: `v24.0.0-rc.4:src/rpc/blockchain.cpp` (written as `bc.cpp` below) and `v24.0.0-rc.4:src/validation.cpp` (`val.cpp`).

## Verdict: ACCEPT-WITH-NITS

`getblockstats` is a line-for-line port of Core's handler. Every statistic, the selection logic, the argument and error order, and the two undo errors match Core. The two findings that matter are about tests, not behaviour:

- **Mutant 33 is not equivalent.** It is equivalent only on regtest. On mainnet and testnet before V20, the parent's `nBits` decides the subsidy, and no test reaches that. My V20-flag mutant survives for the same reason (m-1).
- **A hand-rolled compact size is untested past 252 bytes** (n-2).

Neither is a wrong answer in the code as written.

Counts: M 0, m 1, n 3.

---

## Findings

### m-1: the subsidy seam's bits and V20 inputs are unchecked off regtest; mutant 33 is not equivalent

`dashd-node/src/rpc.rs:616-634` (`Writer::block_subsidy`), notably `:629` (`active_at(id, V20)`) and `:631` (`prev.header.bits`).

Core's `GetBlockSubsidyHelper` (`val.cpp:1815-1876`) reads `nPrevBits` only on the pre-V20 path (`force_fixed_base_subsidy` is false):

- the early-mainnet bugged difficulty: `nPrevHeight <= 4500`, MAIN (`val.cpp:1820-1822`);
- the three eras' formulas (`val.cpp:1836-1855`).

On regtest every block carries the pow-limit bits (`fPowNoRetargeting`), and V20 is at 432 with the live chains stopping below it. In the early era, the formula `1111/(d+1)^2` clamps to 500 for any `d < 0.49`. So replacing the parent's bits with any small-difficulty compact (the mutant uses `0x1e0fffff`) changes nothing a test can see. **On regtest the mutant is equivalent. On mainnet and testnet it is not:**

- **Mainnet, block 1,000,000** (`prev_height` 999,999, pre-V20 GPU era, real difficulty about 10^8, so base 5):
  - Core: `subsidy` = **334,559,821**.
  - The mutant: `d ≈ 2.4e-4`, base `2222222/((d+2600)/9)^2 = 26.6`, which clamps to 25. That gives **1,672,799,096**, five times Core's.
  - (Four declines at the 210,240 interval, then the 10% treasury cut; computed from `val.cpp:1857-1873`.)
- **Testnet, CPU era** (`5465 <= prev_height < 17000`): base `11111/(((d+51)/6)^2)` moves with any `d` above about 0.01, so the same mutant shifts the answer by whole DASH.
- **Mainnet heights ≤ 4500:** the bugged `0xffff / (bits & 0xffffff)` gives about 0.5 for a mantissa of `0x01fffe`, so base 493 against the mutant's 500.

The V20 input has the same gap. My mutant 9 (`v20 && !mutant`, so `v20` is always false) survives both the lib tests and the getblockstats DIFF (§3). D43 already lists the V20 switch as a deferred live case for MN0-2/CH7a. That is fine for the *live* check, but nothing in the tree pins the plumbing before then. An `active_at(id)` → `active_after(Some(id))` slip (the child's activation) would also pass every current test.

The arithmetic itself is dash-consensus's oracle-tested `block_subsidy_inner`, so the risk is confined to the three arguments. That is why this is minor, not Major. But a bits mix-up here would make every mainnet `getblockstats` before V20 wrong, silently.

**Fix (either is enough; the first also closes n-1):**

1. Have the seam build the parent's `HeaderCtx` exactly as connect does, and call `dash_consensus::subsidy::block_subsidy(Some(&prev_ctx), v20, subsidy)`. Connect does this in `dashd-chainstate/src/connect/serial.rs:545-559` (`subsidy_of`) and `:771-773`, via `ChainAncestry::header`. Then add one dashd-node unit test of `block_subsidy`, over whatever in-memory index the chain tests use, with two hand-set inputs:
   - a parent whose bits are not the pow limit, under mainnet or testnet subsidy parameters (for example `prev_height` 6000 with the CPU-era formula);
   - a block at a lowered `V20Height`.

   If the in-memory index refuses non-pow-limit regtest bits, extract the closure body into a fn over `(prev entry, v20, &SubsidyParams)` and test that instead.
2. At minimum, record mutant 33 and the V20-flag mutant in D43 as *not equivalent, deferred* (with the mainnet numbers above), not as an equivalent survivor. The MN0-2/CH7a live case should then also assert the block at `V20Height - 1` against `V20Height` (already listed) **on a chain whose bits are not the pow limit**. Regtest can't provide one, so name the testnet or mainnet oracle (`dash-oracle-sys` already exports `block_subsidy_inner`) as the check.

### n-1: the seam re-implements `dash_consensus::subsidy::block_subsidy`

`dashd-node/src/rpc.rs:625-631`. The genesis branch (`config().genesis_reward`), the `i32::try_from(height)` and the `block_subsidy_inner` call repeat `dash_consensus::subsidy::block_subsidy` (`dash-consensus/src/subsidy.rs:284-297`). Connect already calls that function (`serial.rs:558`, `:773`), along with the `HeaderCtx` it builds.

- **Failing input:** none today. It is a second copy of the "genesis or parent" rule, which could drift from connect's.
- **Fix:** as m-1 (1). The closure becomes a lookup of `prev`, `active_at`, and one `block_subsidy(prev.map(...).as_ref(), v20, subsidy)` call.

### n-2: `txout_size` hand-rolls `GetSizeOfCompactSize`, and nothing tests a script of 253 bytes or more

`dashd-rpc/src/blockchain/stats.rs:103-112`.

- **Mutant:** my mutant 10 maps `0xfd..=0xffff` to 1 instead of 3, and survives both the lib tests and the getblockstats DIFF (§3). The arms are correct as written.
- **Failing input** (for the mutant): a block whose transaction spends, or creates, an output with a 253-byte P2SH-redeem-sized script (for example a bare 1-of-3 multisig padded with pushes). Under the mutant, `utxo_size_inc` and `utxo_size_inc_actual` drift by 2 per such output.
- **Fix:**
  - Use the existing `dash_consensus::primitives::serialize::compact_size_len` (`dash-consensus/src/primitives/serialize.rs:192`). It is `pub` but in a `pub(crate)` module, so either re-export it or serialize the `TxOut`.
  - Add one row to `getblockstats_sums_a_block_and_its_undo_as_core_does` with a ≥253-byte script in the block or in its undo. Core's value is `8 + 3 + len + 41`.

### n-3: D43's FT claim cites a branch this review did not see

`docs/node-program/designs/RP1a-notes.md` (D43, last bullet): "the FT `rpc_getblockstats.py` passes (M4, passlisted on node/func-compat-v24.0-rc4-rp1a-2c2)". This review only checked the DIFF, not the FT run. It is a provenance nit: cite the log or the run (for example `/work/scratch/rp1a-2c2/fc2.log`, rc 0) next to the claim, as the other D-rows do.

---

## 1. getblockstats parity (`stats.rs` against `bc.cpp:2211-2449`)

Each item was checked line by line:

- **Argument order.** Both do `EnsureAnyChainman`, then `ParseHashOrHeight`, then the `stats` read (`get_array`/`get_str`), then `GetBlockChecked`, then `GetUndoChecked` (`bc.cpp:2279-2292`), then compute, then the selection check. `stats.rs:183-203` follows the same order, so `[-1, [1]]` reports the height and `[h, ["bogus"]]` on unreadable undo reports the undo error, as in Core.
- **`ParseHashOrHeight`** (`bc.cpp:108-134` against `stats.rs:33-57`): the same negative and after-tip texts, `RPC_INVALID_PARAMETER`, and `getInt<int>` through `get_int_i32`. A hash goes through `ParseHashV("hash_or_height")` and `LookupBlockIndex`, giving `RPC_INVALID_ADDRESS_OR_KEY` "Block not found". A headers-only or invalidated block by hash then fails or succeeds through `block_read_error` (`GetBlockChecked`'s texts).
- **The `Needs` flags** (`bc.cpp:2294-2303` against `stats.rs:127-171`): all six key lists are identical, including `avgfeerate`/`feerate_percentiles`/`minfeerate`/`maxfeerate` in both the inputs and size lists.
- **Initial values:** `minfee`/`minfeerate` = `MAX_MONEY`; `mintxsize` = `MaxBlockSize()` = 2,000,000 (default `fDIP0001Active = true`).
- **The loop** (`bc.cpp:2324-2400` against `stats.rs:360-443`):
  - `outputs` is counted unconditionally.
  - The output loop is gated on `loop_outputs`; `out_size` = ser(TxOut) + 41.
  - The genesis skip is `pindex.nHeight == 0` (`:2336`); the unspendable skip is `IsUnspendable` (`:2338`). `is_unspendable` is `OP_RETURN` first, or length > `MAX_SCRIPT_SIZE`, as Core's.
  - The coinbase `continue` comes before `inputs`/`total_out`.
  - Size is `GetTotalSize` (the full serialization, matched in the DIFF).
  - `vtxundo.at(i - 1)` maps to `range_check`, with the same libstdc++ text as D41.
  - Platform transfer: `IsPlatformTransfer` is `IsSpecialTxVersion() && nType == 9` (`dash-consensus/src/primitives/canonical.rs:115`). `GetTxPayload` reads the whole payload or nothing (`is_complete`). The fee replaces inputs minus outputs (`:2378-2382`).
  - `CHECK_NONFATAL(MoneyRange(txfee))` comes before the pushes. The feerate is `txfee / tx_size` or 0.
- **Overflow:** wrapping adds on `tx_total_out`/`tx_total_in` match Core's (UB, wrapping in practice), and only for inputs a connected block can't produce. `totalfee` cannot overflow `i64`: the fees in one block are bounded by the coins it spends.
- **Medians and percentiles** (`bc.cpp:2155-2199`):
  - `CalculateTruncatedMedian`: `size % 2 == 0` is the same test as `is_multiple_of(2)`; the middle pair is averaged and truncated toward zero.
  - `CalculatePercentilesBySize`: the weights are computed in `double`, `int64 >= double` uses the same promotion, and the remaining percentiles are filled with the back value. An empty input gives zeros.
- **The output object** (`bc.cpp:2405-2435`):
  - All 27 keys are in Core's order. `avgfee`/`avgtxsize` divide by `vtx.size() - 1` (`size_t`). Both numerators are ≥ 0, so the signed quotient equals Core's unsigned one.
  - The `MAX_MONEY`/`MaxBlockSize` sentinels print as 0. `utxo_increase` = `outputs - inputs`, and `_actual` = `utxos - inputs`.
  - `subsidy` is `GetBlockSubsidy` (see §2).
  - There are no `swtotal_*`/`swtotal_weight`/`total_weight`/`avgfeerate` weight variants: Dash's handler has none (`bc.cpp:2232-2268`), and neither does the port.
- **Selection** (`bc.cpp:2437-2449`): an empty array means everything. `std::set` is a `BTreeSet<String>`, so the byte order is the same and duplicates collapse. The first unknown name in set order gives `RPC_INVALID_PARAMETER` "Invalid selected statistic '%s'" (`:2446`); the DIFF covers `inv_1` to `inv_4` and `inv_aaa`.
- **Pruned against unreadable undo** (`bc.cpp:806-825` against `stats.rs:453-462`; `rpc.rs:582-611`):
  - `IsBlockPruned` gives "Undo data not available (pruned data)".
  - A null undo position, missing files or a failed read give "Can't read undo data from disk".
  - The genesis block reads no undo (`stats.rs:195-201`, as `bc.cpp:811`).
  - `getblock` still turns both errors into "no undo" (`.ok()`, `blockchain.rs:478`), as `blockToJSON`'s `have_undo` does.
  - A stale block that was never connected (data present, no undo position) answers "Can't read…", as Core does.
  - **The pruned path is reachable only from the unit test** (`stats/tests.rs:181`, a fake `undo_pruned`). The node can't prune (`-prune` must be 0, `init/support.rs:94`), and Core refuses `-prune=0` on a pruned datadir. Neither DIFF reaches it, and no DIFF can until CH8. That is acceptable.

No `getblockstats` parity defect found.

## 2. Subsidy (6b1800e92) and pruneblockchain

**Subsidy.** `GetBlockSubsidy` is `val.cpp:1890-1895`: genesis reads `GenesisBlock().vtx[0]->GetValueOut()`, then `GetBlockSubsidyInner(pprev->nBits, pprev->nHeight, DeploymentActiveAt(*pindex, V20))`.

- The seam reads the parent's bits and height and `active_at(id, V20)` (= `DeploymentActiveAt(block)`). It passes them to `block_subsidy_inner`, which returns `nSubsidy - nSuperblockPart`: the treasury share is 10% before V20 and 20% from V20, after `nBudgetPaymentsStartBlock`.
- The Platform share is *not* part of `GetBlockSubsidy` in Core (it is `GetMasternodePayment`'s), and the port correctly leaves it out.
- The parameters are the miner's, built from the same `ChainParams` as connect's `payments.subsidy()`, including regtest `-budgetparams` and devnet `-highsubsidy*`. Every loaded node attaches the miner (`node.rs:324-336`), so the "Internal bug detected: subsidy" path is test-only, as D43 says.
- The halving boundary is checked live by DIFF 148–152: the decline lands at the block whose parent is at 150, and mutant 32 (own height) dies in the DIFF.
- The seam is pure: one V query turn, no writes, no new crate edge (D44 checked against `dashd-node/Cargo.toml`). The gap is m-1.

**pruneblockchain** (`bc.cpp:1077-1135` against `stats.rs:472-475`). `EnsureAnyChainman`, then `!IsPruneMode()` gives `RPC_MISC_ERROR` "Cannot prune blocks because node is not in prune mode." (`:1093`) before the parameter is read. That is the whole of Core's reachable behaviour while init accepts only `-prune=0`.

- The height and timestamp forms (`> 1e9` → `FindEarliestAtLeast(t - TIMESTAMP_WINDOW)`), "Blockchain is too short", `MIN_BLOCKS_TO_KEEP`, the returned `GetFirstStoredBlock - 1`, and the prune locks are all past the refusal. They are rightly deferred to CH8 (D44).
- The arity and type errors (`[]`, `["1"]`) come from the server's `IsValidNumArgs`/type check and are byte-equal in the DIFF (`prune_none`, `prune_str`).

## 3. Mutants and the DIFF

**What the DIFF compares.** `getblockstats_match_dashd_v24` (`rdashd/tests/rpc_diff.rs:2440-2616`) compares each JSON-RPC batch reply **byte for byte**, as whole HTTP responses with only `Date` masked. A mismatch is then attributed per call. It covers:

- every height by height and by hash (0–103);
- each statistic alone at 101–103;
- 18 error shapes;
- an invalidated tip by hash and by height, and after reconsider;
- four prune calls;
- heights 148–152 across the first decline.

dashd's own replies at 101–103 are also asserted against Core's `rpc_getblockstats.json`. The comparison is strict, not loose. It does **not** reach the pruned-undo error (§1), a Platform transfer (unit test only, `stats/tests.rs:309`), a ≥253-byte script (n-2), or a V20 or non-pow-limit-bits subsidy (m-1).

**Implementer's mutants.** I read `/work/scratch/rp1a-2c2/mut-manifest-r2.txt` and `mut-r2.log`. Mutants 1–16, 31 and 32 die, each in the lib tests or the DIFF. Mutant 33 survives, and it is **not equivalent** (m-1).

**My mutants**, env-gated `RVMUT=n` in the reflink copy (`/work/scratch/rp1a-2c2-r1-opus/apply_mutants.py`, `mut.sh`, results in `mut.log`):

| # | Mutant | lib (dashd-rpc 202) | DIFF | Result |
|---|---|---|---|---|
| 6 | 2c-1 r2: `entry_port(Invalid)` → 1 | FAIL `dashd_decoderawtransaction_crafted_fixtures` | getblock DIFF FAIL (2 of 83 differ) | **dies** |
| 7 | 2c-1 r2: ProUpShareTx prints `rewardAddress` "" for a non-empty script without a destination | FAIL (same) | getblock DIFF FAIL (4 of 83 differ) | **dies** |
| 8 | 2c-1 r2: `operatorReward` = `n * 0.01` | FAIL (same) | getblock DIFF FAIL (4 of 83 differ) | **dies** |
| 9 | the subsidy seam's `v20` forced false | n/a (seam is in dashd-node) | getblockstats DIFF pass | **survives** (m-1) |
| 10 | `txout_size`: 3-byte compact size counted as 1 | pass | getblockstats DIFF pass | **survives** (n-2) |

So db1155e00's three crafted rows do kill review mutants 6–8 in the unit fixture test. The stored-block DIFF's new `platformHTTPPort":0` assertion (`rpc_diff.rs:2355`) also pins mutant 6 live. 4d7a9b564 is docs-only (pointers to BL-516/BL-517), and it is accurate.

## 4. What I ran

- `df -BG /work`: 275G free (≥ 230G). I used a reflink copy of the worktree at 50b596caf, `/work/scratch/rp1a-2c2-r1-opus/wt`, with `CARGO_BUILD_JOBS=8`, nice 19, and `TMPDIR` inside the copy. The copy is left in place.
- `cargo test -p dashd-rpc --lib`: **202 passed, 0 failed**.
- `cargo test -p rdashd --test rpc_diff getblockstats`: **1 passed** (`getblockstats_match_dashd_v24`, against the real dashd v24 rc.4).
- I then ran the mutants, `RVMUT` = 6, 7, 8, 9 and 10, against one lib build and one `rpc_diff` build, with the results in the table above. Mutants 6–8 also ran through `getblock_transactions_match_dashd_v24`; each fails on byte differences against dashd (`rpc_diff.rs:2312`), not on setup. The mutants are env-gated: with `RVMUT` unset, the lib baseline in the same build is still 202/0.
- I did not rerun the dashd-node 168, dashd-mining 12 or dash-dml 7 suites, clippy, or the FT.
- I read Core rc.4's `rpc/blockchain.cpp`, `validation.cpp:1815-1895` and `chainparams.cpp` (regtest `V20Height = DIP0003Height = 432`, halving 150, budget start 1000, `fPowNoRetargeting`).
- I made no edits to the worktree, and no pushes or posts.
