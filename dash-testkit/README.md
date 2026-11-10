# dash-testkit

A regtest harness for running `rdashd` beside Dash Core's `dashd` (PLAN O16; PRD US-DEV-09,
REL-5.7). It is test-only (layer T), so use it only as a dev-dependency.

It wraps dash-spv's `test-utils` without moving them. `DashCoreNode`, `DashdTestContext` and the
`contrib/setup-dashd.py` environment stay in dash-spv and are re-exported here. On top of them it
provides:

| Item | What it does |
|---|---|
| `RustNode` | Runs `rdashd` in an owner-only (0700) data directory with its own `HOME`, and takes optional mock time, P2P, RPC and extra arguments. It is started once the process is still running, this run logged `init message: Done loading`, and the pid file names it; this run's log must also hold `rdashd version`, so another binary at `RDASHD_PATH` (Dash Core, say) fails with `StartError::NotRdashd`. An unsupported option comes back as `StartError::Refused { option, owner }`. Arguments that daemonize (`-daemon`, `-daemonwait`), set what the harness owns (the chain, `-datadir`, `-debuglogfile`, `-pid`, `-shrinkdebugfile`, `-conf`, `-includeconf`) or what it reports (`-mocktime`, `-listen`, `-port`, `-bind`, `-connect`, `-server`, `-rpcport`, `-rpcbind`, and `-rpcuser`, `-rpcpassword`, `-rpcauth`, which would remove the cookie) are refused before anything starts. It always passes `-daemon=0 -daemonwait=0`, which override a `dash.conf` in an existing data directory (other conf settings still apply, so keep option-bearing confs out of test datadirs), and `-shrinkdebugfile=0`, so a restart on a debug.log over 11 MB still finds its own lines. `stop()` sends SIGTERM to the spawned process and returns its pid, exit status and this run's debug.log; it is cancel-safe, so a `timeout` around it still leaves the node to be killed. Dropping a node kills it, and on Linux the node also dies with the thread that started it (`PR_SET_PDEATHSIG`): start nodes on a thread that outlives them, not in `spawn_blocking` or `block_in_place`. P2P and RPC stay off (`-listen=0 -server=0`) unless configured. |
| `MixedNetwork` | Starts N `dashd` and M `rdashd` regtest nodes on one mock clock. Ports come from dash-spv's shared per-process allocator; rdashd's are taken after the dashd nodes are up. For edges, a dashd uses `addnode onetry` and an rdashd gets `-connect`/`-listen`. `start` returns once every edge is a peer: each dashd edge's peer has sent its `version`, and each dashd has as many handshaken inbound peers as edges lead into it. dashd arguments are held to the same rules. `advance_mocktime` tries every node and moves the network's clock even if some fail; the error names the nodes left behind. |
| `MockTime` | Gives every node the same `-mocktime` at start, then `setmocktime` on `advance`. |
| `NodeRpc` | A one-method JSON-RPC trait. `dashcore_rpc::Client` implements it, which serves dashd today and rdashd from RP0. |
| `NodeImpl` | The `DASHD_IMPL` switch, which lives in dash-spv. |
| `TestDir` | A 0700 temporary directory, retained like dash-spv's datadirs. |

## Environment

