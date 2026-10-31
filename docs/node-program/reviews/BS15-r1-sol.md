# BS15 r1 — Sol

Reviewed `node/bs15-x11-blockhash` at `4cc009decf3646e1cfd0c08d82790fa9ac657ca2`, eight commits over `1b1722fb`. Review date: 2026-10-09. Followed `REVIEW-COMMON.md` and read the `dash-x11-xh1` memory note, program charter/process, DEC-61, PLAN BS15/XH1 and architecture context. The implementer checkout was read-only. Builds and experiments used a detached checkout under `/work/scratch/bs15-review/`, `TMPDIR=/dev/shm/bs15-r1-tmp` (0700), and `CARGO_BUILD_JOBS=8`.

The header-hash change is correct across the tested chains and both backends. The release artifact has a reproducible test-compilation failure that should be fixed before publishing.

## Severity-ranked findings

1. **Major — the published crate cannot compile its differential tests.** `dash-x11/Cargo.toml:23`, `dash-x11/tests/core.rs:42`, and `dash-x11/README.md:94`.

   Cargo removes the path-only `dash-sha256` dev-dependency but includes `tests/core.rs`, whose unconditional `core_fixture` implementation still uses `dash_sha256::Sha256`. This is a packaging gap in the new publishable deliverable, although the test dependency already existed before BS15. A user or distribution rebuilding the published source with `cargo test` fails before running its Core differential checks. The package/library build succeeds; `cargo publish --dry-run`'s library verification does not establish that the shipped tests compile.

   Independently reproduced with `cargo package -p dash-x11 --no-verify`, extraction of `target/package/dash-x11-0.45.0.crate`, then `cargo test --manifest-path <extracted>/Cargo.toml --no-run`. Exit 101:

   ```text
   error[E0433]: cannot find module or crate `dash_sha256` in this scope
     --> tests/core.rs:42:22
   42 |     let mut stream = dash_sha256::Sha256::new();
   error: could not compile `dash-x11` (test "core") due to 1 previous error
   ```

   **Fix:** use a registry-available SHA-256 dev-dependency for the fixture stream, or publish/version `dash-sha256` and retain it in the packaged manifest. Keep the always-running differential fixture test in the release. Add a packaged-source `cargo test --no-run` check; rerun `core_fixture` after changing its stream hasher.

2. **Minor — the SPV startup detection hook is conditional on INFO logging and comes after initial hashing.** `dash-spv/src/client/lifecycle.rs:205` (also constructor line 56 and genesis verification line 397).

   `hash_x11::backend()` is evaluated inside `tracing::info!`, so filtering out INFO skips the call entirely. On a checkpoint/resume path without an earlier hash, detection can still happen on the first hashing task. On the genesis path, construction already hashes the genesis header before `start_sync` reaches this line. Thus the comment's claim that this runs detection here instead of on the first header is not reliable. Fallback details are only an INFO message, so a WARN-only operator does not see the failure warning.

   A scratch test using the exact logging expression under `NoSubscriber` verified that its initialization expression is never evaluated. **Fix:** initialize independently of log filtering, before constructor paths that can hash; then log the selected backend and emit a WARN when selection reports fallback. Keep lazy initialization for ordinary `dashcore` library users. The full node's strict operator option and CS0 adoption are explicitly assigned to **BL-35**; their absence is not a new BS15 blocker.

## Downstream audit

- `BlockHash` unconditionally wraps `hash_x11::Hash`; `Header::block_hash()` serializes the 80-byte header into its engine. There is no remaining feature-dependent SHA256d block-hash computation in the audited dashcore, SPV or FFI paths. SHA256d uses remaining in SPV are network checksums/filter headers, not block identities. FFI callbacks and wallet block-info conversions copy the raw 32 bytes.
- **SPV storage does not depend on the inner Rust hash type.** `dash-spv/src/types.rs:75` writes the consensus header followed by the raw hash; its decoder reads those bytes directly. `storage/segments.rs:51` retains the 112-byte record size. The hash index is rebuilt from stored hash bytes. The old SPV dependency already enabled `core-block-hash-use-x11`, so existing SPV records remain X11, with unchanged contents and layout.
- Scratch probes under both backend feature sets confirmed consensus encoding is 32 raw bytes, direct bincode encoding/decoding preserves those bytes, serde JSON uses the same reversed display direction as SHA256d identifiers, and binary serde keeps the same representation. Changing the inner type does not introduce a serialization tag or an extra field.
- A standalone consumer that previously calculated and persisted default-build SHA256d block identifiers must regenerate them from headers and update its references. Decoding does not transform an existing 32-byte SHA256d value into X11. The changelog documents the computed-value/API break. This affects builds that previously lacked the old X11 feature, including no-default-feature builds; previously X11-enabled consumers keep their header hashes. No additional in-repository persisted SHA256d block-key consumer was found.
- `Reject::hash` becomes an X11-named type, but consensus decoding/encoding still carries an opaque 32-byte identifier. A transaction reject is not rehashed as X11. Existing transaction-reject vectors passed in the library suites. Default-build code assigning a SHA256d raw hash to that field has a source break, as documented.
- `x11-c` unifies across normal dependencies on this package version, selects C for every exactly-80-byte `hash_x11` input, and implies `x11`. It does not select C for `dash_x11::x11` callers, and non-80-byte wrapper inputs still use Rust. Consequently an all-features run exercises C header hashing, not the default Rust wrapper. Both feature configurations were built and replayed separately below; Cargo fingerprints and runtime backend lines confirmed the distinction.

## Self-test and C read audit

