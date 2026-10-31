# O12a mixed-network harness, mode A plus observer — review r2

Reviewed the complete O12a stack at:

- rust-dashcore `ac41eae46..54d2d127d` (`node/o12a-mixed-a`);
- Dash Core `e74d176d922^..6d1ec1f4dbc` (`node/o12a-mixed-a`);
- DEC-451/DEC-457 and O12a D1–D12 in `DECISIONS.md` and `O12a-notes.md`;
- the committed `mixed-a.json` and `mixed-a-obs.json` passlists.

Validation run:

- `TMPDIR=/work/scratch/o12a-sol-r2 python3 contrib/node/func-compat/func-compat.py selftest` — 72 tests passed;
- `TMPDIR=/work/scratch/o12a-sol-r2 python3 -m unittest test_framework.rust_nodes` — 9 tests passed;
- `git diff --check` was clean for both branch ranges.

## Findings

### 1. Blocker — the nightly does not export the O12a Core branch

Evidence: `contrib/node/func-compat/func-compat.py:293-298,848,979-980`; `contrib/node/func-compat/mixed-a-nightly.sh:60-64`.

`mixed_a()` passes `args.branch` to `build()`, but `build()` defaults a missing branch to `PINS["v24.0"].branch`, `node/func-compat-v24.0`. The nightly command never supplies `--branch`. At the reviewed refs, `node/func-compat-v24.0` has the O11 index-mask code, while the O12a-only `non-mn`, `RDASHD_OBSERVER`, observer setup/rejoin, and `RPC_FAILURE` code are on `node/o12a-mixed-a`. The nightly therefore exports a tree that cannot run this mode: `RDASHD_NODES=non-mn` is rejected by the old parser, and `RDASHD_OBSERVER=1` is ignored. The resulting test failures have no rdashd starts and can be emitted as ordinary `skip` entries, so the wrapper can return 1 with a result instead of identifying a broken O12a run.

Suggested fix: make the nightly pass an explicit O12a Core branch (for example `--branch node/o12a-mixed-a`, preferably through a checked-in `MIXED_A_CORE_BRANCH` setting), and record/verify that branch in the run metadata. Add an orchestration test that asserts the nightly command exports a tree containing the role-mask and observer hooks.

### 2. Major — observer topology is changed by an existing DTF test loop

Evidence: Core `test/functional/test_framework/test_framework.py:1959-1968`; `test/functional/feature_llmq_chainlocks.py:38-43`.

`add_rust_observer()` appends the observer to `self.nodes`, and its setup contract says node 0 is the observer's only peer. `feature_llmq_chainlocks.py` then runs `for i in range(2, len(self.nodes)): self.connect_nodes(i, 1)`. With four masternodes, the observer is the newly appended index 5, so this loop explicitly connects observer 5 to masternode 1. When node 0 is later isolated, the supposed observer still has a masternode peer and can receive blocks/sporks through that path. This invalidates the observer variant's claim that it observes node 0 alone and can hide a real rdashd divergence behind the remaining Core network. The current passlist stops this test during setup on `-spentindex`/`getnetworkinfo`, but the topology failure is reached as those known blockers land.

Suggested fix: keep the observer out of generic topology-building loops (provide a framework-level regular-node view or make `connect_nodes` ignore observer links whose endpoints are not node 0), and add an observer-mode regression that isolates node 0 and asserts the observer has no other peer. Audit all `self.nodes` loops that create or tear down links, not only the adapted two-node unpacking.

### 3. Major — observer rejoin is not covered for the framework's bulk-start API and can use stale process state

Evidence: Core `test/functional/test_framework/test_framework.py:681-695,1970-1991`.

