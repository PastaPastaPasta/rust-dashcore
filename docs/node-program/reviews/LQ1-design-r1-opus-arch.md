# LQ1 design review r1: architecture, interfaces, performance, verification (Opus, reviewer 2 of 2, DEC-42)

| | |
|---|---|
| Design | `designs/LQ1-quorum-members.md` r0 (2026-10-08) |
| Focus | interfaces (EV2 Fable and Opus, LQ2, CH7b, SG1, DK/LQ4, dash-spv), the effect API, the six levers, the verification plan, operations against the oracles, phasing and the PLAN. Line-by-line Core equivalence is reviewer 1's scope; I checked Core only where an architecture claim depends on it. |
| Checked against | Dash Core `v23.1.8` and `v24.0.0-rc.3` (`git show <tag>:src/...`); `designs/EV2-dml-fable.md`, `designs/EV2-dml-opus.md`, `designs/A1-architecture.md` §2.5, §2.10.1, §2.13, App. A; PLAN §4.4 rows; ORACLE.md; rust-dashcore `dev` 67b307c6, `node/integration` (O4 fixtures, `dash-sha256`), `node/pf0-counter-gate` (perfgate) |
| Mode | read-only; nothing run against the oracles |

## Summary

The design is strong. The module split, the trait boundary to EV2, the cache-free pure layer with effects, the fresh-scan policy and the `log2` treatment all hold up. I confirmed against Core that LQ1 does not depend on MN-list iteration order (CHAMP or memcmp), weights or any evonode rule beyond the Platform filter. I found no blocker.

Six majors need fixing before implementation starts:
- **M1:** the `QuorumMemberSource` contract conflicts across LQ1, both EV2 designs and A1, including how errors map.
- **M2:** the rotation memo's key (L4) leaves out inputs.
- **M3:** the never-slower check against Core is not actually set up.
- **M4:** the `membersSig` certificate is weaker than claimed, and the dash-spv arbitration rule can hide bugs.
- **M5:** the operations proposal (Q4) misclassifies `quorum rotationinfo`, and its live-datadir reflink copy is not a consistent snapshot.
- **M6:** the dash-spv cross-check calls code that dash-spv does not export.

Each fix is local: a text change, a key change, a PLAN edit or a small API export.

---

## Major findings

### M1 (major): `QuorumMemberSource` has three incompatible definitions, and one of them turns a local DB fault into an invalid block

**Where:**
- LQ1 §2.5 l.305-315 and §2.2.4 l.180 ("no change is needed on their side");
- EV2 Opus §2.11 l.512-527 and §3.8 l.792;
- EV2 Fable §3.2 l.457-466 and §2.7 l.417;
- A1 App. A l.2779-2780.

**The facts:**
- **A1** defines `members(t, base) -> QuorumMembersResult`, with no sink and no `Result`.
- **LQ1** defines `QuorumMemberSource<E: MnEntryView>` with `members(t, base, &mut dyn EffectSink) -> Result<QuorumMembers<E>, LlmqError>`. `LlmqError::Store` is defined as a node-level error that is never "invalid" (§3.12 l.764).
- **EV2 Opus** defines a non-generic trait returning `Result<Arc<[Arc<Dmn>]>, MemberError>`, has EV0 land it (l.556), and states at l.792 that "`MemberError` therefore maps the same way" as other exceptions, i.e. to `failed-procspectxsinblock`.
- **EV2 Fable**'s `BuildInput` (l.457-466) has `members: &dyn QuorumMemberSource` but **no `EffectSink` at all**. Its trait (l.417) has no sink either.