The default wrapper reaches `dash_x11::init()` on the first hash. Fast candidates that fail self-test are skipped, the failed candidates remain observable in `Selection`/its display, and failure of every candidate including portable panics. The nine dispatch/portable unit tests passed, including simulated fallback and all-candidates-fail cases. This is not a returned hashing error; an unhandled panic through the existing C FFI boundary can abort the host. The fallback contract and node-side strict-mode work remain as described by DEC-61/BL-35, subject to the SPV logging finding above.

The first 80-byte **C** hash does **not** run the Rust self-test. A separate-process scratch probe observed `dash_x11::selection() == None` after that call, then `Some` after a 79-byte call. Calling `backend()` explicitly initializes the Rust fallback even in C mode. The claim that self-test runs on the first hash therefore needs that backend qualification; this branch does not add a C self-test.

The linked `rs-x11-hash` fork's safe Rust function passes only a pointer to C, whose BLAKE stage always reads 80 bytes. BS15's wrapper checks `&[u8; 80]` first and safely sends every other length to Rust; boundary-length tests passed under both backends.

A full repository Rust-source search found exactly two direct calls: the guarded wrapper at `hashes/src/hash_x11.rs:42`, and the existing unrestricted public slice helper at `dash-coreimport/src/tx.rs:225`. **There is no direct Rust call in node-fuzz:** its `rs-x11-hash` manifest entry is a patch for its transitive coreimport dependency. The standalone store bake-off likewise has a patch entry, not another call. Coreimport's internal production callers supply 80-byte headers, but its public slice-taking helper still permits a short-input out-of-bounds read. That is an existing, out-of-scope residual; changing it to `&[u8; 80]` or the guarded wrapper would close it. BS15 fixes the hashes wrapper, not the dependency's API generally.

## Independent replay and performance evidence

Fresh read-only oracle dumps reproduced the implementer's live tips. The CS2 mainnet dump was checked by successor linkage and its independently recorded best-header hash `0000000000000010139913e908c46f54b74d4ca594bdb3876058e5ef3ab39e99`.

- Mainnet live: **2,094,671 records**, heights **0..=2,094,670**, tip `0000000000000008e53829d71dfa617a35e406718f0ef0af269987cb287e3bb7`.
- Mainnet CS2: **2,551,767 records**, heights **0..=2,551,766**.
- Testnet live: **1,569,146 records**, heights **0..=1,569,145**, tip `00000010191b3fa186f0372d7752aa4029a29e51fe63744fb1093d82341040fe`.

These are record counts, not the inclusive final heights stated in the review prompt. For every corpus, both `x11` and `x11-c` passed both integration tests: `Header::block_hash()` and the hashes-level replay (selected wrapper, Rust automatic backend and portable backend). That is twelve successful test invocations, with zero mismatches. Command shape:

```text
DASH_X11_CHAIN=<corpus> cargo test --release -p dashcore -p dashcore_hashes \
  --features dashcore_hashes/<x11-or-x11-c> \
  --test x11_chain --test block_hash_x11 every_oracle_header -- --ignored --nocapture
```

The Rust runtime line selected `dash-x11 'sse,aes,avx2'`; the C runtime line selected `rs-x11-hash (80-byte headers)`. A negative control flipped one genesis nonce byte while retaining the original oracle hashes. Both integration tests failed (exit 101), identifying height zero; all three hashes-level computations detected it.

An independent optimized wrapper loop, measured with `perf stat -e instructions:u -r 3` at 1,000 and 2,000 hashes and differenced to remove startup costs, gave approximately **39,280 instructions/header for Rust** and **184,279 for C**, closely reproducing the claim. The Core 169,617 figure and aarch64/QEMU evidence were not freshly remeasured; XH1's existing evidence covers the unchanged `dash-x11/src` implementation. This is not a new PF0 merge-gate receipt.

The saved implementer gate log names head `4cc009decf36`, ends `GATE EXIT 0`, and records the two SPV failures followed by **54 passed, 0 failed** on its BL-20 rerun. I inspected those logs; I did not rerun the entire workspace/dashd gate. The new oracle-diff step was run independently below.

## Validation

Both required commands exited **0**, using the pinned oracle archives without unpinned overrides.

`cargo test -p dash-oracle-sys --features oracle --no-fail-fast`: **62 passed, 0 failed** in total. Actual per-target `test result` summaries, with elapsed-time suffixes omitted:

```text
lib:           test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
archive_check: test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
contexts:      test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
contextual:    test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
script:        test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
smoke:         test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
doc-tests:     test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`cargo test -p node-l1 --features oracle --no-fail-fast`: **26 passed, 0 failed** in total. Actual per-target summaries, with elapsed-time suffixes omitted:

```text
lib:                 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
l1_boundaries:       test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
l1_coin_boundaries:  test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
l1_coin_records:     test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
l1_twins:            test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
private_seed_replay: test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
recorded_fixtures:   test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
doc-tests:           test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Additional checks: dashcore library tests passed under default Rust and C (**580 passed, 0 failed, 17 ignored** each); both known-hash/genesis tests passed under each backend; hashes library tests passed under each backend (**23 passed, 0 failed** each); self-test suite **9/0**; scratch serialization/initialization/log-filter probes passed; targeted all-features/all-targets clippy with `-D warnings` passed; branch diff whitespace check passed. Packaging and the extracted library build passed; extracted test compilation failed as finding 1. README, docs URL, MIT license inclusion and dependency version metadata otherwise checked out. Scratch experiments are not proposed implementation changes.

The detached checkout, extracted package/builds, review dumps/probes and private temporary directory were removed after validation. This report is left uncommitted in the integration worktree.

**Verdict: CHANGES-REQUIRED.** Fix the packaged differential-test compilation failure before publishing; correct the SPV detection hook in the same follow-up.
