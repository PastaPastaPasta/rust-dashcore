# O2a-EV — Sol review, round 1

Reviewed Core `a8a85663c67699c930ec8966e18b1c6a0844ac7e` (five commits over `9c6dd45e0b`) and rust-dashcore `4b4e6a79e1b35de90141794a2cd733c10128990a`. The Core worktree has advanced, so this review uses the supplied revision’s Git blobs and authenticated archive. One oracle pin, v24 only under DEC-82. No branch edits, commits, pushes, GitHub comments, private-directory access, or chain/bench filesystem access.

## Findings

1. **Major — PLAN’s canonical-hash acceptance evidence is missing** (`docs/node-program/PLAN.md:648`; `dash-oracle-sys/tests/mnlist.rs:65`). No supplied evidence compares `oracle_mnlist_canonical` with O8 `getcanonicalmnlist` at ≥20 heights. The unit recorder covers codec correctness; the listdiff harness reconstructs arbitrary internal IDs and total-registration counts (`scripts/dml-listdiff-check.py:317–326`). Thus listdiff/SML equality does not establish the full canonical hash of the actual historical list, which includes those fields. **Fix:** supply ≥20 independently recorded O8 hashes compared against the oracle using actual snapshot bytes/IDs/counts, or record an explicit manager disposition moving this external prerequisite out of this gate. Evidence was requested during review; none supplied so far. This is an acceptance gap, not a demonstrated implementation defect.

2. **Minor — the harness overstates quorum signature coverage** (`dash-oracle-sys/scripts/dml-listdiff-check.py:35`; `dash-oracle-sys/examples/dml_listdiff.rs:235`). Pinned Core `CheckLLMQCommitment` calls `Verify(..., false)` (`src/llmq/commitment.cpp:301`); the omitted quorum block processor verifies signatures. `dml_block(check_sigs=true)` therefore cannot authenticate supplied member keys for a non-punishing commitment. Changing one member in the sample at 2,044,405 still gives four matches. **Fix:** correct the script’s explanation and preferably run existing `oracle_verify_commitment(..., true)` on every supplied non-null commitment, with a changed-key negative control. Reviewer-side explicit verification passes **10/10** current sampled commitments, so the current member evidence is independently substantiated.

3. **Minor — the generic C ABI concurrency contract omits new network-dependent entries** (pinned Core `src/test/oracle/oracle.h:36`). It says unlisted calls can run during `oracle_set_network`, while the new MN-list section and sign-hash/root implementations read the selected network. A raw C caller following the generic list can race network selection. **Fix:** include these entries in the restriction, or express it in terms of each entry’s selected-network requirement. Safe Rust wrappers already hold the per-pin chain mutex throughout selection, calls and retries.

## Fidelity, state and binding review

The entries call Core’s real canonical/snapshot, property-hash, state/list diff, ApplyDiff, AddMN/UpdateMN/RemoveMN, ForEachMN, payment/projection, SML and provider-signing functions. Property preimages are cross-checked against Core’s own private hash function; the committed vectors span address classes and both BLS encodings. Header/FFI signatures and `repr(C)` validation-state layout match ABI 8, including `dml_apply`’s added canonical output. Output allocations remain owned by Rust and scalars use correctly sized storage.

`dml_block` checks tip extension (`oracle.cpp:2959`), runs the V24 shared-output/CheckSpecialTxInner loop (`:2741`), then applies raw-helper preconditions (`:2995`), builds with Core (`:2997`), sets the hash, and optionally calls ProcessBlock (`:3005`). The quorum context check preserves Core’s earlier payload/type/version/height rejections. Exception/rejection stage mapping matches the scoped pipeline. The tip-extension refusal is a ConnectBlock orchestration precondition and prevents entering the known inconsistent-snapshot path. It does not replace a special-transaction verdict.

Each contextual call owns fresh in-memory EvoDB, manager list/diff caches, metadata and versionbits state. Its destructor restores the BLS scheme and mock time; the member seam bypasses the process-wide member cache. Rejected/processed calls and retries reconstruct a context rather than retaining a DB. The explicit signature negative control also rejects after successful verification in the same process. No additional shim state leakage was demonstrated.

**SML run-once:** accept the bespoke retry slot in place of PendingOutput. `oracle.cpp:3045–3099` validates output arguments before Core, serializes with ProcessStateMutex, keys the pending result by selected network/input/list, stores leaves/root/status/state, serves identical E_BUFFER retries without calling Core, and clears on completion or another input. Rust holds chain selection across retry. The short-buffer test checks returned output; ordinary unique-leaf inputs cannot expose the difference between one and two Core executions, so run-once correctness also rests on this control-flow inspection. Core’s process-wide Merkle memo is retained intentionally and documented.


## Evidence and deviation judgments

