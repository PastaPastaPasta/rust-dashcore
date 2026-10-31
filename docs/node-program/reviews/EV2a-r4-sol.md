# EV2a r4 — narrow Sol review

Reviewed exact `node/ev2a-dml` head `729f8521c1aeadffdb3f02bd5b23391c6d9f87ff` in detached worktree `/work/scratch/ev2a-r4-sol/wt`. Scope is R3-M1, R3-m1 and R3-n1 only, plus the logic of the private-gate wiring, over `c34a88ec6..729f8521c` (`c46d58275`, `57b29086c`, `729f8521c`). I read the r3 review and used the code-review-validator workflow. No new finding in the reviewed comparisons or gate logic.

## Finding closure

**R3-M1 — closed.** `node-l1/src/dml.rs:193` now compares all three fields returned by Core's list read: complete canonical bytes, hash, and complete evodb bytes. The test-only `dash-dml/src/testkit.rs:213` writer follows Core's v24 `CDeterministicMNList::Serialize`: block hash, height, registration counter, CompactSize count, then complete masternodes in `core_order()`. The member encoding includes internal IDs. Raw evodb bytes therefore observe collision-bucket order as well as content; `IsEqual` is no longer the fuzz comparison. Both the canonical-input and disk-input legs use this helper.

The canonical list's evodb read-back comparison **is complete for the oracle outputs**. `compare_evodb_reads` (`node-l1/src/dml.rs:202`) requires Rust's reader to consume the entire disk input, asks Core to read that same disk input, and compares every returned field against Core's resulting list. Core's `Unserialize` inserts entries in file order with `AddMN(..., false)`, so a bucket serialized newest first is reversed by reloading it. Requiring the reread disk bytes to equal the original disk bytes would impose behavior Core itself does not have. Comparing Rust's complete reread outputs with Core's reread outputs preserves the ID/counter checks and observes the reversed bucket's complete order. The committed `collide` guard independently passed against pinned Core, including `reread.disk != original.disk` and complete agreement after rereading.

`compare_diff_outputs` (`node-l1/src/dml.rs:216`) compares the built diff directly against **both** `BuildDiff` byte outputs before performing the disk read-back check. Core's own v24 BuildDiff writes additions in internal-ID order; both serializers write sorted updates/removals. The two produced encodings coincide in this domain. The read-back must consume all bytes and re-encode to those same bytes; it cannot replace or weaken the preceding raw-byte check. Reordered updates are caught even though the decoder normalizes their order.

`compare_unique_outputs` (`node-l1/src/dml.rs:231`) checks both the preimage and SHA256d of Rust's preimage against Core's returned hash. `diff_mnlist_order` retains its complete order/canonical-list comparison and operation-failure index/class/text comparison; its implementation did not change in this range. The accepted D3 refusal-text projection and D12/D17/D20 domain exclusions remain unchanged.

**R3-m1 — closed.** `unique_property` (`node-l1/src/dml.rs:828`) uses `(kind & 0x7f) % 6` and retains bit 7 for the operator scheme. The example now uses the shared `unique_seeds()` source. The replacement IPv6 and CJDNS values lie in Core's domain. The corpus has 15 unique seeds; all 15 accepted on both sides. `unique_seeds_reach_their_property` (`node-l1/tests/l1_dml.rs:634`) checks the intended property/scheme, the committed content-addressed input, and `BothAccept`, so aggregate agreement cannot hide the old selector error.

**R3-n1 — closed.** `.gitattributes` marks the public seed paths binary. `git check-attr` confirms binary set and text/diff/merge unset on both previously offending paths; `git diff --check c34a88ec6..729f8521c` passes with the corpus included.

## Independent guard mutations

I independently constructed evodb-output perturbations of the pinned `collide` list, retaining Core's correct canonical bytes and hash: change only the counter; change one internal ID; swap two rows with the same 64-bit collision prefix. Each is caught as `evodb list` by the actual shipped comparator. These independently reapply the three list-output guard cases, rather than relying on the implementer's mutation report.

I also copied the exact reviewed DML comparator module and committed L1 guards outside the worktree, correcting only fixture paths/import routing, and replayed four source regressions. The unchanged copy passes all 11 active L1 tests. Every mutant builds successfully and the indicated committed guard fails with libtest exit 101:

| Source regression | Guard and observed failure |
|---|---|
| Remove the evodb-list assertion | `list_outputs_guard_the_counter_ids_and_order`: `Ok(())` instead of `Err("evodb list")` |
| Restore evodb-diff decode/re-encode normalization, including its normalized read-back reference | `diff_outputs_guard_the_update_order`: `Ok(())` instead of `Err("evodb diff")` |
| Remove comparison with Core's unique hash | `unique_outputs_guard_the_hash`: `Ok(())` instead of `Err("hash")` |
| Restore the full-byte `% 6` selector | `unique_seeds_reach_their_property`: `Service` instead of `OperatorKey { legacy: true }` for selector `0x85` |

