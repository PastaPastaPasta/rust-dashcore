# O8 StateDigest v1 — independent review, round 3

2026-10-13. Narrow review of rust-dashcore `07e7af7f3..547943cf2` and Dash Core `02118350e1..b28fa4c1ea`. Review and mutations used private detached copies under `/work/scratch/o8-sol-r3`; the source worktrees, branches, and GitHub were untouched.

## Findings

No new findings in the round-2 fix delta.

## Round 2 closure

| R2 item | Judgment and file:line evidence |
|---|---|
| F1 — deployment `since` including the tip | **Closed.** `conformance/src/digest/extract/checks.rs:307-318` checks each listed BIP9 deployment's `status_next` and `since` against that block's own RPC reply. `:523-544` derives the child's state start from the reply's `since` or `h+1` and checks an active deployment's `height` against that result. `:360-386` retains the child's independent check of its parent's `since`, including a boundary between runs with different parameters. `conformance/src/digest/extract/tests.rs:1195-1225,1274-1294,1583-1620` exercises own reply, child reply, tip, state transitions, active height, and parameter boundaries. Core's RPC supplies `status`, `since`, `status_next`, and active `height` at `src/rpc/blockchain.cpp:1478-1534`; the digest writer uses `StateSinceHeight(&index, ...)` at `src/statedigest.cpp:181`. |
| F2 — `is_rm` for a cached, already removed lock | **Closed.** Core `src/instantsend/db.cpp:379-389` bypasses the cache to look up the root lock, sets `removed` only when it queues that lock's removal, then writes the batch. A failed LevelDB write throws before the boolean reaches the caller (`src/dbwrapper.cpp:200-214,261-269`). `src/instantsend/instantsend.cpp:339-351` returns that boolean, and `src/evo/chainhelper.cpp:94-103` emits `is_rm` only when it is true. `src/test/statedigest_tests.cpp:161-206` covers a stored root and a confirmed root that remains discoverable through cache. |
| `join` visibility | **Closed.** `dash-statedigest/src/recording.rs:147-149` declares `join` private. |
| Spec wording | **Closed.** `docs/node-program/designs/O8-statedigest-v1.md:227,386,405,489` describes the root removal condition, the return path, the test, and the BIP9 `since` rule. |
| Vector provenance | **Accurate.** `test-vectors/statedigest/PROVENANCE.md:14-22,54-57` distinguishes the stored r4 build from the later r5 extraction. All 81 stored nodes' decoded digest, decisions, and golden files were byte-identical to the r5 extraction files, and their hashes matched both sets of `extract.json`. All 81 decoded `blocks.dat.zst` files and r5 plain `blocks.dat` files matched the stored hash. The r5 extraction log records 81 successful nodes, zero failed; its `dash-conformance` binary hashes to `99d6b0d3cd63…`, while stored manifests name `98ac3f623765…`. |

## Mutation probes

| Fix | Isolated mutation and focused check | Result and restoration |
|---|---|---|
| F1 | In private `checks.rs`, allow a wrong own-reply `since` at the four-block fixture's tip (`since.as_int() == Some(want) || h == 3`). Run `cargo test -p dash-conformance --all-features deployment_keys_are_checked_against_getdeploymentinfo -- --exact digest::extract::tests::deployment_keys_are_checked_against_getdeploymentinfo`. | **Caught:** exit 101; the tip fixture unexpectedly extracted, so `expected an error` failed at `tests.rs:526`. Restored with `cp -p`, `touch`, and `cmp` against the backup and detached source; the focused test then passed. |
| F2 | In a private copy of the configured Core build, remove the boolean guard at `src/evo/chainhelper.cpp:99-101`, emitting `is_rm` after every found conflict. Rebuild `src/test/test_dash`; run `--run_test=statedigest_tests/is_rm_only_for_a_removed_lock`. | **Caught:** exit 201; `statedigest_tests.cpp:206` saw an `is_rm` for a confirmed root no longer in the DB. Restored with `cp -p`, `touch`, and `cmp` against the backup and detached source. The unmutated copied binary passed all six `statedigest_tests` cases. |

## Validation and limits

- `cargo test -p dash-statedigest -p dash-conformance --all-features`: exit 0.
- `cargo clippy -p dash-statedigest -p dash-conformance --all-features --all-targets -- -D warnings`: exit 0.
- `cargo fmt --all -- --check`: exit 0.
- `contrib/node/check-pure.sh`: exit 0, eight pure crates.
- `python3 -I -m unittest discover -s contrib/node/statedigest/tests`: 27 passed after setting `STATEDIGEST_CHECK` to the private example binary. The initial runs failed only because the fixture's default example path pointed at the checkout's `target/` while this review used a separate `CARGO_TARGET_DIR`.
- Copied Core `test_dash --run_test=statedigest_tests`: six cases passed, zero errors. The F2 mutant was rebuilt from a private copy of the supplied configured build, whose reviewed source files matched the detached Core revision.
- The provenance check compared stored decoded bytes to the existing r5 re-extraction artifacts and their hashes; it did not rerun 81 RPC extractions. The full 4,469-test manager gate, full Core suite, functional tests, and live oracles were outside this narrow round.

**Verdict:** APPROVE

Sol gate satisfied: yes
