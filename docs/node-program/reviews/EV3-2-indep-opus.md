# EV3-2 (credit pool): independent consensus review (Opus)

| | |
|---|---|
| Reviewed | `node/ev3-credit-pool` e351bcfeb, only the EV3-2 delta: `git diff e44726761 e351bcfeb` (115a68e6f, e351bcfeb) |
| Against | Dash Core `v24.0.0-rc.3`: `evo/creditpool.{h,cpp}`, `util/ranges_set.{h,cpp}`, `evo/assetlocktx.{h,cpp}`, `evo/specialtxman.cpp` (`ProcessSpecialTxsInBlock`, `CheckCreditPoolDiffForBlock`), `evo/cbtx.h`, `validation.cpp:355-380`, `rpc/blockchain.cpp:1789-1823` |
| Context | EV3-notes (branch copy) D8–D14, D21, §4.2–4.3, A-1/A-2/A-5; DEC-276; PLAN EV3 row |
| Method | Line-by-line read against Core; `git archive` build in /work/scratch/ev3-2-review (seeded target, deleted after); `cargo test -p dash-dml --lib creditpool` 14/14; cargo-mutants 27.1.0 on `creditpool.rs` + `creditpool/ranges.rs` (`--lib --test store`); hand mutants for what cargo-mutants does not generate |
| Verdict | **ACCEPT-WITH-NITS** (no B items) |

## 1. Accept/reject parity

Everything below was checked line by line and matches rc.3.

- **`GetDataFromUnlockTx`** (`creditpool.rs:191-209` vs `cp:31-47`): payload, then per-output
  `MoneyRange` in order, fee (u32) plus outputs. Both reasons are `TX_CONSENSUS`.
- **`Lock`** (`:395-411` vs `cp:291-305`): a complete `CAssetLockPayload`, then the *first* output
  whose script's first byte is `OP_RETURN` (an empty script skipped). No range check on its value,
  as in Core (`CheckAssetLockTx` owns it).
- **`Unlock`** (`:414-428` vs `cp:307-328`): data, then `sessionUnlocked + toUnlock > currentLimit`
  (`failed-creditpool-unlock-too-much`), then the pool's and the block's indexes
  (`…-duplicated-index`). The limit check comes before the index check, as in Core.
- **`ProcessLockUnlockTransaction(s)`** (`:434-476`): only special-version transactions. The
  package form rolls back exactly the indexes it inserted, as `cp:350-374` does.
- **`GetCreditPoolDiffForBlock`** (`:482-499`): `vtx[1..]`, the first reject as
  `BLOCK_CONSENSUS` with Core's reason and debug text (`tx.txid()` is `GetHash()`, the stable
  txid for v2 unlocks).
- **`CheckCreditPoolDiffForBlock`** (`:506-527`): the DIP0008 and V20 gates, then the
  `GetTotalLocked()` equality, `bad-cbtx-assetlocked-amount` with an empty debug message.
- **The catch arms.** `failed-procassetlocksinblock`, `failed-getcreditpooldiff` and
  `failed-checkcreditpooldiff` are unreachable at rc.3:
  - `GetTxPayload` swallows its own exceptions.
  - The parent's `GetCreditPool` already ran (or threw) at `stm:848`, inside the outer `try`, under
    the same V20-at-block condition, so the throw surfaces as `failed-procspectxsinblock`.
  - `PlatformShare` asserts rather than throws.

  D8 and D12 are right.
- **`GetCreditDataFromBlock`** (`:219-258` vs `cp:58-108`): the DIP0003 gate; empty `vtx`, empty
  payload or a non-special-version coinbase gives nothing; the CbTx is decoded from the payload
  bytes whatever the `nType`, and must be fully consumed. base-sdk's `CoinbaseCommitment::decode`
  reads by the same version thresholds as `CCbTx` (v0 and v≥5 decode; balance from v3). The unlock
  loop includes `vtx[0]`, as Core's does. Indexes are deduplicated and the amounts are not (D10);
  the outcome does not depend on order.
