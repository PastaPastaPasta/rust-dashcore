# AR3 `rdash-cli` independent review, round 1 (Sol)

Scope: `c68b1c3e4..b40f5ecdc` in a detached scratch worktree. Reference: Dash Core tag `v24.0.0-rc.3`, `src/bitcoin-cli.cpp`, `src/rpc/client.cpp`, `src/rpc/request.cpp`, and the cached release `dash-cli`/`dashd`. The implementer's worktree and branch were not changed.

## Findings

1. **High — possible security note: see manager** (`rdash-cli/src/main.rs:14-23`).
2. **Medium — `-generate` reports success after a failed `getnewaddress` RPC with code 0** (`rdash-cli/src/cli.rs:696-725`). Core `src/bitcoin-cli.cpp:1196-1218` calls `ParseError`, then dereferences the absent request handler; the stock process aborts. Local scripted 500 reply `{"result":null,"error":{"code":0,"message":"CANARY failure"},"id":1}`: stock exited by signal with no success output; Rust exited 0 and printed `error code: 0` on stdout. The crash avoidance is justified, but a failed generate must not tell scripts that a block was generated. Fix: return a nonzero exit code and print the RPC error on stderr; document this deliberate departure from stock and add a regression row.
3. **Medium — the XCLI test silently disappears on macOS and Windows** (`rdash-cli/tests/xcli/main.rs:12-13`). `#![cfg(target_os = "linux")]` removes both test functions on those targets, so a green test run gives no sign that the differential gate did not run. Keep a portable differential subset active where possible; at minimum, emit an explicit named skip test with a reason in CI rather than compiling the whole test out. Gate parity claims should be qualified to Linux until those targets run it.
4. **Low — expected failure rows can pass for the wrong reason** (`rdash-cli/tests/xcli/main.rs:46-91`, `rdash-cli/tests/xcli/support/mod.rs:432-469`). The harness checks the stock exit code and equality, but a row expecting code 1 can pass if both binaries fail before the intended RPC step with the same message and no request. Require at least the expected request count/method for HTTP failure rows and a distinctive error/body assertion for the intended path. Successful rows and the fake server's request byte comparison are substantially stronger.
5. **Medium — possible security note: see manager** (`rdash-cli/src/stdin.rs:41-78`).
6. **Medium — possible security note: see manager** (`rdash-cli/src/http.rs:180-260,384-510`).

## Fidelity and deviation dispositions

- Argument setup and precedence, `-named`, `-stdin*`, wallet endpoint encoding, `-rpcwait` retries/deadline, auth and cookie lookup, configuration handling, `-getinfo`, all `-netinfo` levels/columns/order, `-addrinfo`, `-generate`, and color handling were compared with Core source and covered by successful stock differential rows. No additional deterministic byte divergence was found in those paths. The 28 separately scripted HTTP status/header/chunk/body edge probes matched after masking only the different ephemeral ports; neither binary panicked on those inputs.
- `dash-rpc-spec::CONVERT_PARAMS` has **347 entries**, identical in order and in method/index/name/`preserve_str`/miner flags to Core tag `v24.0.0-rc.3`'s `vRPCConvertParams` (blob `260e3875ec50e7bcc4c8f15645f334aed86eeb07`). Positional and named conversion use the first matching rule as Core does. The XCLI conversion sweep exercises all 289 commands but does not by itself prove every table entry; the independent table comparison supplies that check.
- **Default datadir:** accept under DEC-201. The `ArgsManager::set_default_data_dir` field defaults to `None`; only `rdash-cli` sets it before paths are resolved, and it clears path caches. The `TODO(ND1)` is a clean replacement seam for ND1's product parameter. An explicit `-datadir` still takes precedence. The two XCLI lookup rows verify the Rust directory and rejection of Core's directory.
- **`getrpcinfo.duration` and `getnettotals.timemillis`:** accept the narrow digit normalizers for server clock values. The other output bytes and exit code remain compared.
- **Short batch reply:** accept the documented null substitution in place of Core's out-of-bounds read; it ends in a normal error. A dedicated regression case should lock in this safety behavior.
- **Code-0 `getnewaddress` error:** challenge the current exit-0 choice; see finding 2.
- **`100 Continue` followed by immediate close:** accept omission of the race assertion; same-connection deterministic cases are tested.
- **Non-UTF-8 stdin parameters:** accept refusal as a documented, safe departure. Independent local probes showed stock sent the raw non-UTF-8 byte in both `-stdin` and `-stdinwalletpassphrase` JSON bodies, while Rust exited 1 with `standard input is not valid UTF-8` and sent no request. The four deviation rows pass.
- **Non-UTF-8 argv:** refusal is documented, but disposition depends on finding 1.
- **No libevent `event_base`:** documented socket idle timeout model; the idle server row passes. Longer trickle/whole-request timing is not established by the matrix and should remain an explicit limitation.
- **Linux-only XCLI and `free_port()` race:** the race is a minor test flake risk; the silent platform gating needs finding 3 addressed.

## Harness assessment

The baseline matrix passed **994/994 offline** and **234/234 live regtest** rows. Fake server rows compare stdout, stderr, exit code, and request bytes. Live rows compare the output and exit code; volatile rows allow up to five near-second-boundary attempts, with zero timing retries in this run. Mirror state-changing rows also compare chain tips. The code-1 premise weakness and platform gating above limit the claim; neither invalidates the large set of successful code-0 and request byte checks.

## Mutation checks

Each behavior was changed alone in the detached worktree, the original source was restored and touched before the next build, and a scratch-only runner called the **unchanged XCLI row builders**. The runner was removed after these checks.

| Mutation | XCLI group | Failed rows | Representative row |
|---|---|---:|---|
| Request `Content-Type: application/json` → `text/plain` | request | 62/67 | `user and password` |
| Default RPC host `127.0.0.1` → `127.0.0.2` | no-server | 29/58 | `connection refused` |
| Warmup code `-28` → `-29` | rpcwait | 2/11 | `warm-up twice, then ready` |
| Remove matching quote unwrapping in conversion | named | 4/41 | `quoted JSON by name` |
| Omit IPv4 from `-addrinfo` output loop | handlers | 3/252 | `-addrinfo mixed` |

Every mutant made its selected XCLI test fail. Other rows in the same groups passing are expected because those paths do not exercise the changed behavior.

## Validation notes

- `RUSTC_WRAPPER= CARGO_BUILD_JOBS=12 TMPDIR=/work/scratch/ar3-sol-r1/tmp CARGO_TARGET_DIR=/work/scratch/ar3-sol-r1/target DASHD_V24_PATH=.../dashcore-24.0.0-rc.3/bin/dashd cargo test -p rdash-cli -p dash-args`: passed; includes both XCLI tests. Test target was reflink seeded from the implementer's existing target, then kept private to the detached scratch worktree.
- After mutation restoration, the scratch worktree diff was empty, `cargo test -p rdash-cli -p dash-args --lib` passed, the XCLI integration target rebuilt with `--no-run`, and `cargo fmt --all -- --check` passed.
- Local socket probes used only the cached v24 `dash-cli`, loopback fake servers, and private HOME directories. No live mainnet or testnet oracle was used. A scripted empty `-getinfo` batch made stock abort while Rust exited 1 with a normal JSON type error.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
