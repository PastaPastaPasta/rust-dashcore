CHANGES-REQUIRED

1. **MAJOR — Transaction scripts bypass Core's CompactSize size limit at the reviewed SDK pin.**

   **Locations:** `dash-consensus/src/primitives/decode.rs:142`; `Cargo.toml:53`; `node-l1/tests/l1_boundaries.rs:125`.

   `decode_tx` delegates to base-sdk `33fcc59e1868`. Its script/vector decoder bounds the length by the remaining input, without Core's `MAX_SIZE` check. A sufficiently populated script therefore decodes in Rust even when both Cores reject it. This propagates into `decode_tx_exact` and transactions inside `decode_block`. The boundary table does not catch this: its oversized prefixes have insufficient following bytes, and the prefix loop at lines 137–153 covers counts and payloads, omitting the two script lengths.

   **Concrete failure input:** concatenate the following bytes, where repetition counts are decimal:

   ```text
   01000000                         # version 1, normal type
   01                               # one input
   07 repeated 32 times             # previous transaction hash
   00000000                         # previous output index
   fe01000002                       # CompactSize(33,554,433)
   00 repeated 33,554,433 times      # scriptSig
   ffffffff                         # sequence
   00                               # zero outputs
   00000000                         # lock time
   ```

   The complete input is 33,554,488 bytes. Rust accepts and consumes it; `Oracle::tx_decode` rejects it at **both** pins, and `node_l1::compare_tx` returns `Divergence("decode verdict")`, without an allowlist disposition. A second reproduction uses zero inputs and one zero-valued output with a 33,554,433-byte `scriptPubKey`; it produces the same disagreement. Reducing either script to exactly 33,554,432 bytes makes both sides accept and all compared fields agree. The standalone reproducer and results are `/work/scratch/cs0-impl-gpt/probe.rs` and `max-size-probe.log`.

   **Core citations:** v23.1.8 `src/serialize.h:38`, `:288–317`, particularly `:314–315`; v24.0.0-rc.3 `src/serialize.h:42`, `:292–321`, particularly `:318–319`. Both define `MAX_SIZE = 0x02000000` and reject a larger size with range checking enabled. Script fields are serialized/deserialized as part of `CTxIn` and `CTxOut`: v23.1.8 `src/primitives/transaction.h:141` and `:179`; v24.0.0-rc.3 `:161` and `:199`.

   **Fix:** finish the BS4a dependency and re-pin the reviewed implementation before claiming decode parity, adapting CS0 to its final APIs. The inspected BS4a `3c971b1` vector decoder already uses `CompactSize::decode_size`. Require the Core size bound on every relevant script/vector path and add fully populated script cases at `MAX_SIZE` and `MAX_SIZE + 1` on both pins, including block embedding and cursor rollback. Remove the DDE allowlists when that dependency lands. CS0 §6.4/§11 explicitly require this dependency at merge. STATUS's proposed re-pin after CS0 needs reconciliation with that approved acceptance order; it does not make this exact head equivalent to Core.

2. **MAJOR — The new sink crate fails to compile on Windows and breaks an existing CI group.**

   **Locations:** `node-sink/src/lib.rs:28`, `:56`, `:63`, `:70`; `.github/ci-groups.yml:13`; `dash-consensus/Cargo.toml:29`.

   The unconditional `std::os::unix::fs` import and Unix permission methods cannot compile for Windows. `node-sink` is an unconditional workspace member, is newly assigned to the `core` test group, and is an unconditional dev dependency of `dash-consensus`. `.github/workflows/rust.yml:140` invokes the existing group workflow on Windows, whose test runner runs every assigned crate. This introduces a direct build failure for the public core group as well as consensus tests; it does not depend on private data being enabled.

   **Concrete reproduction:**

   ```sh
   cargo check --locked -p node-sink --target x86_64-pc-windows-gnu
   ```

   This fails with six errors, beginning with E0433 at `node-sink/src/lib.rs:28` (`cannot find unix in os`) and followed by missing `mode`/`from_mode` methods. The cross target is installed on agentbox; `dash-sha256` checks successfully before this failure. Results are in `/work/scratch/cs0-impl-gpt/windows-check.log`.

   **Core citations:** v23.1.8 and v24.0.0-rc.3 `doc/build-windows.md:1–15` both support Windows builds. The binding program requirement is PRD Q8/NFR-PORT-01: Windows x86_64 is tier 2 for node/CLI. This is a portability/build defect, with no differing consensus expression to cite.

   **Fix:** separate portable seed/report/manifest interfaces from Unix permission implementation and gate Unix-specific tests/imports appropriately. Provide Windows private-file handling, or make unsupported private operations refuse explicitly while allowing ordinary public tests with the private environment unset. Check the Windows target and the Windows core/consensus test build before handoff; a silent permission-check fallback would weaken the sink contract.

