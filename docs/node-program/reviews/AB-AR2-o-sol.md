# AB-AR2-o calibration review

**Verdict: CHANGES-REQUIRED**

Reviewed only `node/ab-ar2-o` at `47a95239db23c939452a8aee33e2a52f0a75f2f1`, against base `65ec9df2`, AB-AR2.md, PLAN §4 AR2, and Dash Core tag `v24.0.0-rc.3` (`da567d7dba945143eee4d2ce1621c21b32089989`). The implementer's worktree was untouched. Review and experiments used a detached worktree under `/work/scratch/ab-review-ar2-o/`.

The four commits are:

- `45f0efdd` — feat(dash-rpc-spec): add the generator that derives the RPC table from Dash Core
- `0a5833fa` — feat(dash-rpc-spec): add the v24 RPC table, help rendering and its tests
- `01144da5` — ci: run dash-rpc-spec in node-app with dashd v24; keep typos off its Core text
- `47a95239` — refactor(dash-rpc-spec): address review of the generator and tests

## Findings

### 1. Major — regeneration assumes a Linux host, contrary to DEC-84

**Locations:** `dash-rpc-spec/generator/build-core.sh:22`; also `build-core.sh:12`, `build-dump.sh:29`, and `regenerate.sh:31`.

The documented source-build step selects `depends/*-linux-gnu*/share/config.site`. Core's native depends build on macOS produces an Apple/Darwin host directory, so this glob cannot find its configuration. With `set -euo pipefail`, the failed `ls` aborts the step. This is an explicit Linux-only assumption in the supplied regeneration workflow, despite AB-AR2's DEC-84 requirement.

Even when a developer supplies an already-built Core tree, the remaining scripts assume GNU `realpath -m`, `sha256sum`, and Bash namerefs (`local -n`, requiring Bash 4.3 or newer). They cannot run as documented with stock macOS tools and its Bash 3.2. The portability conclusion is from the concrete commands; macOS execution was unavailable on this VM. The Rust library itself contains no corresponding Linux dependency.

**Suggested fix:** choose the depends configuration using the actual host triplet, remove the Bash nameref requirement, and use portable path/hash handling (Python is already required). Validate the generator workflow on both Linux and macOS. Merely labeling the helper a “Linux host build” does not satisfy the task's portability instruction.

### 2. Minor — the live-test port race can select and shut down another test daemon

**Locations:** `dash-rpc-spec/tests/dashd_help.rs:69`, `:110`, and `:181`. The generator has the same endpoint-ownership problem in `dash-rpc-spec/generator/generate.py:76` and `:107`.

`free_port()` drops its listener before dashd binds, leaving a race. All instances share `rpcspec:rpcspec` credentials. Startup returns immediately on any successful `getblockcount`, before checking whether its child exited; cleanup sends `stop` to that endpoint without proving that it belongs to the child.

**Concrete reproduction:** start a separate, review-owned v24 regtest dashd on RPC port 41422, with `-listen=0`, `-rpcuser=rpcspec`, `-rpcpassword=rpcspec`, and no loaded wallet. Run the unmodified `help_matches_dashd` test with `DASH_RPC_SPEC_RPCPORT=41422`. In this experiment, the wallet phase completed its comparisons against the pre-existing daemon even though its own daemon could not bind. Cleanup stopped the pre-existing daemon (exit 0). The disabled-wallet phase then reached that same daemon during shutdown and failed its overview assertion because wallet commands were present. The test exited 101 after 61.50 seconds.

An occupied explicit port exercises the same collision path as an automatically selected port being taken between selection and binding. This compromises fixture isolation and can disrupt another concurrent run; it is a harness defect, not evidence of incorrect table contents.

**Suggested fix:** authenticate with a per-instance cookie or unique credentials, check child termination during startup, and never send cleanup RPCs without establishing endpoint ownership. Retry bind collisions with a fresh automatically selected port; fail clearly for an occupied explicit port. Apply the ownership check to generator capture too.

## Independent acceptance evidence

