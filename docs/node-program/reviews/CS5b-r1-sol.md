# CS5b — first Sol consensus review

Reviewer: GPT-6.1-Sol, xhigh. Date: 2026-10-09.

Reviewed `node/cs5b-inputs` at **3e9a066037adab01818a4d7e9b06e954ce9fe08d**, the seven commits after **ea501ed7**. The implementer worktree was read-only. Experiments ran in a detached worktree under `/work/scratch/cs5b-review/`, with `CARGO_BUILD_JOBS=8` and a mode-0700 `TMPDIR` under `/dev/shm`.

Read `REVIEW-COMMON.md`, PLAN's CS5b row, PROCESS/DEC-92, and the `cs5a-coin-codecs` memory note. There is **no standalone CS5 design document** in either the integration `docs/node-program/designs/` tree or the linked node-program designs tree. I used A1 §2.4, §2.16, Appendix A and Appendix E for the design requirements; this missing reference is a scope limitation, not evidence of a missing implementation.

Core comparisons use **v24.0.0-rc.3** (`da567d7dba945143eee4d2ce1621c21b32089989`), including `consensus/tx_verify.cpp`, `validation.cpp`, `crypto/muhash.cpp`, `kernel/coinstats.cpp`, `primitives/transaction.cpp`, and `evo/assetlocktx.*`. L1 tests use the authenticated pinned oracle archives. Applied the code-review-validator workflow. No commits, pushes, GitHub comments, or private-tree inspection.

## Findings, in severity order

No blocker or major finding. No consensus divergence found within Core's defined input domain.

### Minor M1 — the documented txid-order guard is ineffective

**Location:** `dash-consensus/src/coins/utxo_hash.rs:125` (also `:151`).

When a new txid differs from the current one, `push()` calls `apply()` before checking order. `apply()` takes `self.txid`, leaving `None`; consequently the following `is_none_or(...)` assertion always succeeds on a transaction boundary. It cannot reject a descending txid.

**Concrete reproduction:** use a height-1 non-coinbase coin with value 1 and script `OP_TRUE`, and push outpoints with txids `[2; 32]`, then `[1; 32]`, both at index 0. Both calls succeed, despite the public `# Panics` contract. `finish()` produces a different hash from pushing the same coins in ascending order. I executed this reproduction. Returning to a previously flushed txid also splits one transaction into multiple serialized groups.

This does not change the result for a valid Core database cursor, whose txids are ordered. It does silently permit a caller error to produce an incorrect UTXO commitment, so the guard should work as advertised.

**Suggested fix:** check the previous txid's ordering **before** calling `apply()`. Add a regression asserting that descending txids panic, while descending output indices within one txid remain accepted and sorted. Check before writing any part of the rejected group to the hash state.

### Minor M2 — the crate's integration contract omits the new CS5b APIs

**Location:** `dash-consensus/ARCHITECTURE.md:73`.

The architecture document stops at coin/undo codecs and never documents the added input checks, mutable sequence-lock heights, coin sigops, or UTXO hash helpers. A1 Appendix E requires this document to describe the crate's interfaces and deviations. These APIs have material caller obligations: input checks require validated outputs and a non-coinbase transaction, sigop counting requires available coins, sequence-lock evaluation requires a consistent parent/MTP context, and `HashSerialized2` requires database txid order.

**Concrete failure scenario:** a chainstate or mempool caller following the architecture document has no guidance on the distinction between a `ViewOr::View` fatal storage failure and a transaction rejection, or on retaining the mutated heights for Core-compatible lock points. The function-level comments are substantially better than the architecture document, but the shared integration guide is incomplete.

**Suggested fix:** add a short CS5b section summarizing these contracts, the asset-unlock early return, parent-MTP usage, `prev_heights` mutation, Core serialization/order, and the differential/vector coverage.

## Consensus assessment

