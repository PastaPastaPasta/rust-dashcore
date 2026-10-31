# O16 round 2 — Sol narrow review

Range `cf5db2f48..a991a3157` (two commits, both signatures valid). Reviewed detached `a991a3157`; the implementation branch was not edited. The implementer's full gate reports 4,791 passed, 0 failed; I ran the targeted checks below independently.

## Round-1 findings

| Round-1 finding | Result | Evidence |
|---|---|---|
| CLI arguments can daemonize the node | **CLOSED for the reported builder path** | `dash-testkit/src/rust_node.rs:106-110,146-172,316-317` rejects `-daemon` and `-daemonwait`, including `--` and `-no` forms, in `with_args` and again before spawn for direct edits to public `extra_args`. `dash-testkit/src/network.rs:75-86` applies the check to both builders. `dash-testkit/tests/rust_node.rs:48-82` checks process identity and refusal. A separate config-file route remains open below. |
| A dead child can satisfy readiness | **CLOSED** | `dash-testkit/src/rust_node.rs:330-362` takes this run's log offset, requires the pid file to match the held child, and calls `try_wait` before returning ready. `dash-testkit/tests/rust_node.rs:102-167` covers ready-then-exit and two concurrent contenders. A child may exit immediately after the check, as with any process readiness observation. |
| Stale fallback rdashd binary | **CLOSED** | `dash-spv/src/test_utils/node.rs:95-112` requires `RDASHD_PATH` and gives a build command; no target-directory fallback. `.github/workflows/build-and-test.yml:91-98` builds the checkout's rdashd and exports its path before tests. |
| Existing datadirs remain 0755 | **CLOSED** | `dash-testkit/src/dir.rs:54-64` sets permissions after creation, including on an existing directory; `dash-testkit/src/rust_node.rs:318-320` applies it to datadir and HOME. `dash-testkit/tests/rust_node.rs:85-99` starts with both at 0755 and asserts 0700. |
| Masternode matrix omits txindex | **CLOSED** | `dash-spv/src/test_utils/masternode_network.rs:176-178` passes `-txindex=1`; `dash-testkit/README.md:54-69` lists it under IX1 and no longer claims literal refusal order. |
| Failed setup loses retained logs | **CLOSED** | `dash-spv/src/test_utils/fs_helpers.rs:39-88` provides unconditional retention and a panic guard; `dash-spv/src/test_utils/context.rs:65-85` and `masternode_network.rs:133-149,181-190,267` arm it through setup. `dash-testkit/src/network.rs:180-189` retains on a returned rdashd error. `dash-testkit/tests/retain.rs:30-101` exercises panic, returned refusal, and switched dash-spv startup panic. Reverse drop order retains the datadir before its `TempDir` deletes it; retention I/O failure is reported without a second panic. |

## New findings

### Major — an existing `dash.conf` can still daemonize `RustNode`

`RustNodeConfig::with_binary` accepts an existing datadir (`dash-testkit/src/rust_node.rs:72-80`), and `RustNode::start` passes no `-daemon=0` or `-daemonwait=0` in its generated arguments (`rust_node.rs:114-140`). The refusal only scans `extra_args` (`rust_node.rs:316-317`). rdashd reads `<datadir>/dash.conf`; it creates an empty one if absent (`dash-args/src/args.rs:1346-1370`), but a caller or fixture can populate an existing one. The README warns against this (`dash-testkit/README.md:12`), yet the public start path still loses ownership of the process.

Using the binary built from this checkout and the same core arguments as `RustNode`, a datadir with `daemon=1` made the launcher return 0 while a distinct PID from `regtest/dashd.pid` remained alive. `-daemon=0` prevented that case, but a config with `daemonwait=1` still forked; only **both** `-daemon=0` and `-daemonwait=0` kept the spawned PID equal to the pid-file PID. All probe processes were stopped. Add both explicit false command-line options, or reject daemon settings in the effective config before spawn, then test an existing datadir containing each setting. This is the same unowned-process consequence as round 1 through a different input path.

### Minor — Windows option syntax bypasses `sets_option`

On Windows Core lowercases option names and converts `/` to `-` before parsing (`~/workspace/dash/src/util/system.cpp:256-268`). `dash-testkit/src/rust_node.rs:148-154` accepts only `-`/`--` and retains case. Thus `/datadir=...`, `/daemon=1`, or `-Daemon=1` can bypass the builder guard on Windows even though Core reads them as owned options. The Unix integration tests do not run on Windows, so this is source-level evidence, not a runtime reproduction. Normalize these forms under `cfg(windows)` or document and enforce a platform-specific restriction.

## Requested focused checks

- **Argument semantics:** Unix `-nodaemon`, `--daemon=1`, and `-daemon=0` are all refused; the latter is conservative. Core's `InterpretKey` strips `no`, consistent with this check (`~/workspace/dash/src/util/system.cpp:129-145`). Neither testkit builder writes an option-bearing conf file. rdashd itself creates an empty default `dash.conf`, and caller-written content is the blocker above.
- **Readiness:** `log_start` excludes previous bytes (`rust_node.rs:330-354`); pid-file equality binds the line to the held child in ordinary runs. A held, unreaped child's PID cannot be reused. A prior log line alone does not satisfy readiness, even if the OS later reuses a PID. External log rewriting remains outside this test harness's guarantees.
- **Retention:** The guard is armed before node start and disarmed after setup, with the `TempDir` declared earlier. Its `Drop` runs during unwinding and handles copy errors without panicking (`fs_helpers.rs:42-88`). It does not retain failures before it is armed, such as the initial fixture copy or wallet JSON read; those precede node startup and its logs.
- **CI:** `.github/workflows/build-and-test.yml:93-98` appends `RDASHD_PATH` to `$GITHUB_ENV` for later steps and adds `.exe` when `runner.os == 'Windows'`. The `node-runtime` group is included in dashd setup and cache steps (`:56-89`). `ci_config.py verify-groups` passed. Windows execution was not available locally.

## Local validation

- `cargo build -p rdashd --all-features` in a private target: passed.
- `cargo test -p dash-testkit --all-features -- --test-threads=4`: **18 passed, 3 ignored, 0 failed**. The rust_node suite was rerun after restoring all mutations: 11 passed, 1 ignored.
- `cargo fmt --all -- --check`, `python3 .github/scripts/ci_config.py verify-groups`, and `git diff --check`: passed.
- Three temporary mutants in the detached checkout were each killed by their targeted test: readiness returned before `try_wait` → `a_node_that_logs_ready_and_exits_did_not_start` failed; removed start-time `check_args` → `a_daemon_argument_is_refused_before_anything_starts` failed; removed `set_permissions` → `its_data_directory_is_owner_only_even_if_it_existed` failed. Original files were restored; the checkout is clean.

CHANGES-REQUIRED
