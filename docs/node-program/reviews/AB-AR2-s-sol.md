# AB-AR2-s review

Verdict: **CHANGES-REQUIRED**

Reviewed only `node/ab-ar2-s` at `e18345b956226cc5af27d9b52d14a09232cb226b`, in a detached worktree at `/work/scratch/ab-review-ar2-s/wt`. The implementer's worktree was not modified. Reference: the archived source of `~/workspace/dash` tag `v24.0.0-rc.3`, peeled commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`, and the matching release binary under `/work/scratch/bs4a-decode-domain/dashd/`. Briefs: AB-REVIEW.md, REVIEW-COMMON.md, AB-AR2.md; PLAN §4 AR2 and A1 §2.17.

The four commits reviewed:

- `0b0b5674` feat(dash-rpc-spec): generator for the Dash Core v24 RPC table
- `24355c94` feat(dash-rpc-spec): table of every Dash Core v24 RPC with verbatim help
- `41487669` ci(dash-rpc-spec): add to node-app group, fetch the v24 dashd, note R2
- `e18345b9` fix(dash-rpc-spec): check aliases and completeness live, model build options

## Findings

### 1. Major — RPC readiness does not establish ownership; both launchers can mutate another daemon

**Locations:** `dash-rpc-spec/tests/dashd_help.rs:173` (also :29, :101, :176, :201); `dash-rpc-spec/generator/regenerate.sh:55` (also :40, :50, :59).

The Rust test releases its selected TCP port before spawning dashd. It then accepts any successful `getblockcount` response using the constant credentials `spec:spec`. Checking that the child has not exited does not prove the child bound that endpoint: it may still be starting or unscheduled while another instance owns the port. The version check also cannot distinguish two instances of the same release. The generator has the same problem with `u:p`, and checks RPC success before even testing whether its child remains alive. Both subsequently issue `createwallet spec` to the accepted endpoint.

**Concrete reproduction, entirely on reviewer-owned daemons:**

1. Start a fresh genuine v24 regtest incumbent on port 41430, with credentials `spec:spec`, and verify `listwallets` is `[]`.
2. In a detached copy of the live test, change only `free_port()` to return 41430 to deterministically simulate losing the port-allocation race. Point `DASHD_V24_PATH` at a wrapper that executes the genuine binary for `-version`, but sleeps 30 seconds before executing it for startup.
3. Run the live test. It **passes 1/1**, in 1.13 seconds of test execution; the spawned wrapper never reaches dashd startup. The incumbent remains alive and its `listwallets` is now `['spec']`.
4. Repeat with another fresh incumbent on port 41432 and credentials `u:p`. Run the **unmodified** `generator/regenerate.sh <Core repo> v24.0.0-rc.3 <delayed wrapper> 41432`. It **exits 0**, emits 289 commands / 347 conversion rows, and creates `spec` in the incumbent while its own child has not bound RPC.

This violates the explicit requirement that a test never talk to a daemon it did not start, and allows a green test/generation result against the wrong instance. Cleanup does correctly kill only the tracked child PID; I did not observe an RPC `stop` sent to the incumbent. That does not prevent the unauthorized RPC calls and wallet creation.

**Suggested fix:** establish readiness from the spawned child's own datadir/startup evidence, use a unique per-instance cookie or credentials rather than shared credentials, bind specifically to the loopback address being queried, and reject/retry bind failures or child exit before accepting the endpoint. Add a collision test with an incumbent daemon and a delayed child; assert that the incumbent receives no test RPCs or mutations and cannot make the test pass. Apply the ownership checks to both launchers.

### 2. Minor — the committed live test never checks `-disablewallet` or wallet-only registration fidelity

**Location:** `dash-rpc-spec/tests/dashd_help.rs:192` (also :201, :214); `dash-rpc-spec/tests/table.rs:59`.

The test always starts with wallet support enabled and creates a wallet. Its availability predicate considers only absent compile-time-option commands. It never uses `Registration` to compare a disabled-wallet surface. The unit test checks the three hybrid names, but does not validate the wallet-only set against a live node. The comment at :200 that wallet commands answer help “either way” is misleading for `-disablewallet`: those commands become unknown.

**Concrete regression experiment:** change only `abandontransaction`'s registration in the generated table from `Wallet` to `Node`. All **8/8 table tests** and the wallet-enabled **1/1 live test** still pass. A reviewer-added disabled-wallet comparison fails with `the dashd does not know "abandontransaction"`. This incorrect flag would also make a consumer's disabled-wallet help listing advertise a missing command.

