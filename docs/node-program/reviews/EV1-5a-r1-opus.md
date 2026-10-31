# EV1-5a r1 review (Opus): `check_cbtx` and `cbtx_payload`

- **Branch:** `node/ev1-5a`, head 337285e7a, base a64e41abe (3 commits, 9 files, +591/−34).
- **Pin:** Dash Core `v24.0.0-rc.4` (`~/workspace/dash`). All Core citations below are at that tag.

## Verdict: ACCEPT-WITH-NITS

| Severity | Count |
|---|---|
| M | 0 |
| m | 1 |
| n | 3 |

`check_cbtx` is a line-for-line port of `CheckCbTx`. It has the same order and the same reject strings, all `TX_CONSENSUS`, and evaluates each activation at the same block. `cbtx_payload` matches both of Core's callers. I found no input on which either function and Core disagree, and three extra probes (below) agree with Core. The below-DIP0003 reasoning (5a-D3) is right: on regtest, the code is right and design D18's "v1 or v2" wording is not. The node-l1 row change and its floor are justified, and layering is clean. The one minor finding is a coverage and documentation gap on a Core path the pre-DIP0003 work (DEC-730) cares about. It is not a code bug.

## Findings

### m-1: the pre-special-version coinbase with type bits 5 is neither documented nor tested

- **Where:** `dash-dml/src/specialtx/cbtx.rs:33-34` (doc) and `dash-dml/src/specialtx/cbtx/tests.rs` (no case). The binding sketch is in `docs/node-program/designs/EV1-notes.md`, "How CH7a-2b-2 binds it".
- **Input:** a regtest block below DIP0003 whose `vtx[0]` is a **version-2** (or version-1) coinbase with `nType = 5` in the high bits and no `vExtraPayload`, checked with `fCheckCbTxMerkleRoots`. Below DIP0003 this is reachable: `ContextualCheckTransaction`'s `bad-txns-type` for a non-special version with `nType != 0` sits under `fDIP0003Active_context` (`validation.cpp:356-379`).
- **What Core does:** the pre-loop gates on `block.vtx[0]->nType == TRANSACTION_COINBASE` (`specialtxman.cpp:876`), not on `HasExtraPayloadField`. `GetTxPayload<CCbTx>` then reads an empty `vExtraPayload`, throws inside its own `try`, and returns `nullopt` (`specialtx.h:20-27`). The result is `bad-cbtx-payload` (`specialtxman.cpp:891`).
- **What ours does:** the same result, but only through the `None` arm of `cbtx_payload`. `Tx::payload()` is `None` there because `has_extra_payload_field` is false. `Tx::tx_type()` still reports `CoinbaseCommitment`.
  - My probe `probe_v2_coinbase_of_type_5_is_bad_payload` passes, so the function is right.
  - The only `None` test, `another_types_slot_is_not_a_cbtx`, is synthetic. It passes `None` beside a version-3 tx.
  - The doc says "the caller has already found `tx` to be of type 5" without saying how. A CH7a-2b-2 binding that gates on `has_extra_payload_field()` or `payload().is_some()` would skip the reject Core makes. That is a block Core refuses and we accept, in the pre-DIP0003 domain DEC-730 wants Core-exact.
- **Fix:**
  - Add the probe as a test (a `Transaction { version: 2, tx_type: CoinbaseCommitment, .. }` coinbase gives `bad-cbtx-payload`).
  - Extend the doc at `cbtx.rs:33-34`: "type 5" means Core's `nType` (`Tx::tx_type()`), whatever the tx version, and a pre-special-version tx's slot is `None`, which is `bad-cbtx-payload`.
  - Say the same in the EV1-notes binding sketch ("gate on `tx0.tx_type()`, not on the payload field").

### n-1: a cheap surviving mutant, a coinbase test that ignores the input count

