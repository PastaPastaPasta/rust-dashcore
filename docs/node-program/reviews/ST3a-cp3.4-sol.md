# ST3a CP3.4 — Sol review

Reviewed **500dabd090fe1d9081136f63041a3ec420542944..86925887d0c7c2eb16141d9e1ce227d4f8e46060**, the requested `node/st3a-power-loss` head. **CP3.3-1 and CP3.3-2 are closed under DEC-185, and the R12 restriction is correct. The new shards do not exactly partition the nested search: checker/projection decisions can put one recovery in multiple shards, each of which repeats its nested search. This also occurs in the actual depth-4 campaign.**

Applied the code-review-validator workflow to the actual seven-commit diff, read DEC-185, the CP3.3 report, the reviewer contract, the implementation memory note, and the checkout's amended I41–I43 and §8.4. The external reviewer contract's RAM-backed TMPDIR suggestion was superseded by this task's explicit on-disk requirement.

Work used the detached `/work/scratch/st3a-cp34-sol/wt` at the exact head, with its own Rust `target/`, `CARGO_BUILD_JOBS=8`, and mode-700 `TMPDIR=/work/scratch/st3a-cp34-sol/tmp`. Builds, tests, independent probes, and the nine campaign spot shards ran at **nice 10**; the spot runner used at most **four** processes concurrently. The runner's stub tests use its prescribed inline nice 19. No source edits, commits, pushes, GitHub actions, private reads, chain/bench access, or process-pattern kills. Independent probe sources and logs are outside the worktree in `/work/scratch/st3a-cp34-sol/`. The 39 CPU-hour campaign was not rerun.

## Findings, by severity

### CP3.4-1 — Major: nested searches are repeated across shard boundaries

**Locations at 86925887d:** `dashd-store/src/faulty/harness.rs:1096`, `:1144–1159`, `:2243–2248`; the recovery-decision snapshot at `:917–918`. Test coverage: `dashd-store/tests/power_loss_cp33_witness.rs:918` and `dashd-store/tests/power_loss_toy.rs:1094`.

The outer DFS tree includes decisions forced by recovery, `projection()`, checks, and possibly R12. The shard planner splits that complete tree by prefix. The nested search, however, is deduplicated by **only the decisions recorded before projection and the view checks** (`recovered`). Unsharded exploration keeps one `nested_seen` set for the whole image/kind, so checker-only alternatives of the same recovery get one nested search. Each shard creates a fresh set. If a prefix splits checker-only alternatives, the same recovery gets a nested search in every affected shard.

**A concrete campaign case, both releases.** I used the existing S1 scenario, `Extended`, `Nested::Depth1`, R12 enabled as usual, and narrowed the window to event **53**. No SUT or library implementation was changed. A small external regression demands that the shard sums equal the full search:

```text
S1 V23_1 / V24_0, image 53:
unsharded: 91 branches, 86 R7a converged, clean
depth 0:   91 branches, 86 converged
depth 1:  143 branches, 138 converged
depth 2–6: 247 branches, 242 converged
```

The full search and every shard are individually clean. The five outer branches are preserved; the discrepancy is **156 repeated nested branches** at depth 4. The four power-loss prefixes `e53-pl-0.0`, `e53-pl-0.1`, `e53-pl-0.2`, and `e53-pl-1` share the same recovered-decision key. Each runs the same 52 nested branches, where the unsharded search runs them once. The ledger records 53 branches for each of these four shards, plus 35 for `e53-pk-x`: **247**, exactly the independently reproduced inflated sum. Evidence: `s1-event53-partition.log` and `cp34-probes.rs` in the evidence directory.

**An existing projection-only SUT also exposes it.** The unchanged `Proj("fixed")` from `power_loss_cp33_witness.rs` has 69 full-search branches. Its shard sum is 69 at depth 0 and **105 at depths 1, 2, 3, 4, 8, and 16**. The extra 36 are nested branches repeated after a projection-only cell splits the recovery between shards. Evidence: `projection-partition.log`.

**Actual plan inspection.** A wrapper delegates every ToyChain operation and check unchanged and records exactly the same post-open variable snapshot that the harness uses for `recovered`. Planning all 9,440 depth-4 prefixes finds cross-shard recovered-key groups in each release: **53 in S1, 47 in S2, 63 in S4b, and 2 in S7**. S3, S4, and S11 have none among those prefix representatives. This is an ownership diagnostic, not a computation of the campaign's unique nested-branch total; a repeated key only creates repeated branches when its recovery has captured inner images. The S1 event-53 comparison above proves the branch-level consequence. Evidence: `actual-prefix-keys.log`, `actual-prefix-keys-summary.json`.

The checked-in Restarter partition test passes: 222 branches at depths 0, 1, and 3 in both modes. Its recovery reads the relevant record itself, so it does not exercise this split. The R12 regression also passes because it leaves `nested = None`; its outer-branch equality does not establish nested-branch equality.

