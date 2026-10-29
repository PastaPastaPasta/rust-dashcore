# CS3 consensus review, round 1 — Sol

Reviewed `node/cs3-checks` at `ea501ed7e4418b7ae05db4a13645e902ae791114`, nine commits on `ce224dd4`, in a detached worktree under `/work/scratch/cs3-r1-sol/`. The implementer's worktree was not modified. Reference: Dash Core `v24.0.0-rc.3` (`da567d7dba945143eee4d2ce1621c21b32089989`), with O2a's pinned oracle at `a9a483e51efdadeee37cf40ff8697da62b3cd012`. The reference checks are unchanged by the oracle commit except for test trampolines.

**One major finding prevents the Sol gate from being satisfied.** I found no Rust/Core verdict, class, reason, debug-message, or legacy-sigop disagreement in the comparisons below. The major finding is a reproducible failure of required-seed coverage accounting introduced by adding `Outcome::OutOfDomain` without updating the replay gate.

## Findings

### CS3-S1 — major: required seeds receive successful replay credit without a comparison

**Locations:** `node-l1/src/lib.rs:65`, `node-l1/src/checks.rs:148`, and the affected consumer `node-l1/tests/private_seed_replay.rs:99`–104.

The new `Ok(Outcome::OutOfDomain)` means that nothing was compared. The private replay still tests only `.is_err()` and increments `executed` for every registered seed. A required CS3 seed can therefore have no Core verdict at any supported pin and still contribute to `executed == applicable >= required`, producing a successful receipt. The intentional v23 skip does not make this safe: it also accepts an OutOfDomain result at v24, CS3's only parity reference.

**Reproduction, using synthetic public data only:** add a self-test beside the existing `replay::synthetic` tests. Register `diff_contextual_check_block_header` as required and supply these two seeds:

1. The one-byte input `b"x"`, which cannot parse as a header-check input.
2. A structurally valid input built as follows:

```rust
let mut input = Position::encode_height(Network::Main, 68_590, 17);
input.resize(HEADER_INPUT_LEN, 0);
input[13] = 3; // candidate version 4
input[14..18].copy_from_slice(&150i32.to_le_bytes());
```

The second input selects an eleven-header window where Core's DGW calculation needs more history. Both seeds return `Ok(Outcome::OutOfDomain)` at v24; the replay nevertheless returns `Ok(Replayed { executed: 2, applicable: 2, required: 2 })`. I ran this assertion successfully with `cargo test -p node-l1 --features oracle --test private_seed_replay sol_required_out_of_domain_seed_is_counted_as_executed -- --nocapture`. The temporary test was then removed.

This reproduction does not establish that the real private seeds are out of domain; I did not inspect them. It establishes that the PASS receipt cannot distinguish actual comparison from complete absence of comparison, contrary to the replay's documented coverage contract. The manager's planned change of the five manifest entries to `required` does not close this hole.

**Suggested fix:** identify each target's applicable pins explicitly and require a real `BothAccept` or `BothReject` result for every required seed on its applicable pin(s). For CS3 that means v24; an intentional v23 skip must not receive comparison credit. Fail the receipt if a required v24 seed is OutOfDomain. Add synthetic regressions for both an input-format exclusion and an oracle-context exclusion, and retain a passing in-domain CS3 seed test. Rerun the receipt gate after the change.

### CS3-S2 — minor: the census has no floor for actual check coverage

**Location:** `node-l1/tests/l1_checks.rs:87`–91.

The census requires one acceptance and the listed rejection reasons, but puts no lower bound on comparisons that reach a check. Most cases could become OutOfDomain, or fail both decoders, while a handful of cases keep the test green. `BothReject` includes matched decoding failures, so an OutOfDomain cap alone would not measure check coverage accurately.

The current results are adequate for this review and this is not independently blocking: 9,400 of 11,800 inputs reach actual check verdicts, 2,094 are matched decoding rejections, and 306 are OutOfDomain. The census alone does **not** exercise every reason; the committed boundary rows supply the large-size, payload-size, sigop-limit, and devnet-genesis cases. Across the census and boundary rows, every Core reason template is exercised.

**Suggested fix:** retain per-target counts of actual check verdicts, decoding rejections, and exclusions, and assert conservative minimum check-verdict counts alongside the required reasons. Keep the boundary-row suite's existing prohibition on OutOfDomain results.

## Verification

