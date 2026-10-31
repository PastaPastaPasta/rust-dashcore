# EV2b round 1 (Opus, independent): DML block apply + checked differential

Reviewed rust-dashcore `node/ev2b-dml-block` at `7c6865110` (whole slice, `657a28c28...7c6865110` in `dash-dml`, `node-l1`, `node-fuzz`; D56 delta `118ac6482..7c6865110`) against Dash Core `v24.0.0-rc.3` (`evo/specialtxman.cpp`, `evo/deterministicmns.cpp`, `evo/providertx.cpp`, `evo/netinfo.{h,cpp}`, `evo/dmnstate.h`, `bls/bls.h`) and the pinned shim `f822bbcd74` (ABI 21). Real-chain windows w1b/w3 were out of scope.

## Findings

### 1. Minor: four implemented check steps are never compared exactly, and mutants at two of them survive every corpus

`node-l1/src/dml_checks.rs:950` (ProUpReg C3 `bad-protx-shared-mn`), `:951` (ProUpReg C4 downgrade), `:917` (ProUpServ C6 `bad-protx-version-disallowed`) and `:923` (ProUpServ C7 migration). In the baseline tallies, none of these four steps has an exact rejection in any corpus: generated, raw seeds, checked, probed or probe seeds (output of `l1_dml_block`; ProUpServ C4 appears only 3 times, all raw).

I planted two mutants:
- **SHAREDMN:** C3 never fires.
- **EVOVER:** C6's bound `p.version < 3` becomes `< 2`.

Neither produced a divergence through `compare_block` over the 2,534 committed corpus inputs: 600 generated, 348 `diff_dml_apply` seeds, 300 checked, 300 probed and 986 `diff_dml_block` seeds. With each mutant in place, the full `l1_dml_block` suite still passed 16/16.

D56 lists ProUpReg C3/C4 and ProUpServ C6/C7 as "not probed" and blames a legacy re-read of a basic key failing at T4. That cause applies to ProUpReg C4 at most:
- **C3** is unreachable only because the generator's ProUpReg excludes shared targets (`node-l1/src/dml_apply/checked.rs:824`).
- **C6** is unreachable because ProUpServ always takes the state's version (`checked.rs:790`), so an Evo under V24 is never sent a v2 update.

Today this costs no consensus parity. In both cases the build has the same rule (`dash-dml/src/build/mod.rs` `pro_up_reg` → `bad-protx-shared-mn`) or a following reject (`SetStateVersion` → `bad-protx-netinfo-version`), so the block verdict still matches. The cost is in the harness, which EV1-3/EV1-4 will be judged by: a wrong implementation of these steps would pass it.

**Fix:**
- Add a probe arm that points a ProUpReg at a shared masternode. The steps before C3 (T3–T5, C2) are the ones the generator already satisfies.
- Add a probe arm that sends a v2 ProUpServ (legacy NetInfo plus scalar platform ports) to an Evo under V24.
- Add both to `PROBED_STEPS`, or pin one input each in `check_layer_mutant_inputs_compare_exactly`.
- Correct the D56 note's reason for C3/C6.

### 2. Nit: the block target barely reaches the shared-collateral spend rule

`dash-dml/src/build/mod.rs:531`. Mutant SHSPEND (the rule never fires) diverges on exactly **1** of the 2,534 block-differential inputs (`layer`, one checked seed). The raw-helper L1 does require this path (`node-l1/tests/l1_dml_apply.rs:139`), so the rule is covered. The checked target, the one meant to judge verdict parity, exercises it only by chance. **Fix:** weight the checked generator's shared-collateral spend, or pin the seed.

### 3. Nit: the fuzz entry has no Ambiguous/OutOfDomain ceiling

`node-l1/src/dml_block.rs:243-253,375-384`. `compare_block` folds `CheckOutcome::Ambiguous` and `OutOfDomain` into `Outcome::OutOfDomain`, and the ceilings exist only in the committed-corpus tests (`l1_dml_block.rs:598,612`). A farm run therefore cannot see a drift in the Ambiguous share. **Fix:** have `fuzzctl` or the target's stats tally Ambiguous separately per (reason, type) and report it with the campaign.

No blocker or major.

## Brief questions

**0. dash-dml against Core.** I traced `build()` (`dash-dml/src/build/mod.rs:484-567`) and each handler against `RebuildListFromBlock` (`specialtxman.cpp` v24:309-797). They match in every point checked:
- **Build order.** Prev-list assert as a store fault. Payee taken from `prevList`. Confirmation over `prevList` with banned masternodes included. `DecreaseScores`. Special transactions in block order. Collateral spends keyed on the raw `nType`, with the shared-spend reject. Payee bookkeeping even for a removed or banned payee. Streak reset over the post-payee snapshot.
- **ProRegTx.** Collateral coin check only with a non-null best block; `MAX_MONEY` for an invalid type, as `dmn_types::Invalid`. Replacement before the duplicate checks. Entry, then share-owner/owner, then operator duplicates. `internal_id` taken from the counter after the removal (`RemoveMN` leaves `nTotalRegisteredCount`). Banned when the NetInfo is empty. The V24 any-scheme probe with a null self.
- **ProUpServTx.** `IsValidMnType`, then entries held by another masternode, hash, type mismatch, `ApplyProUpServTx` (version pre-set under V24, then `SetStateVersion(max)`), revive (shared-aware), and the migration probe on the *old* key.
- **ProUpRegTx, ProUpRevTx, shares.** ProUpReg targets and operator-change ordering match. ProUpRev keeps legacy before V24 and takes the max-version after it. ProUpShare and ProUpSharedReg (reset, `SetStateVersion(old)`, `SetLegacy(false)`, then the ungated collision probe and the payee-reuse check) also match.
- **QcTx.** Truncating `%`; a negative or absent ancestor gives `bad-qc-quorum-hash`; punishment in member-vector order skips absent masternodes and uses `CalcPenalty(66)` on the list as rebuilt so far.
- **Support code.** `PoSePunish`/`DecreaseScores` (`pose.rs`), `GetMNPayee`'s P1 fast path with literal-scan fallback (`payee.rs`), `SetStateVersion`'s NetInfo step (`netinfo/migration.rs`), and `process_block` (resets before the DIP3 pin, snapshot cadence) match too.

