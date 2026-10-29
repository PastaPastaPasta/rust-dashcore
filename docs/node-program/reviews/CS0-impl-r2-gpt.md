APPROVE-WITH-NITS

Focused CS0 closure review, round 2. Reviewed fresh detached worktree `/work/scratch/cs0-r2-gpt/repo` at `d3366be7d69e533cde972e7722c98ea6107d98f3`, 38 commits on integration `2e20826749a73ef69854903c2c5d2316239d9d8b`. Read REVIEW-COMMON, the round-1 review and the CS0 design. Scratch evidence is `/work/scratch/cs0-r2-gpt/`; tests used `TMPDIR=/dev/shm/cs0-r2-gpt-tmp` with mode 0700 and `CARGO_BUILD_JOBS=12`. The temporary directory was removed after testing.

1. **R1 MAJOR — MAX_SIZE script decode mismatch: CLOSED.**

   Locations: `Cargo.toml:53`, `dash-consensus/src/primitives/decode.rs:142`, `node-l1/tests/l1_boundaries.rs:255`, `:287`, `:318`.

   The workspace and nested workspaces now resolve to base-sdk `481f8c351c4f00dfad08a11063d2852e74e06016`. At that revision, `pkgs/types/src/codec.rs:291` checks vector counts with `CompactSize::decode_size`; script buffers use the byte-vector adapter at `pkgs/types/src/adapters.rs:14`; the payload length is checked at `pkgs/primitives/src/transaction.rs:240`. Thus the input/output counts, both scripts and the extra payload enforce the Core cap. The new unit test checks rejection at the prefix for all five fields; CS0 still checks its own block count.

   Re-derived the lost r1 reproducer independently in `probe.rs`. The original 33,554,488-byte scriptSig transaction now rejects on both pins, as does the populated oversized scriptPubKey. Exactly 33,554,432 bytes accepts for each script; 33,554,433 rejects. Added the same independent checks for populated extra payloads. All six inputs agree as streaming/exact transactions and inside blocks on both pins. The probe asserts expected acceptance/refusal, canonical bytes on success, consumed length, suffix preservation and cursor rollback after a later transaction fails. Results: `max-size-probe.log` (12 field/length/pin rows, no divergence).

   The shipped boundary tests also pass: scripts 20 rows, payloads 10 rows, and **5,491 rows total**, zero failures, no allowlist in the comparison code. Core's unchanged bound is v23.1.8 `src/serialize.h:314` and v24.0.0-rc.3 `src/serialize.h:318`. No further fix is required for this finding.

2. **R1 MAJOR — node-sink breaks Windows builds: CLOSED.**

   Locations: `node-sink/src/lib.rs:42`, `:57`, `:86`, `:114`.

   Unix permission handling is behind `cfg(unix)`. On other platforms, private writes/directories return `io::ErrorKind::Unsupported`, privacy checks return false, and a configured private test refuses before accessing private data. Ordinary public interfaces remain available when the private environment is unset; there is no permissive file fallback.

   Re-ran `cargo check --locked --target x86_64-pc-windows-gnu --all-targets` for every crate in the core CI group, plus dash-consensus and node-l1: success (`windows-check.log`). This checks tests and benches as well as the libraries. Windows binaries were not executed. The original unconditional Unix-import/method failure is gone. No further fix is required for this finding.

3. **R1 MAJOR — missing BS2 wrapper/history leg: CLOSED.**

   Locations: `conformance/Cargo.toml:19`, `conformance/src/check.rs:435`, `conformance/src/consensus.rs:44`, `dash-consensus/HISTORY.md:20`.

   The shared per-block checker now invokes the dash_consensus leg for RPC and blk inputs. It checks exact consumption, the block's canonicality flag (the conjunction of its header, count and every transaction's flag), core_serialize, Decoded::core_bytes, size, block hash, every V24_0-selected txid, and the V24_0 merkle root with mutation false. It also selects every txid under V23_1. Any profile difference fails on mainnet; when the leaves agree, both profiles' roots and mutation flags must agree. On testnet, stable-txid differences are counted, as CS0 §9.2 specifies. A decode/canonicality failure additionally decodes the reference transaction spans to locate the problem.

   `cargo test --locked -p dash-conformance` passes **76 tests**. The fixtures cover all three legs, a stable asset unlock, altered oracle hash/txid/root, truncation, trailing bytes, duplicated-leaf mutation, and rejection of a profile-changing txid treated as mainnet. The blk fixture confirms that the wrapper leg also runs on a stale block.

   Re-ran the actual shared checker on a bounded live range fetched exclusively through read-only `contrib/oracle-cli` calls (`getnetworkinfo`, `getblockhash`, `getblock 0/1`):

   - Mainnet heights **1,465,707–1,465,718**: 12 blocks, 1,127 transactions, 12 wrapper checks, zero mismatches.
   - Testnet heights **1,568,771–1,568,782**: 12 blocks, 238 transactions, 12 wrapper checks, zero mismatches.

   `history-probe.rs` additionally compares each fetched block against the pinned in-process Core oracle on **both pins**, and each transaction's rules-selected txid, instance hash, canonical serialization and size. Block merkle roots/mutation, canonicality, canonical bytes, exact consumption and size all agree. No stable-txid difference occurred in these ranges. Evidence: `fetch-history.py`, `history/`, `history-probe.rs`, `history-probe.log`.

   HISTORY.md records the larger implementer runs (1,568,783 testnet blocks and 1,465,719 mainnet blocks) and explicitly labels them partial history, with zero live stale-block coverage. I checked the implementation and reproduced the bounded ranges above; I did not independently rerun those roughly three million blocks. This closes the missing acceptance path without claiming full-history or live-stale coverage.