- Local build: `cargo build -p dash-consensus -p node-l1 --features node-l1/oracle --locked` passed. Workspace format check and targeted Clippy (`-p dash-consensus -p node-l1 --all-features --all-targets -- -D warnings`) passed on the restored reviewed tree.
- `dash-consensus --lib`: 130 tests passed. L0's 83 `tx_valid` rows and the `tx_invalid` BADTX classification passed. The recorded-row suite and quirk 2, 3, 9, and 10 suites passed.
- Both `l1_check_rows` tests passed: all 85 boundary inputs agreed with the live pinned oracle, including the input SHA-256s and committed class/reason/debug records. The offline replay passed separately.
- The five fixed censuses passed with zero divergences. Actual check verdicts / total inputs were: transaction 2,001/3,000; block 2,542/3,000; contextual transaction 2,641/3,000; contextual block 1,478/2,000; contextual header 738/800. OutOfDomain counts were respectively 0, 0, 5, 239, and 62. All five CS3 proptest twins also passed.
- Independent scratch probes added 10,959 target comparisons with zero OutOfDomain results and zero divergences, plus 4,224 direct contextual-header verdict comparisons. These covered version/type combinations across DIP0003, simultaneous transaction failures, rejection order inside blocks, merkle mutation and nonadjacent duplicates, payload parsing edges, asset-unlock instance commitments, malformed script pushes and sigop counting, mixed final/non-final input sequences, the lock-time threshold, CSV/MTP activation, signed header versions, MTP equality, and the future-time boundary.
- All five fuzz targets passed through `node-fuzz/run.sh`, using the 85 public boundary rows as seeds and a fresh mode-0700 scratch sink. Requested budget: 30 CPU seconds per target; actual aggregate approximately 274 CPU seconds. Executions: transaction 14,308; block 122,936; contextual transaction 12,502; contextual header 314; contextual block 9,181. Zero divergences, crashes, or sink failures. The oracle used the pinned coverage-instrumented `fuzz` archives. Two run receipts report a dirty tree because the temporary synthetic replay test was present; production sources were unchanged throughout.
- A source census found the same 29 reject-reason templates in Core and Rust, including formatted `bad-version`; no missing or extra templates. Dynamic reason/debug formatting was also compared through the oracle.
- The normal private-test wrapper returned `receipt PASS` for both tests, with output restricted to pass/fail. No private file or input was inspected. CS3-S1 limits what those receipts establish.
- I did not repeat the implementer's full 3,139-test workspace gate or run PF0; those remain separate manager gates. Scratch tests and resources were removed after review. No commit, push, or GitHub comment was made.

## Consensus assessment and deviations

The port follows Core's check order, including PoW/devnet before merkle checks; root mismatch before mutation and the asset-unlock commitment; block size before coinbase position; transaction checks before aggregate sigops; and contextual finality/type checks before sigops and coinbase-height/type checks. The DIP0001 limits use Core's strict `>` boundaries, and the DIP0003 version/type allowlist matches v24, including types 10–12. Value accumulation cannot overflow before its range check: each operand is bounded by MAX_MONEY. Within the accepted block-size domain, even twenty sigops per script byte remains below `u32::MAX`; wrapping sums also preserve Core's unsigned arithmetic.

Merkle mutation matches Core's sibling-pair test before odd-leaf duplication; it is correctly not an unconditional ban on every nonadjacent repeated txid. CbTx v4+ commitments use stable asset-unlock transactions' instance hashes in block order, require complete payload decoding, and return `BLOCK_MUTATED` on commitment mismatch. BIP34 uses the exact serialized-height prefix until DIP0003. Header time checks use MTP equality as rejection and v24's NodeClock without a peer offset. Signed versions render with the correct eight-digit two's-complement hexadecimal form. Finality uses strict comparison and the all-final-sequences exception, including vacuous truth for zero inputs.

- **Explicit `HeaderParams`: accepted.** It is an immutable derived parameter view, consistent with the existing `PowParams` API. Callers must derive it from the same chain as `ConsensusCtx`; no current mismatch was found.
- **Panic with no previous header: accepted.** It is documented and matches Core's assertion. Genesis does not go through this entry point.
- **Empty first-transaction vin under BIP34: accepted as an oracle-domain restriction.** `CheckBlock` rejects that block before contextual checking in the real validation sequence. The standalone Rust fallback to `bad-cb-height` avoids an unchecked read; no claim of standalone oracle parity is made for this excluded case.
- **Uncapped census exclusions: minor as CS3-S2.** Current coverage is substantial. Counting exclusions as successful required-seed replay is a separate major defect, CS3-S1.

**Sol gate: NOT SATISFIED.** Resolve CS3-S1 and rerun the required-seed receipt checks before the sequential Opus-high pass.

**Verdict: CHANGES-REQUIRED.**