- **Where:** `dash-dml/src/specialtx/cbtx.rs:40`, and the tests `payload_of_a_non_coinbase_is_refused` and `payload_must_decode_to_its_last_byte` (`tests.rs:325`, `:339`).
- **Mutant:** I replaced `!tx.is_coinbase()` with `!tx.inputs().first().is_some_and(|i| i.prevout.is_null())`, which is base-sdk's own `is_coinbase` shape. **All 16 branch tests pass.**
- **Input that kills it:** a type-5 tx with two inputs, the first null. Core's `IsCoinBase` needs `vin.size() == 1`, so Core gives `bad-cbtx-invalid` and the mutant gives none.
- **Why only a nit:** in Core that input is refused earlier on both paths. `CheckTransaction` gives `bad-txns-prevout-null` (`consensus/tx_check.cpp:68-71`) and `CheckBlock` the coinbase rules, so the mutant is equivalent on inputs that reach this code.
- **Fix:** add the two-input case. My probe `probe_two_inputs_first_null_is_not_a_coinbase` kills the mutant.

### n-2: design D18's §6.1 vector still says "v1 or v2"

- **Where:** `docs/node-program/designs/EV1-specialtx.md:877`.
- **Input:** a regtest block at height 2 to 431 (defaults) with a v1 CbTx.
  - Core refuses it with `bad-cbtx-version`. `DIP0008Height = 1` (`chainparams.cpp:822`), and `DeploymentActiveAt(*pindexPrev, DIP0008)` is true from `pindexPrev` = block 1 (`cbtx.cpp:35-38`).
  - The design's vector as written would expect it accepted.
- **Fix:** the manager or EV1-5 amends the vector to "v2, or v1 in block 1, or v1 with `-testactivationheight=dip0008@N` above DIP0003", which is 5a-D3's ask. Nothing changes in this slice.

### n-3: 5a-D3 doesn't cover devnet, which DEC-730 names

- **Where:** `docs/node-program/designs/EV1-notes.md:11` (5a-D3), and the `below_dip0003_*` tests.
- **Input:** devnet block 1.
  - Devnet sets DIP0003, DIP0008 and V20 all to 2 (`chainparams.cpp:575-586`). Only block 1 is below DIP0003, and with `pindexPrev` at 0 it takes v1 and v2.
  - `-testactivationheight` and `-dip3params` don't apply on devnet: `UpdateActivationParametersFromArgs` and `UpdateDIP3ParametersFromArgs` are `CRegTestParams` members (`:879-880`, `:1085`, `:1146`).
- **Fix:** add one sentence to 5a-D3. A devnet row in `below_dip0003_*` is optional, if `ChainParamsBuilder` builds devnet cheaply.

## Focus answers

### 1. Parity with rc.4

- **Order, strings and states** (`evo/cbtx.cpp:24-55`):
  1. `nVersion == INVALID || >= UNKNOWN` (`:26-28`), before the `pindexPrev` guard;
  2. the guard (`:30`);
  3. height (`:31-33`);
  4. DIP0008 (`:35-38`);
  5. V20, both halves (`:40-43`);
  6. `requires_unlock_root = is_v24_active && isV20`, both halves (`:47-51`).

  Every reject is `TX_CONSENSUS`, with `bad-cbtx-version` or `bad-cbtx-height`. `cbtx.rs:76-105` matches this exactly. The version constants match `cbtx.h:32-39` (`INVALID = 0`, `UNKNOWN` = 5).
- **Activation points:**
  - DIP0008 is `DeploymentActiveAt(*pindexPrev)`, and ours is `active_at(prev, ..)`.
  - V20 is `DeploymentActiveAfter(pindexPrev)`, and ours is `active_after(cons.prev, ..)`. Per `deploymentstatus.h:13-16` that is `pindexPrev->nHeight + 1 >= V20Height`.
  - V24 is the argument, which every rc.4 caller derives as `GetSpecialTxRules(pindexPrev or tip)`:
    - the block path, `validation.cpp:2665` (`pindex->pprev`);
    - the mempool, `validation.cpp:1406` (`tip`);
    - the miner, `miner.cpp:526`.

    The doc's "`GetSpecialTxRules(pindexPrev).v24`" is correct.
