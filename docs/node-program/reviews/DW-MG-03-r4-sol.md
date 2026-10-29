# DW-MG-03 — independent security and correctness review, Sol round 4

Target: `dw/mg-03-masternodes` at `9ea3804d` in a private detached worktree. Reviewed `c245a294..9ea3804d` excluding `docs/generated`, against MG-03 D33–D36 and the unchanged v3 contract. The branch under review was not edited, committed, or pushed.

## Verdict: CHANGES REQUIRED

### Major 1 — a comma-separated `--dapi` value can expose one address in the log

Locations: `rust/bin/dwcli/src/log.rs:34-79,119-149`; `rust/crates/dw-engine/src/session.rs:916-930`; `rust/crates/dw-engine/src/masternodes/reads.rs:796-845`. The pinned SDK's `packages/rs-dapi-client/src/address_list.rs:608-617` splits the string on commas; `dapi_client.rs:226-230,1245-1270` logs a failing address or its containing `ExecutionError` at WARN/ERROR with `?address`/`?error`.

Scenario: pass `--dapi 'http://127.0.0.1:1/pathTOKEN,http://127.0.0.1:2/pathTOKEN'` to a Platform-enabled `dwcli mn credits` call for a listed evonode, with `DWCLI_LOG=info`. The engine accepts the one argument as two DAPI addresses. On a failed identity-balance request, the SDK logs one address, including `/pathTOKEN`. `option_values` keeps the whole comma-separated argument. `forms` parses it as one URL whose path contains both addresses, and records no individual `/pathTOKEN` or individual URL. The one-pass redactor can hide the host while leaving `pathTOKEN` in stderr. This is the M1 value-echo rule on a valid input shape missed by the D33 sweep, which tests separate URL arguments but not a comma-separated address list. Split DAPI list values for redaction using the same address-list grammar as the SDK, then cover an individual-address log line.

### Low 2 — release-profile tests cannot compile the debug-only simulator tests

Locations: `rust/crates/dw-appdb/src/lib.rs:26-27,1526-1554`; `rust/crates/dw-engine/src/masternodes/follow.rs:562-583`; `rust/crates/dw-engine/src/masternodes/tracking.rs:778-870`; `rust/crates/dw-engine/Cargo.toml:71`.

Scenario: `cargo test --release -p dw-appdb` or `cargo test --release -p dw-engine masternodes` compiles the `#[cfg(test)]` test bodies but not `dw_appdb::durability_sim`, because the module also requires `debug_assertions`. The tests' unguarded imports therefore fail to resolve. Ordinary release binaries exclude the simulator as intended; this only breaks release-profile tests. Gate those tests with `debug_assertions` or make the test-only module available under `cfg(test)` without exposing it to release binaries.

## Disposition of the round-3 findings

- **M1:** event output is now a fixed variant name for `mn`; the subscriber is installed before engine/session creation. The single-URL, devnet, and setup-error paths have no remaining value echo found. The comma-separated DAPI case above remains.
- **M2:** the ID clear commits with `synchronous=FULL` before the vault write. A crash before the clear leaves old vault key A with A's ID; after the clear and before B is durable it leaves no ID; after B is durable it leaves B's ID or no ID. On the next unlocked read, a recorded mismatch is dropped before it can hold a role. A read unable to compare because the tracking lock is busy, the vault locks, or a key read fails leaves that ID out. A stream of continuously queued tracking mutations can delay the first comparison indefinitely, leaving roles `unchecked`; after mutations quiesce, the next read can compare. I found no persistent false-held-role path or close lock inversion in the changed code.
- **M3:** the global and per-masternode version-3 markers use the durable setting write. The follower waits for both writes before reporting its first-sighting change. If the per-masternode write fails after the global one, the durable global marker still suppresses payout attribution and a later tick can retry.
- **L4:** uncompressed WIF is refused before storage with fixed text; compressed WIF and 64-hex secrets use the compressed public-key ID consistently.

The simulator module and hooks require `debug_assertions` plus `test` or `durability-sim`. The feature is only in `dw-engine`'s dev-dependency; the release and distribution profiles do not enable debug assertions, and build scripts do not enable the feature. Ordinary release artifacts exclude the simulator.

## Validation

The review used a detached worktree at `9ea3804d` and a mode-0700 target under `/work/scratch/dw-mg03-r4-sol`. A fresh `cargo test` build was started there, then stopped because host load exceeded 300. Exact-head test executables built after `9ea3804d` were reflink-copied from the completed MG-03 target into this private target and run from the detached checkout. This validates their behavior at the reviewed head, though they were not rebuilt from scratch in this worktree.

- `dw_engine-7a8fcb9de1def85d masternodes --test-threads=1`: **30 passed**, 1,024 filtered out.
- `mn_masternodes_contract-512d5ed25536d047 --test-threads=1`: **8 passed**. Its first run found only 2/8 because `CARGO_MANIFEST_DIR` was baked as the completed target's deleted checkout path. A private `bwrap` bind mapped that path to this checkout, and all 8 then passed; no reviewed file was changed.
- `mn_cli-e43a7730c1d0dce5 --test-threads=1`: **3 passed**, including the global-option redaction sweep with `DWCLI_LOG=info` and `--verbose-events` (665 seconds under load).
- Direct `dwcli mn list-state` probe with `DWCLI_LOG=info`, `--verbose-events`, mainnet and an HTTP `--quorum-url` containing a dummy user-info token: exit 0, one JSON stdout line, four kind-only event lines, and no token in stdout/stderr. A separate probe confirmed the CLI accepts a comma-separated `--dapi` argument. A small probe using the pinned `url` crate confirmed that parsing this combined argument creates a combined path and no value covering an individual address's path token in an SDK log line. The Major 1 SDK failure case was source-traced, not exercised against a synced evonode.
- Release-profile test compilation in Low 2 was inferred from the `cfg` gates; no release-profile test build was run.
