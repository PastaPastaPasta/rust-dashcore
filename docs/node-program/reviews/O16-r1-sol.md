# O16 checkpoint 1 — round-1 review

Range: `node/o16-testkit` `f4046b5f5..cf5db2f48` (10 commits, all valid signatures). Detached checkout: `/work/scratch/o16-r1-sol/src`. No pushes or live-oracle calls.

## Findings

### Major — `extra_args` can daemonize a node and leave an unowned process

- **Location:** `dash-testkit/src/rust_node.rs:115`, `dash-testkit/src/rust_node.rs:260-275`; also `dash-testkit/src/network.rs:70-77` for the builder's exposed argument path.
- **Scenario:** `RustNodeConfig::with_args(["-daemon=1"])` or `MixedNetworkBuilder::rdashd_args(["-daemon=1"])` appends the option without validation. Today's rdashd forks: in a scratch probe the launcher exited 0 while a distinct live rdashd PID remained in the test datadir. `RustNode` owns only the launcher `Child`, so its `Drop` and `stop()` cannot stop that daemon. The equivalent `dashd_args` route appends after dash-spv's `-daemon=0`, so it also needs protection. The doc comment says callers must not daemonize, but the public API does not enforce it.
- **Fix:** Reject `-daemon` and other process-owning overrides in both builders before spawn, or force a final `-daemon=0` after all caller arguments. Add a regression test that verifies the spawned PID is the process stopped on this input.

### Major — a dead child can satisfy readiness from its own final log line

- **Location:** `dash-testkit/src/rust_node.rs:277-285`.
- **Scenario:** The polling loop reads `Done loading` before checking `try_wait`. A child that writes that line and exits before the next poll is returned as `Ok(RustNode)`. A controlled scratch binary wrote that line and exited 1; `RustNode::start` returned `Ok`, and the existing lifecycle test advanced to reading the nonexistent pid file. The log offset excludes previous bytes but does not prove the child is still alive. Concurrent starts sharing a datadir can also see the other process's new line after their respective offsets.
- **Fix:** Check/reap child exit before accepting the log, then verify the pid file identifies this child (or use a process-specific readiness channel). Add tests for ready-then-exit and simultaneous contenders for one datadir.

### Major — the `rdashd` fallback accepts a stale binary

- **Location:** `dash-spv/src/test_utils/node.rs:95-106`.
- **Scenario:** `NodeImpl::Rdashd.binary()` accepts any existing `target/<profile>/rdashd` beside the test executable. `cargo test -p dash-testkit` and `cargo test -p dash-spv` do not build rdashd. After changing rdashd source while retaining an older binary in `target`, the switch silently tests the older executable and can report a false pass or an obsolete refusal. `RDASHD_PATH` bypasses the fallback, but the documented no-variable path is vulnerable. CI's `node-runtime` order builds rdashd first; standalone/local use does not.
- **Fix:** Require a deliberately selected rdashd artifact for these suites, or validate the fallback against a build revision and fail closed when it is stale. Keep CI's explicit build before testkit.

### Major — pre-existing RustNode datadirs do not become 0700

- **Location:** `dash-testkit/src/dir.rs:49-56`, called from `dash-testkit/src/rust_node.rs:244-247`.
- **Scenario:** `DirBuilderExt::mode(0o700)` applies only when a directory is created. `RustNodeConfig::new(existing_path)` is public and supports restarts; if the caller provides an existing 0755 directory, `create_private_dir` succeeds and leaves it 0755. A scratch Rust probe using the same builder pattern confirmed the resulting mode stayed 0755. The current test covers only a freshly made `TestDir` subdirectory.
- **Fix:** Set permissions on the resulting datadir and HOME after `create_dir_all` on Unix, then test an existing 0755 directory.

### Minor — the refused-options matrix omits a masternode blocker

- **Location:** `dash-testkit/README.md:57-69`.
- **Scenario:** Peeling the actual argument set against the built rdashd on an empty regtest directory reached `-txindex=1 is not supported by rdashd yet (IX1)` for masternode nodes after the P2b/P4 refusals. The table lists `-blockfilterindex=1` as IX1 but omits `-txindex=1`; it also groups rows rather than preserving the literal refusal order it claims. This can make the masternode readiness matrix look complete too early.
- **Fix:** Add `-txindex=1` for masternode nodes and either present the exact order or remove the order claim.

### Minor — handled startup errors are not retained

- **Location:** `dash-testkit/src/network.rs:172-180`, `dash-testkit/src/dir.rs:43-46`; for the switched dash-spv suites, `dash-spv/src/test_utils/context.rs:53-66`.
- **Scenario:** A `MixedNetwork::start()` refusal keeps `TestDir` in the error, but retention occurs only when the error is dropped during a panic (or `DASHD_TEST_RETAIN_ALWAYS=1`). A test returning the error with `?` fails without panicking, so the directory is removed. In the switched dash-spv suites, startup panics before `DashdTestContext` exists; all 54 refused starts produced no files under `DASHD_TEST_RETAIN_DIR` in this run. This narrows the claimed failed-log behavior.
- **Fix:** Retain failed startup directories explicitly before returning/propagating a startup error, independent of thread panic state.

## Verified claims and test evidence

- `dash-testkit` is a new test-only workspace crate and re-exports dash-spv's existing test-utils; no code moved out of dash-spv. The default dashd timeout remains 30 seconds; MixedNetwork sets 300 seconds for its own dashd nodes. Under `DASHD_IMPL=rdashd`, dash-spv changes only binary selection and passes its existing argument vector unchanged.
- Fresh testkit datadirs are 0700, and HOME/XDG point inside the node directory. The per-run log offsets exclude prior runs. `-listen=0 -server=0` are present by default. `stop()` sends SIGTERM to its held child PID; `Drop` kills and waits on that same child. No scratch rdashd process remained after the runs.
- The new CI group includes dash-testkit after rdashd, sets up dashd for `node-runtime`, retains/uploads failed logs, and uses a separate node-runtime cache key for the dashd/test-data cache. `ci_config.py verify-groups` passed. The changed sources contain no benchmark or PF0 workload edits; dash-spv's test-utils source hash changes through its self dev-dependency.
- `cargo build -p rdashd --all-features`: passed. `cargo test -p dash-testkit --all-features -- --test-threads=4`: 12 passed, 3 ignored, 0 failed. `cargo fmt --all -- --check`: passed.
- Default dashd suites: dash-spv `dashd_masternode` 10/10 and `dashd_sync` 37/37; dash-spv-ffi `dashd_sync` 7/7. All passed with `TMPDIR` and `DASHD_TEST_RETAIN_DIR` under this scratch directory.
- Switched rdashd suites: 0/10, 0/37 and 0/7 passed, respectively; all 54 failed on the Core datadir refusal. Each suite was run separately so the first failing test binary did not prevent the others from running.
- Against today's built rdashd, refusal peeling confirmed IX1, P2b, P4, AM1, WL2a, SP0 and RP0 owners, plus the omitted masternode `-txindex=1` refusal. The probe did not start any live oracle.

CHANGES-REQUIRED