- **Genesis (null `pindexPrev`):** only step 1 runs, as in ours (`let Some(prev) = cons.prev else { return Ok(()) }`). Core never calls `ProcessSpecialTxsInBlock` for genesis, so this matters only for API parity, and that parity holds.
- **Height:** `pindexPrev->nHeight + 1 != cbTx.nHeight` in `int`. Ours compares `i64(cons.height)` with `i64(cbtx.height)`, and `cons.height` is defined as prev + 1. That is equivalent: heights fit in `i32`, and a negative `nHeight` is refused by both.
- **Version boundaries per network** (`chainparams.cpp`):

  | Network | DIP3 | DIP8 | V20 | Below DIP3 |
  |---|---|---|---|---|
  | mainnet | 1028160 | 1088640 | 1987776 | {1,2} |
  | testnet | 7000 | 78800 | 905100 | {1,2} |
  | regtest defaults | 432 | 1 | = DIP3 (`:828`) | block 1: {1,2}; blocks 2–431: {2} |
  | devnet | 2 | 2 | 2 | block 1: {1,2} |

  - On regtest, v3 and v4 are always refused below DIP0003. `assert(V20Height >= DIP0003Height)` (`:964`) runs after `UpdateActivationParametersFromArgs` and `UpdateDIP3ParametersFromArgs` (`:879-880`), so `isV20` is false below DIP0003, and `!isV20 && >= 3` refuses them whatever V24 is.
  - base-sdk's builder returns `V20BelowDip3` for both orderings, and the tests check this.
  - **So the code is right and D18's "v1 or v2" is not, for regtest from block 2.**

### 2. `cbtx_payload`

- **Reads to its end:**
  - `GetTxPayload` returns `ds.empty() ? obj : nullopt` and catches every `std::exception` (`specialtx.h:17-28`). Ours is `Some(Ok(d)) if d.is_complete()` (`consumed == len`), and a decoder error is `Some(Err)`.
  - In the base-sdk pin f27bc4f5, `CoinbaseCommitment::decode` (`pkgs/primitives/src/payload/cbtx.rs:54-82`) uses Core's layouts:
    - v0 and v1 are read as v1;
    - v5 and up are read as v4;
    - `COMPACTSIZE(bestCLHeightDiff)` is range-checked (`CompactSize::decode_size`).

    So v0 and v5+ reach step 1 rather than becoming `bad-cbtx-payload`. Probe `probe_out_of_range_versions_reach_check_cbtx` confirms both, and that v5 in the v3 layout is short.
  - Trailing bytes, a short payload and an empty payload are tested. Another type's slot is tested synthetically. The pre-special-version tx with type bits 5 is m-1.
- **Callers' order:** Core checks `IsCoinBase`, then `GetTxPayload`, then `CheckCbTx` on both paths:
  - `CheckSpecialTxInner`'s type-5 arm, `specialtxman.cpp:247-255`, `TX_CONSENSUS`;
  - the `ProcessSpecialTxsInBlock` pre-loop, `:876-892`, `BLOCK_CONSENSUS` with the same reason. `CheckCbTx` failures add the "Special Transaction check failed" debug message.

  `IsCoinBase` is `vin.size() == 1 && prevout null`, and ours is `canonical::is_coinbase` (`dash-consensus/src/primitives/canonical.rs:105-107`), which matches. rdashd's only caller today is node-l1. The dispatcher's type-5 arm stays `Unported`, and the CH7a-2b-2 sketch in EV1-notes follows Core's order (see m-1 for the gate).

### 3. node-l1 wiring (337285e7a)

- **Dropping `failed-check-special-tx` from the CbTx row is right:**
  - In the type-5 arm, `IsCoinBase` can't throw.
  - `GetTxPayload` catches its own `std::exception`s. Its only statement outside the `try` is the `CDataStream` copy (`specialtx.h:20`), which can only throw `bad_alloc`, and that is not deterministic consensus.
  - `CheckCbTx` throws nothing: it uses `Params()`, `DeploymentActive*` (assert-only) and `state.Invalid`.
- **The floor change is justified:** I counted the `owned(..)` steps. There were 10 at a64e41abe (9 lines + the CbTx row) and there are 9 at the head. The floor `>= 9` is tight, as `>= 10` was.
- **The walk is consistent:**
  - Step 3.8 gets only `bad-cbtx-invalid`.
  - Step 3.9 gets the remaining reason (only `bad-cbtx-payload` can arrive).
  - The `Stop::Bug("3.9")` arm is unreachable.
  - `with_cons` keeps `height = tip + 1` and `prev = tip`, so it agrees with the `ConsensusCtx` contract.
