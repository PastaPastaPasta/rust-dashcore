# CH3a a1: consensus review (Opus r1, second reviewer)

Range `f7abb5ef3..346bc39ca` (a6c76e0ac, 49e33220d, cd1754735, 346bc39ca), read in a detached worktree at `346bc39ca`. I compared it with Dash Core in `~/workspace/dash`. The code cites **v24.0.0-rc.3** line numbers, the repo's convention. I checked those against the rc.3 tag; every cite resolves: `GetBlockScriptFlags` `validation.cpp:2419`, `IsBIP30Repeat` `:6785`, `bad-cb-amount` `:2849`, `HasLockInputs` use `:2805`. Sol cited develop `15db478750`, whose lines are offset by 1–4. `git diff v24.0.0-rc.3 15db478750` shows no change to any in-scope rule: payments.cpp/.h, coinstatsindex.cpp, chain.cpp, instantsend/lock.cpp, kernel/coinstats.cpp, governance/superblock.cpp, primitives/transaction.{h,cpp}, and the BIP30/flags/assumevalid/value-payee lines of validation.cpp. Design: CH3 rev2.1 `91cac4d95` §2.5–2.8, §3.3, §8.1, D5–D8, D52–D55.

I read Sol r1 (ACCEPT) only after doing my own comparison. My review is independent and reaches the same verdict. Below I note where I looked further than Sol did.

## Findings

No consensus-parity defect. No security-relevant item. Nothing requires a change before merge.

| # | Sev | Where | Note | Fix |
|---|---|---|---|---|
| F1 | info (forward to a3) | `connect.rs:39-48` | a1 ships only `is_bip30_repeat`. The BIP34 shortcut and the `HaveCoin` probe belong to a3 (§8.1). Core's shortcut reads the ancestor of `pindex->pprev`, not of `pindex` (`validation.cpp:2598-2601` rc.3). So on mainnet, block **951 is still probed** and the skip starts at **952**. On testnet, 76 is probed and the skip starts at 77. Devnet and regtest have a null `BIP34Hash` (`chainparams.cpp:552`, `:785`), so no real block hash matches and the probe always runs, including under `-testactivationheight=bip34@h`. a3's boundary tests should pin 951/952 and 76/77, plus regtest with a moved BIP34 height. `bad-sb-start` (`v:2617-2622`) sits between the probe and the flags; design §2.2 row 11 already places it. | a3 |
| F2 | info | `payments.rs:330-336` | `superblock_payments_limit` runs before `is_superblock_height`, in Core's order (`payments.cpp:334-336`). Its only `Err` is at a superblock height of 0 or a height above `i32::MAX`. Neither is reachable: genesis returns before `ConnectBlock`'s value check, and `-budgetparams`/`-dip3params` negatives are refused upstream by `dash-params` (`builder.rs:579`, `NegativeHeight`). | none |
| F3 | info (evidence) | `tests/coin_stats.rs` | The mainnet 0–1300 replay is mostly self-consistency: an incremental check against a scan, and the reverse leg. Its one Core number is `txouts` 1269. The Core-sourced evidence is `conformance/tests/coin_stats_goldens.rs` (81 nodes, 21 chains, undo sha256d, totals, `bi`, `getblockstats`, sampled and tip MuHash). Neither Sol's run nor mine ran it, because it is outside the two-crate brief. The 1,000 mainnet goldens are deferred to O8 P-3, as the brief says. Before the merge claims it, the train gate should show `coin_stats_goldens` passing. | gate evidence |

## Rule-by-rule (Core rc.3 cites)