4. **R1 MINOR — minimize-missing-input reports success: CLOSED.**

   Locations: `node-fuzz/run.sh:159`, `:272`, `:277`; self-tests at `:77` onward.

   Re-ran the r1 command against a fresh synthetic mode-0700 tree, with a nonexistent `runs/previous/crashes/missing`. It exits **2** with `the crash file is not an existing regular file`, before building or creating a run (`minimize-missing.log`). A minimization verdict now requires a clean run, libFuzzer status 0, and the produced `crashes/minimized` regular file. `node-fuzz/run.sh --selftest` passes, including real crash minimization, missing input, rejected paths, and an input which does not crash (`fuzz-selftest.log`). No further fix is required for this finding.

5. **New NIT — history evidence confuses client version with protocol version.**

   Location: `dash-consensus/HISTORY.md:14`.

   The configuration row says `protocol 240000`. Both queried v24.0.0-rc.3 oracles report `version: 240000` and **`protocolversion: 70242`**. A reader identifying the exact oracle protocol from this record gets the wrong value. This does not affect the comparisons or their verdicts.

   Suggested fix: label 240000 as the client version, or replace the text with `client version 240000, protocol 70242`. The read-only responses are retained in `history/mainnet-networkinfo.json` and `history/testnet-networkinfo.json`.

Fold/rebase preservation:

- The literal **head tree is not identical** to `cs0-repin-cherrypick-ref` (`135704dd93ad6eb0ffbcac97b810221dfb8c0b4e`). Head tree: `f143e6a8b5ca87818c3469d448f2d53d0027c25c`; reference tree: `173293fddeeec5d96fda2338232aac7c509bbebe`. The reference ends at the script-boundary commit, before the final four commits.
- Comparing the matching folded endpoint **3b2eeb67** against that reference gives **zero differences outside `docs/node-program`** (`code-at-fold.diff` is empty). The only three differences are integration's STATUS.md and its DW-E0-08/ST3a design-review documents; none was changed by CS0 commits between the stated base and that endpoint. This establishes preservation of code, tests, manifests and lockfiles at the corresponding fold endpoint.
- Inspected the subsequent four commits separately (`post-fold.diff`): the dash-pkc::bls::BlsCtx re-export and equivalent from_legacy/is_legacy use; all-five-prefix MAX_SIZE regression checks; the populated-payload boundary test and shared helper extraction; and HISTORY.md. These account for every additional difference. No unexplained code delta or new behavioral regression found.
- In a second fresh detached worktree, both `cargo check --locked -p dash-consensus --all-targets` and **`cargo build --locked -p dash-consensus --all-targets`** pass at **c0b2e242, 5bd1f56f, 9ee8e501, ed4c6d4f and 1070d245**. These sample the skeleton, primitives, L1 addition, release-interface refactor and BS2 leg. Results and individual build logs are in `sample-build-summary.log` and `sample-<commit>.log`. This is a sample, not a claim that all 38 commits were built.

Other verification: the focused dash-consensus/node-sink/node-l1 suite passes **118 test results**, including four compile-fail doctests and two explicitly skipped private-environment tests; all **18 node-l1 tests** pass. Targeted all-target clippy with `-D warnings`, workspace formatting, base-sdk pin checking (including nested workspaces), the pure-crate gate with both feature configurations, and git diff whitespace checking pass. Logs are retained in scratch. The full 2,929-test gate, multi-day fuzz budgets, benchmarks, real private tests/receipts and EMBARGO-1 content were not independently reviewed in this focused round; the private acceptance remains with the designated reader. No private-tree access, branch source edits, commits, pushes or posts were made. Both review worktrees remain clean.

Summary: **all three r1 majors and the r1 minor are closed**. The corresponding fold preserves the reference's implementation tree, and the later changes pass the focused checks. No new blocker, major or minor found; one documentation nit remains. **APPROVE-WITH-NITS** for the public CS0 closure scope.
