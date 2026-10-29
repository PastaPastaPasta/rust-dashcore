# O6 review (round 1, Sol)

Status: complete.

## Scope and checks

- [x] Read contract/plan/decisions/quirks and branch diff.
- [x] Compare each classifier with Dash Core v24.0.0-rc.4.
- [x] Audit block walker completeness and read-only behavior.
- [x] Run real/scan unit tests, fmt, clippy.
- [x] Inspect raw outputs and research records; run check-quirks.py.
- [x] Exercise at least one mutant per scan.

## Findings

### Major: scan (d) omits the v24 CbTx version rule

`conformance/src/scan/classify.rs:313-334` implements the v1/v2/v3 and DIP0008/V20 checks but
explicitly does not implement `requires_unlock_root`. Core rc.4's `CheckCbTx` at
`src/evo/cbtx.cpp:45-50` requires CbTx version >= 4 exactly when both `is_v24_active` and V20 are
active, and rejects v3 there. Testnet has v24 active in the scanned post-1,578,000 history while
the scan has no v24 activation input and accepts a v3 CbTx at such a height. A reflink probe with a
synthetic testnet block at height 1,580,000 produced no scan-(d) hit for this Core-invalid v3
CbTx. Therefore the claimed testnet `d = 0` is not sound: a real bad v3 CbTx in that range would
be silently missed. Add the network's v24 activation state (including the EHF activation rule) to
the classifier and a boundary fixture/mutant; keep version-4 payload parsing and test both sides
of the activation.

### Major: scan (e) omits `IsValidMNActivation`

`conformance/src/scan/classify.rs:443-459` checks only payload version, bit range and duplicates.
The O6 plan explicitly includes the Core activation predicate, but rc.4's
`evo/mnhftx.cpp:124-126` calls `CChainParams::IsValidMNActivation`, whose implementation is
`src/chainparams.cpp:109-130`. In rc.4, for example, bit 28 is the known non-EHF TESTDUMMY
deployment and `IsValidMNActivation(28, current MTP)` returns false; a type-7 bit-28 transaction
would be rejected by Core but this scan reports only `e.signal` and no invalid-activation flag.
The omission is not pinned by the fixtures or the mutant set (which has no activation-validity
mutant). Implement the known-deployment/time-window check using the block's MTP (or explicitly
mark the scan unable to make this verdict), and add a positive/negative fixture. The four observed
testnet signals happen to use EHF bits, so the recorded count does not repair this classifier gap.

### Major: the output-path guard is bypassed by a symlink

`conformance/src/scan/run.rs:105-108` compares `std::path::absolute` strings. `absolute` does not
resolve symlinks, so an existing `OUT` symlink outside the blocks directory whose target is inside
`blocks/` passes the guard; `write_outputs` then follows it and writes `scan-*.ndjson` and
`summary.json` into the oracle directory. This violates the read-only requirement (and the test only
covers a lexical child path). Canonicalize the existing blocks directory and the output target (or
its existing parent when the target is new) before comparing, reject symlink-resolved descendants,
and add a symlink regression test.

### Classifier checks without findings

Scan (b)'s signed version and DIP3 boundary/type-key handling matches `ContextualCheckTransaction`
and `tx_check.cpp`; the exemption counters and one-byte coinbase script case are preserved. Scan
(c)'s old-budget window (`offset < 100`) and superblock `R + GetPaymentsLimit` bound match
`masternode/payments.cpp:262-345`, including the strict boundaries. Scan (f)'s configured-type
and enabled-height table matches `ChainstateManager::IsQuorumTypeEnabled` in
`validation.cpp:6654-6705`; evaluating at mined `pprev` is the same context used by
`ProcessBlock`. The documented quorum-base caveat can create false positives for non-null rows,
not false negatives for the disabled-type count.

The hand parsers enforce canonical CompactSize, exact payload consumption, bitset padding and
BLS malleability states. Scan (a) checks both framing rules and the header merkle root. CbTx/MNHF/QC
fixtures and the 16 in-process mutants cover the implemented predicates, but do not cover either
major above.

### Completeness, records, and hygiene

The blk walker validates magic/length/header framing, resynchronizes while accounting for every
skipped byte, allows only append growth of the newest file, and rechecks generations. `Chain::build`
anchors the best chain and gives one record per linked height; every selected block is read and its
merkle root is checked. Any unframed block, merkle mismatch, subsidy disagreement, file rewrite,
or scan issue is placed in `incomplete` and the CLI returns exit 2. Anchored stale/unlinked records
are reported separately as intended by DEC-600/R3. The direct read-only tests and lexical
output-inside-blocks-dir test pass; the symlink case is the hygiene finding above.

Raw `/work/scratch/o6/out/{testnet,mainnet}` summaries match the research JSON counters, ranges,
tips, hit rows and incomplete list. The research markdown is consistent, and tracked mainnet text
uses only “data ends at 2,094,670; see manager” for the partial range. Fixture provenance is present.
`python3 -I contrib/node/check-quirks.py --core /work/home/agent/workspace/dash` passes.

### Validation

Passed: `TMPDIR=/dev/shm/o6-rv-sol-tmp CARGO_BUILD_JOBS=12 cargo test -p dash-conformance --test scan_real --all-features -- --nocapture`; `cargo test -p dash-conformance --lib scan::tests --all-features -- --nocapture` (including `mutants_are_killed`); `cargo test -p dash-conformance --test scan_run --all-features -- --nocapture`; `cargo fmt --all -- --check`; and `cargo clippy -p dash-conformance --all-targets --all-features -- -D warnings`.

## Verdict

CHANGES-REQUIRED