- **Script flags** (`connect.rs:22-31`, `script/flags.rs:112-132`): P2SH always, then DERSIG/CLTV/CSV/BIP147 via `DeploymentActiveAt` (buried `nHeight >= DeploymentHeight`, `deploymentstatus.h:34-38`; Rust `deployments.rs:191-196`). This matches `GetBlockScriptFlags` `validation.cpp:2419-2448`.
  - Core v24 has no DIP0020/DIP0024/V20/MN_RR/V24 block script flag. DIP0020 is always on (`script/interpreter.h:101-105`), and the port adds none.
  - All four flags plus P2SH equal `MANDATORY_SCRIPT_VERIFY_FLAGS` (`script/standard.h:47-51`).
  - The test heights equal `chainparams.cpp:180-183/380-383/553-556/786-789`, including regtest BIP147 = 0. Each is tested at ±1 on all four networks.
  - `lock_time_flags` = CSV at the block (`v:2626-2629`).
- **BIP30** (`connect.rs:48`): `IsBIP30Repeat`/`IsBIP30Unspendable` return false (`v:6785-6795`). Their only caller is `ConnectBlock`, and coinstatsindex.cpp has no BIP30 branch, so `total_unspendables_bip30` stays 0. No upper re-enable bound exists in Core or here. For the BIP34 shortcut, see F1.
- **Assumevalid** (`connect.rs:61-101`): conditions 1–4 are ANDed, then `equivalent_time <= 1_209_600`. This matches `v:2542-2571`.
  - `block_proof_equivalent_time` (`pow/mod.rs:121-142`) matches `chain.cpp:147-160` in detail: strict `>` picks the sign (equal work gives −0 = 0), `arith_uint256(int64 spacing)`, and a 256-bit wrapping product. The `tip` proof is the divisor, `bits() > 63` clamps to `±INT64_MAX` (never MIN), and the low 64 bits are cast after the check.
  - Zero proof panics where Core throws `uint_error`. Only headers that passed PoW reach it.
  - The doc's account of what `fCheckCbTxMerkleRoots` gates is checked against rc.3 `specialtxman.cpp`:
    - CbTx presence, version and payload: `:816-839`.
    - The `check_sigs` pass-through: `:871`.
    - `CheckCreditPoolDiffForBlock` only on a read CbTx: `:882`.
    - `m_qblockman.ProcessBlock(.., fBLSChecks)`: `:895`.
    - The MN-list and quorum merkle roots, gated through `opt_cbTx`: `:928-950`.
- **`has_lock_inputs`** (`connect.rs:107-109`) = `instantsend/lock.cpp:59-62`. Its `IsAssetUnlockPayload` reads the raw version, type and size (`transaction.h:383-387`).
- **`Tx::value_out`** (`primitives/tx.rs:177-194`) = `GetValueOut` (`transaction.cpp:130-139`). This commit only moves it. The `asset_unlock_payload_fee` refactor (`tx/inputs.rs:147-156`) has the same truth table as before: no payload, an incomplete one, or a non-AU decode all give `bad-assetunlocktx-payload`.
- **Payments** (`dash-evo/src/payments.rs`), branch for branch against `masternode/payments.cpp:262-441`:
  - **`IsBlockValueValid`:**
    - Order: `< nBudgetPaymentsStartBlock` → reward; `< nSuperblockStartBlock` → old budget; then `GetPaymentsLimit`; then `!IsValidBlockHeight` → reward; then the max bound before any governance query; then `IsValid()`, `NoCheck`, `IsSuperblockTriggered`, and the EV3 seam.
    - All five `strError` texts are byte-identical, with the `%d` amounts.
    - `IsValidBlockHeight` = `height >= start && height % cycle == 0` (`superblock.cpp:119-124`; `params.rs:124-126`).
  - **`IsOldBudgetBlockValueValid`:** both out-of-range texts. The window test `h % cycle < window` has `h ≥ budget start ≥ 0`, so C++ `%` and Rust `%` agree. `NoCheck` accepts in the window.
  - **`IsBlockPayeeValid`:** `IsTransactionValid` comes first and returns true below `DIP0003EnforcementHeight` (`>=`, `deploymentstatus.h:66-69`). Then `IsValid()`, `< nSuperblockStartBlock`, `NoCheck`, and the triggered branch, whose reward is `blockSubsidy + feeReward`.
  - Each governance query is asked exactly when Core asks it (once per function at a superblock height, so twice per block). The `&mut` trait keeps `IsSuperblockTriggered`'s side effects ordered.
  - `SuperblockCheck::for_block` = `v:2841-2843`; `BLOCK_RESULT_UNSET` strings `v:2849`, `:2864`.
