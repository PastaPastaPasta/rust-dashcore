# O2a-CJ1 CoinJoin oracle entries and `diff_coinjoin`: Opus-h review, round 1

Reviewed: rdc `node/o2a-cj1` a5d2a9c59..**4007329ab** (2 commits, both `%G? = G`) and Core
`node/o2a-cj1-oracle-v24.0` 5c9e9117a2..**5e9abb6b28** (1 commit, `G`), in detached worktrees, on the
pinned plain archive (`/work/cache/oracle/5e9abb6b28…/plain`, sha256 as pins.txt). Core line numbers
are v24.0.0-rc.3 `0364ed626e`; `git diff 0364ed626e 5e9abb6b28 -- src/coinjoin` is empty. I skipped
the SB4 vectors as the brief says.

## Verdict: CHANGES REQUIRED (one Medium, a small fix)

The plumbing is sound. The shim calls Core's own rules over Core's own view, the seams are scoped
per thread and per call, and no process-global state leaks between calls. But the in/outs case's
domain is wrong.
- At session denomination 0, which the harness picks (`DENOMS[0]`), Core's `checkTxOut` accepts
  any non-denominated amount, so `IsValidInOuts` sums arbitrary `CAmount`s. That is signed-overflow
  UB inside the oracle.
- D6 says this cannot happen.
- dash-coinjoin's port panics on it in any build with overflow checks.
- The first `diff_coinjoin` fuzz-flavour smoke crashed after 8 CPU-s (F0), so the target fails
  its nightly smoke as registered. The tier-1 gate has no fuzz-smoke step, which is why it passed.

The other findings are harness blind spots in the collateral case, neither a divergence:
- an unseeded max-fee boundary (F1);
- dash-coinjoin's own `MoneyRange` checks, which the D5/D7 design hides from the diff (F2).

Each was shown with mutants that every `l1_coinjoin` test misses. I found no Rust/Core
disagreement on a reachable input, and no new weakness in Dash Core: F0 needs a session
denomination of 0, which Core never has.

## Findings

**F0. Medium: at session denomination 0, `IsValidInOuts` sums unchecked amounts. The oracle runs Core's signed overflow, dash-coinjoin panics, and the fuzz smoke fails at once.**
- **Core.** `checkTxOut` (`coinjoin.cpp:337`) passes an output when `AmountToDenomination(nValue) == nExpectedDenom`. `AmountToDenomination` returns 0 for every non-denominated amount, so with `session_denom == 0` any value passes, `i64::MIN` and `i64::MAX` included. Then `nFees -= txout.nValue` (`:371`) and `nFees += coin.out.nValue` (`:402`) overflow `int64`, which is UB.
  - The promotion and demotion validators reject a session denomination with no larger neighbour first, and the final transaction's `nLargerDenom` of 0 never matches. So `session_denom == 0` is the only way in.
  - D6's last sentence ("`IsValidInOuts`' `nFees` cannot overflow: it adds a value only after `checkTxOut` has held it to a denomination") is false there.
  - D16's "every … session … call is built inside the oracle's domain" is false too: the shim refuses nothing for in/outs.
- **Node.** `dash-coinjoin/src/inouts.rs:175` (`fees -= txout.value`) and `:187` panic with "attempt to subtract/add with overflow" wherever overflow checks are on (tests, debug, the fuzz build). They wrap in release. The comment at `:168` ("Every amount below passed the denomination check") has the same false premise.
- **Harness.** `node-l1/src/coinjoin.rs:141` `DENOMS[0] = 0`, a raw value tag (2), and a P2PKH script are all the fuzzer needs.
- **Concrete input** (my own construction, not the private finding): case 2, mainnet, flags 0, denom byte 0. The view has one chain coin at outpoint 0: raw value 30,000, script 0x10. No mempool, mask 0. Inputs `[0]`; outputs one, raw value `i64::MIN`, script 0x20.
  - `compare_coinjoin` calls `oracle.cj_in_outs` first, so Core runs `nFees -= INT64_MIN`.
  - Then `is_valid_in_outs` panics at `inouts.rs:175`. This was reproduced on the plain archive in the test profile.
  - The same entry with 12,345-duff values is `BothAccept`: both sides really do accept non-denominated amounts at denom 0.