**Impact:** the specified exact-partition property fails, including at the deployed depth 4. The 191.9M total is a count of executed attempts, containing repetitions, rather than the cardinality of the unsharded search. I did **not** demonstrate an omitted branch, a false clean, or a durability failure from this duplication. Additional checks do not by themselves invalidate the clean individual executions. Nevertheless, the requested no-duplication proof and I43's exact-partition claim are false, so I cannot satisfy this review gate.

**Suggested fix:** assign each `(image, kind, recovered decisions)` nested search to exactly one deterministic owner consistent with the unsharded DFS's first representative. Preserve exhaustive enumeration of all outer checker/projection alternatives, and preserve the canonical representative's projection assignment and R7a outcome. Do not deduplicate only by the numeric prefix or remove checker decisions from outer coverage. Add the S1 event-53 nested comparison and the existing Proj fixed/bug cases at depths 0, 1, 2, 4, and beyond the leaf depth; compare branch multiplicities, all R7a counters, and findings. Keep R12's independent all-zero owner rule.

### CP3.4-2 — Minor: resume skips a shard whose latest attempt failed if an older attempt was ok

**Location:** `contrib/node/st3a-shards.sh:91–95`, `:140–142`, compared with `:153–166`.

`ALREADY` includes any historical `ok` row at this head, while the final verdict uses the **last** row. For a same-head ledger containing `ok` followed by `FAILED` for one shard, a resume skips that shard forever and finishes not-clean. This can occur when overlapping same-head attempts finish with different results. My stub probe seeded that history, made the next attempt capable of passing, and observed **zero shard executions**, `skipped 3`, `ok=2 failed=1`, exit 1. Evidence: `runner-probes/latest-failure.json`.

This is fail-closed and does not affect the supplied ledger, which has exactly one row per shard. **Suggested fix:** derive the resume skip set from the last row at this head, using the same selection rule as the verdict, and extend `cp33_shard_runner_ledger` with an older-ok/latest-failed case.

## Answers and closure by mechanism

**1. Partition and ledger.** The **outer prefix algorithm** partitions deterministic, variable-length decision trees correctly. `Dfs::limited(depth)` advances only positions below the depth; a short leaf produces its complete shorter prefix. `Dfs::pinned(prefix)` advances only later positions. Distinct terminal prefixes are disjoint, and every outer leaf has one generated prefix. An independent probe imports the actual `decide.rs` and compares multisets over **300 adaptive trees**, domains 1–4, leaves at depths 1–6, and shard depths **0–8**: all **2,700** comparisons pass with multiplicity one (`dfs-partition.log`). The nested ownership problem is CP3.4-1.

The runner's verdict is fail-closed for missing or failed planned shards: only the last same-head `ok` row counts; a missing row enters `failed`; a nonzero shard exit, missing SHARD result, or missing passing test cannot produce an `ok` row. The real shard test asserts `Report::is_clean()`. The ledger append is serialized by `flock`. Seven independent stub cases verified failure, no result, no test, a missing same-head row, recovery on resume, and head changes (`runner-probes/results.json`). A normal same-head resume may safely skip successful completed shards under an unchanged deterministic binary/plan. Different-head rows are ignored, and the fresh plan is generated again. CP3.4-2 is the latest-failure exception to resume progress; it remains not-clean.

**2. R12.** `harness.rs:1116–1120` is correct. The first DFS leaf selects zero at every decision; it belongs to the unique all-zero prefix, including an empty or shortened prefix. The first branch of that pinned shard therefore equals the unsharded first branch for the image/kind. A nonzero prefix cannot own it. R12 runs there once, subject to the same continuation, successful-open, and fault conditions as unsharded exploration; `r12_all` retains its existing override. The actual campaign disables neither continuation nor this owner. The fix does not skip an eligible all-survived R12. The duplication finding concerns nested searches and does not refute this restriction.

**3. CP3.3-1 — Closed.** The synthesized `Mark::Abort` block is removed. Forward open/apply errors and recovery errors select `End::Refused`; successful returns select `Returned`. `reference_outcome` derives the end from `Outcome`, and `sanctioned_stop` first requires `Outcome::Aborted`. `Refused` suppresses the path-balance Model complaint only. R8 still requires an abort in the SUT trace in the fatal epoch. `torn_fail_stop` and `sanctioned_stop` invoke that same scan and cannot qualify an unmarked fatal return. No harness-generated abort remains. The Marker negative and marked/wrong-message/late-write controls are in the gate; refusal controls preserve R0 without a synthetic mark.

**CP3.3-2 — Closed under the binding amendment.** `Report::is_clean` rejects both `truncated` and `sampled`, as well as unjudged branches. Nested truncation is recorded before sampling. The walk remains biased, explicitly a smoke test that can only find. The extended component removes its sampler and routes through the shard ledger; invoking its test without a shard or plan fails. The independently rerun Skew regression passes: the capped sampled report has **8,216 branches, 2 truncated, 2 sampled, 23 sampled branches, no findings, and is not clean**; the exhaustive defective control finds R1, and its fixed control is clean. CP3.4-1 is a new exhaustive-ownership defect, not a reopening of sampling certification.