I found no consensus divergence. The CONF mutant (confirmation off by one, planted in both the fast list and the reference twin) diverged on 56 checked inputs (`list`), so the block differential sees build-level drift that the twin cannot.

**1. r3 Major: closed.**
- **Shared unknown-version predicate → `None` (the r3 mutant through the shared predicate).** Divergences: raw 2 (the two pinned inputs), checked 224, all `check layer: reason`. This matches D56. As Sol r4 noted, the build-only guard bypass is now unreachable behind an exact check rejection.
- **PORT (mine):** dash-dml's `CheckNetInfo` map reports `BadPort` as `bad-protx-netinfo-addr`, a wrong Rust reason behind a Core C4/C2 rejection. Raw 22, checked 2, all `check layer: reason`.
- **C8EX (mine):** ProReg C8 loses its same-collateral exemption. Raw 3, checked 371 (`check layer: reason` / `rejects a block Core's checks accept`).
- **TYPEMM (mine):** ProUpServ C4 never fires. Raw 3, `check layer: passes a step Core's reason names`.

The baseline had 0 divergences on the same 2,534 inputs. `dml_checks::compare` (`:1034-1106`) applies the rejection-point rule as ruled. An earlier transaction is a divergence. A passed implemented step naming Core's reason is a divergence. OutOfDomain needs a reached missing step naming the reason. A Core accept with any Rust reject is a divergence. The `Walk` guards (`row`, `finish`, `missing`/`verdict` ownership) prevent a skipped implemented step.

**2. Mutant E: acceptable as coverage-only, not a hiding place for a real bug.** Masking needs a later implemented step of the same transaction to reject with Core's (label, reason). Otherwise Rust either accepts the transaction or rejects it with another reason, and line 1073 (`passes a step Core's reason names`) fires because T11/T7 are implemented and were passed.

Core's `Validate` (`netinfo.cpp` v24:588-616) checks duplicates and earlier entries' `ValidateService` before the invalid entry's `Malformed`. So on inputs where C4/C2 would give port, address, duplicate or anything else, a skipped T11/T7 is a divergence. On inputs where it gives `bad-protx-netinfo-bad`, the block verdict and reason equal Core's, an equivalent mutant at the outcome level. Deserialization clears an invalid CService or Domain to `Invalid` (`netinfo.h` v24:243-263), so this does not depend on the network. The step pin (`l1_dml_block.rs:299-351`) covers the step itself.

**3. Ambiguous: bounded in the corpora and sound as an exclusion.** Every observed case is `bad-protx-payee-reuse` with Rust rejecting at a payout-independent step: ProUpReg C2 (hash) or ProReg T9–C4 (versions, NetInfo, reward). A genuine Rust false rejection there can land in Ambiguous, but only on inputs that also carry a payout reuse. Those predicates do not read the payout list, so the same bug shows on the non-reuse inputs that are compared. A systematic hiding place would need a bug correlated with payout reuse, and I see no mechanism for one. Ceilings: 9 generated, 3 seeds, 0 in the checked/probed corpora, reproduced exactly. See nit 3 for fuzzing.

**4. Deferred lows: no false negative.**
- **(a) Members-missing exception** (`dml_block.rs:273-280`). It applies only when Core rejected in its loop and every Rust check passed, so the outcome is already OutOfDomain; it can never turn an Exact or a build compare into a pass. A wrong quorum base height in the build would give `bad-qc-quorum-hash` (an Invalid), not this store fault. The same QC shape in a Core-accepted block reaches the unexcepted `store fault` divergence.
- **(b) C8 exemption.** It matches Core (`specialtxman.cpp` v24:1227-1241). It is now mutation-tested: C8EX produced 374 divergences, and the generator's replacement keeps the old service (`checked.rs:712-745`). This closes the "not mutation-tested" low.

**5. Tests.**
- `cargo test -p node-l1 --features oracle` (pinned plain archive, sha256 `c7c0ac11…` per `pins.txt`): **EXIT 0**, 25 test binaries ok. One test was ignored (`DASH_DML_CORE_LISTS` unset).
- `l1_dml_block`: 16/16, with tallies identical to D56. Generated: 197/350/9 + 44 accepts. Seeds: 97/200/3 + 48. Probed: 317/0/0. Probe seeds: 623/0/0, with 373/78/24 accept/build/process. Checked: 412/439/34.
- No wall-clock failures at load 120–580.
- Mutant runs used env-gated mutations and a scratch scan test in a detached worktree, never on the branch.

Verdict: ACCEPT-WITH-NITS
