# CH3a a7-1 (StateDigest records over CoreConnector): independent review, round 1 (Opus)

| | |
|---|---|
| Reviewed | `node/ch3a-a7` at 63d401c79, the slice `git diff bd65b38b0 63d401c79` (27133fa54, a7750af21, d2508d7cd, 5ca7f806b, 31fd88c92, 63d401c79): 26 files, +3085/−48, all six commits signed (`%G?` = G). |
| Against | Dash Core `v24.0.0-rc.3`: `validation.cpp` (`v`) :1884-1889 `GetBlockSubsidy`, :2239-2243 `AbortNode(state, …)`, :2505-2511 the `CheckBlock`/MUTATED branch, :3428-3437 `ConnectTip` (`BlockChecked`, the `ConnectBlock … failed, %s` line); `consensus/validation.h` :92-98 `Error`, :105-115 `ToString`; `consensus/params.h` :22-48; `deploymentinfo.cpp` :9-22; `versionbits.cpp` :192-197; `index/coinstatsindex.cpp` :117-139, :403-422. O8's emitter `node/state-digest-v24.0` (cc11b5d525): `statedigest.cpp` (`AttemptScope::Connected`, `Rejected`, `Threw`, `Failed`, `Disconnected`), `statedigest.h` `Guard`, and the `validation.cpp` wrappers (patched :2281-2296 `DisconnectBlock`, :2486-2512 `ConnectBlock`). |
| Context | O8 spec §4, §5, §8; O9a compare notes; CH3a D133–D147; the manager's brief and the implementer's flags. |
| Method | Everything was read against Core with `git show`; no Core checkout moved. I built in a detached worktree at `/work/scratch/ch3a-a7-review/wt` with a target reflink-seeded from the implementer's and every `.rs`/`Cargo.toml` touched. Ran `cargo test --locked -p dashd-chainstate -p dash-statedigest -p dash-conformance -p dash-consensus` (1,297 passed, 0 failed, 22 ignored, at load ≈30; `connector::digest` 12/12 plus `digest_tap`; `digest_replay_core` 3/0 in 220 s, its testnet test skipping loudly). Ran the release testnet L2 once against the implementer's window, read-only. Seven env-gated hand mutants (`A7MUT=n`), each run against the lib's `digest` tests, and the survivors against the FT replay. Read the gate run dir and the bench_inputs output. No dashd, no network or oracle calls, no PF0. |
| Verdict | **ACCEPT WITH NITS.** No Blocker or Major. Three Minors: two are D-row accuracy (D137's Core claim is wrong; D138's divergence is wider than the row says), and one is a test gap (two mutants survive every test and corpus). Plus four Nits. Every implementer claim I could check holds. |

## 1. Key fidelity

### Derived keys (O8 §5.2), against `AttemptScope::Connected`

`connect/digest_log.rs:124-149`:

- **`bls`** is `!active_after(Some(id), V19)`. Core's is `!DeploymentActiveAfter(&index, V19)`. Equal.
- **`bur`**: `BURIED` (`:45-61`) has Core's `BuriedDeployment` enum order (`params.h:22-39`, HEIGHTINCB … WITHDRAWALS), and bit *i* = `active_after(Some(id), dep_i)`. Equal.
- **`dep`**: `VERSIONBITS` (`:66-70`) follows `DeploymentPos` order with `VersionBitsDeploymentInfo`'s names (`deploymentinfo.cpp:9-22`). The rows are `versionbits(Some(id), pos)`, which is `State(&index)` and `StateSinceHeight(&index)`: the answers for the child. Equal.
- **`undo`** is `sha256d` of the `CBlockUndo` streamed for the connect (`serial.rs` `scratch.undo`, built every connect, before the `HAVE_UNDO` skip). This is Core's `TapUndo(blockundo)` before `WriteUndoDataForBlock`. It is `None` at genesis.
- **The header keys** are O9a-2's `header_fields`, unchanged.

### Golden keys (O8 §5.3)