**Open-stage commits.** `c59b1df63` carries CH2b's OpenFailed mechanism; `58b371be3` supplies the local Fixture release. `8930875d2` wraps both a failed faulty lookup and a missing binding; `FlatFileError::open_failed` uses the wrapper alone, with no NotFound fallback. Injected read faults run before lookup and stay plain pread errors; short reads of an existing file stay plain. ENOSPC/EDQUOT survive wrapping for the failure class. The production and faulty stage regressions are included in the local gate. I found no new issue in these changes.

## Campaign integrity and spot evidence

Audited `/work/scratch/st3a-power-loss/shards-86925887d` without modifying it. All **9,440 expected IDs** appear exactly once in the plan and ledger; all ledger rows have the full requested head and `ok`. Every row exactly matches its sole `SHARD <id>` log line, its successful one-test result, and the three fields in its time file. All rows have one image, zero findings, zero truncated, and zero unjudged. A freshly compiled scratch binary regenerated **the identical plan, byte for byte**, at depth 4.

Exact totals:

- Executed branches: **191,923,870**.
- R7a converged: **173,563,384**.
- R7a sanctioned fail-stop: **16,917,738**.
- R7a recovered after fail-stop: **1,412,172**.
- R7a fail-stop after fail-stop: **260**.
- Both witnessed counters and unjudged: **0**.
- R7a sum: **191,893,554**; plus **30,316 outer branches** = **191,923,870**. This identity also holds separately for every ledger row.
- CPU time: **140,310.45 seconds**, matching the summary's rounded 140,310.

The plan SHA-256 is `d96dfd5ce9f5e9939b7114387dfa2e62ec2140e6d3e05d19320124c987c7e799`; the ledger SHA-256 is `a5f28e14c53d08dd8a1b78651e5712e0e5b390cb74b0cacd64a8cc8e828dd33b`. Detailed component sums, artifact hashes, and the reproducible audit are in `ledger-audit.json`, `ledger-audit.log`, and `audit-ledger.py`.

Nine independent spot shards, spanning both releases, zero/nonzero prefixes, a short prefix, ProcessKill, recovered-after-fail-stop, fail-stop-after-fail-stop, and the R12 regression image:

```text
S1-V23_1:e10-pl-0.0.0.0
S1-V24_0:e11-pl-0.0.1.0
S1-V23_1:e10-pl-0.0.0.1
S1-V23_1:e53-pl-1
S1-V24_0:e53-pl-0.0
S2-V23_1:e136-pl-1.0.0.1
S2-V24_0:e127-pl-0.0.0.3
S4b-V23_1:e147-pk-x
S4b-V24_0:e126-pl-0.1.0.0
```

All nine exited 0 and matched **every reported count and finding field** in their ledger rows exactly. Together they executed **47,070 branches** in **26.33 CPU-s**. Evidence: `spot-results.json`, `spot-expected.json`, `spots.log`, and per-shard logs under `spots/`. The focused partition regressions are separate small test explorations, not a campaign rerun.

## Validation and limits

The full local gate passed at nice 10:

```text
cargo fmt --all -- --check                              exit 0
git diff --check                                       exit 0
cargo test -p dashd-store --features faulty --no-fail-fast
                                                       exit 0; 296 passed, 0 failed, 20 ignored
cargo clippy -p dashd-store --features faulty --all-targets -- -D warnings
                                                       exit 0
```

The unmarked-fatal-return negative, Marker controls, Skew, lost-file/open-stage tests, both checked-in partition tests, and the R12 regression all passed. Clippy also passed in an independent run with `RUSTC_WRAPPER=`. The new external demanding partition tests failed as described in CP3.4-1, exposing coverage missing from the green standard gate. Evidence: `gate.log`, `gate.status`, `gate-results.json`, `fmt.log`, `diffcheck.log`, `clippy.log`, and `clippy-no-cache.log`.

The independent release build used `cargo test --release -p dashd-store --no-default-features --features faulty --test power_loss_deep --no-run`. The initial default-feature build was stopped during prolonged native archive writes; the completed gate reuses the matching **unchanged** prebuilt `librocksdb-sys 0.19.0+11.8.1` static dependency read-only through `ROCKSDB_LIB_DIR`/`ROCKSDB_STATIC=1`. All reviewed Rust code and tests build in the scratch worktree's own target. Only the initial build's verified owned process IDs were terminated, without a cmdline pattern. Native-dependency provenance and the stopped build log are retained.

This review verifies the supplied plan/ledger and sampled executions, not all 191.9M attempts independently. The small S1 and Proj cases disprove exact nested partitioning without requiring the campaign again. No data-loss or false-clean counterexample was established from duplication; the unresolved gate requirement is the promised disjoint enumeration and trustworthy unique coverage accounting. The scratch `target/` was deleted after validation; the independent release test binary is retained under `bin/fresh-power_loss_deep` in the evidence directory. The detached source worktree is clean. The report and evidence remain uncommitted.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
