# SB7 Sol review, round 1

Reviewed head: `068f270a6392d7e94f7b13015aaef438811a425d`,
base: `8eae79676`, branch: `node/sb7-sml-core`.

During the review the live worktree advanced to `3d1668790`. Its only diff from
the requested head is a two-line provenance comment in the history test; no
runtime or test behavior changed. This report's verdict remains for `068f270a6`.

Scope: shared SML entry/hash/roots/scores, incremental root cache, CbTx proof and
root checks, coinbase-CL predicate, dashcore/dash-spv adapters, pure-crate layering,
and PF0 workloads. The implementation branch is read-only. PLAN §7 is handled by
codenames only; excluded locations are not read.

## Findings

### M1 — major: history acceptance can pass an incomplete dump

`dash-evo/tests/cbtx_root_history.rs:99`, `:104`, `:108`, `:143` at the reviewed head.

The history checker treats gzip/read failures and malformed JSON as successful
end-of-input, requires only one compared CbTx, and checks neither consecutive
heights nor an expected final height. A one-record prefix of the testnet dump
followed by a truncated JSON record exits successfully: heights `7000..=7000`,
one CbTx root checked, zero mismatches. This cannot satisfy PLAN SB7's "CbTx MN
root equal over all history" acceptance criterion. The supplied mainnet logs also
cover disjoint windows rather than every eligible height through the cap.

Suggested fix: give acceptance runs an expected start/end (with mainnet end no
greater than 2,090,000), assert consecutive heights and the expected final height,
and fail on read/parse errors. An explicitly labelled advisory prefix mode may
remain useful for a dump still being written. Supply complete capped coverage
and re-run acceptance before claiming this gate is satisfied.

## Code and deviation assessment

- **Entry/hash:** matches `CSimplifiedMNListEntry::CalcHash` at the v24 pin:
  the version is excluded; proTxHash/confirmedHash and stored operator bytes
  are included; type starts at v2; Evo's port is little-endian at v2 and absent
  at v3; platform node ID remains. Legacy service ports and ADDRv2 ports are
  big-endian. Extended map framing, purpose order, first-duplicate retention,
  invalid-entry canonicalization and unknown-version short-circuit match Core
  for values Core actually stores. The adapter canonicalizes before conversion.
  Scratch-only checks of 16 version/type/network cases passed, including
  v3 repeated/out-of-order purposes and raw non-point operator bytes.
- **List/root:** ProTxHash's `Ord` is memcmp, not numeric uint256 order. Both
  full and cached roots use that order; empty pure lists give the zero root.
  Mutation detection compares real siblings before odd duplication at every
  level. The optimization retains only parents entirely before the earliest
  move/dirty leaf and propagates that boundary upward. It clears obsolete
  equality flags when the tree shrinks. No root or mutation mismatch found.
- **Quorum root:** compared against `CalcCbTxMerkleRootQuorums` and
  `GetQcHashes`: previous non-rotated hashes are newest first; null commitments
  skip; transaction errors stop in block order; a full non-rotated set loses
  its last element before appending; rotated entries overwrite by index;
  indexed sets merge before the per-type count check; final hashes sort by raw
  bytes before merkle hashing. Stale configured types remain included.
  Caller-supplied SerializeHash is an acceptable boundary for SB7: callers
  must pass the commitment's serialized-object hash, not its signature hash
  or transaction hash. `u8` map keys preserve Core's enum ordering.
- **CbTx/proof:** MN comparison precedes the version-gated quorum calculation
  in the intended caller; quorum errors propagate before root comparison.
  The partial-tree extractor reproduces Core's count/bit/hash limits, flag
  consumption, and duplicate-real-subtree rejection; an odd implicit sibling
  remains legal. The proof binds exactly the supplied transaction at index 0.
  Root checking is a primitive here; SB11 still owns authenticated header and
  coinbase/payload orchestration.
- **Coinbase-CL:** activation-at-block, unavailable/empty data, coinbase/type,
  complete payload consumption, version >=3, height equality, scheme-resolved
  signature validity and `diff < height` follow v24. No `rules` parameter is
  appropriate under the v24-only ruling. `TxRef`, `NoTransactions`, and the
  returned `BlsEagerSig` preserve the required facts and object semantics.
- **calculate_quorum:** single SHA-256 twice, numeric little-endian scores,
  valid/confirmed/Evo filters, descending score and descending collateral
  outpoint (raw-byte txid then index) match `llmq/utils.cpp:160-186`.
  Truncation preserves that order. Snapshot indexing and the used/unused
  rotation paths now consume this vector rather than a map that drops ties.
  Input-order ties without collateral are acceptable for the SML boundary:
  a genuine distinct-entry tie requires a hash collision. The legacy public
  score-map adapter keeps its old shape; member-selection callers no longer
  use it. `Some(0)` means an empty result in the explicit Option API; Core's
  sentinel maxSize=0 is represented by `None`.
- **Adapters:** signatures for the moved hashes/roots/scores remain; empty
  dashcore roots retain `None`. Mutated trees newly produce `None`, the intended
  Core-matching rejection. No other adapter result regression found in the
  real Core golden fixture or the SML tests. The removed free merkle helper
  lived in a private module, so its removal does not remove a public path.
- **Layering/dependencies:** keeping BLS commitment validation/message verify
  in dashcore until SB8 agrees with DEC-109's one shared commitment verifier
  and the BS12 serial-verification prerequisite. No DML/store/imbl dependency
  enters dash-evo. New library code has no runtime/clock/I/O/global state or
  pooled BLS verification calls. An unconditional dash-evo/primitives dependency
  is consistent with retaining dashcore's public adapters. The supplied Android
  size evidence is +61,368 bytes (+0.49%) at bfd282914; the later commits only
  add tests. I did not repeat the mobile link.