- `golden()` (`:161-202`) appends genesis with an empty undo and the genesis coinbase value as its subsidy. Core does the same: `coinstatsindex.cpp:124`, and `GetBlockSubsidy` returns `vtx[0]->GetValueOut()` for genesis at `v:1886`.
- For other blocks, `stats` comes from `block_stats` and `bi` from `block_info(prev)` in the spec's seven-column order.
- **Continuity.** The accumulator's tip must be the block's parent on append and the block itself on rewind. This mirrors the index's checks at `coinstatsindex.cpp:133-139` and `:417-422`.
- **Rewinds** happen only for `active && Ok(DisconnectResult::Ok)` (`connector.rs:204`). That is exactly the disconnect after which `DisconnectTip` flushes, the one the index later rewinds. Core rewinds lazily, at the next `BlockConnected`, through `Rewind` → `ReverseBlock`, but the state it reaches is the same.
- **`subsidy_of`** (`serial.rs:537-550`) takes the parent's `nBits` and height, and `v20 = active_after(at.prev, V20)`. That is `DeploymentActiveAt(*pindex, V20)`, as at `v:1887`. Equal.

### Excluded keys (D144)

`mnl`, `qc`, `cp` and `ehf` are absent from every `C`, and the tests assert that Core's side is `null` in every paired record. The patch writes them `null` below DIP0003, and below V20 for `cp`/`ehf`. So the exclusion passes over nothing Core recorded. I confirmed this by reading the patch: `dip3 ? a.mnl : "null"`, and `cp ? a.ehf : "null"` with `cp` empty unless V20 is active at the block.

### Keys whose equality hides a divergence (surveyed over the testnet golden slice and all 8 FT goldens)

- **`dep`, rows `v24` and `evo_shares`:** `["defined",0]` at every height of the testnet window and of all 8 FT nodes. Only `testdummy` moves: `failed,0` throughout on testnet, which exercises the NEVER_ACTIVE branch; on regtest it goes defined→started(144), and defined→started(10)→locked_in(20)→active(30) on `feature_new_quorum_type_activation`.
  - So a port error in those two deployments' BIP9 parameters, in their EHF path, or in `ChainDeployments`' `NoEhfSignals` (its own doc admits it diverges after V20 on chains with MNHF signals) is invisible today.
  - **Corpus that would exercise it:** `feature_mnehf` and `feature_evo_shares_activation`. Both are evo, so they need CH7a's hooks, G1 lifted and a real EHF source.
- **`bls`:** constant within each corpus (testnet `true` at 0–6,999, regtest `false` everywhere). The `active_after`/`active_at` choice is pinned only by the genesis unit test (M5 below). A testnet window across V19 (850100) would cover it, but that is past DIP0003, so it waits for evo.
- **The V20 branch of `subsidy_of`:** never taken in any corpus, and M7 survives (m-3).
- **`bi[4]`** (`unspendables.genesis_block`) is 0 at every non-genesis height. That is by definition, so harmless.

The rest move: `bur` (testnet 0→1→3→7→15→47→111), `undo`, `stats`, `bi[0..3,5,6]`, `amount`, `unspendable` and `muhash`.

## 2. The record position (D137) and the None path

- **dashd-node (`digest: None`, `dashd-node/src/chainstate.rs:427`).**
  - `record_connect` and `record_disconnect` return `out` unchanged.
  - The tip paths skip them behind `ran && self.digest.is_some()` (`connector.rs:320`, `:428`). The proposal path calls `record_connect`, which returns at its first line.
  - Consensus and crash behaviour are unchanged.
- **New per-block work on the None path:**
  - two bools (`active`, `ran`) and one `is_some()` branch;
  - `self.helper.begin_attempt(cx, at)?` (`serial.rs:627`), a dynamic call to `NodeHelper`'s default `Ok(())`.

  That is a handful of instructions per block, so the PF0 risk is negligible. bench_inputs agrees: only `dashd-chainstate` is CHANGED (`/work/scratch/ch3a-a7/bench-inputs-a7-1.out`), so the usual PF0 re-gate applies, under the 10-21 CPU-change hold.