**Failure scenarios:**
- **(a) EV2 Opus mapping.** EV2b's QcTx PoSe branch calls `members`. LQ1 hits a missing MN-list diff or a missing work-block list (local corruption) and returns `LlmqError::Store`. Under EV2 Opus's mapping this becomes `DmlBuildError::Exception`, then `failed-procspectxsinblock`, and a valid block is marked invalid on a local fault. Core v24 does the opposite: `EvoDbInconsistencyError` becomes `state.Error` "so the block is not marked invalid" (v24 `evo/specialtxman.cpp:989-992`). The A1 rule and EV2 Opus's own §2.10 l.487 also say never invalid for local corruption.
- **(b) EV2 Fable.** D1 (the sink parameter) cannot be implemented, because EV2b has no sink to pass.

LQ1's statement that EV2 needs no change is wrong for both designs.

**Fix:**
- Put **one** trait in EV0, types first, as EV2 Opus proposes: `fn members(&self, t, base, effects: &mut dyn EffectSink) -> Result<QuorumMembers<Arc<Dmn>>, MemberError>`.
- Define `MemberError { Store(StoreError) /* fail-stop, never a verdict */, UnknownLlmqType(u8) }`. LQ1's generic form can stay internal (`get_all_quorum_members<L>`).
- State the mapping in the trait doc: `Store` goes to the node-error path, never `failed-procspectxsinblock`.
- Give EV2 Fable's `BuildInput` an `effects` field.
- Send this to the EV2 reconciler now, because reconciliation is in progress.

### M2 (major): the L4 rotation-memo key leaves out the coinbase inputs the computation reads, so "exact by construction" is false as written

**Where:** LQ1 §5.3 L4 l.858; §2.3 `LlmqSource::coinbase_cl` l.204-212; §3.7.3 l.598-602.

**Inputs of one rotated cycle.** I enumerated the inputs of `compute_by_quarter_rotation` for a cycle base B (template T or mined M, same prev):
- **Inputs that are identical for T and M, or covered by the key:**
  - `work = B−8` and `c_k`/`work_k` (ancestors of `work`);
  - the lists at the four work blocks;
  - `is_quorum_rotation_enabled`, via `ancestor(B, cb−1)` = the shared prev, and DIP0024 is a buried deployment (v24 `consensus/params.h:33`);
  - `skip_removed` (height and network);
  - the previous rows (in the key);
  - `BlsCtx` and the profile bit (in the key).
- **Inputs missing from the key:** the four `coinbase_cl(work_k)` results. Each is either `BlockDataUnavailable` or the coinbase facts, and §3.7.3 makes block-data availability an explicit input. The modifiers of the current cycle and of all three previous cycles depend on them.

So the cycle-base hash really is only the effect key: v24's own comment at `llmq/utils.cpp:~467` says "everything is derived from the cycle's work block". That part of the claim is right. But a memo keyed on `work BlockId` returns a stale result if any `coinbase_cl(work_k)` answer changes between fill and hit. The design's contract (§3.7.3) says it can, when data stops being available.

A second gap: the memo must never store an `Err` (a `StoreError` is transient).

**Fix:** key on the derived values instead of their sources: `(type, work_k hashes for k = 0..3, the four modifiers actually computed, a digest of each previous row read, or "absent")`.
- The modifiers subsume `BlsCtx`, the profile bit and data availability, and with L5 they cost four table reads.
- The twin then compares the memoized result with a fresh computation on every xcheck call.
- Do not cache errors.
- Keep emitting the effect under the new cycle-base hash on every hit. The design already does this, which keeps Core's one-row-per-template-hash parity.

**Why this matters:** with this fix L4 is exact, and the template/mined-block sharing claim holds. Core recomputes on every distinct template hash at a cycle-base height, because `mapIndexedQuorumMembers` is keyed by `(cycle hash, index)`. So "Core pays two or more" is accurate, and GBT at a cycle base can beat Core by more than 2× on repeated templates.

### M3 (major): the never-slower check against Core is not set up

**Where:** LQ1 §5.3 l.849, §5.4 l.864-875, §7 LQ1b-G l.1017.