- **200-block comparison:** the final pinned replay, rather than the earlier dev run named by the summary text, is `listdiff/replay-pinned.jsonl` / `replay-pinned.log`. Its 200 match records align with 200 unique saved samples: 197 listdiff/dml_block/payee/SML matches and 3 without payee before enforcement. The payee leg checks membership in the coinbase’s payout-script candidates, rather than an independent unique proTxHash oracle; the listdiff’s last-paid updates supply the complementary state check. There are 10 non-null commitments, 4 punishing, 1 external ProReg collateral and 1 ProUpReg collateral.
- **Independent sampling:** 12 saved blocks, all ≤2,090,000, cover every non-null commitment and both collateral cases. All applicable legs match. A separately built pinned checker also matches the two collateral samples and both post-V20 commitments (4/4). Tampering each expected leg is detected. Removing the ProUpReg coin at 1,879,258 gives `bad-protx-collateral` at Check. The reviewer’s initial assertion expected `error` rather than `mismatch`; saved output was correct, the expectation was corrected, and the reviewer-built checker confirmed it.
- **Collateral/member inputs:** ProUpReg uses its MN’s collateral coin independently of payout script. The V20 list-at-work-block and modifier rules match pinned `llmq/utils.cpp:87–114,717–726`: base−8 after activation at the base; modifier activation at the work block; signature/height or hash fallback. All 10 sampled non-null commitments additionally pass explicit signature verification; the changed-member control fails that check.
- **Prior finding 9:** rejecting it is justified. Core returns an empty projected-payee vector for negative count before deployment/count/streak work. The distinct negative-consecutive-payment guard remains appropriate for the nonnegative-count path.
- **Quorum block processor omission:** acceptable for PLAN’s explicitly scoped DML check/build/process pipeline, with external `bad-qc-block` identified and O2a-LQ1 owning full quorum processing. These entries do not claim a complete ConnectBlock verdict. Finding 2 corrects an evidence overclaim resulting from this scope.
- **ExtAddr omission in the mainnet extractor:** acceptable for the sampled, explicitly version-1/2 range. V24 crafted blocks and codec/signature vectors give separate coverage; future V24 replay needs a version-3 encoder.
- **No separate negative computed-height block:** acceptable. Payload height must equal tip+1, and non-null commitment verification requires quorumIndex to match a nonnegative base-height remainder before build. Null commitments skip the raw calculation. The existing bad-height case covers the earlier rejection.
- **Reproducible pins:** both plain/fuzz two-root logs record equal archives matching pins.txt; the reviewer build independently authenticates the plain cache. The two-root builds were not repeated.
- **SB4 regeneration:** all 46 changed JSONs differ from integration only in provenance and the two benchmark instruction counts. Independently verified binary hash, embedded commit, current harness hash and 23/23 retained archive hashes. Independent check mode passed: **40 exact scenario vectors** plus **28 ChainLock scenarios × 8 randomized schedules**. Real-header-only and benchmark jobs were not rerun; their changed keys were inspected above.
- **O1c/ABI 9:** the later merger must rebase/renumber/re-pin. This is not a blocker here.

## Validation

Scratch: `/work/scratch/o2a-ev-r1-sol/`; temp mode 0700. Commands use `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and the required disk TMPDIR. The implementer cache was read-only. No RPC calls or full 200-block rerun.

- `cargo test --locked -p dash-oracle-sys --features oracle --no-fail-fast`: **80 passed, 0 failed**, including 12 MN-list and 3 crafted-block tests.
- `cargo clippy --locked -p dash-oracle-sys --features oracle --all-targets -- -D warnings`: passed.
- `cargo fmt --all --check`: passed.
- Saved-sample replay and negative controls: `sample-results.jsonl`, `sample-check.log`, `independent-checker.log`, `collateral-control.json`. Explicit commitment verification: `commitment_probe.rs`, `commitment-probe.log`, `wrong-members-control.json`.
- SB4 check runs the existing script’s check mode through a scratch import wrapper that directs its TemporaryDirectory to the required disk TMPDIR; branch script unchanged. `SB4_BUILD` identifies the retained build tree. Completed successfully: **40 exact scenario vectors**, **28 randomized scenario sets** at 8 schedules each; evidence in `sb4-check.log`. Real-header/benchmark jobs were excluded with `--no-headers`.

The implementer’s full integration gate records EXIT 0 at the reviewed Rust head, after the stated isolated dashd-flake rerun. I did not repeat that gate, private tests, two-root builds, or the 200-block execution. `git diff --check` reports whitespace only inside the generated format-patch artifact; this is patch context/footer formatting, not added Rust/C++ source whitespace.

The implementation and supplied sample pass the completed technical checks. Finding 1 prevents satisfying the stated acceptance gate; findings 2–3 are minor and do not independently prevent approval. Neither branch was modified; this report is uncommitted.

possible security note: see manager

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