- **With a sink:** see m-1. The port's order (effect log, `end_attempt`, record, commit) matches where Core writes. What happens after a failed write does not match.

## 3. D133, D138

- **D133** (`digest_tap.rs:76-82`) is exact against the patched `DisconnectBlock` wrapper: `DISCONNECT_OK` → `ok`, `DISCONNECT_UNCLEAN` → `unclean`, anything else → `failed`, and `catch (...)` → `failed`. The port maps `Err(Fatal)` to `failed`.
  - No test anywhere writes an `unclean` or `failed` `D` through either writer, so M3 survives (m-3).
- **D138** is real. Core's `AbortNode(state, …)` calls `state.Error`, which keeps `CheckBlock`'s reason because the mode is `M_INVALID`, keeps its debug message, and sets `M_ERROR` (`validation.h:92-98`). Core's `R` is therefore `error` / `bad-txnmrklroot` (or `bad-txns-duplicate`) / CheckBlock's `dbg`. The port's `SystemError(CORRUPT_BLOCK)` (`serial.rs:646-647`) gives `why` = the abort text and `dbg:null`.
  - It is acceptable to leave for a7-1: the case needs a corrupt block file, and the `AbortNode` message and the latch are identical.
  - It is not confined to the digest, though (m-2).

## 4. `DeploymentView::versionbits` (27133fa54)

- It is a defaulted method returning `None`. The 18 other `DeploymentView` impls are unchanged and no existing caller calls it, so no existing behaviour changes.
- `Deployments` overrides it by delegating to the existing `state` and `state_since_height` (CS4, accepted).
- Every `ConnectCx` the connector sees is built from `ChainDeployments::view()`, a `Deployments`: the tip path at `abc.rs:272` and proposals at `proposal.rs:66`. So every `C` has `dep`. `BuriedOnly` (`seams.rs:295`) feeds only `contextual_check_block`.
- **Period boundary:** `deployments_answer_versionbits_from_their_cache` pins `at(9) = (Started, 10)` and so on, and the FT node above matches Core at 9, 19 and 29.
- **Disabled deployment:** testnet `testdummy` is `failed,0` from genesis, which matches `GetStateSinceHeightFor` returning 0 for NEVER_ACTIVE (`versionbits.cpp:195-196`) and the port's `versionbits.rs:183`, `:506`. The spec's trap (`getdeploymentinfo` omits NEVER_ACTIVE) concerns the extractor's RPC cross-check, not this path.

## 5. Replay drivers (D145, D35)

- **No vacuous pass.**
  - The 8-node set is derived and asserted equal to `NON_EVO`.
  - Linear mode asserts `pairs == chain.len()`, no skipped heights, no uncovered keys, and every required key `equal == pairs`.
  - Stream mode asserts the single mismatch at exactly the first off-chain Core record, `pairs` (k or k+1), and every key equal over the k records before it.
  - The driver checks the miss slot after each block and first after the join, and then checks V's AbortNode, the abort messages, both latches, and V's tip against `extract.json`'s summary tip and the golden's last block.
- **The cursor advances.** Strict `finish()` at `end_attempt` turns an unasked recorded tail into a miss. `the_d35_cursor_takes_two_is_conflict_answers_for_a_removal` passes.
- **Missing inputs fail loudly.** `Vectors::tests()` errors on `read_dir`, and the tests `unwrap`. The testnet test skips (with a stderr banner) only when the variable is unset; a set variable with missing or changed files panics on read or on the pins.
- **Thin stream coverage.** `rpc_invalidateblock/node1`'s stream pairs only genesis (it stops at record 1). This is declared in D145 and is fine.

## 6. Tests, L2, gate claims

- **Unit and FT tests:** as in the header.
- **Testnet L2, from my own release build:** exit 0, pairs 6,999, range (1, 6999), all 18 compared keys `equal 6999 of 6999`, test 85.7 s. This matches D146.
- **Gate run** `1021-113434-gate-63d401c79-Ka6`: every step exits 0, and the test step reports "tests passed 6651 failed 0". Matches the claim.

### Mutants