**The facts:**
- **PF0 is not merged.** It is "in review" (STATUS).
- **No Core-microbench driver.** It measures Rust criterion benches and whole-node workloads; `measure --dashd` covers dashd **node** workloads only (perfgate README l.11, l.171). There is no workload kind that runs a patched `bench_dash` filter.
- **No owner or phase.** The proposed `src/bench/llmq_members.cpp` appears in no phase of §7 and has no task.
- **No fixture loader.** It must load per-block MN lists, coinbase blocks, snapshot rows and a mined index into an in-memory EvoDB. That is the same machinery as the §6.2 `oracle_llmq_ctx_*` entry points, which do not exist yet either (O2a is queued).
- **Fixtures not yet available.** The fixtures (§5.4: "lists as EV2 canonical bytes … from EV2's oracle dump") need O8's dump RPC, which is not built.
- **The comparison comes last.** The only Core comparison is in the last phase (LQ1b-G). LQ1a-B through LQ1b-F would merge with no Core comparison at all, and the per-PR gate has no previous head for new workloads.
- **Unequal workloads.** Core's side includes `ReadBlockFromDisk` for every modifier (in-memory blocksdir). Rust's side reads a fixture or L5. "Instructions vs Core" then measures different work, which matters for the ≥ 2× claims.

**Fix:**
- **Build the Core bench on the shim.** Make it a perf-stat driver over the O2a follow-up's `oracle_llmq_*` context (Q5). Use the same fixture files, built with B1's flags; check that the shim's compile flags match the release `-O2` build. Then add a `core = …` workload kind to perfgate, or record that the comparison is manual per milestone, as CS0 N4 did.
- **Assign it.** Give the bench to LQ1a-B together with `crit:llmq_members_nonrot`, so the first members PR is compared with Core, not only with itself.
- **Use fixtures that exist today.** Use `protx listdiff` replay lists or synthetic lists until O8 exists.
- **Compare like with like.** Report Core both with and without block reads.

### M4 (major): the `membersSig` certificate cannot certify member order or non-signers, and §6.4 lets it settle disagreements it cannot decide

**Where:** LQ1 §0 item 8 l.50, §6.3 item 2 l.939-942, §6.4 l.963-972.

**The fact:** Core's secure aggregation sorts the public keys before hashing and aggregating. `CoreMPL::VerifySecure` in `src/dashbls/src/schemes.cpp:240-269` (v23.1.8; v24 is the same library) serializes each key, `std::sort`s by memcmp, derives the coefficients over the sorted list, then verifies. `CFinalCommitment::Verify` passes only signers' keys (v23 `llmq/commitment.cpp:72-90`). So the certificate checks exactly this: **the multiset of operator keys at signer positions**.

**What it misses:**
- any permutation among signers;
- any permutation among non-signers;
- any wrong masternode at a non-signer position, whose key is not in the aggregate.

So §6.3's sentence "a wrong order … fails with overwhelming probability" is false for order errors that stay on one side of the signer/non-signer split. A tie-break or collateral-order bug is exactly such an error: it reorders equal or near entries without changing the set. Yet that is the bug class the text says the certificate "would catch".

**How §6.4 can be gamed.** A divergence between LQ1 and dash-spv that matches a listed dash-spv gap is "recorded, not failed", and "the commitment's `membersSig` decides who is right". Take a real LQ1 ordering bug that does not cross the signer/non-signer split:
1. It disagrees with dash-spv.
2. It gets labelled "no collateral tie-break" or "mode 2 filled".
3. Both member lists verify against `membersSig`.
4. The bug is recorded as dash-spv's gap.

**Fix:**
- **Correct the claim.** The certificate certifies the signer key set only.
- **Make `quorum info` the order gate.** §6.3 item 1, run over **all** quorums, rotated included, is the only order-exact L2 check. It must be a merge gate for LQ1b-E/F, not depend on Q4's outcome (see M5 for how to make that possible).
- **Make §6.4's classification mechanical.** A divergence may be classified only when LQ1's trace shows the trigger actually fired for that quorum:
  - mode 2 present in the capture's snapshot;
  - a pre-V19 cycle;
  - a `try_add` rejection on that path;
  - `enabled < size/4`.

  "No collateral tie-break" can never apply to a real capture: equal scores are impossible on real chains (§3.4), so a classification under that label is a failure. Otherwise the divergence fails, and the arbiter is `quorum info` (once the oracle covers that height), not `membersSig`.