| Variable | Effect |
|---|---|
| `DASHD_PATH`, `DASHD_TEST_DATA`, `DASHD_MN_DATADIR` | From `eval $(python3 contrib/setup-dashd.py)`. `MixedNetwork` takes its dashd from `DASHD_PATH`. |
| `RDASHD_PATH` | The rdashd binary, required by `RustNode` and by `DASHD_IMPL=rdashd`, which check from its debug.log that it is rdashd. There is no fallback, so a stale build is never tested: run `cargo build -p rdashd` and point `RDASHD_PATH` at a copy of `target/debug/rdashd` (`cargo test --workspace` rebuilds that file with test features). CI's node-runtime group does this. |
| `DASHD_IMPL=rdashd` | dash-spv's `DashdTestContext` and `MasternodeTestContext` start rdashd in place of dashd, with the same arguments. They check that the node is rdashd, and a missing RPC miner fails the test rather than skipping it. |
| `DASHD_TEST_RETAIN_DIR`, `DASHD_TEST_RETAIN_ALWAYS` | Copy a failed test's directory (or every test's) to `<dir>/<test name>/<label>`. A failed setup is retained too: a dash-spv context whose setup panics before the context exists (in a node's start, say), and an rdashd that fails a `MixedNetwork` start (the error says where). |
| `DASHD_TEST_LOG` | rdashd also prints its log to the test's console (`-printtoconsole=1`). |
| `SKIP_DASHD_TESTS` | Skips the tests that start dashd. |

## What runs against today's rdashd

rdashd today is the process shell from ND1. It has init, the Core-format debug.log, the pid file, the data directory lock, SIGTERM shutdown and `-mocktime`.

| Test | State |
|---|---|
| `rust_node`: start/init log/pid, the stopped pid is the spawned one, daemon arguments and daemonizing confs, owner-only datadir (also when it existed as 0755), lock (in turn and two at once), ready-then-exit and a foreign pid file are not started, restart (also past 11 MB of debug.log), start mocktime, refusal with owner, Dash Core as rdashd refused, an abandoned stop still kills, the node dies with its starting thread (Linux), clean shutdown | runs |
| `retain`: a panicking test's node, a refused rdashd's network (no panic) and a switched dash-spv start that panics, under `DASHD_TEST_RETAIN_DIR` | runs (needs `DASHD_TEST_DATA`) |
| `mixed_network`: 2 dashd (connected, handshaken at both ends) + 1 rdashd process on one mock clock; unit test: an accepted connection without a handshake is not an edge | runs |
| `wrong_binary`: Dash Core at `RDASHD_PATH` under `DASHD_IMPL=rdashd` is refused by dash-spv's harness | runs (needs `DASHD_TEST_DATA`) |
| `rust_node::serves_rpc_and_follows_setmocktime` | ignored: needs RP0 |
| `mixed_network::dashd_and_rdashd_connect_both_ways` | ignored: needs P2b and P2a-net |
| `mixed_network::advance_mocktime_moves_every_node` | ignored: needs RP0, plus P2a-net or P2b |

## The dashd suites as rdashd (`DASHD_IMPL=rdashd`)

This was measured at f4046b5f5 plus this branch, on 2026-10-13. Run it with
`DASHD_IMPL=rdashd cargo test -p <crate> --test <suite> -- --test-threads=4`.

| Suite | Tests | Result | First wall |
|---|---|---|---|
| dash-spv `dashd_sync` | 37 | 0 pass | Core data directory refused |
| dash-spv `dashd_masternode` | 10 | 0 pass | Core data directory refused |
| dash-spv-ffi `dashd_sync` | 7 | 0 pass | Core data directory refused |

Every suite starts its node on a prebuilt Dash Core regtest data directory (`DASHD_TEST_DATA`,
`DASHD_MN_DATADIR`). rdashd refuses to write into a Core data directory; importing one is CH5
(`-importcoredatadir`). The next walls are rdashd's refusals of the harness's own arguments,
found by removing them one at a time on an empty data directory. Each is listed with its owning
row:

| Option | Owner | Suites |
|---|---|---|
| `-blockfilterindex=1` (with `-peerblockfilters`) | IX1 | all |
| `-txindex=1` | IX1 | masternode (Core requires it with `-masternodeblsprivkey`) |
| `-bind`, `-listen=1`, `-port` | P2b | all |
| `-peerbloomfilters`, `-whitelist` | P4 | all |
| `-masternodeblsprivkey` | AM1 | masternode |
| `-wallet`, `-fallbackfee` | WL2a | all (dashd's wallet funds and mines the tests) |
| `-sporkkey` | SP0 | masternode |
| `-server`, `-rpcport`, `-rpcbind`, `-rpcallowip` | RP0 | all |

Even with all of those in place, the suites drive dashd's wallet RPCs (`sendtoaddress`,
`generatetoaddress` and others). Before rdashd can stand in, they need either rdashd's wallet
(WL2a) or a fixture rebuilt for a wallet-less node. Running the suites as rdashd `-spv`, where the
SPV client is rdashd itself, comes with SB18 (DEC-81).
