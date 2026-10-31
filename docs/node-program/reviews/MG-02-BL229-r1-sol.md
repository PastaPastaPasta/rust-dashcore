# BL-229 governance voter resolution — Sol narrow review round 1

Target reviewed: `dw/bl-229-gov-resolved` at `f9d2fd04`, detached worktree, against Dash Core v24 behavior.

## 1. Trust

**Result: no finding.** `verify` first compares the decoded transaction txid with the header checked SML `pro_tx_hash` (`rust/crates/dw-engine/src/governance/collateral.rs:161-178`), then requires a ProRegTx payload and calls the shared `dw_protx::payloads::checked_collateral`. Internal collateral is rewritten to `(proTxHash, vout)` and its output amount is checked against the registered type (`rust/crates/dw-protx/src/payloads.rs:250-279`); external collateral remains the payload outpoint, matching Core's accepted ProRegTx semantics. Stored bytes take the same deserialize-and-verify path (`collateral.rs:181-185`), and failed rows are deleted before any fetch result is accepted (`collateral.rs:349-358`). The cache key is the 32-byte proTxHash and the table is in the per-network `app.sqlite` (`V2026101901__governance_registrations.sql:1-9`), so a corrupt or cross-network row cannot be trusted as another registration. `resolved` is keyed by collateral outpoint (`collateral.rs:223-238`), which prevents duplicate tally entries.

This agrees with Core's mapping: `CGovernanceObject::ProcessVote` looks up the masternode by the vote outpoint (`/work/home/agent/workspace/dash/src/governance/object.cpp:392-398`) and validates the signature against that masternode (`object.cpp:458-474`, `vote.cpp:231-238`). Core's v24 vote file also deduplicates by vote hash (`object.cpp:383-389`). I found no path for a fetched or stored byte to bind a vote to a different proTxHash or to add the same resolved outpoint twice.

## 2. D81 versus Core

**Result: no finding.** `GovernanceStore::set_resolved` compares the previous and new voting key for the same collateral outpoint, marks that old `(outpoint, old key)` as a re-keyed pair, and does not treat its old vote as a D72 wrong-key copy (`rust/crates/dw-governance/src/store.rs:1183-1228`). `drop_unbacked_votes` then removes that old-key vote because the resolved outpoint now admits only the new key; it is not placed in `refetch` or `uncounted` (`store.rs:1228-1243`). The vote cap is recomputed over the remaining unresolved slots (`store.rs:1244-1252`).

That matches Core's trigger and scope: v24 calls `RemoveInvalidVotes` after a synced masternode-list update (`/work/home/agent/workspace/dash/src/governance/governance.cpp:1150-1195`), includes a voting-key change or removal, and revalidates every vote for that outpoint (`/work/home/agent/workspace/dash/src/governance/votedb.cpp:81-100`). For funding votes on proposals Core checks the voting key (`object.cpp:458-474`); the wallet stores only those funding votes. A copy under a third key remains the separate D72 uncertainty path, as intended.

## 3. Resource bounds and privacy

**Result: no finding.** `claim` increments the session counter before starting work, clamps it to `SESSION_FETCHES = 4_000`, and never claims more after the ceiling (`collateral.rs:284-307`). Each round has at most 64 claims and `buffer_unordered(IN_FLIGHT)` enforces eight concurrent source calls (`collateral.rs:36-37`, `368-371`). Request failures are the only inputs to `Round.failed`; `Pace` waits 5 seconds and doubles to 5 minutes, while a clean round resets the delay (`collateral.rs:432-457`). Missing, undecodable, wrong-txid, and wrong-payload answers are misses; three misses put that proTxHash in `refused` for the session (`collateral.rs:310-325`). SDK requests have a 10-second timeout, two retries, and failed-address banning (`collateral.rs:71-82`); the RPC client has a 10-second timeout (`collateral.rs:88-114`).

The resolver is joined to the governance task and receives the same stop signal (`rust/crates/dw-engine/src/governance/sync.rs:234-257`), so it does not fetch while sync is disabled/stopped. The only identifiers sent are SML proTxHashes taken from the current list (`collateral.rs:284-295`); no wallet id, address, or wallet-specific state enters either source request.