The mutation sources, driver, results and failing logs are retained under `/work/scratch/ev2a-r4-sol/`. No reviewed source was edited.

## Private-gate script logic

`c46d58275` is satisfactory within this static review. `run_private_tests` accumulates failure status from the prior receipt checks/classifier and from `e5_private_suite`, so the fourth leg is required whenever `dash-dml` exists. E5 requires a 0700 directory, a two-line manifest with a positive test count and SHA256, and matching suite bytes. It runs the exact run-specific integration-test target and requires both Cargo exit 0 and a libtest summary with exactly the declared count passed, zero failed and zero ignored; a missing summary fails. Counts alone are printed. The suite copy uses a run-specific name, is removed on completion, and is covered by the existing run's EXIT cleanup. The new selftest cases cover count mismatch, hash mismatch, failed tests, missing/wrong-mode E5, concurrent runs, and cleanup. `bash -n` passes.

I did **not** execute the private suite or the script's selftest and did not read `~/private`. This review establishes the wiring's logic, not a new private-suite execution receipt.

## Validation

- `cargo test --locked -p node-l1 --features oracle --test l1_dml -- --nocapture`: **11 passed, 0 failed, 1 ignored** (full-size mainnet import, outside this narrow rerun).
- Public-seed replay: codec **121 accept / 86 reject / 4 outside domain**; diff **74 / 141 / 5**; order **9 / 7 / 0**; unique **15 / 0 / 0**.
- `cargo clippy --locked -p dash-dml -p node-l1 --features node-l1/oracle --all-targets -- -D warnings`: passed.
- Workspace formatting check, full commit-range diff check, and shell syntax check: passed.
- Both linked plain-oracle archive copies were independently hashed and match the recorded `e3bdd706…2a5f61fda0cf1e34b60204` pin. Builds removed `DASH_ORACLE_ALLOW_UNPINNED`; the standalone output probe also asserts `ARCHIVES_PINNED`.

## Four five-minute smoke legs

A scratch-only adapter loaded the unchanged registry and called the existing `fuzzctl.smoke`/`run.sh` path with a 300-CPU-second budget for each selected target, one leg at a time, with runner selftest and triage disabled. It used a fresh 0700 sink at `/work/scratch/ev2a-r4-sol/sink`, never the actual private tree. All four completed legs passed:

| Target | Executions | Measured CPU seconds | Result |
|---|---:|---:|---|
| diff_mnlist_codec | 167,267 | 322.2 | PASS |
| diff_mnlist_diff | 9,321 | 322.2 | PASS |
| diff_mnlist_order | 235,010 | 323.6 | PASS |
| diff_unique_hash | 5,515,069 | 326.5 | PASS |

Aggregate fuzzer CPU: **1294.6 seconds** against a 1,200-second requested floor. **Zero divergences, crashes, sink failures or refusals.** Each verdict names the clean exact reviewed head and the pinned fuzz archive (`56b03a46…b523f`), whose linked copy was also independently hashed. This is smoke evidence, not DEC-184 certification.

One initial compilation-only runner was stopped using verified numeric process IDs to move the long job to a user systemd unit; no fuzz segment had started. That incomplete build directory is retained and excluded from the four completed receipts above. No process was killed by command-line pattern.

## Limits and final state

The known overall gate EXIT 1 results, scanner-version issue, BL-20 reruns and manager-confirmed BL-83 slow-unit classification remain outside this brief; no overall gate or private suite was rerun. There is no new private-suite receipt or certification claim here.

Builds and test programs used `nice -n 10`, `CARGO_BUILD_JOBS=8`, and disk-backed `TMPDIR=/work/scratch/ev2a-r4-sol/tmp` at 0700; the fuzz runner used its own per-run temporary directories inside the same scratch evidence tree. The ordinary and fuzz workspaces had independent build directories. **Both Cargo target directories and the standalone test binaries have been deleted.** The detached source worktree is clean and unchanged; no edits, commits, pushes or GitHub actions occurred. This report is uncommitted.

Evidence is retained under `/work/scratch/ev2a-r4-sol/`: `provenance.json`, `fix-range.diff`, `l1-dml.log`, `clippy.log`, `fmt.log`, `mutations.log` and `mutations/`, `output-mutations.rs` and its driver/log, copied plain/fuzz build outputs and archive-hash JSON, `smoke.py`, `fuzz-smoke.log`, `fuzz-verdicts.json`, and the scratch-only sink.

**Verdict:** APPROVED

Sol gate satisfied: yes