- **Fuzz smoke (fuzz archive edb3b6fa…, rdc 4007329ab clean).** `fuzzctl smoke --only diff_coinjoin`, run into a private dir of my own (`DASH_EMBARGO_DIR` under this review's scratch, 0700). Result: `fail`, `crashes=1`, `divergences=0`, 14,317 executions in 8 CPU-s at ~1,800 exec/s, libFuzzer status 77. The backtrace is `panic_const_sub_overflow` ← `dash_coinjoin::inouts::is_valid_in_outs` ← `compare_in_outs` (`node-l1/src/coinjoin.rs:639`).
- **Reachability: none in production.** Core's server refuses an invalid `dsa.nDenom` (`server.cpp:925`) before it sets `nSessionDenom` (`:1015`). The client's wallet refuses to select inputs at an invalid denomination (`wallet/coinjoin.cpp:54`). Its rebalance path filters queues by a non-zero target. dash-coinjoin mirrors the server check (`server.rs:447`, `:497`). So there is no security note, but D15's fuzzing evidence doesn't exist until this is fixed, and the nightly's fuzz-smoke leg will fail.
- **Fix (any one of the first two, plus the rest):**
  - *Shim:* refuse `oracle_cj_in_outs` with `session_denom == 0` (`ORACLE_E_ARG`, "no session has denomination 0; Core's nFees then sums unchecked amounts"). The harness then predicts the refusal exactly, as for collaterals: `ORACLE_E_ARG` iff denom 0, and a mismatch either way diverges.
  - *Or:* scan `nFees` in Core's order with `__builtin_*_overflow`, as `oracle_cj_collateral` does. That keeps the non-overflowing denom-0 rows comparable.
  - *dash-coinjoin:* make `fees` arithmetic non-panicking (`checked_*`; overflow can only come from an invalid session denomination, so `Err(ErrFees)` or `wrapping_*` are both fine where the oracle refuses), and correct the `:168` comment.
  - Correct D6 and D16.
  - Add the input above as an `l1_coinjoin` row, then rerun `fuzzctl smoke --only diff_coinjoin` before merging.

**F1. Low: the max-fee bound of `ATMPIfSaneFee` (0.1 DASH) has no boundary row in the differential.**
- Core `coinjoin.cpp:447`: `result.m_base_fees.value() > DEFAULT_MAX_RAW_TX_FEE` (`:25`, `COIN / 10`) refuses, so a fee of exactly 10,000,000 duffs is valid. Rust `dash-coinjoin/src/collateral.rs:33`: `fee <= MAX_RAW_TX_FEE`.
- The seeds' "too large a fee" collateral (`node-l1/src/coinjoin.rs:1144`) spends a 10.0001 DASH coin, far above the bound. No seed, `collateral_overflow_rows` row or random input sits at the bound.
- **Mutant A** (`<=` → `<` at `collateral.rs:33`): all 10 `l1_coinjoin` tests pass. The crate's `core_ports.rs:905` `is_collateral_valid_rules` catches it.
- **Concrete input:** case 3, mainnet, flags 1 (the mempool accepts), one chain coin of 10,010,000 duffs (raw value tag) at outpoint 0, spending it into one P2PKH output of 10,000 duffs. The fee is exactly 0.1 DASH: today `BothAccept`; under mutant A, `Divergence("collateral verdict")`. With a coin of 10,010,001 it is `BothReject`.
- **Fix:** add the two collaterals (fee = `MAX_RAW_TX_FEE` and `+ 1`) to `seed_inputs` (on all four networks, like the other collateral seeds) or to `collateral_overflow_rows`. This is the same class as Sol r1's pool-size note.

**F2. Low: dash-coinjoin's own `MoneyRange` checks in `is_collateral_valid` are invisible to the differential.**
- CJ0 D12 makes `is_collateral_valid` hold every output, input coin and running sum to `MoneyRange` (`collateral.rs:56`, `:60`, `:75`, `:79`), and the `MempoolAccept` trait doc (`collateral.rs:21-24`) tells implementers they need not repeat those checks. So these lines are what keep a node's verdict Core's.
- D5 makes the harness's mempool answer refuse whenever an amount is out of range. D7 then skips the test-accept call count out of range (`node-l1/src/coinjoin.rs:747`). Out of range, both sides are therefore always `false`, whether or not dash-coinjoin checks anything. Core's verdict there comes from ATMP's `CheckTransaction`/`CheckTxInputs` (`consensus/tx_check.cpp:38-45`, `consensus/tx_verify.cpp:188-191`), which the harness models and the node does not have.
- **Mutant H** (delete the input checks at `:75-77` and `:79`) and **mutant H2** (delete the output checks at `:56-58` and `:60`): all 10 `l1_coinjoin` tests pass. `core_ports.rs:949` `is_collateral_valid_money_range` catches H; H2 passes the crate too (mutant F, `:60` alone, is equivalent: an output total above `MAX_MONEY` always fails the fee check first).
- **Fix (tested in scratch):** in `compare_collateral`, add `else { check(view.test_accept_calls.get() == 0, "asked the mempool out of range")?; }` to the `if in_range` block. Out of range, D12 means Rust must return before asking, at any position of the out-of-range amount. Then add two seeds:
  - a coin of `MAX_MONEY + 1` spent into a 10,000-duff P2PKH output, which catches H;
  - a 20,000-duff coin spent into outputs (−10,000, P2PKH) and (0, P2PKH), which catches H2.
  
  The baseline stays green with the check, including the seeds and the 1,600 random inputs. H and H2 each then fail with "asked the mempool out of range". The patch is in `/work/scratch/o2a-cj1-r1-opus/logs/suggested-check-and-probes.patch` (scratch, deleted after this review; the three lines above are all of it).

**F3. Nit: on the collaterals the oracle refuses for overflow, the node's verdict is never evaluated.**
- `node-l1/src/coinjoin.rs:736-739` returns `OutOfDomain` as soon as the predicted refusal matches, without calling `is_collateral_valid`.
- Overflow needs an amount outside `MoneyRange`: at most 20 in-range values cannot overflow `i64`. Every overflowing prefix Core sums is also a prefix Rust checks: outputs through the first failing script, since Rust checks the range before the script; inputs up to the first unusable coin. So the node's answer there must be `false`, with no test-accept call.
- **Fix:** before returning, `check(!is_collateral_valid(&view, tx) && view.test_accept_calls.get() == 0, "overflow collateral")?`. That turns the 7 `collateral_overflow_rows` refusals, and any the fuzzer finds, into evidence for the node, not just for the shim's prediction.

**F4. Nit: `BlsVerdict::Unsupported` is out of the domain (`node-l1/src/coinjoin.rs:825`), though CJ0 D9 says CoinJoin can never produce it.**
- Basic-scheme verification never answers `Unsupported` at base-sdk 8382b76 (`pkgs/pkc/src/bls/core.rs:77-87`). Mutant D, which verifies under the legacy flag, was reported as a "signature verdict" divergence, not hidden.
- But if a base-sdk bump started answering `Unsupported` for basic, every signature row would silently become `OutOfDomain`. `random_inputs` only requires compared > skipped per case, and only `message_rows` asserts outcomes.
- **Fix:** treat it as `divergence("Unsupported under the basic scheme (CJ0 D9)")`.

**F5. Nit (docs): `docs/node-program/compat/COVERAGE.md:309` (§21.2) still lists `oracle_coinjoin_valid_in_outs` as "still outside every task".**
- O2a-CJ1 adds `oracle_cj_in_outs` and seven siblings, and COVERAGE.md has no CoinJoin row. O2a-CO updated COVERAGE.md for its entry (row U5); this branch doesn't touch it.
- **Fix:** replace the name in §21.2 with the eight `oracle_cj_*` entries (O2a-CJ1, ABI 20, `diff_coinjoin`). Name the remaining groups (rounds, session replay) as O2a-CJ2.

**F6. Info (coverage, for O2a-CJ2):** the public functions of dash-coinjoin that `diff_coinjoin` doesn't reach are listed in the coverage table below. The pure ones that sit outside F2's group 1 are:
- `MixShape::of_entry`/`add`/`is_covered` (Core `common.h:147-185`, read by `server.cpp:321,343,385`);
- `is_expired` (`coinjoin.cpp:92`);
- `state_timeout`;
- the BIP69 comparators;
- `can_announce_dstx_to` (`net_processing.cpp:3612`).

Each is crate-tested only (`state_timeout`, `bip69_input_cmp`/`bip69_output_cmp`/`is_bip69_sorted`, `test_accept_if_sane_fee` and `is_pay_to_public_key_hash` aren't even named in a crate test file; they are reached through their callers). That matches CJ0 F2's group 1, so this is not a gap in O2a-CJ1. The session replay (group 3) should drive `MixShape`/`IsCovered`, and P6a/F3 owns `can_announce_dstx_to`.

No possible security weakness in Dash Core was found, so there is no private note. F0's overflow needs a session denomination of 0, which neither Core's server nor its client ever has (F0, reachability). The signed overflow in `IsCollateralValid`'s sums (`coinjoin.cpp:461-490`) is reachable from a peer's collateral. It is already recorded in the merged CJ0 notes (D12) and in D6. In the release build it wraps, and ATMP refuses the transaction.

## The brief's questions

### 1. Shim soundness

- **Core's code, Core's view.**
  - `oracle_cj_in_outs` calls `CCoinJoinBaseSession::IsValidInOuts` (`coinjoin.cpp:264-435`). It is a protected *static* member, reached through a derived `SessionAccess`; it reads no session state.
  - Its arguments are a real `Chainstate` (in-memory coins DB plus `CoinsTip()` holding the coin records), a real `CTxMemPool`, and a real `CInstantSendManager`. Core builds its own `CCoinsViewMemPool(&CoinsTip(), mempool)` inside (`:374`).
  - `oracle_cj_collateral` calls `CoinJoin::IsCollateralValid` (`:458-503`) with the view's `ChainstateManager`. It builds the same view (`:477`) and calls `ATMPIfSaneFee` → `ChainstateManager::ProcessTransaction`.
  - Mempool coins come from `addUnchecked`, so `CCoinsViewMemPool::GetCoin` (`txmempool.cpp:1866-1887`) is Core's own. For a normal tx, `addUncheckedProTx` is a no-op, and special txs are refused (D8).
  - The Rust `View::coin` (`node-l1/src/coinjoin.rs:419-433`) mirrors Core's lookup:
    - mempool first, with a past-the-end index missing;
    - chain coins that a mempool tx spends are still returned;
    - a value of −1 counts as missing, which equals Core's `IsSpent()` (`CTxOut::IsNull`).
- **Seams.**
  - `IsLocked` and `ProcessTransaction` are cross-object calls (coinjoin.o → instantsend.o / validation.o), so `--wrap` applies. The `static_assert`s pin both member types.
  - The wrappers answer from `thread_local` pointers, which `SeamScope` sets and clears by RAII, so an exception unwinding out of Core also clears them. They are installed only inside `WithView` (process-state mutex held), and fall through to Core's function otherwise.
  - `in_outs_seams` shows that the wrap is live: an unlocked mempool coin gives `ERR_MISSING_TX`, a locked one is accepted. Core's real `IsLocked` would say false for both. The shim's own overflow pre-scan reads `env.Locked()` rather than `IsLocked`, which is the same set.
  - `ATMPIfSaneFee` (`:439-455`) reads only `m_result_type` and `m_base_fees`. It asks with `test_accept=true` once, so `calls[1]` is always 0, as asserted.
- **Process-global state.**
  - The view and structure entries run under `WithNetwork`/`WithContext`, which hold `ProcessStateMutex`; on the Rust side, `on_chain` takes the pin's write lock and selects the network (`dash-oracle-sys/src/lib.rs:689-703`). `WithContext` sets and restores the BLS scheme and mock time as for every contextual call (`oracle.cpp:1046-1186`).
  - The five shared entries run under `Guarded` only, with the Rust read lock. That is safe because they read nothing global:
    - `ValidatePromotionEntry`/`ValidateDemotionEntry` and `common.h` use constants and `GetLargerAdjacentDenom`, not `Params()`. `GetMin/MaxPoolParticipants` (`coinjoin.cpp:651-652`) are read only by `IsValidStructure`, which is a context call.
    - `CheckSignature` passes explicit schemes (`CBLSSignature(Span, false)`, `VerifyInsecure(.., false)`, `SetBytes(.., legacy)`), never `bls::bls_legacy_scheme`.
    - `IsTimeOutOfBounds(current_time)` takes the time as an argument.
    - `CCoinJoinQueue`/`CCoinJoinBroadcastTx` serialization (`coinjoin.h:270-276`, `:324-332`) has no version branch.
  - Each view call builds fresh per-call objects: an in-memory `CEvoDB`/IS DB, the mempool and the chainman. Nothing persists.
- **Fuzz flavour.** Core has no `FUZZING_BUILD_MODE`/`G_FUZZING` branch outside `src/test/fuzz` (`git grep`), so the fuzz archive is the same code built by clang with `-fsanitize=fuzzer-no-link`. libFuzzer runs in-process on one thread, and the seams are thread-local and RAII-scoped. The gate on 4007329ab is tier 1, which has no fuzz-smoke step, and no `diff_coinjoin` run existed in the private tree. So I ran one against the fuzz archive. The seams and state behave there: 14,317 executions with no divergence, including the seeds' locked and unlocked mempool rows, which would diverge if the fuzz archive's `--wrap` were dead. But the run crashed on F0 after 8 CPU-s.

### 2. Verdict compare completeness

| entry | Core returns | compared | not compared |
|---|---|---|---|
| `cj_rule` | `out` | always | `ORACLE_E_ARG` domain (D9, D10); the Rust side widens (`denom.rs:262-310`), so it can't panic there |
| `cj_entry` | verdict, `PoolMessage` | both (`strict`) | — |
| `cj_in_outs` | verdict, message, consume, counts | all four (`strict`) | — (but at session denom 0 Core's answer is computed through UB and the node panics: F0) |
| `cj_collateral` | verdict, `calls[0]`, `calls[1]` | verdict; `calls[1] == 0`; `calls[0]` in range | `calls[0]` out of range (D7, F2); the node's verdict on overflow refusals (F3) |
| `cj_message_hash` | sig hash, dsq hash | both, after both decode | — |
| `cj_check_signature` | verdict | when the key is canonical and the node's verdict isn't `Unsupported` | non-canonical keys (D12), `Unsupported` (F4) |
| `cj_queue_time_out_of_bounds` | flag | always, after decode | — |
| `cj_dstx_structure` | valid, possibly-post-V24 | both (`ORACLE_E_CONTEXT` diverges) | — |

- **Can a disagreement hide?**
  - **Overflow refusal:** the prediction is checked both ways, so a refusal can't be wrong silently. Only the node's own answer goes unchecked there (F3), and that answer is forced to `false`.
  - **`INT64_MIN` refusal:** it costs only inputs whose input coins sum to below `INT64_MIN + out`, i.e. negative coin values. Both sides reject those anyway.
  - **D12's non-canonical keys:** I probed the infinity and zero encodings (`c0 00…`, `40 00…`, `00…`) for keys and signatures in both schemes, 18 combinations. Core rejects every one, and `from_canonical` refuses the keys, so nothing Core accepts is hidden.
  - **A non-canonical `fReady` byte:** a dsq with `fReady = 2` hashes, verifies and time-checks the same on both sides (Core re-serializes it as 1).
  - **F2's out-of-range collaterals** are the one class where the harness forces agreement.

### 3. Coverage against dash-coinjoin

"diff" = driven by `diff_coinjoin`/`l1_coinjoin` (seeds, rows or random); "crate" = only `dash-coinjoin/tests/*.rs`; "caller" = only through another function's tests.

| module | function | diff | crate | scope |
|---|---|---|---|---|
| denom | `amount_to_denomination`, `denomination_to_amount`, `is_collateral_amount`, `calculate_amount_priority`, `gap_threshold`, `are_adjacent_denominations`, `larger_adjacent_denom`, `smaller_adjacent_denom`, `should_promote_denoms`, `should_demote_denoms` | ✔ (rule case, every pick) | ✔ | CJ1 |
| denom | `denomination_index`, `is_denominated_amount`, `is_valid_denomination` | via callers (structure, in/outs) | ✔ | CJ1 |
| denom | `denomination_to_string` | — | ✔ | display only |
| denom | `MixShape::of_entry`/`add`/`is_covered` | — | ✔ | CJ2 (F6) |
| inouts | `is_valid_in_outs`, `validate_promotion_entry`, `validate_demotion_entry`, `validate_final_tx_composition` | ✔ | ✔ | CJ1 |
| inouts | `is_pay_to_public_key_hash` | via callers | caller | CJ1 |
| inouts | `bip69_*`, `sort_bip69`, `is_bip69_sorted` | — | `sort_bip69` only | CJ2 |
| collateral | `is_collateral_valid` | ✔ (F1, F2 gaps) | ✔ | CJ1 |
| collateral | `test_accept_if_sane_fee` | via `is_collateral_valid` | caller | CJ1 |
| dstx | `is_valid_structure`, `check_dstx_signature`, `PoolLimits::of` | ✔ | ✔ | CJ1 |
| dstx | `is_expired`, `DstxManager::*`, `validate_dstx`, `can_announce_dstx_to` | — | ✔ | relay (P6a/CJ2) |
| queue | `is_time_out_of_bounds`, `check_queue_signature` | ✔ | ✔ | CJ1 |
| queue | `QueueEntry`/`QueueManager`/`MixingCounters`, `client_process_dsq` | — | ✔ | CJ2 |
| pool | `state_timeout`, `PoolMessage`/`PoolState` text | message ids only | caller | CJ2 |
| relay | `wants_dsq_after_senddsq`, `dsq_relay`, `relay_inv_kind`, `announce_inv_kind`, `dstx_for_getdata` | — | ✔ | P6a |
| rounds | `real_rounds`, `capped_rounds`, `is_fully_mixed`, `stops_mixing` | — | ✔ | CJ2 group 2 |
| client/server | the session state machines | — | ✔ (`sessions.rs`) | CJ2 group 3 |

Nothing in this branch leaks into O2a-CJ2's scope:
- the shim reaches `IsValidInOuts` (static) and reads no `CCoinJoinBaseSession` member;
- `IsValidStructure` is the only relay-side check, and it is in F2's group 1;
- no session, queue manager or DSTX manager state is built.

### 4. Mutants

Implementation-only, in `dash-coinjoin/src`, none of them Sol's (`denom.rs:58`, `dstx.rs:329`). Each was reverted after its run.

| # | mutant | `l1_coinjoin` | `dash-coinjoin` tests |
|---|---|---|---|
| A | `collateral.rs:33` fee `<=` → `<` (max fee) | **missed** (10/10 pass) | caught: `is_collateral_valid_rules` |
| B | `collateral.rs:85` `< COLLATERAL_AMOUNT` → `<=` (min fee) | caught: `session_rows`, public seeds ("test-accept calls", case 3) | caught: `is_collateral_valid_rules`, `_money_range` |
| C | `queue.rs:25` `> QUEUE_TIMEOUT` → `>=` | caught: `message_rows`, seeds (case 6) | caught: `queue_timestamp_validation` |
| D | `queue.rs:38` verify flag `Basic` → `Legacy` (signature domain) | caught: `message_rows`, seeds (case 5, "signature verdict") | caught: `queue_sign_and_verify` |
| E | `dstx.rs:83` `inputs < min` → `inputs + 1 < min` (per-network minimum participants) | caught: `structure_rows`, seeds (case 7) | **missed** |
| G | `dstx.rs:54` post-V24 maximum `+ 1` | caught: `pool_size_rows`, seeds | caught: 4 tests |
| F | `collateral.rs:60` drop the running output-sum check | missed | missed (equivalent: see F2) |
| H | `collateral.rs:75-79` drop both input range checks | **missed** | caught: `is_collateral_valid_money_range` |
| H2 | `collateral.rs:56-60` drop both output range checks | **missed** | **missed** |

- With F1's two rows, A becomes a divergence ("collateral verdict").
- With F2's check and seeds, H and H2 become "asked the mempool out of range", with the baseline still green.
- E shows the differential carrying a boundary the crate tests don't: the 1-input (1, 10) broadcast on the three networks with 2 minimum participants.

### 5. D-rows and the ABI remap

- **D11 holds.**
  - The context seeds every deployment's cache entry at `tip->GetAncestor(h - (h + 1) % period)` (`oracle.cpp:1099-1107`), with the period being `VersionBitsConditionChecker::Period()` (`versionbits.cpp:255`). For tips below the first period's end, that ancestor is `nullptr`, and `GetStateFor` reads the seeded `nullptr` entry. That is why height 1 with V24 `ACTIVE` reads as active, as `pool_size_rows` expects.
  - The harness's `structure_context` uses the same period formula (`window_size` if non-zero), and moves a period-ending tip down one, where `DeploymentActiveAt(tip)` and the next block read different periods.
  - `v24_active` follows `GetStateFor`'s short-circuits: `ALWAYS_ACTIVE` → active, `NEVER_ACTIVE` → failed.
  - On the node side, `validate_dstx` passes `v24_active_at_tip()` (`dstx.rs:289`), which is the same `DeploymentActiveAt(tip)` Core uses.
- **D12 holds**: see the infinity and zero probes under question 2. The excluded keys are ones no masternode list holds.
- **D6 and D16 do not hold for in/outs** (F0). D6's claim that `IsValidInOuts`' `nFees` cannot overflow fails at session denomination 0, and D16's "built inside the oracle's domain" fails with it. For collaterals D16 holds as written: a wrong refusal can't pass silently, and the node-side gaps next to it are F2 and F3.
- **D17 holds.** The CJ1 server runtime must not use `MempoolAccept` for collateral or final-tx admission until MP1a's differential covers the implementation it plugs in. F2 adds a related point: until then, dash-coinjoin's own range checks (CJ0 D12) are evidenced only by `core_ports.rs`.
- **For the train driver (ABI remap):**
  - The Core branch sets `ORACLE_ABI_VERSION` 22 → **20**, below its base, while `oracle.h` still carries the "chain sessions (ABI 22)" section.
  - Merged onto integration, the number must be bumped above every ABI already there: ≥ 23, or the next free number after the bundle's other riders under DEC-238 (23 is O2a-MNHF's reservation). It must not stay 20, and must not reuse a reserved number without a DEC row.
  - The number appears in:
    - `oracle.h:80` and the section heading at `:1877` ("CoinJoin rules (ABI 20)");
    - `dash-oracle-sys/src/ffi.rs:11`;
    - `tests/smoke.rs:135`;
    - `README.md` ("Entry points (ABI 20)" and the cj row);
    - the module docs of `dash-oracle-sys/src/coinjoin.rs:1` and `node-l1/src/coinjoin.rs:2`;
    - O2a-CJ1 notes D1.
  - F0's shim fix (if taken in the shim) changes the archive anyway, so do the remap on the fixed head and re-pin once.
  - The remapped header changes the archive, so pins.txt needs the new commit and both sha256 columns. Install both flavours into `/work/cache/oracle/<new>/` before any scratch cleanup.
  - The entries are purely additive, and `git diff 5c9e9117a2 5e9abb6b28` touches no existing entry, so stacking has no semantic conflict. The only textual overlaps are the ABI line, the README header and the `oracle_fns!` list.

## Checks run

- `cargo test -p dash-oracle-sys --features oracle --test coinjoin --test smoke` (pinned plain archive, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, NODE_BENCH_ROOT unset): **7 + 18 passed**.
- `cargo test -p node-l1 --features oracle --test l1_coinjoin`: **10 passed** (the nine Sol ran plus `pool_size_rows`). No wall-clock failure.
- `cargo test -p dash-coinjoin`: 9 + 47 + 12 + 21 passed.
- Throwaway probes in the scratch worktree:
  - BLS infinity and zero keys and signatures: 18 combinations, all Core-reject and harness out of domain;
  - a dsq with `fReady = 2`: hashes, signature and time all agree;
  - a null-ProRegTx DSTX: `BothReject`;
  - the fee bound at 10,010,000/10,010,001 (`BothAccept`/`BothReject`);
  - out-of-range and negative-amount collaterals (`BothReject`).
- Fuzz flavour: `python3 -I node-fuzz/fuzzctl.py smoke --only diff_coinjoin --no-selftest --no-triage`, with `DASH_EMBARGO_DIR` a fresh 0700 dir under this review's scratch, not the shared private tree. Result: **fail**, `crashes=1 divergences=0`, 14,317 executions, 8 CPU-s, 671 s wall (mostly the node-fuzz build): F0. The finding stayed in that private dir and is deleted with the scratch. The F0 input in this review is my own reconstruction.
- Signatures: `git log --format='%h %G?'` is G on all three commits. `git diff --check` is clean in both ranges. The branch heads equal the reviewed SHAs.
- Gate: run `1018-010640-gate-4007329ab-GmW`, `steps.tsv` all exit 0 (oracle-diff 1720 s, test 1397 s; `sb12a-captures` skipped).
- Nothing was edited, committed or pushed on any branch. The scratch target and worktrees are deleted.
