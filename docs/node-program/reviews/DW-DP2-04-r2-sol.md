# DW DP2-04 — Sol review, round 2

Reviewed only `1a2b35e3..ed16feb9` (five signed fix commits) in a detached worktree. Read the delta in full. The implementer's worktree, branch, and GitHub were not changed.

## Findings

1. possible security note: see manager

2. **Medium — F4 remains partially open: the identity write lock has no 30-second end-to-end bound.** `private_details.rs:464-493` checks the deadline before each open, but not after the last open. `:683-723` then applies details and performs per-contact database work under the same lock without checking the deadline. A temporary exact-head probe gave one open a 4-second delay with a 2-second read budget; settle succeeded after 4.52 seconds. The new loop check and yield do improve cancellation between opens. The existing `a_slow_read_ends_at_its_deadline` test covers multiple opens, not a slow final open or the apply phase.

3. **Medium — F3 remains partially open: a publish can still run an unbounded library fetch.** The engine's pre-read at `private_details.rs:884-887` refuses an over-cap result before redeeming a grant, and `:134-146` counts raw SDK page entries. The library publish called at `requests_net.rs:167-185` makes its own uncapped fetch after that pre-read. Documents added in between can therefore make the write hold the identity lock for unbounded fetch and open work. The contract documents this residual at `docs/contracts/m4-dashpay-engine.md:2176-2182`; a bounded library publish API is needed to close F3 fully.

## Closure table

| Round-1 finding | Judgment | Evidence |
|---|---|---|
| S1 | Closed for settle; possible security note: see manager | possible security note: see manager |
| F2, synchronous SQLite read | Closed | `contacts_model.rs:613-619` reads the in-memory cache; `private_details.rs:269-343,552-588,617-641` loads and writes through. |
| F3, publish document cap | Partially closed | `private_details.rs:134-146,884-887`; the second fetch at `requests_net.rs:167-185` remains unbounded. |
| F4, read-first lock deadline | Partially closed | `private_details.rs:464-493,683-723`; the final-open probe above exceeded its deadline and succeeded. |
| F5, stale digest rows | Closed | `private_details.rs:709-720` calls the transactional `forget_contact_infos_except` at `dw-appdb/src/lib.rs:870-893` and signals. |
| M26, source gone after signed write | Closed | `private_details_tests.rs:945-964` exercises the unavailable source and later retry. |

The new cache epoch check at `private_details.rs:297-309` correctly rejects a load overtaken by a write; `:329` bumps that epoch. The per-identity lock order remains consistent in `private_details.rs:661-680,824-847`: no wallet-state guard is held through the network read. possible security note: see manager

## Mutation table

Each mutant was applied alone from a saved exact-head file. After each focused run, the file was restored with `cp -p`, touched so Cargo could not reuse the mutant build, and `cmp` verified. These mutations differ from the implementer's M01–M36 and R1–R4. The initial R8 command matched zero tests and was discarded; the table uses its rerun against a real removal test.

| Reviewer mutant | Focused test | Result |
|---|---|---|
| R5: make a completed digest load install an empty map | `contact_reads_digests_from_memory` | Killed: expected loaded state never appeared. |
| R6: ignore the cache load epoch mismatch | `contact_reads_digests_from_memory` | Survived: this test does not arrange an intervening write. Coverage gap; the exact-head code has the check. |
| R7: refuse exactly 1,000 documents (`>=` instead of `>`) | `a_publish_over_the_document_cap_is_refused` | Killed: the boundary publish failed. |
| R8: possible security note: see manager | possible security note: see manager | Survived (1 real test); possible security note: see manager. |

## Validation and limits

- `cargo test --locked -p dw-engine -p dw-appdb -p dw-app`: passed. Unit suites: 759 engine passed (6 ignored), 73 appdb passed, 115 app passed. All 13 `e0_05_bringup` tests and the remaining integration and doctests passed.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked -p dw-engine -p dw-appdb -p dw-app -p dwcli --all-targets -- -D warnings`: passed.
- Restored-source focused control: 23 private-details tests passed, 1 public-testnet test ignored. The temporary deadline probe passed, was restored with `cp -p` and `touch`, and `cmp` matched the saved exact-head test file.
- Raw malformed-entry counting in `SdkInfos` was reviewed in source but not exercised through a live SDK page. Public testnet T2 was not run.
- The detached scratch worktree used mode 0700, its own `TMPDIR` and `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=12`, and `nice -n 10`.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