3. **MAJOR — The required BS2 history pass through the new wrappers is absent.**

   **Locations:** `conformance/Cargo.toml:18`; `conformance/src/check.rs:319`; `dash-consensus/ARCHITECTURE.md:24`.

   CS0 design §9.2 requires a third `dash_consensus` leg of the BS2 checker, and §14 requires labelled partial-history evidence before merge. At `f5f079c2`, `dash-conformance` has no `dash-consensus` dependency, and no conformance source uses its wrappers, `CoreBytes`, or release rules. `check_block_bytes` still decodes only `DcBlock` and `BsBlock`. The entire CS0 change in that checker is a six-line lint exemption for base-sdk's merkle helper. Consequently, successful BS2 runs cannot exercise this implementation's serializer, cached hashes, canonicality proof, or profile-selected merkle result.

   **Concrete missing-coverage input:** Core's public `src/bench/data/block813851.raw` (999 transactions, 749,213 bytes), or any block fed to the existing BS2 RPC/blk checker. That checker runs both older libraries and performs **zero** CS0 wrapper decodes. CS0's separate fixture test covers this one block, but cannot supply the promised best/stale history checks or both-profile mainnet invariant. This is a missing acceptance path, rather than a demonstrated incorrect hash for that fixture.

   **Core citations:** v23.1.8 `src/primitives/block.h:207–211` and `src/primitives/transaction.h:244–249`; v24.0.0-rc.3 `src/primitives/block.h:213–217` and `src/primitives/transaction.h:271–277` define the serialization to compare. Both pins' `src/consensus/merkle.cpp:65–72` select transaction `GetHash()` as the block leaves. The RPC txid references are v23.1.8 `src/rpc/blockchain.cpp:167–177` and v24.0.0-rc.3 `:168–178`.

   **Fix:** add the specified wrapper leg to the shared per-block checker. Compare hash, every rules-selected txid, merkle root/mutation, canonicality, canonical bytes and size; run both profiles on mainnet history. Record the actual synced networks/ranges, stale coverage and revisions as partial-history evidence. Use the permitted read-only RPC path for review/acceptance runs; this review did not access `/work/chains` or `/work/bench`. If the manager elects to defer the leg, explicitly amend CS0's approved scope and acceptance rather than treating existing BS2 runs as wrapper evidence.

4. **MINOR — Crash minimization reports success when libFuzzer fails before processing an input.**

   **Location:** `node-fuzz/run.sh:231` (also `:172` and `:211`).

   The verdict expression accepts **any** nonzero libFuzzer status in `mode == "minimize"`, and does not require a minimized artifact. A missing crash pathname passes the lexical prefix check, libFuzzer exits 1 with an error, and the runner nevertheless exits 0 with `status=pass` and zero executions. This can cause O3/triage tooling to treat an unsuccessful minimization as completed work. Budgeted fuzz runs still require status 0; this finding concerns the minimization mode.

   **Concrete reproduction:** create an empty synthetic directory `P/fuzz` with mode 0700 under scratch, then run:

   ```sh
   DASH_EMBARGO_DIR=P node-fuzz/run.sh selftest_mismatch --pin v24.0 \
     --minimize P/fuzz/selftest_mismatch/runs/previous/crashes/missing
   ```

   Use absolute `P`, and leave `missing` nonexistent. I ran this after `--selftest`, using only synthetic public data. The runner returned 0 and printed `mode=minimize executions=0 ... status=pass`; its verdict recorded `libfuzzer_status: 1`, and `crashes/minimized` did not exist. Evidence is `/work/scratch/cs0-impl-gpt/minimize-probe.log` and the synthetic tree `minimize-probe/`.

   **Core citations:** not applicable to the shell runner's exit-status contract. The applicable requirement is CS0 §9.3/DEC-30 N5, which makes runner verdicts fail closed for every target on both pins; neither Core release defines this project-specific runner mode.

   **Fix:** validate that the requested crash is an existing regular file before launch. Determine successful minimization from libFuzzer's documented status and a produced artifact, handling any intentionally expected crash status narrowly. Add missing-input and unsuccessful-minimization self-tests that require a failing runner verdict and exit status.

Review scope and positive evidence:

