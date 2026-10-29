# O12a mixed-network harness, mode A plus observer — review r3

Reviewed:

- rust-dashcore `ac41eae46..e1b20993f` (`node/o12a-mixed-a`), with focused comparison of the r2 delta `54d2d127d..e1b20993f`;
- Dash Core `e74d176d922^..5cf1b61285d` (`node/o12a-mixed-a`), with focused comparison of the r2 delta `6d1ec1f4dbc..5cf1b61285d`;
- DEC-451 and DEC-457, and O12a D1–D13 in `docs/node-program/designs/O12a-notes.md`;
- the round 2 review and both committed mode-A passlists.

Validation run:

- `TMPDIR=/work/scratch/o12a-sol-r3 python3 contrib/node/func-compat/func-compat.py selftest` — 80 tests passed;
- `PYTHONPATH=/work/scratch/o12a-sol-r3/pylib TMPDIR=/work/scratch/o12a-sol-r3 python3 -m unittest test_framework.rust_nodes` from the Core functional-test directory — 15 tests passed (the temporary `dash_hash` dependency was installed under the scratch directory);
- `git diff --check` was clean for `54d2d127d..e1b20993f`, `6d1ec1f4dbc..5cf1b61285d`, and the full stack ranges `ac41eae46..e1b20993f` and `e74d176d922^..5cf1b61285d`;
- static audit of the 35 DTF scripts found the only `self.nodes` loops that build links are `feature_llmq_chainlocks.py:42` and `feature_llmq_connections.py:216,221`; no `self.nodes` loop disconnects or restarts nodes. The `feature_mnehf.py:43` restart loop is bounded by `num_nodes`, and its node-0 restart is covered by the observer hook.

## Findings

None.

## Checks with no finding

- The nightly and `mixed-a` use the checked-in `MIXED_A_CORE_BRANCH`, export the O12a branch into `v24.0-wallet-node_o12a-mixed-a`, record `branch` and `commit` in the run report, and reject a wrong branch or missing hook before producing a result. The nightly removes an old result before each variant and maps a missing result or exit above 1 to exit 2. The export test exercises both the O12a and pin branches.
- The rerun merge now holds every no-rdashd rerun unless it reproduces the committed `NO_RDASHD` entry. `MixedARun.test_a_skip_rerun_against_a_committed_pass_is_unconfirmed` covers the r2 finding-4 case and leaves the first entry with `unconfirmed` metadata.
- The crash classifier treats any qualified `http.client.*` exception as transport and flags it, while the fixed exception table deliberately includes `http.client.LineTooLong` outside `TRANSPORT`. The shutdown-only `CannotSendRequest` exception remains classed without a crash flag.
- All adaptation tripwire starts, including the datadir-lock probe, pass `-listen=0`; the fake daemon refuses normal starts without it. The focused tripwire and full selftest suites pass.
- The observer implementation keeps the node in `self.nodes`, filters observer links to node 0, makes node 0 the dialer, starts the observer and late nodes through the bulk-start path with their node-owned arguments, and calls `force_finish_mnsync` on every rejoin. Process exit while `running` is true raises an assertion instead of restarting; `-connect=0` skips the automatic rejoin until an explicit reconnect.
- The fake-node tests are mutation-sensitive for the observer fixes: they fail on an observer link to another node or reverse dialing, on bulk-start indexing or duplicate observer starts, on missing late-node argument handling, on automatic rejoin during `-connect=0`, on restarting an exited observer, and on missing mnsync calls. The real-node observer paths remain setup-blocked at `getnetworkinfo`, as documented.

## Verdict

**ACCEPT**