- **CheckTxInputs:** asset unlocks return first; all missing inputs are checked before maturity/value checks; maturity is exactly 100 blocks; maturity precedes the value check on each input; input values and their running sum are range-checked; output coverage and fee computation follow Core. Rejection class, reason, debug text and successful fee are compared by L1. Borrowed coins preserve Core's in-memory values rather than introducing an amount-compression round trip. Storage errors stay separate from validation rejections.
- **Special transactions:** `GetAssetUnlockFee` requires the complete payload and a positive fee. BLS payload readability is checked under either scheme, matching Core's eager-wrapper fallback. The other special types use ordinary input/value accounting here. Asset-lock payload validation and v24 shared-collateral-spend rules are separate Core checks; the latter runs after `CheckTxInputs` in `ConnectBlock`, so its absence from this helper is correct.
- **Coin sigops:** P2SH counts the last pushed redeem script from each spent P2SH output; the total includes it only under the P2SH flag. Coinbase counting avoids coin reads. The new wrapper correctly uses the existing Core-compatible script counter and expands compressed coin scripts before inspecting them.
- **Sequence locks:** unsigned version comparison, `LOCKTIME_VERIFY_SEQUENCE`, disable/type/mask bits, 512-second time units, maxima over inputs, and last-invalid-height/time subtraction follow Core. Time locks use the MTP of `max(coin_height - 1, 0)`; evaluation uses the parent MTP. Same-block/mempool-height coins therefore work at zero relative lock. Disabled heights are zeroed only when BIP68 is enforced; otherwise all heights remain untouched. The context's consistency is an explicit precondition.
- **BIP112/113:** the recorded BIP112 replay uses the disclosed test-local Core `CheckSequence` port; it does not prove CS1-b's future production signature checker. The interpreter path and the BIP113 parent-MTP cutoff agree with Core's recorded mempool/block verdicts. CS1-b must rerun the vectors with its production checker when integrated.
- **MuHash:** multiplication, squaring, reduction and the inverse exponentiation chain match Core's limb implementation. Finalization divides the denominator, resets it to one, and hashes the 384 little-endian numerator bytes with single SHA256. Serialized state is numerator then denominator. Boundary-state probes found no panic or mismatch.
- **hash_serialized_2:** valid ordered inputs match Core: best-block hash, txid groups, outputs sorted by index, the quirk-33 boolean header, `VARINT(index + 1)`, expanded script with CompactSize length, nonnegative-signed amount VARINT, and group terminator, under SHA256d. MuHash coin serialization correctly uses the full height/coinbase code and fixed-width outpoint/value fields. M1 concerns enforcement of the ordering contract.

Documented panics mirror Core's assertions/preconditions: missing coins in sigop counting, non-asset-unlock coinbase input checking, malformed output values reaching `GetValueOut`, inconsistent sequence-height counts, or missing ancestry. They are not new rejection rules for validated transactions. Heights outside Core's signed-int domain and fabricated MTP values outside actual header-derived ranges are not parity evidence.

## The three disclosed evidence gaps

1. **Mutated `prev_heights`: Core-backed evidence is appropriate, because min-height/min-time/verdict equality does not observe this side effect.** The current oracle copies heights and discards the result, and `node-l1/src/inputs.rs:345` also discards Rust's mutated vector. I closed the immediate review gap with a scratch C++ harness compiling the unchanged `CalculateSequenceLocks`, `EvaluateSequenceLocks` and `SequenceLocks` definitions extracted from the pinned Core source, using Core's real `CTransaction`/`CBlockIndex`, transaction constructor and ancestry implementation. Compared the resulting locks, verdict and mutated heights against Rust for **210 cases**, covering mixed disabled/enabled inputs, type flags, zero/same-block heights, versions including negative values, inactive flags and unknown flag bits: **all agree**. For durable regression coverage, extend the oracle ABI with an output-height array, expose it in `SequenceLocksResult`, and compare it in `diff_sequence_locks`. A Core-built fixed-vector exporter would also suffice. This is no longer an unverified merge blocker for the reviewed revision.

