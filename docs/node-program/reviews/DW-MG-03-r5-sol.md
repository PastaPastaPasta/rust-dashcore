# DW-MG-03 — independent security and correctness review, Sol round 5

Target: `dw/mg-03-masternodes` at `8121c4ea`, reviewed in a detached worktree against `9ea3804d..8121c4ea` (excluding `docs/generated`), MG-03 D37–D38 and the unchanged v3 contract. For the `14df3b9d` merge I reviewed the intersection of DP5-01's `dwcli` log initialization and the MG-03 redactor. The branch under review was not edited, committed or pushed.

## Verdict: CHANGES REQUIRED

The round-4 comma-separated DAPI finding and the release-profile test failure are closed. A new log-redaction composition bug in the merge can expose a link token in an `mn` trace line.

## Findings

### Major 1 — replacing short option values before link detection can expose a link

Location: `rust/bin/dwcli/src/log.rs:30-33,159-163` (with the short-value matching at `:168-196`). `Line::drop` first replaces every occurrence of each global option value, then the `mn` sink runs `dw_uri::redact::redact_links`. The first pass can destroy *both* a link scheme and its secret-parameter marker, so the second pass no longer recognizes a link. For example, `--network devnet:p` is valid and adds `p` to `option_values`. If an SDK trace error contains `dashpay://invite?pk=SECRET`, the first pass produces `dash<redacted>ay://invite?<redacted>k=SECRET`; the shipped `dw-uri` redactor leaves that string unchanged. The pinned SDK logs `?error` at WARN/ERROR on a failed DAPI request, and a gRPC status message can supply the link text. It reaches stderr, including any file to which stderr is redirected, with `SECRET` intact. I checked both original and transformed strings against the compiled `dw-uri` redactor: the original becomes `<link>`, while the transformed string stays visible. The SDK error-to-trace path is source-traced; I did not stand up a gRPC server returning this status. Apply link redaction to the original event before replacing option values, then cover a short option value and a link-bearing event in one test.

### Low 2 — `test-support` is public in an opt-in debug non-test build

Locations: `rust/crates/dw-engine/src/lib.rs:47-48`, `rust/crates/dw-engine/Cargo.toml:17`, `rust/bin/dwcli/Cargo.toml:40`. `cargo build -p dw-engine --features test-support` in the normal debug profile exports `pub mod test_support` and its public `fetch_identity_balance` function, even though that command builds no tests. `dwcli`'s dev-dependency intentionally enables it for a cross-crate test, and standard release/dist profiles disable `debug_assertions`, so I found no path into their binaries. This is a narrow, read-only debug API exposure; the source comment's “tests only” claim is stronger than its gate.

## Round-4 disposition and boundary checks

- **Major 1 closes:** `option_values` splits each `--dapi` argument on commas, matching the pinned `AddressList::from_str` after the engine joins arguments. It adds both each raw item and its trimmed form before deriving URL, host, port and path forms. The SDK's separate failing-address line is covered. The two-address SDK-request test passed. A raw percent-encoded address is covered before URL parsing, its parsed host and path are also included, and both raw and canonical trailing-slash forms are present. ASCII-case matching and the new peer and unbracketed IPv6 host forms showed no additional value echo in the reviewed paths.
- **Low 2 closes:** the simulator-dependent appdb/follower tests and tracking's fault-injection module are now gated by `test && debug_assertions`; release builds omit the simulator and those tests. Both exact release-profile commands compiled and passed.
- **Log setup:** `redact_panics()` is installed before clap parsing. Failed parses use fixed, value-free error text; `log::init` runs only after a successful parse, before `run`, and the `mn` quiet panic hook then withholds panic payloads. There is no separate `dwcli` log file writer; `DWCLI_LOG` writes to stderr. I found no ordinary pre-init user-value trace path. The new finding concerns post-init trace events.

## Validation

Private `CARGO_TARGET_DIR` under `/work/scratch/dw-mg03-r5-sol` (mode 0700), initialized with a reflink copy of a recent MG-03 target; Cargo then built the changed workspace crates against the detached checkout.

- `cargo test -p dwcli`: **passed** (68 passed, 1 ignored across unit and integration binaries); includes the new DAPI SDK log test and `mn_cli` 3/3.
- `cargo test -p dw-engine masternodes`: **passed** (30 passed, 0 failed; other test targets filtered out).
- `cargo test --release -p dw-appdb`: **passed** (108 passed, 0 failed).
- `cargo test --release -p dw-engine masternodes`: **passed** (25 passed, 0 failed; other test targets filtered out).
- Direct probe of the compiled `dw-uri` redactor: `dashpay://invite?pk=SECRET` became `<link>`; `dash<redacted>ay://invite?<redacted>k=SECRET` remained unchanged.