- **Arithmetic (CAmount UB):**
  - `blockReward = subsidy + nFees` has `nFees` bounded by `bad-txns-accumulated-fee-outofrange` (`v:2717-2720`). Subsidy can leave the int64 range only through a devnet high-subsidy factor, and there CS2 already wraps as the release `imul` does.
  - `blockReward + GetPaymentsLimit` is computed across translation units (no LTO), so Core's binary adds with plain wrapping.
  - The comparisons are against `GetValueOut`, which `CheckBlock` keeps in `MoneyRange`.
  - I found no reachable input where wrapping changes a verdict.
- **Coin stats** (`coins/stats.rs`):
  - `append_block`/`reverse_block` match `CustomAppend`/`ReverseBlock` (`coinstatsindex.cpp:117-234`, `:403-505`) term for term. That includes the reverse `unclaimed` formula, which I checked algebraically: it restores both `U` and `R` exactly, given the per-append invariant `U = P + Sub − N − C`.
  - The unspendable skip uses `IsUnspendable` (OP_RETURN or > `MAX_SCRIPT_SIZE`). Coinbase is tested as `IsCoinBase` (exactly one null-prevout input), so an asset unlock with no inputs takes the undo path with zero coins, as in Core.
  - Outpoints use `GetHash`, the stable txid for v2+ asset unlocks (`transaction.cpp:115-117`; `tx.rs:80-84`).
  - Spent-coin bogosize uses the expanded script length.
  - `getblockstats` matches `rpc/blockchain.cpp:2209`, `:2322-2390` for these seven fields:
    - `utxo_size_inc` includes unspendable outputs and genesis outputs.
    - The platform-transfer fee is replaced by the payload fee before `MoneyRange`.
    - The genesis undo is empty.
  - **What a malicious block could change:** a block's outputs, spends and undo are what Core's index hashes too. Duplicate outpoints within one block are rejected earlier (`CheckBlock` duplicate-tx/merkle checks), and Core's MuHash is a multiset with the same insert/remove sequence. I found no input that makes the two differ.
  - `check_pairing` returns `Err` only for a block/undo pair that Core could never produce.
- **Kept simplifier choices:**
  - `hash_serialized_2` is a thin, ordered wrapper over the vector-tested `HashSerialized2`. Its panic on out-of-order input protects quirk 33's grouping.
  - `from_parts`/`muhash` are needed by a12's seeded index and change no decision. D34 still forbids golden seeding for a per-block parity claim, and the doc says so.
  - The `PaymentsParams` accessors are read-only and keep the cycle > 0 invariant.
  - All are fine.
- **Governance-free scope:** no `IsValidSuperblock`, `GetBlockTxOuts` or payee match is computed. Both stop at `Ev3Branch` with Core's arguments, and `sb_payments` is never asked by a1 code. EV3 must call `superblock_payee_valid` after its masternode check passes, as the doc says.

## Test run

`CARGO_BUILD_JOBS=6 cargo test -p dash-consensus -p dash-evo` in the private worktree (target under this directory, deleted afterwards): exit 0; 47 test binaries and doctest suites, 390 passed, 0 failed, 4 ignored. That includes 155 `dash-consensus` and 54 `dash-evo` unit tests, `mainnet_coin_stats_through_1300`, the Core UTXO hash vectors and the doctests. `conformance/tests/coin_stats_goldens.rs` was not run (F3).

VERDICT: ACCEPT
