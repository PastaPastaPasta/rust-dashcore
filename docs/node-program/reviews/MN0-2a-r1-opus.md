# MN0-2a review r1 (Opus-h)

- Under review: head `18f787a73` on `node/mn0-2a`, range `ca76c146f..18f787a73` (4 commits).
- Reference: Dash Core v24.0.0-rc.4 (`49caf372bb`).
- Class: CON, critical (DEC-92).

**Verdict: APPROVE-WITH-NITS.** No blocker or major. Fix m1 (a fixture gap shown by a mutant claimed equivalent that isn't) before merge. m2 and m3 are doc/contract fixes. The nits are routing notes.

## What I ran

All runs used a reflink copy of the worktree at `/work/scratch/mn0-2a-rv-opus/` with a seeded target, `TMPDIR=/dev/shm/mn0-2a-rv-opus-tmp` and `CARGO_BUILD_JOBS=12`.

| Run | Result |
|---|---|
| `cargo test -p dashd-mining -p dash-dml` | EXIT 0. dash-dml lib 181 passed (2 ignored), plus consumers 11, entry_view 1, mnhf 2, node_members 1, quirks 4, reconstruct 8, scaling 1, store 7, windows 2. dashd-mining lib 12, `evo_dashd` 1/1 (0.46 s). |
| Second capture: the committed `capture_mn0_evo.py`, run unchanged with dashd rc.4 (`sha256 e1be2564…f823a7`, matching the fixture's provenance) | The JSON is **byte-identical** to `dashd-mining/tests/data/mn0-evo-regtest.json` apart from `captured_utc`. |
| Four extra dashd rc.4 chains (configs below), replayed through `evo::block` with a copy of `evo_dashd.rs`'s `replay` | **656 more blocks byte-exact.** nBlockTx, nBlockSize, ComputeBlockVersion and the fences all pass on every chain. |
| M17 (implementer's mutant: platform reward at `height + 1`) on those chains | **Killed** at `brr-late` h440 and `mnrr-early` h19. It survives all three committed chains. |

The probe chains. Mock time, address and RPC are the capture script's own, all from a fresh datadir:

| name | args | tip | what it adds |
|---|---|---|---|
| `brr-late` | `-testactivationheight=brr@1000` | 460 | MN_RR (432) below BRR, so `GetMasternodePayment` takes the MNPI steps (350 + k·10; the 9th at h > 440) |
| `mnrr-early` | `-dip3params=2:2 -testactivationheight=v20@10 -testactivationheight=mn_rr@10` | 60 | MN_RR below the realloc start (BRR 1, so `nReallocStart` = 20): 20 % at h ≤ 19, 75 % from 20 |
| `legacy-bls` | `-dip3params=2:2 -testactivationheight=v19@100 -testactivationheight=v20@150 -testactivationheight=mn_rr@150` | 200 | null commitments at qc versions 1 and 2 (legacy BLS, type 103 rotated at index 0/1) before V19, then 3/4 |
| `v24-before-v20` | `-dip3params=2:2 -testactivationheight=v20@350 -testactivationheight=mn_rr@350 -vbparams=v24:0:9223372036854775807:0:100:80:60:5:0` | 370 | V24 active from 300 with V20 off: CbTx stays v2 (Core's `fV24 && fV20`), then v4 at 350 |

I did not re-run tier 1 or bench_inputs; the implementer's results stand.

## Answers to the brief

### 1. Era switches

Each of these matches rc.4 `node/miner.cpp:176-363`:
- `Eras::after`: all flags are `DeploymentActiveAfter(prev)`. V24 is versionbits/EHF, as `:203` reads it.
- The coinbase:
  - It is v3, type 5, with scriptSig `OP_RETURN` (`:280-283`).
  - vout[0] is `GetBlockSubsidyInner(prev bits, prev height, fV20)`. MN0-1's source already passes the real V20 flag (`source.rs:197-200`).
- The CbTx:
  - The version order matches `:285-293`.
  - `nHeight` is set.
  - merkleRootMNList is computed over the tip's list. With the empty-list fence and no provider transactions, that equals the list after the block.
  - merkleRootQuorums is set from DIP0008.
  - From V20: the null CL (diff 0, 96 zero bytes), then `creditPoolBalance = GetTotalLocked` of the diff built with `platformReward = PlatformShare(GetMasternodePayment(h, subsidy, EvoReward))` under MN_RR (`evo/creditpool.cpp:285-303`). The order is locked − unlocked + locked + reward, checked.
  - v4 adds the asset-unlock root.
- The payments:
  - `block_tx_outs` is rc.4 `GetBlockTxOuts` (`masternode/payments.cpp:131-200`). Under EvoReward the platform `OP_RETURN` is pushed first, and `GetCounts().total() == 0` returns `true` with it kept.
  - `FillBlockPayments` (`:444-474`) appends them after vout[0] and subtracts each from it, matching the Rust fold.
  - `GetMnRewardEraAfter` depends only on V20/MN_RR (`validation.cpp:5903-5909`), so ignoring EVO_SHARES is correct. EVO_SHARES isn't read anywhere on the miner path.

Within MN0-2a's scope (empty list, empty pool and mined index, no carried CL, no trigger, empty mempool), I found no input state that gives bytes different from Core's. Seven dashd chains, 1,043 blocks across every CbTx version, legacy and basic null commitments, and both realloc branches, all match.

The out-of-scope states:
- **Non-empty list:** `NotReady("DIP0003", "MN0-3")`. This uses `counts().total()`, the same predicate Core's `GetBlockTxOuts` uses.
- **Carried ChainLock under V20:** `NotReady("V20", "MN0-3")`. See m2 on the flag's contract.
- **Superblock trigger:** `NotReady("superblock", "GV1b")`, matching rc.4's `IsSuperblockTriggered(tip_mn_list, nBlockHeight)` at `:451`.
- **Mempool:** not representable. `evo::block` has no mempool argument and hard-codes `fee_reward 0`, so MP1a's wiring is forced to change this signature; see n1 for BL-468's text.
- **Non-empty mined-commitment index or pool after a list emptied:** see m3.

### 2. Commitments

`mineable_commitments_tx` (`pool.rs:184-212`) is rc.4 `GetMineableCommitmentsTx` (`llmq/blockprocessor.cpp:938-958`): one tx per commitment, `nVersion 3`, `nType 6`, no inputs or outputs, `nLockTime 0`, and payload `{u16 1, u32 nHeight, qc}`. `None` maps to Core's `false`. The existing `mineable_commitments` matches `:882-936`:
- the `GetNumCommitmentsRequired == 0` → nullopt case;
- `pindex` = tip for a new block;
- rotation and V19 read at `pindex`;
- `signingActiveQuorumCount` quorums when rotating;
- break on a null quorum hash, then skip mined ones;
- the pool's commitment, or a null one with index and version.

The accounting is `nBlockSize = 1000 + Σ GetTotalSize`, `nBlockTx` counts commitments only, and the sigops are the reserved 100, as at `:236-243`/`:259-261`.

What the fixture plus my probes exercise:
- Types 100/102/106 at qc v3, and v1 before V19.
- Type 103 non-rotated at v3/v1 (12–20), and rotated at v4/v2 with indexes 0 and 1.
- The DIP0003-alone and DIP0008 eras, with windows from 10–20 to 490–500.

`Core ApplyTx` throwing for a commitment isn't reachable for Core's own null commitments on an empty list, so not modelling it is fine.

### 3. Fences

For an empty-list regtest, the three fences cover every branch that needs data MN0-2a doesn't have (payee, carried CL, trigger).

Core branches the committed fixture never exercises:
- **Unfenced, but now verified by my probes:**
  - legacy-BLS null commitments (V19 above DIP3);
  - MN_RR below the realloc start;
  - BRR above MN_RR;
  - V24 with V20 off.

  m1 asks for at least the M17 chains to be committed.
- **Unfenced and not verified against dashd:**
  - a non-zero merkleRootQuorums, or `HasMinedCommitment` skipping, from commitments mined while masternodes existed;
  - a cached real commitment in the pool.

  Both rest on `quorum_root`'s and `mineable_commitments`' unit tests (m3).
- **Mempool-only, and unrepresentable here:** the credit-pool lock/unlock session, a non-empty asset-unlock root, and fees.

### 4. Encoder

At the fork pin f27bc4f5, base-sdk `CoinbaseCommitment::encode` (`pkgs/primitives/src/payload/cbtx.rs`) is driven by version, exactly like Core's `CCbTx` `SERIALIZE_METHODS`:
- v1: version, height, mnRoot;
- ≥ 2: quorums root;
- ≥ 3: `COMPACTSIZE(diff)`, the 96-byte sig, the balance;
- ≥ 4: the asset-unlock root.

`evo.rs` sets every `Option` consistently with the version. `BlsGlobalSig::decode` accepts all-zero bytes by name (`wrap_global.rs:56`), so the `expect` in `null_signature` can't fire, and `null_chainlock_is_dashds` pins the 96 zero bytes against dashd's. The encoder choice is sound.

**What the v4 blocks actually pin:** the field's presence, its position after `creditPoolBalance`, and the value zero. Every v4 block (49 committed, 21 more in `v24-before-v20`) has no asset unlock, and rc.4 `CalcCbTxMerkleRootAssetUnlocks` over an empty set is `ComputeMerkleRoot([]) = 0` (`evo/cbtx.cpp:158-172`). The root's computation over real unlocks isn't pinned; a mutant returning zero would survive. Unlocks only arrive through the mempool, so that's out of MN0-2a's scope. The multi-leaf vector gap is already BL-23.

**DEC-617 risk (n2).** Upstream addedfa16's `CoinbaseCommitment` differs from the fork's:
- it has **no `merkle_root_asset_unlocks`**, with a doc of "Payload version (1, 2, or 3)";
- `height` is a `BlockHeight` (u32);
- `best_cl_signature` is a `BlsSigBytes`;
- the encoder is presence-driven.

If the merge resolves `cbtx.rs` toward upstream, `evo.rs` and `evo/tests.rs` stop compiling. That failure is loud, not a silent byte change. The presence-driven encoder alone would still give the same bytes, because `evo.rs` keeps the fields consistent.

The v24 chain in `evo_dashd.rs` is exactly the Core-produced vector DEC-617 (10) asks for to keep the fork's v4 field. It only protects the merge if MN0-2a is on integration (T45) before the SW-UPSTREAM branch follow-merges T45. Otherwise the resolver has to be told about it.

### 5. Surviving mutants

- **M24: equivalent, agreed.** The mutated condition is only reached after `if !eras.v20 { return }` (`evo.rs:265`). With V20 on, `version >= 4` holds exactly when V24 is on, so `version >= 4 || (m && v24)` is the same as `version >= 4`.
- **M17: not equivalent.** See m1.

### 6. Provenance

The fixture is Core-produced and reproducible:
- the script is in-tree and readable, with no wallet, a fixed address and fixed mock time;
- `dashd_sha256` matches the rc.4 binary;
- my independent rerun is byte-identical apart from `captured_utc`, and so is the implementer's own `mn0-evo-regtest.rerun.json`.

The README's rerun recipe works as written.

## Findings

### m1 (minor, fix before merge): M17 isn't equivalent; the fixture misses the height-dependent platform reward

- **Where:** `dashd-mining/src/evo.rs:276` (`platform_reward(eras.mn_rr, height, …)`). The gap is in `contrib/node/mn0-captures/capture_mn0_evo.py` `CONFIGS` and `tests/evo_dashd.rs`.
- **Scenario:** `GetMasternodePayment(h, v, EvoReward)` is `3v/4` only once h ≥ BRR and h ≥ `nReallocStart`. Below that, it follows the MNPI steps or the 20 % base. In every committed chain, all MN_RR heights are ≥ 20 with BRR at 1, so an off-by-one height in the CbTx's platform reward never changes `creditPoolBalance`.
- **Proof:** with `-testactivationheight=brr@1000`, h440 vs h441 crosses the MNPI step at 440. With `v20@10 mn_rr@10`, h19 vs h20 crosses the realloc start. The mutant gives a different `creditPoolBalance` (and coinbase txid) at both heights. The unmutated code matches dashd there.
- **Fix:**
  1. Add `brr-late` and `mnrr-early` (the args above, tips 460 and 60) to `CONFIGS`.
  2. Recapture, and extend the `seen` table: `brr-late` `{3: 29}` with 45 commitments; `mnrr-early` `{2: 8, 3: 51}` with 71.
  3. Rerun M17.

  `legacy-bls` (`{2: 148, 3: 51}`, 279) and `v24-before-v20` (`{2: 348, 4: 21}`, 533) are cheap and cover the other unexercised branches, so I'd add them too. Allow about 2 minutes of capture time.

### m2 (minor): `EvoChain::chainlock`'s contract doesn't match `CalcCbTxBestChainlock`

- **Where:** `dashd-mining/src/evo.rs:52-56`.
- **Scenario:** the doc says the flag means "the parent CbTx's non-null one, or the best ChainLock at the tip". rc.4 (`node/miner.cpp:117-171`) returns `true` in two cases:
  - the parent CbTx carries a non-null CL;
  - the best known ChainLock has height ≥ `DeploymentHeight(V19)`, **at any height, not only the tip's**. Core inserts it with `bestCLHeightDiff = prev height − CL height`.

  A best CL below the V19 height is dropped (counts as `false`). If MN0-2b or MN0-3 implements "a CL at the tip" literally, a node holding an older best CL with a CL-less parent gets a null CL. That block is valid (null is allowed when the parent has none), but it differs from Core's.
- **Fix:** define the flag as "`CalcCbTxBestChainlock(pindexPrev)` returns true" and spell out both arms and the V19-height filter. Also add a unit row in which a best CL below the tip counts as `true`, once the reader exists (MN0-3).

### m3 (minor): "only an empty list is assembled" doesn't fence a chain that had masternodes

- **Where:** the module doc at `dashd-mining/src/evo.rs:1-5`, the `EvoChain::list` doc at `:50-51`, and the fence block at `:150-158`.
- **Scenario:** a regtest or devnet whose masternodes were all revoked keeps:
  - commitments mined while they existed, which give a non-zero merkleRootQuorums and make `HasMinedCommitment` skip;
  - possibly a cached real commitment in the pool.

  `evo::block` assembles this with no NotReady. The bytes then rest only on `quorum_root`'s and `mineable_commitments`' unit tests, never on a dashd comparison; every MN0-2a block has a zero quorum root. The carried-CL fence exists for the same history ("a list emptied after masternodes signed"), so the two are inconsistent.
- **Fix, either of:**
  - **(a)** Fence it: NotReady when the pool holds a commitment for the type, or when `quorum_root` is non-zero. The latter is cheap, because the root is already computed.
  - **(b)** State the reliance in the doc, and carry "the chain ever had registered masternodes" into MN0-2b's NotReady predicate. DEC-612's "MNs registered and a mining window open" doesn't cover an emptied list.

  I'd take (b) plus a sentence in DEC-612's MN0-2b row.

### n1 (nit, manager): BL-468 should name `evo::block`

`evo::block` hard-codes these empty-mempool assumptions:
- `fee_reward: 0` (miner vout and `GetBlockTxOuts`' `blockSubsidy + feeReward`);
- no `CMNListBuilder` (`merkle_root_mn_list_full(tip list)`);
- the credit-pool diff seeing only commitments;
- an asset-unlock root that is always zero.

The signature has no mempool, so MP1a must touch it. BL-468's row should still list these, so the test it asks for ("a non-empty mempool changes the template") also covers the CbTx fields.

### n2 (nit, manager → SW-UPSTREAM owner): `cbtx.rs` resolution

See §4. Tell the step-A resolver about two things:
- `evo_dashd.rs`'s v24 chain is the DEC-617 (10) Core vector that keeps the fork's `merkle_root_asset_unlocks` and version-driven encode;
- `evo.rs` depends on `height: i32` and `BlsGlobalSig`.

Alternatively, make sure MN0-2a (T45) is in the follow-merge before `pkgs/primitives` is resolved.

## Scratch

My copy, target, captures and probe test are under `/work/scratch/mn0-2a-rv-opus/` and `/dev/shm/mn0-2a-rv-opus-tmp`. Both are removed at the end of this review. The probe configs above are everything needed to reproduce: add them to `CONFIGS` and rerun the README recipe.