- **Non-vacuity.** Every digest reports compared counts against the expected count from `quorum listextended`.

### M5 (major): Q4's premise is wrong, and the reflink-copy remedy is not a consistent copy

**Where:** LQ1 §6.3 items 1, 3 and 5 (l.936-951); §8 Q3 (l.1031) and Q4 (l.1032); the evidence probe in `/work/scratch/lq1/`.

**(a) `quorum rotationinfo` is not a pure read.**
- **The path.** Post-V20, `BuildSimplifiedMNListDiff` calls `BuildQuorumChainlockInfo` (v24 `evo/smldiff.cpp:221-226`). That calls `qman.GetQuorum` for every new quorum in each embedded diff (`:76-85`), then `BuildQuorumFromCommitment` on a cache miss, then `GetAllQuorumMembers`, and for a rotated type a cold members cache makes it write `llmq_S`. `BuildQuorumRotationInfo` builds four to five such diffs.
- **The lock.** It also runs **holding `cs_main`** (v24 `llmq/snapshot.cpp`, `AssertLockHeld(::cs_main)` at the top of `BuildQuorumRotationInfo`).
- **Scale.** About 1,300 testnet and about 2,800 mainnet requests, mostly post-V20 (testnet V20 is 905,100). That is the **heaviest** oracle load in the plan, done under `cs_main`.
- **The same path elsewhere.** Q3's P2P `getqrinfo` captures and item 5's `quorum selectquorum` (via `ScanQuorums`) take the same path for 60_75.
- **The probe.** The design's own testnet probe touched only pre-V20 heights, so it saw the pure-read case.

**(b) The identical rewrite is not what disturbs the oracle.**
- **The node does it anyway.** A running v24 node rewrites the same rows itself whenever its members cache is cold. For example, after each service restart, verifying an ISDLOCK goes through `SelectQuorumForSigning(60_75)` and `ScanQuorums` to `GetQuorum` and `BuildQuorumFromCommitment`. Serving `mnlistdiff`/`qrinfo` to peers does the same.
- **What actually disturbs it** is `cs_main` hold time and CPU on an oracle that is still syncing and shared with every other agent.

**(c) A reflinked copy of a running oracle is torn.**
- **Per-file copies.** XFS reflink copies one file at a time; there is no directory-level atomic snapshot on `/work` (XFS, checked with `df -T`).
- **Unflushed state.** The mainnet oracle runs `dbcache=12000` (ORACLE.md), so up to 12 GB of chainstate/evodb state is unflushed at any moment. A copy taken while it runs has LevelDB manifests, logs and blk tails from different instants.
- **A1's own rule.** A1 §2.9 (l.1117-1119) forbids this as an import source: "The source must be stopped and locked, or be a filesystem snapshot … Agreement on a tip is not an atomic multi-directory snapshot."
- **"O7's link-count rule" is not enough.** It governs who may *write* hardlinked files; it does not make a live copy consistent. O7 builds its reference by `-reindex-chainstate` over hardlinked sealed blocks, which is the consistent way.

**Fix:** ask the manager to rule on one of these, and correct Q4's text whichever is chosen:
1. **Throttle on the live oracles.** Treat identical-byte `llmq_S` rewrites as non-disturbing (the node does them itself). Run items 1, 3 and 5 and the Q3 captures through `oracle-cli` with rate limits, at idle priority, never while the oracle is in IBD or in a B1 window. Cap concurrency at 1, because `cs_main` is held.
2. **Use a consistent replica.** For mainnet `quorum info`, use the O7 v23.1.8 mainnet reference once it exists. It is built consistently, owned by the program, and its members are pin-identical per §3.13. For testnet, use a disposable replica built by O7's procedure (hardlinked sealed `blk`, reflinked tails, `-reindex-chainstate`), or a copy taken while the service is stopped, which needs manager approval because it is a disturbance.