- **Unknown active type:** returning `excess-quorums` rather than Core's assert
  is acceptable only on this documented impossible-input boundary: connected
  active types must be configured. A caller must not treat unknown stored types
  as ordinary peer/block failures. An empty unknown bucket can currently pass;
  it is likewise outside the GetQcHashes contract, not a reachable valid-chain
  parity difference.
- **PF0:** the registered IDs hit the changed code. Each incremental setup
  clones a cached list outside the measured span, then hashes changed entries
  and paths inside it; the validity toggle guarantees a real update. The insert
  row exercises a shape change. The full row executes the Rust full-root
  implementation (sorting, all leaf hashes, full tree). The recorded ratio
  1,198,259,362 / 2,257,099 = 530.88 is supported for in-region instructions.
  This is a Rust full-root baseline following Core's algorithm, not measured
  C++ Core performance; preserve that qualification in the performance claim.

## Validation and limitations

- Read REVIEW-COMMON, SB7 specifications and the two named memories.
- Task-specific scratch and build constraints override REVIEW-COMMON's `/dev/shm`
  recommendation: `/work/scratch/sb7-r1-sol/tmp`, mode 700; own worktree target;
  `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`.
- No branch edits, commits, pushes, GitHub comments, or oracle writes.
- Independently passed dash-evo library tests (15), Core's copied SML vector
  test (1), and CbTx/proof/predicate tests (6). The history test is ignored in
  ordinary runs and was exercised separately for the incomplete-dump finding.
- The all-features/all-targets normal dependency layering check passed;
  `cargo fmt --all --check` passed.
- Independently passed dashcore's 52 `sml::` tests, including the real Core
  golden entry hashes and mutated-root adapter checks. A pre-existing unused
  serde-import warning in `dash/src/hash_types.rs:352` appeared with default
  features; no SB7 diagnostic appeared.
- Independently passed all 32 targeted dash-spv `sml_engine::` tests, including
  real QRInfo/MnListDiff aggregate verification, rotated-quarter reconstruction,
  cycle signature inputs, missing work-list handling and pruning behavior.
- Scratch-only adversarial harness compiled the unchanged reviewed entry/root/
  type sources against the worktree's compiled dependencies. Its 9 tests
  passed: 30,000 mixed insert/remove/replace/swap edits, 1,000 deferred-read
  checkpoints, empty/single-leaf states, repeated reads, duplicate subtrees,
  and growth/shrink/reorder boundaries from 0 through 257 leaves. Both the
  root and mutation flag matched `compute_merkle_root` at every checkpoint.
- A scratch adapter harness passed 16 manually framed hash cases across
  versions 1/2/3 and regular/Evo fields, canonical/invalid legacy addresses,
  repeated and out-of-order v3 purposes, invalid service entries, empty maps,
  and unknown extended-net-info versions. This is independent framing checked
  against Core's serialization code; it is not a new C++ oracle run.
- The audited pure-clippy configuration passed targeted all-features
  dash-evo library clippy with `-D warnings` and forbid-level disallowed-call/
  type rules. Pure dependency `cargo deny ... check bans` passed.
  A single-thread scratch process decoded a real v3 CbTx and ran the predicate
  20 times: `/proc/self/task` was 1 before decode, 1 after decode, 1 after use.
- Independently replayed the complete existing testnet dump: 7000..=1569775,
  1,562,776 CbTx roots, 1,563 full-root cross-checks, maximum 614 entries,
  zero mismatches. Independently replayed the capped mainnet tip window:
  2069569..=2090000, 20,432 roots, 21 full-root checks, maximum 3,825 entries,
  zero mismatches.
- Audited every supplied history dump's height sequence. At scan time the
  available continuous mainnet prefix ended at 1,742,459, followed by separate
  windows 1,889,857..1,910,000; 1,979,713..2,000,000; and
  2,069,569..2,090,000. Thus gaps remained at least in
  1,742,460..1,889,856, 1,910,001..1,979,712, and
  2,000,001..2,069,568. Two prefix files and the corrected V19 re-dump still
  had incomplete gzip streams. These are live capture-progress observations,
  not evidence of a Rust mismatch. The old V19 dump's three mismatches have
  a documented missing-platform-port explanation and corrected spot captures;
  they are not counted as a second implementation finding.
- No new RPC requests, benchmark measurements, Android links, or C++ builds
  were run. The 530.88x ratio and mobile-size delta were checked from the
  provided artifacts and benchmark code rather than independently reproduced.

Targeted commands (all with the required scratch/build/nice environment):

```text
cargo test -p dash-evo --lib --test sml_core_vectors --test cbtx --test cbtx_root_history
cargo test -p dashcore --lib sml::
cargo test --locked -p dash-spv --lib sml_engine:: -- --test-threads=1
cargo clippy --locked -p dash-evo --lib --all-features -- -D warnings -F clippy::disallowed_methods -F clippy::disallowed_types
cargo deny --manifest-path dash-evo/Cargo.toml --config contrib/node/pure-deny.toml --all-features --exclude-dev --locked check bans
python3 contrib/node/check-layering.py dash-evo dash-dml imbl imbl-sized-chunks
cargo fmt --all --check
```

The extra harness sources and history replay logs are retained under
`/work/scratch/sb7-r1-sol/`. The report is uncommitted, and the reviewed branch
was not changed by this review.

The examined runtime code and documented deviations are acceptable for this
scope. M1 still prevents approval: the acceptance harness can pass an incomplete
capture, and complete mainnet coverage through the permitted cap has not been
demonstrated. Close M1 with strict bounded acceptance and complete coverage.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