The current flags are correct: independent checks of both modes passed, including the three hybrids. This finding concerns missing regression coverage, not an observed incorrect wallet flag.

**Suggested fix:** run the committed live comparison in both wallet-enabled and `-disablewallet` modes. Derive availability from wallet registration and build options, compare the listing and argument map in each mode, assert that wallet-only commands are unknown when disabled, and retain `protx list`, `protx info`, and `getcoinjoininfo` in both modes.

## Acceptance evidence and gates

Fresh checks used `TMPDIR=/dev/shm/ab-review-ar2-s-tmp`, mode 700, and `CARGO_BUILD_JOBS=6` for the touched-crate Rust gates.

- `cargo fmt --all --check`: pass.
- `cargo clippy -p dash-rpc-spec --all-features --all-targets -- -D warnings`: pass.
- `cargo test -p dash-rpc-spec --all-features`, with the genuine pinned binary and no skip flag: **9 passed / 0 failed** (8 table tests, 1 live test; no unit/doc tests).
- `python3 -I -m unittest discover -s dash-rpc-spec/generator -p test_gen.py`: **10 passed / 0 failed**.
- `ci_config.py verify-groups`, `check-r2.sh`, and `check-pure.sh`: pass; the workspace has 29 classified crates and 3 pure crates.
- Independent live comparison on reviewer-owned daemons, ports 41420 and 41422: wallet-enabled **289/289** command help strings equal the committed table; disabled-wallet **180/180** available commands have the same help and **109/109** wallet-only commands return the expected unknown-command text. A detached Rust probe additionally confirmed the disabled-wallet `help_where` listing and **242** argument-map rows. The enabled mode checks **417** rows.
- Dropping just the `getblock` `verbose` alias causes the live test to fail with `argument map differs from the dashd's`, confirming the reported mutation sensitivity.
- The independent Rust parser checks **347** conversion rows. The vendored `client.cpp` is also byte-identical to the pinned source, SHA-256 `ed1c83adcf82a8bab08c8b331fa9e0a998cdbcc296d4f71f28437a6a31d489fd`.
- `generator/regenerate.sh` on port 41426: exit 0; `git diff --exit-code` confirms both `src/generated.rs` and the vendored `client.cpp` are byte-identical. An earlier attempt on 41424 encountered an IPv4 bind collision, left dashd listening only on IPv6, and kept probing the unavailable IPv4 endpoint; I terminated that owned run. The successful rerun establishes determinism, but the collision illustrates the launcher's weak readiness handling.

Source/help spot checks covered more than ten distinct commands: `getblock` aliases/default/skip flag; `stop` hidden `wait`; `echo` and `echojson` skip flags; `deriveaddresses` RANGE; `gettxoutsetinfo` numeric/string override and fallback hint; `fundrawtransaction` OBJ with skipped type checking; `sendmany` and `simulaterawtransaction` OBJ_USER_KEYS; `quorum info` skipped boolean check; all three hybrid registrations; the `protx shared_register_prepare_evo` wrapper and wallet registration; and the two external-signer guards. No incorrect top-level argument metadata was found in those checks. The table contains 73 two-word commands, 177 node-only, 109 wallet-only, and 3 hybrid commands.

The full workspace gate and the implementer's reported dash-spv BL-20 rerun were not independently repeated. The required reviewer gates on the touched crate were repeated as listed above.

## Deviations and remaining limits

- The two PRD adjustments are justified by this pin: `protx shared_register_prepare_evo` exists and is appropriately MUST; `getprioritisedtransactions` is absent. Counts remain 289 commands, 266 MUST / 22 SHOULD / 1 WON'T.
- The crate, CI group, pinned binary setup, and R2 treatment are present. With no runtime dependencies, an R2 deny-wrapper entry is unnecessary; the classification check passes.
- There is no complete live enumeration of two-word commands; coverage relies on source registration tables plus one-way parent-help checks. This is accurately disclosed.
- Nested argument structure remains only in help text. `skip_type_check` and OBJ versus OBJ_USER_KEYS have source-based extraction rather than complete live verification; the spot checks above matched Core.
- Generator Python tests are not run by the added CI group. Connecting them to CI would improve protection of the generator, but I found no failure in the current 10 tests.
- Linux was exercised. Windows/macOS were not executed; the Rust test has platform-aware binary paths and external-signer availability handling. No demonstrated portability failure is being asserted.

All mutation experiments were confined to the detached reviewer worktree and restored afterwards. Reviewer-owned daemons were stopped, the detached worktree and scratch/temp directories were removed, and this report was left uncommitted.

Verdict: **CHANGES-REQUIRED**

blockers=0 majors=1 minors=1 nits=0