| # | Mutant | Result |
|---|---|---|
| M1 | no golden rewind on a clean active disconnect (`connector.rs:204`) | killed (2: `a_reorg_records_its_ds_and_rewinds_the_golden_keys`, `a_log_that_missed_a_block_stops_v`) |
| M2 | record written after the commit (connect path) | killed (3, incl. `a_failed_record_write_stops_v_before_the_commit`, `a_failed_commit_follows_its_c`) |
| M3 | `D.res` swaps `unclean` and `failed` (`digest_tap.rs:79-80`) | **survives** the lib's digest tests and the FT replay (m-3) |
| M4 | `subsidy_of` off by one: the block's own header for the parent's (`serial.rs:547`) | survives the unit tests; **killed by the FT replay** (`mining_basic/node0` diverges at h150, the regtest halving), which is in tier 1 |
| M5 | `bls` from `active_at(id, V19)` (`digest_log.rs:144`) | killed (`genesis_records_a_c_without_undo_or_stats`) |
| M6 | `dep` dropped (`digest_log.rs:135`) | killed (2) |
| M7 | `subsidy_of`'s V20 flag as `active_after(at.id)`, i.e. `DeploymentActiveAfter(pindex)` (`serial.rs:546`) | **survives** everything, the FT replay included (m-3) |

## 7. Findings

### Blocker / Major

None.

### Minor