- **`ConstructCreditPool`** (`:274-329` vs `cp:167-241`):
  - The no-data branch checks `PrevLocked`, then `PrevIndexes`.
  - Index `Add` failure gives `IndexDuplicated` before the window is read.
  - `latelyUnlocked = prev + unlocked − distantUnlocked` is computed in every era, left to right.
  - The three eras use Core's clamp order:
    - pre-v22: `+ > Low`, then `max(Low, bal/10) − lately`, clamped at 0, then
      `min(·, High − lately)`;
    - v22: `min(bal, 2000)`;
    - v24: `UnlockLimitV24(bal, distantBalance)`.
  - A negative result gives `NegativeLimit`.
  - Every `what()` text matches, including the trailing `\n` and tinyformat's `%08d` on negative
    remainders (`0.-0000005`, the same as Rust's `{:08}`).
- **`UnlockLimitV24`** (`:151-165` vs `cph:149-159`): `max(W·20/100, 2000 DASH)`, then
  `min(max(0, drop − (W − bal)), bal)`. Integer truncation matches: `W·20/100 = trunc(W/5)` for
  either sign, as in C++.
- **The platform reward** (`:335-345` vs `cp:285-288`): `MN_RR` active after the parent, then
  `PlatformShare(GetMasternodePayment(h, subsidy, EvoReward))`, from the subsidy alone. The live
  walk checks it at every testnet block from MN_RR (1,066,900), because `check_block_diff` runs on
  each mined block and the walk reported 0 rejects.
- **`RangesSet`** (`ranges.rs`): the following match `rs:13-111` and `rsh:93-125` exactly.
  - `add`/`contains` use `range(..(v,v)).next_back()` and `range((v,v)..).next()` for
    `lower_bound`'s predecessor and position.
  - `WrappedSuccessor` at the top index.
  - The `[0,1)`/`[MAX−1,0)` pair is not merged in either insertion order.
  - The decoder rejects all five cases in Core's order.

  The representation is canonical, so insertion order inside a block cannot change the stored set.
- **Reorg order.** Pools and data are per block hash and depend only on ancestors (the V24 state
  included, EHF signals coming from ancestors). A reconnect re-derives the same value, and
  `write_derived` turns any difference into a store fault. An index used on an abandoned branch
  is free on the other, as in Core.
- **V24 changes at rc.3 in this slice's scope:** only `UnlockLimitV24` and the era switch
  (`cp:210-211`). Two others are relevant at the edges:
  - Asset-unlock payload v2 (stable txid, `assetlocktx.h:79-83`) has the same wire layout. It only
    changes `GetHash()` in the diff's debug text (see N2).
  - CbTx v4's `merkleRootAssetUnlocks` is decoded by `credit_block_data`, and checked elsewhere
    (BL-145 n3).

  `CRangesSet`'s wrap-around and strict decoder are binary-wide, not deployment-gated (D1).

## 2. Arithmetic

Amounts are `i64` throughout. Where Core's `int64` overflow would be undefined behaviour, the port
uses `wrapping_*`:

- `GetTotalLocked`;
- `latelyUnlocked`;
- the pre-v22 sums;
- `W·20`.

The operand order is Core's left-to-right.

None of these can overflow on a checked chain:

- Unlocks per block are ≤ `currentLimit` ≤ balance (shown for each era, see §3).
- Locks are funded by real inputs (≤ supply), plus same-block unlock outputs, which are bounded by
  the limit.
- Output totals are bounded by `bad-txns-txouttotal-toolarge`.
- `W·20` is bounded by Core's `static_assert`.

Division is `i64 /` (truncation toward zero) at both sites, the same as C++. There is no saturating
arithmetic anywhere, which is correct: Core has none.

## 3. Store semantics (D8, D9)

- **Zeros where Core returns nothing.**
  - `connect_records` writes `Some(CreditBlockData::default())` for a DIP0003 block without
    readable CbTx data (`creditpool.rs:564`).
  - `credit_data_at` answers zeros without a read before DIP0003 (`:601-603`).
  - Both of Core's readers (`cp:199-205`, `:247`) map `nullopt` to 0.

  This matches.
- **No record where Core's read throws.** `Err(_) => None` for the data (`:565`), and the pool is
  the error when V20 is active. A later read of that data is `StoreError::Inconsistent`, where
  Core would throw. I checked that no connectable block reaches this:
  - `CheckSpecialTxInner` runs `CheckAssetUnlockTx` on every special transaction of any DIP0003
    block (`stm:224-264`), so a bad payload is rejected as `bad-assetunlocktx-payload`.
  - Out-of-range outputs are rejected by `CheckTransaction`.
  - This holds before V20 too, so a pre-V20 window start cannot carry one.
  - Assumevalid does not skip `ProcessSpecialTxsInBlock`.
- **A missing record is always a store fault, never a verdict.** Both readers return
  `StoreError::inconsistent`: `pool_after` (`:587-590`) and `credit_data_at` (`:604-606`).
  `credit_pool_info` propagates the fault, and returns a *stored* `PoolError` as `Ok(Err)`, in
  Core's read order (pool first, then `GetBalanceAt`).
- **Can construction throw on a checked chain?** It cannot, which confirms D8:
  - pre-v22: `latelyUnlocked ≤ High` is inductive, because unlocks ≤ `High − lately` and the
    distant term only subtracts;
  - v22 and v24: the limit is ≥ 0 whenever the balance is ≥ 0, and the balance stays ≥ 0 because
    unlocks ≤ limit ≤ balance in every era;
  - `PrevLocked`/`PrevIndexes` need a DIP0003 block without a CbTx, which is `bad-cbtx-version`.

## 4. Test discrimination

### 4.1 What covers what

The live walk proves the following for every block it walked, but only for pre-V24 eras:

- the pool chain;
- the window-start indexing;
- the stored `CreditBlockData.balance` read (`window.balance`, equal to `GetBalanceAt`);
- `window.unlocked`;
- the CbTx balance with the platform reward;
- the pre-v22 10% rule, including the truncation of real balances.

Mainnet stops at the 2,090,000 cap, below MN_RR and WITHDRAWALS, so the v22 era and the platform
reward are walked on **testnet only**. That is sufficient, but say so in the evidence.

The following V24 paths are covered only by unit tests or L1:

| Path | Unit | L1 (`diff_credit_pool`) | Discriminates? |
|---|---|---|---|
| `unlock_limit_v24` 20 % branch | yes (`v24_limit`) | yes, testnet step with W ≈ 71 k DASH and a non-round W, so truncation counts | **yes** |
| `MIN_UNLOCK_DROP_V24` floor (W < 10 k DASH) | yes (5000/5000) | **no**: W is fixed at about 71 k DASH | unit only |
| clamp to 0 (`limit < 0`) | yes (1000/5000) | **no**: `prev.locked` deltas reach at most 16 × 256 DASH, far short of W − 14.2 k | unit only |
| clamp to `balance` (needs drop > W) | yes (100/0) | **no** (as for the floor) | unit only |
| era switch `eras.v24` in `construct_pool` | **no**: see N1 | yes (flags=3 rebalanced: V24 ≈ 14 k against V22 = 2000 DASH) | L1 only |
| window-start balance as `UnlockLimitV24`'s second argument | **no**: see N1 | yes, but only through one fixed testnet window block | L1 only |
| v2 asset unlock (stable txid in the diff's debug text) | no | no (fixture unlocks are v1; no op edits the version byte) | **none**: N2 |
| CONS-10 wrapped ranges (binary-wide) | yes | yes (op 5, seed `[5,0,1,5,0,2]`) | yes |
| window-start `unlocked` subtraction (every era) | yes | no (the fixture window block has no unlocks) | unit + live walk |

### 4.2 Mutants

**cargo-mutants** on `creditpool.rs` and `ranges.rs` (dash-dml lib tests + `tests/store.rs`, 3
shards). Of 151 mutants, **135 were caught, 7 missed, 9 unviable and 0 timed out.**

Five of the misses are equivalent:

- `unlock_limit_v24` `<`→`<=` at :153 and :157, and `>`→`>=` at :160: at equality, both branches
  give the same value.
- `construct_pool` `>`→`>=` at :310: at `bal + lately = 100 DASH`, both branches give `bal`, since
  `bal/10 ≤ 100`.
- `construct_pool` `<`→`<=` at :312: the value 0 is clamped to 0.

Two are real, but outside consensus (N6):

- `RangesSet::range_count → 1` (`ranges.rs:138`): the only assertion uses a single-range set.
- `decode_bounded` `count > max`→`>=` (`ranges.rs:166`): the boundary `count == max` is untested.

**Hand mutants.** cargo-mutants cannot generate these. Each was run against the dash-dml suite and
node-l1's oracle-backed `l1_credit_pool`, which passes 4/4 on the unmutated tree:

| Mutant (`creditpool.rs`) | dash-dml tests | L1 |
|---|---|---|
| H1: `if eras.v24` → `if false` (V24 arm gone) | **survived** | caught |
| H2: window balance → 0 in the V24 call | **survived** | caught |
| H3: `distant.unlocked` passed as W | **survived** | caught |
| H4: no `− distant.unlocked` | caught | survived (the fixture's window block has no unlocks; the live walk covers it) |
| H5: `tx.instance_hash()` in the diff's debug text | **survived** | **survived** |
| H6: no zero record for a DIP3 block without data | caught | survived |
| H7: `prev.locked` as the V24 balance | caught | caught |

H1–H3 confirm N1, and H5 confirms N2.

## 5. Proposed follow-ups: are any blocking?

None blocks EV3-2. Each gates a later step:

- **CH7a samples (A-2).** Gate on CH7a's READY, not on this slice. They must include:
  - a stored `Err` parent pool that yields `failed-procspectxsinblock` *after* the `bad-cbtx-*`
    checks and *before* EV1's loop and the V24 shared-collateral check (`stm:846-870`);
  - EV1's `bad-assetunlock-duplicated-index`, which pre-empts the diff's pool-duplicate reject on
    real blocks;
  - `connect_records`/`pool_after`/`credit_data_at` over the real store, which the walk bypasses
    with its own `BTreeMap`.
- **The chainstate codec for `credit_data`/`PoolState` (A-1).** Gate on CH7a. N3 makes it total.
- **The L1 window-start mutation.** Not blocking now, but required before V24 can activate on
  mainnet. With N1 fixed, the unit tests discriminate every branch. The cheaper route to Core
  coverage is a second testnet credit step from just after testnet V20 (905,100 + a few hundred
  blocks), where W is small or zero. Under flags=3 it reaches the floor and the clamp to
  `balance` against the oracle, with no header relinking.
- **A walk past V24.** It cannot run yet (V24 inactive on both oracles). File it as a BL row that
  blocks the release that turns V24 on. Testnet V24 signalling opened on 2026-09-24
  (`chainparams.cpp:422`), so re-run the testnet walk across the activation once it locks in.

## 6. Findings

### Blocking

None.

### Nits

**N1: the unit tests do not discriminate `construct_pool`'s V24 branch or its window-balance
input.** `creditpool/tests.rs:352-354` and `:544-566`.

- `construct_adds_indexes_and_slides_the_window` checks the V24 pool at balance 800 and W = 1000.
  Then `unlock_limit_v24(800, 1000) = min(1800, 800) = 800`. The clamp to the balance dominates,
  so the result is the same:
  - as V22's `min(800, 2000) = 800`;
  - with W = 0 (`min(2800, 800) = 800`);
  - with `distant.unlocked` (10) passed for W.
- `connect_records_follow_the_deployments` (V24, W none) never asserts `current_limit`, and its
  values give 40 in both eras anyway.

So deleting the `if eras.v24` arm, or dropping or swapping `distant.balance` at
`creditpool.rs:304-305`, passes every dash-dml test. Only L1's testnet step catches it.

*Failure scenario:* a refactor routes the wrong window field, or the V22 arm, into V24 blocks. The
dash-dml suite stays green, and the regression shows only in the oracle-linked L1 run.

*Fix:* use values where the clamp does not dominate, for example prev or balance 20,000 DASH,
W = 21,000 and window unlocked 10, so V24 = 3200, V22 = 2000, and W = 0 or the swapped W = 10
both give 20,000. Also assert `current_limit` in the `connect_records` test.

**N2: no test covers a v2 asset unlock (the V24 stable txid) in the diff.**
`creditpool.rs:492` uses `tx.txid()`. Core's `GetHash()` is the stable txid for v2+ unlocks
(`assetlocktx.h:79-83`).

The fixture's unlocks are v1, and no L1 op edits the payload's version byte. A switch to
`instance_hash()` would therefore pass every test, and it diverges from Core's debug message on
every rejected v2 unlock after V24. The same holds for any v2-specific payload-decode drift.

*Fix:*
- add an L1 op that sets the unlock payload's version byte to 2 (the oracle judges it);
- add a unit case asserting the debug text of a rejected v2 unlock carries the stable txid.

**N3: `PoolError::UnlockData(&'static str)` (`creditpool.rs:104`) is not a closed type, but it is
about to be persisted (A-1).** The chainstate decoder will have to intern arbitrary strings back to
`'static`, or reject them.

*Fix:* replace it with `UnlockData(UnlockDataReason)`, where
`enum UnlockDataReason { Payload, TxoutOutOfRange }` and `Display` gives Core's two strings. Then
`PoolState`'s codec is total and `same_record` stays exact.

**N4: the walk's evidence line should say that mainnet covers only the pre-WITHDRAWALS,
pre-MN_RR era.** That follows from the 2,090,000 cap against MN_RR at 2,128,896 and WITHDRAWALS at
2,201,472. The v22 limit and the platform reward are live-proven on testnet alone.

*Fix:* a sentence in EV3-notes §4.3 and in the READY evidence.

**N5: `EvoDelta::credit_data`'s and `EvoStore::credit_data`'s docs say "Core would throw at each
read, the store answers a fault"** (`store/mod.rs`, `store/delta.rs:37-40`).

Add the reason it can never be a verdict: no connectable DIP0003 block holds such data, because
`CheckSpecialTxInner` → `bad-assetunlocktx-payload` and `CheckTransaction` vout ranges run on
every DIP0003 block, including before V20. Then the next reader does not have to re-derive §3.

**N6: two `RangesSet` test gaps, outside consensus but on the importer's `cpm_S` decode path
(A-5).** Two mutants survive:

- `range_count()` returning 1 (`ranges.rs:137-139`): `ranges_add_merges_neighbours` asserts it
  only on a one-range set.
- `count > max_ranges` → `>=` (`ranges.rs:166`): `decode_bounded` is tested only at
  count = max + 1.

*Failure scenario:* an off-by-one in the bound refuses a Core snapshot with exactly `max_ranges`
ranges.

*Fix:*
- assert `range_count() == 2` in `ranges_wrap_at_the_top_index`;
- add `decode_bounded(.., 2)` on a canonical two-range encoding, which must decode.

## 7. Verdict

**ACCEPT-WITH-NITS.**

- The port is bit-for-bit with rc.3 on every accept/reject path, reason string, class,
  arithmetic order and store semantic I checked.
- D8 and D9 hold on checked chains.
- No B items.
- N1 and N2 are test-discrimination gaps on V24 paths. They should land before V24 can activate,
  and preferably with this slice, since they are a few lines each.
- N3 should land before A-1's codec.
- N4–N6 are documentation and test hygiene.
- No Core security note.

Scratch: /work/scratch/ev3-2-review (three archive copies, reflinked targets, mutant output) was
deleted after the run.
