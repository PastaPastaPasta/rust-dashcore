# RP1a-1a round-2 review (Sol)

Branch `node/rp1a-1`, head `569991d6f`, reviewed against Dash Core `v24.0.0-rc.4`.

## Verdict

**ACCEPT**

All round-1 findings are closed. No open findings.

## Round-1 finding status

- **M-1 — closed.** `dashd-chainstate/src/flush/control.rs:445-473` makes `copy_point` delegate to `copy_point_flush`; `copy_point_flush` executes the ALWAYS packet with `copy_point=true` and waits for release. `dashd-chainstate/src/flush/site.rs:300-310` uses it for `gettxoutsetinfo`, receives the scan epoch, and asserts it equals the released epoch. The new test at `dashd-chainstate/src/chain/tests/node_stores.rs:748-802` verifies the hook precedes the V answer, the snapshot is at the flushed tip, and a block flushed during the walk is excluded.
- **m-1 — closed.** `dashd-node/src/rpc.rs:472-480` maps `CoinsStatsError::Interrupted` to `TxOutSetError::Interrupted`; `dashd-rpc/src/blockchain.rs:512-518` maps that to Core's `-9`/`Shutting down`. The RPC unit test covers the reply.
- **m-2 — closed.** `dashd-chainstate/src/chain/tests/node_stores.rs:748-802` is dashd-free and exercises the real flush, snapshot acknowledgement, tip/epoch assertions, and mid-walk exclusion. The skip-flush mutant fails the hook/tip assertions.
- **m-3 — closed.** `dashd-chainstate/src/coins/stats_job.rs:86-91` drops both snapshot and DB handle before sending the result.
- **n-1 — closed.** `dashd-node/src/rpc.rs:397-407` supplies the null position unless `HAVE_DATA` is set; `dashd-node/src/p2p/chain.rs:879` asserts the complete `FlatFilePos(nFile=-1, nPos=0)` line.
- **n-2 — closed.** The header-only pruned-node case is covered at `dashd-node/src/chain/tests.rs:1179-1190` and returns `-1 Block not found on disk`; the `nTx > 0` mutant fails this test.
- **n-3 — closed/documented.** `docs/node-program/designs/RP1a-notes.md:52-59` records the maximum-mocktime Core nanosecond-overflow behavior as the DEC-618 allowed diff, with no closer.
- **n-4 — closed/documented.** `docs/node-program/designs/RP1a-notes.md:27-38` records the DEC-618 AbortNode/fatal-flush difference in D11 and states the rdashd behavior.

## Validation

- `cargo clippy -p dashd-rpc -p dashd-chainstate -p dashd-node --all-targets -- -D warnings` — passed.
- `cargo test -p dashd-rpc` — 102 passed.
- `cargo test -p dashd-chainstate --lib the_coins_scan_reads_a_copy_point_at_vs_tip` — passed.
- Copy-point control/machine tests and both getblock tests — passed.
- `cargo test -p rdashd --test rpc_diff` against the pinned rc.4 dashd — 10 passed.
- `cargo fmt --all -- --check` — passed.
- Mutants: plain flush instead of copy-point flush — killed; skipped flush — killed; removed `nTx > 0` pruned check — killed.