- **m-1. D137 says Core lets a failed record write change the attempt's outcome. It does not.**
  - Core records inside `attempt.Guard(…)`, which is `noexcept` and turns the failure into `AttemptScope::Failed`. `Failed` calls `Fail` → `AbortNode` → `StartShutdown`.
  - The patched `ConnectBlock` says so: "The block's outcome is decided; a recorder failure from here on only fails the recording" (`validation.cpp:2501` on `node/state-digest-v24.0`). It returns `ok` unchanged, and `ConnectTip` then flushes the view and commits evo (`v:3446-3448`). For an `R`, it marks the block `FAILED_VALID` (`v:3434-3435`) before the shutdown lands. `DisconnectTip` likewise flushes after a failed `D` write.
  - The port makes the write failure the outcome (`connector.rs:134-149`, `:185`, `:212`): nothing is committed, and an invalid block is not marked failed. `a_failed_record_write_stops_v_before_the_commit` asserts exactly the non-Core result (`digest.rs:582` "nothing committed", `:585` `!FAILED_VALID`).
  - After a restart with the log enabled, the two nodes therefore stand at different tips, or with a different invalid mark.
  - The impact is confined to a run with `-statedigestlog` whose write fails. `digest: None` is unaffected.
  - Fix, either way (the manager's call):
    - match Core: report the write failure through the abort path and keep `out`, so the commit or the `InvalidBlockFound` proceeds and V stops on the latch afterwards; or
    - keep the stricter rule and rewrite D137 as a declared divergence, with the reason, removing "Core's recorder likewise …" as an equivalence claim. Fix the test's doc to match.

  The remaining D137 citations (`v:3431`, `:3446-3448`, `:3281`, `:3285-3287`, and `assert(flushed)` after a written `C`) are right.
- **m-2. D138's divergence reaches the log and `BlockChecked`, not only the digest.**
  - In Core, `ConnectTip` passes the state to `GetMainSignals().BlockChecked(block, state)` (`v:3432`) and logs `ConnectBlock <hash> failed, ` + `state.ToString()` (`v:3436`), which is `bad-txnmrklroot, <CheckBlock's debug message>` (`validation.h:105-115`).
  - The port sends `BlockState::Error(<abort text>)` (`abc.rs:650`) and logs `failed, Corrupt block found indicating potential hardware failure; shutting down` (`abc.rs:664`). `ActivateBestChain`/`ProcessNewBlock`'s error text differs the same way.
  - The `AbortNode` message, the UI message and the latch are identical.
  - This is pre-existing (the `Mutated` branch predates a7-1). Accept for a7-1, but widen D138 to name the log line and `BlockChecked`, and open a backlog item to have this `SystemError` carry `state.Error` semantics: keep the reject reason if the state is already invalid, and keep its debug message.
- **m-3. Two branches of the new code are pinned by no test and no corpus (M3, M7).**
  - **M3:** no `D` with `unclean` or `failed` is written anywhere, by `DigestLog` or by `DigestTap`. Add two connector rows:
    - a VerifyDB-target disconnect that returns `Unclean`;
    - a tip disconnect that returns `Failed`, for example a missing undo position (`UNDO_READ_FAILED`).

    Each asserts `res`, and that no rewind happened (`stats_tip` unchanged: the next active `C` still continues). An `Err` disconnect asserting `failed` is cheap too.
  - **M7:** `subsidy_of` differs from the mutant only at block V20−1, with `nPrevHeight > nBudgetPaymentsStartBlock`. No real non-evo chain reaches that block (testnet V20 is 905100), but NoEvo's G1 does not refuse V20−1, so it is testable now. Use a unit rig whose params put V20 at some height H with the budget start below H−1, connect up to H−1, and assert `stats[0]` and `bi[6]` at V20−1 against `block_subsidy_inner(…, false)`. Otherwise record in D141 that CH7a's evo corpus is what pins it.

### Nits

- **N1. `dep` goes silently absent if a view gives no versionbits answer** (`digest_log.rs:132-141`). Today it never happens (§4), and a comparison requiring `dep` would report it as uncovered. But the same function makes a missing `undo` or subsidy a `Fatal`. Consider a `Fatal` ("no versionbits answers for the block at h") so that a future non-caching view cannot quietly thin a log.
- **N2. The testnet compare starts at height 1** (`digest_replay_core.rs:474`, per the brief), so genesis's pair is never compared on testnet. Genesis's subsidy reaches h1's `unspendable` and `bi`, so a genesis error would still surface at h1. Still, compare from 0, or give the reason in D146.
- **N3. Name the O8 citation in D137 and D138 by function, not by line.** The `statedigest.cpp:198-230` line numbers are of a local, unpushed branch that has moved before (cc11b5d525 after #7876). `AttemptScope::Rejected`/`Threw`/`Failed` are stable names.
- **N4. `Rewind::undo` carries the read buffer's tail (the checksum),** and `undo_record` decodes a prefix without checking what follows. That is correct, because `read_undo_record` verified the checksum before `DisconnectResult::Ok`. Add a one-line comment at `digest_log.rs:113` saying the trailing bytes are expected, so nobody later adds a "bytes after the record" check that breaks rewinds.

## 8. Rulings on the implementer's flags

1. **D133 `D.res` mapping:** correct and exact, including `DISCONNECT_UNCLEAN` → `unclean`, and `FAILED` and a thrown exception → `failed`. Accepted. It needs a test, though (m-3, M3).
2. **D138 divergence:** real, and acceptable in a7-1 as a declared divergence. It is wider than stated (m-2). The port should eventually return Core's reason via `state.Error` semantics, as a backlog item outside a7-1.
3. **D135, `fx` absent until CH7b:** accepted. Absent is the right encoding when the writer cannot see the effect. The tests assert that Core's `fx` is `[]` in every paired record, so nothing is lost while G1 refuses evo.
4. **D145, stream-mode R/D parity unverified:** accepted as declared. Linear mode carries the coverage claim (O9a D6). a7-2's side-block/rejection corpus must carry R/D content, M3's `res` values included.
5. **`replay.rs`:** confirmed visibility-only. Five items became `pub(crate)`, plus a rustfmt rewrap of `check_shape`'s signature. No logic changed.
6. **Testnet L2 not in tier 1:** the evidence is enough without it. Of the mutants I tried, every one that tier 1 misses (M3, M7) testnet misses too, and the subsidy off-by-one (M4) dies in tier 1's FT replay. What the testnet window adds is the difficulty-era subsidy formula and real transaction shapes, both through code that predates a7-1 and is pinned elsewhere. Keep it env-gated, rather than commit ~11 MB of raw blocks, and run it at merge prep with the pins check. Put the pair count and the 18 coverage lines in the merge-prep receipt.

## 9. Possible security note

None from this slice.