The 4,000 counter is a logical registration-fetch budget; SDK internal retries can produce more transport requests for one claimed fetch. That is consistent with the implementation's documented retry policy and does not let an attacker create unbounded work. If the product requirement intends 4,000 individual wire requests, the SDK retry count would need to be included in the shared budget.

## 4. Store and migration

**Result: no finding.** The migration creates a strict per-network table keyed by a non-null 32-byte proTxHash (`rust/crates/dw-appdb/migrations/V2026101901__governance_registrations.sql:6-9`). Reads happen once per resolver session (`collateral.rs:187-200`, `349-358`); valid rows populate memory without a fetch, invalid rows are deleted and fetched again, and rows no longer present in a non-empty list are removed from memory and SQLite (`collateral.rs:241-264`). Writes and deletes are one SQLite transaction (`rust/crates/dw-appdb/src/lib.rs:1032-1055`), with persistence before the in-memory `settle`, so a crash costs a later fetch but cannot make an unpersisted row trusted. The session's `app.sqlite` is under `<data root>/<network>` (`rust/crates/dw-engine/src/engine.rs:108-128`).

The migration schema-rewind probe drops `governance_registrations` before deleting the later schema history (`rust/crates/dw-engine/src/platform/recovery_tests.rs:3016-3043`). `set_resolved` recomputes the per-key cap after removing re-keyed or otherwise invalid votes (`rust/crates/dw-governance/src/store.rs:1244-1252`). The restart, tampered-row, unlisted-row, and one-read-per-session tests cover the required restart behavior.

## 5. Tests and mutants

**Result: acceptable, with one coverage nit.** The reported 39/42 collateral mutants killed is strong. The three request-settings mutants are SDK transport details that require a real DAPI client/server; the settings are explicit and locally reviewable at `collateral.rs:71-82`, while the resolver's source-independent bounds are covered by unit tests. The targeted resolver suite passed all 10 tests, including tx binding, internal/external collateral, tampered rows, retries/misses, restart, eight in flight, and the 4,000 ceiling.

The `resolve_loop` wiring is covered indirectly by the regtest path and the pure `Pace` behavior is directly tested. The `RpcSource` `-5` response branch and stop/cancellation race are not directly unit-tested. This is not a correctness blocker because `-5` is a simple documented dashd not-found mapping and cancellation is fail-closed, but a local HTTP mock and a stop-during-fetch test would make those paths reviewable without a live node.

## 6. MG-01 guard

**Result: pass.** `clippy.toml` and `rust/crates/dw-protx/tests/no_operator_signing.rs` are unchanged from `49bfe12d`. `cargo test -p dw-protx --test no_operator_signing` passed all 5 tests, including `no_operator_key_signing_path_remains`. No operator-key signing API or call path was added; the governance resolver only parses public ProRegTx bytes and funding votes remain ECDSA/voting-key based (`rust/crates/dw-governance/src/vote.rs:62-64`, `149-178`).

## 7. Findings and scope

### Nit

- `rust/crates/dw-engine/src/governance/collateral.rs:118-149` (`RpcSource`) has no direct test for a JSON-RPC `-5` reply, malformed result, or transport error, and `collateral.rs:471-505` has no direct stop-during-fetch test. Add a small local HTTP mock and a cancellation test when convenient. The existing regtest and resolver tests cover the production outcome and bounds, so this is a test-coverage improvement only.

No Blocker, Major, or Minor findings. The diff stays within DEC-265/D80/D81, including the regtest RPC seam, cache migration, shared collateral check, and the narrowly related per-key-cap recomputation. No unrelated scope creep was found.

## Validation

- `CARGO_TARGET_DIR=/work/scratch/dw-bl229-r1-sol/target cargo test -p dw-engine governance::collateral --all-features` — 10 passed.
- `CARGO_TARGET_DIR=/work/scratch/dw-bl229-r1-sol/target cargo test -p dw-governance --lib` — 195 passed.
- `CARGO_TARGET_DIR=/work/scratch/dw-bl229-r1-sol/target cargo test -p dw-appdb migration_tests --lib` — 38 passed.
- `CARGO_TARGET_DIR=/work/scratch/dw-bl229-r1-sol/target cargo test -p dw-protx --test no_operator_signing` — 5 passed.
- `cargo fmt --all -- --check` — passed.

ACCEPT WITH NITS