- Reviewed the detached fresh worktree at `f5f079c2d9c6db0809e52943ab9630e3be6b7f2a`, integration base `17bc2e66`, and the requested decisions/design reviews. Core citations refer to peeled release commits `728f5055836c6d29806412fc7223ac8fe05af991` (v23.1.8) and `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18` (v24.0.0-rc.3). Oracle archives are the committed `pins.txt` builds: `350ce242a59c` and `23769258c239`, checked by the crate's build process.
- **N1:** `Tx::txid(&UngatedRules)` and `Block::merkle(&UngatedRules)` force an explicit rules argument. No selected-release hash is stored. Stable hashing uses the canonical serialization with the final 132 bytes zeroed, and the alternate root carries its own mutation flag. Beyond the shipped boundaries, I tested two differently signed instances with equal v24 txids: Rust/Core agree on both roots, with mutation **false under v23 and true under v24**. Core references: v23 `transaction.cpp:96–98`; v24 `transaction.cpp:82–87`, `:115–124`; both `merkle.cpp:45–72`.
- **Header/N3:** private immutable header/hash fields and a hash-computing constructor preserve the binding; the 2,000-header test verifies one X11 per construction/handoff. `CoreSerialize` is sealed, `Decoded`'s proof-bearing fields are private, and `CoreBytes` has no public raw-byte conversion or mutable byte access. The four compile-fail doctests passed. I found no additional safe public construction escape.
- **Decode/merkle:** streaming versus exact trailing-byte policy, transactional cursor updates, block count rejection and item-start offsets match the stated contract. The public witness-marker-shaped `00 01` case is read as empty inputs followed by one output, matching Dash's witness-free decoder on both pins. Disjoint sibling comparisons precede odd-tail duplication on every merkle level. The count-only hostile block reproduction rejects with the caller's cursor unchanged. Allocation sizes inspected in CS0 are bounded by actual remaining bytes; finding 1 concerns the missing Core cap inherited by transaction fields.
- **N5/N6:** the fuzz self-test passed, including mismatch, sink failure, outside-runner refusal and merge-child checks. The runner captures child output, clears the target environment and restricts path/flag shapes. The shared purity configuration includes both `HashMap` and `HashSet` bans, merges crate-local encoder/libm bans, and applies forbid-level lints. The purity gate passed for both pure crates and both feature configurations. No unsafe code or explicit untrusted-input panic was found in the new primitive code; this is not a proof over every dependency/input.
- **CS2 preservation:** compared all 62 pre-existing CS2 files outside the five approved reconciliation paths against `988ce12f`; every blob is identical, including its implementation, tests/data and harness code. The deleted provisional ancestry/type body occurs verbatim in `ctx.rs`; clippy contents from `msrv` onward and `Amount`/`FpPending` are also identical. The lockfile adds only the new CS0 runtime dependencies. The live `cs2-diff ... --check` passed, checking 29 recorded files.
- Focused Linux run: `cargo test -p dash-consensus -p node-sink -p node-l1 --features node-l1/oracle` passed, reporting 114 passes including four compile-fail doctests and the two explicitly skipped private-environment tests. All **16 node-l1 tests** passed. Its seven boundary areas total **5,461 rows**, exercised on both pins, with **32 allowlisted pin comparisons** and zero reported failures. The standalone populated-script reproducer exposes an additional unallowlisted mismatch.
- Targeted all-target clippy with `-D warnings`, `cargo fmt --all --check`, base-sdk pin checking, release-reference checking, `check-pure.sh`, `node-fuzz/run.sh --selftest`, and CS2 recorded-corpus checking passed. Commands used `CARGO_BUILD_JOBS=2` and `TMPDIR=/dev/shm/cs0-impl-gpt-tmp` (0700); initial load was 32.71. I did not rerun the full 2,924-test workspace gate, the multi-day fuzz budgets, or performance measurements on the loaded host. The benchmark report uses appropriately matched workloads, but its numeric claims were not independently remeasured.
- Private companion material, private inputs/receipts, marked-case acceptance and deliberately unexplained behavior are **for the designated reader**. I did not search for or access them. Real private tests were left disabled; fuzz-runner reproductions used a fresh synthetic scratch tree only. No branch source was edited, and no commit, push or GitHub post was made.

Summary: **CHANGES-REQUIRED — three major findings and one minor.** Resolve the exact-head decode-domain mismatch/BS4a dependency, restore Windows builds, and implement or explicitly re-scope the missing BS2 wrapper history gate before merge. Fix the minimization false-success path as part of the runner handoff. The release-selected hash/merkle design, immutable header binding, sealed canonical-byte API and CS2 reconciliation otherwise passed this general review; private acceptance remains with the designated reader.