- **Note:** in node-l1, steps 3.9 and §3.10 are reachable only for a coinbase past tx 0, which Core's `CheckBlock` refuses first. Only 3.8 (a non-coinbase type 5) is live in replays. This is not a finding.

### 4. Tests and mutants

- **Vector provenance:** all three hex strings are byte-identical to `dash-evo/tests/data/cbtx_testnet.json`, whose header records `"core": "/Dash Core:24.0.0/"`, `"network": "testnet"`. Each coinbase's sha256d equals the fixture's `txids[0]`, and the block hashes are the testnet blocks at 900000, 905101 and 1569000. They are Core-accepted main-chain coinbases.
  - 905101 is V20's second block (V20Height 905100).
  - `v24 = false` is right: 1569000's CbTx is v3, so V24 was not active there.
- **Edited cases are ones Core rejects with the same string:**
  - **Height + 1:** `pindexPrev` at the original height gives `bad-cbtx-height` (`cbtx.cpp:31-33`), before any version step.
  - **v2 → v3 at 900000:** `!isV20 && >= 3` gives `bad-cbtx-version`.
  - **v3 → v2 at 905101 and 1569000:** `isV20 && < 3` gives `bad-cbtx-version`.

  `CheckCbTx` reads only `nVersion` and `nHeight`, so editing the in-memory version without re-encoding is sound.
- **The 16 named mutants:** I reviewed which test kills each and agree. I also tried by hand:
  - `<` → `<=` on each threshold;
  - `requires = v24 || v20`;
  - V19 for V20;
  - DIP0003 for DIP0008;
  - running steps 3–5 with a null prev.

  All are killed by the existing tests. The one survivor I found is n-1.

### 5. Layering

- The new code is `dash-dml` (layer `Pn`, `contrib/node/layers.toml:74`), and the version constants are in `dash-evo`. There is no `Cargo.toml` change, so no new edges.
- `python3 -B contrib/node/check-layers.py` in the worktree prints `layers ok: L1 L2 L3 L8 …` and exits 0. The one `baseline` line is the existing dash-coreimport edge.
- `NoCheckpoints` became `pub(crate)` for the tests, which is fine.

## What I checked and ran

- Read the full diff, EV1-notes 5a-D1..D8, design D18 and §6.1, DEC-730 and DEC-749.
- Read at rc.4:
  - `evo/cbtx.cpp`, `evo/cbtx.h`;
  - `evo/specialtx.h`;
  - `evo/specialtxman.cpp` 217-275 and 856-930;
  - `deploymentstatus.h`;
  - `chainparams.cpp` (mainnet, testnet, devnet, regtest heights, arg order, asserts);
  - `consensus/tx_check.cpp`;
  - `validation.cpp` 351-380 and the `GetSpecialTxRules` callers.
- Read base-sdk f27bc4f5: `payload/cbtx.rs` (codec), `payload/mod.rs` (`PayloadDecode`, `SpecialPayload::decode`), `transaction.rs::decode_payload`, and `params/types.rs::deployment_height`.
- Checked the fixture's provenance with a Python sha256d of each coinbase against the fixture's txids.
- Built in the reflink copy `/work/scratch/ev1-5a-r1-opus` (nice 19, `CARGO_BUILD_JOBS=8`, TMPDIR in the copy):
  - `cargo test -p dash-dml --lib specialtx::cbtx`: **16 passed**.
  - My 3 probes appended to the copy's `cbtx/tests.rs` (`tmp/probe_tests.rs`): **3 passed**. They cover the v2 coinbase with type bits 5 (m-1), the two-input coinbase (n-1), and v0/v5 decoded and refused at step 1.
  - The n-1 mutant in the copy: 16 branch tests pass and only my probe fails. The file was restored afterwards.
- **Not run:** `node-l1 --test l1_dml_block` needs the `oracle` feature, which links the oracle archive under `/work/cache/oracle`, and the brief puts `/work/cache` off limits. I checked the floor by counting rows instead. The implementer reports tier 1 green.
- The worktree was not modified. The copy is left in place with the probe tests appended.