2. **True value-sum signed overflow does not require a Core-backed verdict before merge.** Every previously accepted prefix is in `0..=MAX_MONEY`. A next coin capable of overflowing `i64` is already outside the money range. Rust's wrapping addition followed by checking both the coin and sum gives a deterministic range rejection. Core's overflowing signed addition is C++ undefined behavior, so removing the oracle's guard and calling that path would not establish portable consensus parity. Keep the Rust no-panic regression and document this distinction. Defined sums above `MAX_MONEY` do need Core evidence; the existing census reaches them, and I added explicit safe boundary comparisons, including `MAX_MONEY + 1` from two individually valid coins. They agree. The `bad-txns-fee-outofrange` branch is unreachable under the stated preconditions, so “every reject reason reached” should be understood as every reachable reason, not this defensive branch.

3. **Nonzero canonical asset-unlock signatures should be exercised against Core; no new oracle or quorum regtest construction is needed for this helper.** The public `dash-oracle-sys/tests/data/v24-asset-unlocks.json` already contains four Core-generated transactions with nonzero signatures, including payload versions 1 and 2. I compared all four through `compare_check_tx_inputs`; each accepts with fee **70,000**. Also compared 96 signature-byte mutations per transaction: **388 comparisons total, all both-accept or both-reject, no out-of-domain skips or divergences**. Fifteen additional cases confirm that the unlock branch precedes missing-input, coinbase-shape and invalid-output preconditions. The immediate evidence gap is closed. Add the four public records as permanent CS5b rows and include a canonical nonzero signature in the generator. Quorum signature authorization remains the separate special-transaction check.

## Verification and limits

Independently completed at the pinned review revision:

- `cargo test -p dash-consensus -p node-l1 --features node-l1/oracle --no-fail-fast`: **298 passed, 0 failed**, including the 15 input/lock unit tests, MuHash vectors, locktime replay, UTXO-hash replay and L1 twins. The runtime private corpus was not supplied to this command.
- `cargo test -p dash-oracle-sys --features oracle --no-fail-fast`: **62 passed, 0 failed**.
- The fixed L1 census: **3,000 input cases** = 940 both-accept, 2,014 both-reject, 46 out-of-domain; **1,509 coin-sigop comparisons**, 377 with nonzero P2SH; **2,000 sequence-lock cases** agree. Both lock kinds and satisfied/unsatisfied verdicts are reached.
- Scratch probes: the 388 signature comparisons and 15 unlock-precedence cases above; **220** safe value-sum/version/non-unlock-special-type comparisons; **210** Core sequence-lock cases with height mutation. No divergences.
- **132 raw MuHash numerator/denominator states** compared against a harness built from pinned Core `muhash.cpp`, then against Python big-integer modular arithmetic, and finally against Rust. Includes 0, 1, 2, `p-1`, `p`, `p+1`, maximum 3072-bit values, dense alternating limbs and deterministic random values. Products, reduced quotients and finalized hashes agree; repeat finalization is stable.
- Regenerated both committed fixtures using the release `dashd` and pinned Core functional framework in fresh isolated regtest datadirs. **34 locktime cases** and **six UTXO heights** reproduce **byte for byte**, including the claimed hashes `d24d174a…62117894` and `5900d5cd…b210e01faf`. Incremental MuHash replay passes.
- `cargo fmt --all --check`, targeted consensus/L1 clippy with `-D warnings`, `git diff --check`, and the fuzz registry check pass.

Recorded implementer results, not independently rerun against private inputs: **private tests PASS, two passing receipts; private seed replay PASS, 4,323 seeds**. Only pass/fail and counts were examined. Nothing under `~/private` was opened. Recorded fuzz smoke: **21/21 pass, 0 divergences, 0 crashes**. The recorded oracle-diff **104/0** is consistent with the oracle/L1 suites.

The implementer gate remains **exit 1** because of the inherited “base SDK re-pin with its audit stamp updated” selftest. The script and the relevant CS3 block code are inherited from the base, not changed by this range. Its initial integration-test failure passed the recorded rerun (54/0). This review does not waive the manager's requirement for a green merge gate after the CS3 fix/rebase, or the separate PF0 gate. I did not rerun the full workspace/performance gate.

## Verdict

**APPROVE-WITH-NITS.** The reviewed consensus behavior and newly acquired Core evidence satisfy the Sol review; address M1 and M2 as small follow-ups. Preserve the useful scratch probes as permanent tests where practical. Proceed to the sequential Opus high review under DEC-92.

**Sol gate satisfied: yes.**