Never use `cp --reflink` of a live datadir. Whatever the venue, the copy must run with its own `-port`/`-rpcport`, `-listen=0`, `-connect=0` and no ZMQ.

### M6 (major): the dash-spv cross-check (LQ1b-F, a PLAN acceptance row) cannot reach dash-spv's rotated construction

**Where:** LQ1 §6.4 l.961-962, §7 LQ1b-F l.1016.

**The facts.** On `dev` 67b307c6:
- **The function is private.** `find_rotated_masternodes_for_quorums` is `pub(super)`, in `dash-spv/src/sml_engine/rotated_quorum_construction.rs:26`.
- **So is the module.** `sml_engine` is a private module (`dash-spv/src/lib.rs:74` `mod sml_engine;`, with no `pub use`). So even `MasternodeListEngine` is not reachable from `dash-conformance`.
- **It needs more than a capture.** The construction is async and needs a `BlockHeaderStorage` for heights (refactored in #1094, c5a9b944).

A `dash-conformance` harness "running dash-spv's `find_rotated_masternodes_for_quorums` on the same capture" cannot compile without a dash-spv API change. No phase or task owns that change, and A1 rule 8 (dash-spv is not a crate dependant) makes the direction sensitive.

**Fix:** invert the harness.
- **LQ1 writes the fixture.** LQ1b-F produces a fixture file per capture: expected members per quorum, plus the trace triggers from M4.
- **dash-spv checks it.** A dash-spv integration test inside the crate feeds the capture through its own `feed_qr_info` and its `test_support` (`sml_engine.rs:793`), and compares against that file.
- **Alternative.** Have the convergence owner add a feature-gated export.

Either way it is a small dash-spv PR that needs an owner in the PLAN row.

---

## Minor findings

**m1. The scratch list is the full `MnList`.** §2.2.4 l.180; EV2 Opus §2.5 l.271-276.
- **Cost.** `MnList::scratch()` maintains the payee queue, the decaying, unconfirmed and streak sets, `by_collateral` and `core_order` invalidation on every `try_add`. A 60_75 cycle performs about 3.4k adds into `MnsUsedAtH` plus as many into the per-index lists, roughly 7k adds, each copying about 8 persistent-map paths. In debug, fuzz and xcheck builds EV2 Opus also runs `check_invariants()`. That is pure overhead on the never-slower path.
- **Fix.** Add a lean `UsedSet: ScratchMnList` with only the unique map, the proTx set and the counts, and keep `MnList` as its twin.
- **Related.** EV2's list-cache retention pins lists at quorum *bases* (EV2 Fable §2.6 step 4; EV2 Opus `CacheHint::QuorumBase`). Rotation reads lists at *work* blocks (`cycle − 8 − k·C`), so a cold cycle can rebuild three lists from up to 575 diffs each. Ask EV2 for a `WorkBlock` hint.

**m2. Downstream consumers need an explicit contract on where an entry's state comes from.** §2.2.1 l.119-121; §2.7 l.336-340.
- **The contract.** Rotated members are entries taken from lists up to three cycles old (v24 `utils.cpp` previous quarters come from `allMns` at `work_k`). Core's `Verify` reads `members[i]->pdmnState->pubKeyOperator` from **that** entry (v23 `commitment.cpp:72-77`).
- **Who it binds.** LQ2's `membersSig` verification, LQ4/DK1 (contribution encryption, member index) and LQ5 (connections by service) must read keys and services from the returned entry, never re-resolve them by proTxHash in the current list. State this in the `QuorumMemberSource` doc.
- **A gap in the trait.** `MnEntryView` has no `operator_key()`. LQ2, the §6.3 certificate and the §6.4 harness therefore cannot be written against the trait. Either add `operator_key()` (and `service()` for LQ5), or state that those consumers are concrete over `Arc<Dmn>` and the SML adapter.

**m3. Iteration order needs an explicit test.** LQ1 is right that it never depends on `ForEachMN` order. In v24 `utils.cpp:400-410` `MnsNotUsedAtH` is a `push_back` into a vector that is then sorted; the used sets are filled from quarter vectors; every `CalculateQuorum` sorts by a total order.
- **The test.** Run every member computation and row build with three `for_each` orders (memcmp, EV2 Opus `core_order()`, reversed) and assert identical members and row bytes.
- **For the EV2 reconciliation.** LQ1 needs neither `core_order` nor weights. Only the Platform type filters on Evo, and only non-rotated (v23 `:290` / v24 `:294`).

**m4. Dependencies are incomplete (Q7, §7 l.1019-1021).** The question asked about `i:BS8`: it is right for LQ1b, but **LQ1a needs BS8 too**.
- **Why LQ1a needs it.** `ScannedQuorum.commitment: Arc<FinalCommitment>` (l.224) and `MinedCommitmentIndex::mined_commitment` (l.289) are BS8 types, used in LQ1a-C. BS8 has not started.
- **The fix.** Either add `i:BS8` to LQ1a, or make the commitment an associated type (`type Commitment`). The scan needs only `quorum_index`, `quorum_hash` and the mined block.
- **Missing from PLAN LQ1a.** The row lists only EV2a and BS5. Add CS0, CS4 (TESTDUMMY for `LLMQ_TEST_V17`), BS6a and BS11a-t, and the O2a follow-up (Q5) as the design's text says.
- **LQ1b's `i:EV2b`.** It should be `EV2a`: the scratch `try_add` is in EV2a-1 (EV2 Opus l.1033). EV2b is a *consumer* of LQ1's trait (EV2 Opus asks for `i:LQ1a` on EV2b), so LQ1b `i:EV2b` makes a needless cycle.

**m5. L5 (the CbTx-CL side table) as written is not exact.** l.602 says "decoded CbTx CL payloads filled at connect time". The decode depends on the **caller's** `BlsCtx` (§3.3 l.412-416: `for_block(h)` at connect, `for_tip` at runtime; they differ where V20 precedes V19 on regtest).
- **What to store.** Store exactly `CoinbaseCl::Coinbase { is_coinbase, n_type, extra_payload }` and decode on read.
- **A miss is not "unavailable".** A table miss (imported datadirs, blocks stored but never connected on a side branch, blocks before the table existed) must fall back to a block read, never to `BlockDataUnavailable`.
- **Pruning.** Name the hook that flips entries when block data is pruned.
- **Size.** Rows are needed only for work-block heights, `h ≡ −8 (mod 24)`, which covers every configured interval.

**m6. L2 relies on an SH1 API that does not exist.** `dash-sha256` (`node/integration`, `src/lib.rs:197-229`) is multi-way only for `sha256d64`, the *double* SHA-256 of 64-byte inputs. The score is a **single** SHA-256 of 64 bytes. A multi-buffer single-SHA256-64 kernel is new SIMD code with no Core counterpart. Give it a task, a twin, and SH1's QEMU recipe (SHA-NI and aarch64), and do not cite DEC-28(a)'s D64 numbers as its expected gain.

**m7. Where the levers live.**
- **L6** needs a new trait method, e.g. `newest_by_index(t, at, index, k) -> Vec<BlockId>`. Add it to `MinedCommitmentIndex` in LQ1a-C, so LQ2 implements it once. I checked Core: `GetMinedCommitmentsIndexedUntilBlock` (v24 `blockprocessor.cpp:802-822`) is round-based with truncation inside a round, so one pass per index gives the same list. L6 is exact.
- **L1's `ScoreMemo`** has no path through `get_all_quorum_members`'s signature.
- **L4** is state. It belongs in `dashd-chainstate` next to `MembersCache` (§3.7.4), not in the pure layer, which §0 item 3 says has no caches.

**m8. Effects outside connect attempts, and the overlay.**
- **Runtime callers have no defined sink.** These callers emit `StoreQuorumSnapshot` with no sink defined: QM1's quorum building, DK1 session init, `qfcommit` intake and the RPCs (§3.7.1 table, l.578). On a consistent node every such emission is a rewrite of a row written when the cycle base connected. Define their sink as "debug-assert equal to the stored row, then drop", or route it to the store.
- **The read-your-own-writes overlay is unnecessary.** §3.7.2 l.594 asks CH7b to layer emitted rows over the store, but within one attempt no computation reads a row it emitted: cycle C's row is read only by cycles after C, and a quorum of base C cannot be mined in C. That overlay needs a `RefCell` shared between `&dyn LlmqSource` and `&mut dyn EffectSink`. Replace it with an assertion.
- **Duplicates.** Let CH7b deduplicate identical rows within one attempt's log; this is unobservable.

**m9. `is_quorum_type_enabled` (Q9).**
- **Agreed:** LQ1a owns it in `dash-evo::llmq::params`, and LQ2, CH7b, DK1, QM1 and LQ5 reuse it.
- **Missing overrides.** The public signature (l.246) lacks the two optional overrides that v24's `quorum dkginfo` passes (§3.2 l.386). Add an `_with(overrides)` variant.
- **No oracle for LQ1a-A's check.** The acceptance "table equals Core's" has none: `oracle_chainparams_json` does not evaluate `IsQuorumTypeEnabled`. Add `oracle_llmq_type_enabled` to the §6.2 set, or derive it from `oracle_llmq_members` returning empty versus non-empty.

**m10. Non-vacuity and races in L2.**
- Every digest and the §6.4 harness must report "compared N of expected M", with M from `quorum listextended`, and must fail when dash-spv resolves fewer than a set fraction of a capture's quorums.
- Item 5 (`selectquorum` at the tip) must bracket each batch with `getbestblockhash` and discard batches where the tip moved.
- Rotated selection (quirk 30c, a recently failed index) gets historical coverage only from L1 fuzzing and constructed vectors. Say so.

**m11. Captures (Q3).** The four O4 `qrinfo` captures all come after V20 (2,224,359 to 2,518,986).
- **Uncovered eras.** None covers DIP24 activation, pre-V19 `skipRemoved`, or the first V20 cycle. That makes the "about 12 per era" addition necessary, not optional. It is subject to M5's ruling, since `getqrinfo` takes the same path.
- **A missed capture.** The design leaves out `dash/tests/data/core_v24_regtest_qrinfo.json`: Core v24 regtest captures from `feature_llmq_rotation`, with small lists that exercise 28e. Add it.
- **More without the oracles.** Generate more captures from the Core functional-test chains (memory note `core-functional-test-chains-as-vectors`); they need no oracle at all.

**m12. Phasing.**
- **Split LQ1b-G.** It bundles the 72 CPU-h budget, the v24 predictor and four consensus levers that each need two reviewers. Split it into G1 (predictor and 72 CPU-h) and G2 (levers, one commit per lever, each with its twin).
- **Keep LQ2 off the levers.** LQ2 merge-depends on "LQ1b". State that LQ1b-E (functionally complete, plus testnet L2) is enough for LQ2, so the levers and budgets stay off the CH7b critical path (week 28).

**m13. L2 list inputs need a stock-Core check.** §6.3 l.932 checks replayed lists against `CanonicalMNListHash`. Stock v24 does not expose it over RPC (it lives only in `evo/snapshot.cpp`), so this needs O8's patch, which is not in LQ1a's dependencies. Until O8 exists, check each block's CbTx `merkleRootMNList`. It is stock, and it covers every field LQ1 reads except the `try_add` inputs (collateral, owner key, platform node id).

## Nits

- **n1.** §2.2.4 uses EV2 Fable's names (`add_mn`, `iter_valid`). Re-target the adapters at the reconciled `EV2-dml.md` (EV2 Opus: `scratch()`, `try_add(Arc<Dmn>, bool)`).
- **n2.** L1 could memoize the **sorted** `calculate_quorum(mn_list(work_k), m_k)` keyed by `(work_k, m_k)` instead of per-entry scores. That list is exactly what the earlier cycle's `build_quorum_snapshot` sorted, so the memo also saves the sort.
- **n3.** The SML adapter cannot honour `collateral() -> &OutPoint`. Document that it returns a constant null outpoint.
- **n4.** The BS5 domain check (§2.6) is a base-sdk change. It could be a check at `LlmqCtx` construction in `dash-evo` instead, which avoids a cross-repo PR for one assertion.

---

## Answers to the six questions

1. **Interfaces.**
   - **`MnListView`/`ScratchMnList` fit both EV2 designs.** The entry has six fields, the list has five operations, and `try_add` has `AddMN`'s acceptance rule. EV2 Opus §2.5 even specifies `try_add`'s failure order for this purpose.
   - **Nothing is missing for selection.** It needs no CHAMP order (m3), no weights, and no evonode rule beyond the non-rotated Platform filter.
   - **What needs fixing:** the shared `QuorumMemberSource` contract (M1), the state-provenance contract and operator-key access for LQ2, DK1 and LQ5 (m2), and the scratch cost (m1).
   - **SG1** consumes only `select_quorum_for_signing` and QM1's quorums, so it is fine.
   - **dash-spv:** see M6.
2. **Snapshots as effects.**
   - **Clean for the connect path.** The pure functions take the sink, emission happens before any `Err`, and A1 §2.10.1's actor applies effects in order regardless of the result.
   - **Clean for templates.** `TestBlockValidity` is a `ChainCommand` on the V actor (A1 l.982), so template effects reach the applier, and row content does not depend on the template hash.
   - **Gaps:** sinks for runtime callers, the needless overlay, and duplicate rows (m8). Store-error mapping through EV2b is in M1.
3. **Performance.**
   - **L1:** exact (pure in 64 bytes).
   - **L2:** exact, but needs a new kernel (m6).
   - **L3:** exact, because the order is total over distinct entries and the non-rotated input comes from one list.
   - **L4:** exact only with the M2 key fix.
   - **L5:** exact only with raw storage and a miss fallback (m5).
   - **L6:** exact (m7).
   - **Template/mined-block sharing:** sound and worthwhile once M2 is fixed.
   - **The never-slower check:** not set up (M3).
4. **Verification.**
   - **Strong pieces:** L1 over Core's real `GetAllQuorumMembers`, `quorum info` over all quorums, and the per-cycle row comparison together catch a selection bug.
   - **Weaker than claimed:** the `membersSig` certificate is a signer-set check, not an order check.
   - **Where it can be gamed:** §6.4's classification can launder ordering bugs. Fix with M4 and m10.
   - **Captures:** the O4 set does not cover the risky eras (m11).
5. **Operations.**
   - **Not safe as proposed:** the reflink-copy plan is a torn copy of a live oracle with a 12 GB dbcache, which breaks A1's own import rule.
   - **Built on a false premise:** `quorum rotationinfo` and `getqrinfo` post-V20 rewrite rows too, and hold `cs_main`.
   - **A manager ruling is needed** (M5).
6. **Phasing and the PLAN.**
   - **Phases:** the three plus four phases are reasonable once G is split and LQ2 depends on E (m12).
   - **`i:BS8`:** right for LQ1b, but LQ1a needs it too, or should decouple from it (m4).
   - **`is_quorum_type_enabled`:** LQ1a should own it, with an override variant and an oracle check (m9).
   - **Missing owners and dependencies:** the Core bench (M3), the dash-spv export (M6), O8 or a substitute for L2 lists (m13).

## Verdict

**APPROVE-WITH-CHANGES.** Fix M1–M6 in the design text and the PLAN rows before LQ1a-B starts. M1 should also go to the EV2 reconciler now. The minors can be folded into the r1 revision.