The observer is appended to `self.nodes` without increasing `self.num_nodes` (`1959-1964`). The inherited `start_nodes()` creates `extra_args = [None] * self.num_nodes`, then indexes that list while iterating every `self.nodes` entry (`681-690`); after an observer exists, any caller using this public restart helper gets an `IndexError` at the observer index. There is also no rejoin hook on `start_nodes()`: only `start_node(0)` and `reconnect_isolated_node(a, b)` call `rejoin_rust_observer` (`1983-1991`). A bulk restart therefore cannot reconnect and finish observer mnsync. Finally, `rejoin_rust_observer()` trusts `observer.running`; `TestNode.running` is only cleared by `is_node_stopped()` (`test_node.py:440-457`), so an rdashd process that exits unexpectedly can leave the flag true and skip the restart, after which base `connect_nodes` talks to a dead observer.

Suggested fix: make the observer participate safely in bulk starts (or override `start_nodes` to supply an observer argument, restart it, force `mnsync`, and call the base `connect_nodes`), and detect `process is None`/`process.poll() is not None` rather than only the stale boolean. Add focused tests for `stop_nodes(); start_nodes()`, an observer process exit, and node-0 restart/reconnect with mnsync completion.

### 4. Minor — a zero-start confirming rerun can be reported as `confirmed` instead of `unconfirmed`

Evidence: `contrib/node/func-compat/func-compat.py:812-820,823-835`.

DEC-457 says a zero-start confirming rerun is `unconfirmed` unless it reproduces the committed `{"status":"skip", "detail":"no rdashd node"}` entry, which is `flaky`. `merge_rerun()` excludes a test from `held` when its first entry is already `NO_RDASHD` (`818-819`). Thus, with a committed `pass`, first run `NO_RDASHD`, and confirming rerun `NO_RDASHD`, the rerun is accepted and `confirm()` labels it `confirmed`; it should remain the first entry with outcome `unconfirmed`. The passlist status stays a skip, but the BL-392 outcome is wrong and can mislead triage.

Suggested fix: hold every zero-start rerun unless `rerun[name] == committed.get(name) == NO_RDASHD`, regardless of the first entry, and add this exact committed-pass/first-skip/rerun-skip case to `MixedARun`.

### 5. Nit — one new crash test is tautological

Evidence: `contrib/node/func-compat/func-compat.py:630-651`; `contrib/node/func-compat/test_func_compat.py:554-556`.

`CRASH` is constructed by joining the `TRANSPORT` set, and the test iterates that same set and asserts that the resulting regex matches each member. This cannot detect a missing transport alternative or a regression in the intended log behavior; it only checks the implementation against its own inputs.

Suggested fix: use a fixed, independently chosen list of representative exception lines and assert the complete `possible_crashes`/classification behavior, including a transport name intentionally absent from the implementation set.

## Checks with no finding

- The r1 fixes are present: test-origin `KeyError`/`TypeError`/`ValueError`/`IndexError` cases classify as divergence; timeout and confirming reruns retain first entries when the rerun starts no rdashd; result publication is last and atomic; and the nightly requires both a status in 0/1 and a nonempty result file.
- `RustNodes.rpc_failure()` maps the traceback's AuthServiceProxy port to `rpc_port(node.index)`, checks the node's rdashd binary, and waits up to the specified default 5 seconds. A dashd target returns no `RPC_FAILURE` line. The focused Core test covers exited/running rdashd and a dashd mapping.
- The observer setup connects node 0 to the observer, then calls `force_finish_mnsync`, `sync_blocks`, and `wait_for_sporks_same`; rejoin uses `BitcoinTestFramework.connect_nodes` rather than the DTF override. The peer-count and `Error: no RPC connection` observer rules are covered by fixed parser tests.
- Both passlists are canonical `sort_keys=True` JSON with stable test ordering/details. The mode-A port base and Core port formula keep p2p/RPC/Tor ports below 32768 for the runner's portseed range. Per-run datadirs are fresh and removed in `finally`, crash evidence is moved to mode-700 directories, and no pattern-based process kill is used.

## Verdict

**CHANGES REQUIRED**
