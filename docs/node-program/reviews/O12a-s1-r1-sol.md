# O12a slice 1, mixed-network harness mode A — review r1

Reviewed:

- rust-dashcore `ac41eae46..13c3bfb2e` (`node/o12a-mixed-a`), including `bb616d0f2`,
  `7f917910d`, and `13c3bfb2e`;
- Dash Core `e74d176d922` (`node/o12a-mixed-a`), the role-mask hook.

Validation performed:

- `python3 contrib/node/func-compat/func-compat.py selftest` — 55 tests passed;
- Core `python3 -m unittest test_framework.rust_nodes` — 8 tests passed;
- Python compilation and `bash -n` checks passed;
- static audit of the pinned Core tree found 35 direct `DashTestFramework` scripts / 39 runner
  names, with no DTF script-level `binary=` or `versions=` override.

## Findings

### 1. Major — a confirming timeout rerun can turn an rdashd result into `skip`

Evidence: `contrib/node/func-compat/func-compat.py:804-811`.

The first pass classifies a timeout only after an rdashd start. The timeout rerun clears the start
log, runs the test alone, and then blindly replaces the first entry with the rerun entry. If the
rerun fails before `TestNode.start` records an rdashd start, `mixed_a_entries` returns
`{"status": "skip", "detail": "no rdashd node"}` and this replaces the original timeout. A
transient rdashd startup failure can therefore make a test whose rdashd sync timed out appear to
have had no rdashd node. This violates the no-real-divergence-as-skip rule and is not covered by
the selftests.

Suggested fix: have `one_pass` return the per-test start counts (or a started bit) and only replace a
timeout with a rerun result if the rerun started rdashd. If the rerun has no start, retain the
original failure and record the rerun as an unconfirmed/harness outcome.

### 2. Major — direct RPC response-shape mismatches are classified as `harness`

Evidence: `contrib/node/func-compat/func-compat.py:613-615, 669-687`.

The classifier recognizes the framework's `Key error` handler, but it labels only
`AssertionError` and `JSONRPCException` as `divergence`; every other test exception falls through
to `harness`. A test that calls an rdashd RPC and gets a missing field (`KeyError`), wrong type
(`TypeError`), or malformed value (`ValueError`) is a real response divergence, yet the passlist
records it as a harness failure. The same problem applies to other test-origin exceptions that
indicate an unexpected RPC result.

Suggested fix: distinguish test-script exceptions from lifecycle/process failures and classify
unexpected test-origin response exceptions as `divergence` (while retaining `harness` for failures
without a test frame or for runner/process infrastructure errors). Add regression cases for
`KeyError`, `TypeError`, and malformed RPC values.

### 3. Minor — nightly can report a broken harness as a normal diff

Evidence: `contrib/node/func-compat/func-compat.py:813-834` and
`contrib/node/func-compat/mixed-a-nightly.sh:42-56`.

`mixed-a` writes `mixed-a-v24.0.json` before writing run metadata, printing the diff, and returning.
The wrapper maps any exit 0/1 with a nonempty result file to 0/1. If the harness is killed or
raises after the result write but before the final diff/return (for example while writing metadata
or due to an output/filesystem failure), the wrapper returns 1 (“differs”) instead of 2 (“broken”).
That violates the documented exit-code contract and can make a partial/broken run look actionable.

Suggested fix: write the regenerated result to a temporary file and atomically publish it only
after all classification, metadata, and diff work succeeds; or publish an explicit completion
marker and have the wrapper require it in addition to the result file.

### 4. Minor — orchestration and nightly status paths are not self-tested

Evidence: `contrib/node/func-compat/test_func_compat.py:337-487`.

The selftests exercise the pure traceback parser, all named failure classes, start-count handling,
crash-datadir movement, and stable diff rendering. They do not invoke `mixed_a`, `run_runner`, or
`mixed-a-nightly.sh`, so they do not cover the timeout-rerun replacement above, a started rdashd
that then fails before RPC readiness, missing-result handling, partial-run cleanup, or the 0/1/2
wrapper contract. The stated 55-test result therefore does not protect the orchestration paths
where the two classification bugs live.

Suggested fix: add fake-runner tests for (a) first-pass timeout plus rerun with zero starts, (b)
rdashd start followed by startup failure, (c) runner crash/missing summary, and (d) nightly exit
0, 1, and 2 with and without a completed result marker.

## Areas checked with no finding

- **Role mask:** `RustNodes.selects` leaves index 0 and nodes created inside the masternode
  context on dashd (`test/functional/test_framework/rust_nodes.py:161-180`). Core wraps both
  `start_masternodes` and `dynamically_add_masternode` (`test/functional/test_framework/test_framework.py:1641-1662, 1797-1804`), including evonodes. Existing-node restarts retain their selected `TestNode.binary` (`test_framework.py:707-710`), so node 0, static MNs, dynamic MNs, and restarted nodes do not switch binaries. The pinned DTF scripts contain no explicit `binary=`/`versions=` override.
- **Start accounting:** `record_start` is called immediately after `Popen` (`test_node.py:282-283`), so a process that starts and then dies before RPC readiness is not mistaken for “no rdashd node.” `mixed_a_entries` checks the start count before emitting `no rdashd node` (`func-compat.py:690-705`).
- **Determinism:** entries and diff lines are sorted; JSON is emitted with `sort_keys=True`; details are option/RPC names or stable source locations. The committed JSON is byte-identical to the same `json.dumps(..., indent=1, sort_keys=True)` rendering.
- **Ports/isolation:** the 2700 base plus Core's `p2p_port`/RPC/Tor offsets stays below 32768 for the runner's portseed range. Each pass uses a fresh `test_runner_*` subtree and removes it in `finally`; only crash evidence is retained. Cleanup uses process handles (`proc.kill()`/`node.kill()` in the adaptation probes), with no pattern-based process killing.

## Verdict

**CHANGES REQUIRED**