The table contents passed the checks performed:

- `cargo fmt --all --check`: PASS.
- `cargo clippy -p dash-rpc-spec --all-features --all-targets -- -D warnings`: PASS.
- `cargo test -p dash-rpc-spec --all-features -- --nocapture`: PASS, **7 table tests + 1 live test**, 0 failures. These ran with `TMPDIR=/dev/shm/ab-review-ar2-o` (mode 0700), `CARGO_BUILD_JOBS=6`, and RPC port 41420.
- Live test: **291 help outputs with a loaded wallet + 291 under `-disablewallet`**, byte-equal. This includes both overviews, unknown-command responses, and all 289 command requests in each mode. Both argument-map checks and the filtered client/server conversion comparison passed. There are 109 wallet-only commands.
- The checked-in `tests/data/client.cpp` is byte-identical to the tag's source: blob `260e3875ec50e7bcc4c8f15645f334aed86eeb07`. The table test independently parsed and compared **all 347 conversion entries**, in order, including `preserve_str` and miner conditionals.
- Built a fresh `git archive` of the pinned Core tag using the existing Core depends installation, with wallet, ZMQ, external signer, and miner enabled. Build: PASS. Ran the unmodified `generator/regenerate.sh` with the release dashd and port base 41430: PASS. It emitted **289 commands / 347 conversion entries**. `git diff --exit-code -- dash-rpc-spec/src/generated.rs` passed: the complete regenerated file is **byte-identical**, including nested argument metadata, gating, classes, help strings, and provenance. Capture contained 289 registered-command help strings with wallet support and 180 without.
- The release dashd SHA-256 equals the generated provenance: `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`. The PRD blob also matches the header.
- CI group verification: PASS, all 29 crates assigned. R2 reachability: PASS. Pure-crate check: PASS, 3 crates. No new deny.toml wrapper entry is needed for this library's empty normal/build dependency graph; the graph check enforces the existing restrictions.

The workspace-wide gate was not replayed in full; the review replay covered the requested formatting, touched-crate clippy/tests, classification checks, and private-test step.

**Private-tests: FAIL; receipt PASS count=1.**

## Source spot-checks and brief compliance

Against the pinned tag, checked more than ten distinct cases: `getblock`'s `verbosity|verbose`, numeric default and skipped type check; `getrawtransaction`'s equivalent alias/default; `getrawtransactionmulti`'s alias; `listtransactions`' `label|dummy`; hidden `stop.wait`; hidden `echojson` and its skipped type checks; `deriveaddresses`' RANGE and omitted fallback; nested `importdescriptors` request/range arguments; nested `fundrawtransaction` options and default hints; nested `scanblocks` scan objects and range default; `protx register`'s array arguments, skipped checks and wallet-only registration; and each of `getcoinjoininfo`, `protx info`, and `protx list`.

Core's `init.cpp` registration order agrees with the dump. The three Hybrid commands use the same help-man functions from wallet and node registration paths, and remain registered under runtime `-disablewallet`; their table classification is correct. The live checks cover their help in both modes. The conversion lookup helpers also preserve Core's first-entry behavior for duplicate keys.

Linking the dump against Core to obtain RPCHelpMan metadata is a justified alternative to textual source parsing: it reads the declared fields directly, and fresh regeneration reproduced the committed output. Help comes from a real release daemon, with a wallet loaded, rather than handwritten fixtures. The two documented PRD gaps are justified by the pinned source: `protx shared_register_prepare_evo` exists and inherits its twin's MUST class, while `getprioritisedtransactions` is absent. Totals remain MUST 266 / SHOULD 22 / WON'T 1. The documented exclusion of the special JSON-valued `help dump_all_command_conversions` from the string renderer does not prevent the brief's per-command help acceptance checks.

No RPC data discrepancy was found. Changes are required for the generator's explicit portability contract; the independently reproduced daemon-isolation defect should also be fixed.

**Verdict: CHANGES-REQUIRED**

blockers=0 majors=1 minors=1 nits=0
