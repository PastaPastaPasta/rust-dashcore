# DW-E0-02 review r1 (sonnet)

Branch `dw/e0-02-runtime-hygiene` in /work/home/agent/workspace/dw-wt/e0-02-runtime-hygiene, `git diff main...HEAD` (1307283, bc50ce6, c5d12ca). Task: E0-02 in docs/design/ROADMAP.md:64.

**Verdict: CHANGES-REQUIRED** (one major, small fix; everything else is minor or nit).

## Gates (run in rust/, TMPDIR=/dev/shm/e0-02-review-tmp, umask 022)

- `cargo fmt --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: all green, 54 "test result: ok" lines, 0 failed. The new tests ran:
  `engine::tests::engine_workers_have_an_8_mib_stack`, `engine::tests::engine_blocking_threads_have_an_8_mib_stack`, `session::tests::{initial_protocol_version_seeds_the_sdk, missing_ca_certificate_is_a_config_error, a_file_without_a_certificate_is_a_config_error}`, `platform_status_offline`.
- Extra experiments (scratch /work/scratch/e0-02-review, since deleted except `certs/` and logs): a stack mutation run, and TLS runs of the built `dwcli` against `openssl s_server`. Results are inline below.

## Findings

### M1 (major): the CA "PEM check" is a substring test. A corrupt PEM passes it and then panics an engine task at the first DAPI call

`rust/crates/dw-engine/src/session.rs:646-659` (`build_sdk`) only checks that the bytes contain `-----BEGIN CERTIFICATE-----`.
The SDK builds the TLS config at the first request, in `rs-dapi-client/src/transport/tonic_channel.rs:68-70`:
`builder.tls_config(tls_config).expect("Failed to set TLS config")`.
tonic's `convert_certificate_to_pki_types` returns `CertificateParseError` for a PEM block that is truncated or has bad base64. The `expect` turns that into a panic.

Reproduced with the built `dwcli`, using a PEM that starts with the BEGIN line and is cut off after 300 bytes, and one with a BEGIN line plus `Zm9vYmFy` and no END:

```
thread 'dw-engine' panicked at .../tonic_channel.rs:70:10:
Failed to set TLS config: tonic::transport::Error(Transport, CertificateParseError)
error: internal error: engine task panicked: task 45 panicked with message "Failed to set TLS config: ..."
```

- So c5d12ca's stated goal ("catch a wrong file here instead of as a transport error") holds only for files with no CERTIFICATE block at all, such as a key file or a DER file.
- A damaged file (partial copy, line-wrapped wrongly, concatenated key plus truncated cert) still gets through `open_network` and fails later with `internal` (a panic).
- Worse, in a long-lived GUI session that first DAPI call comes from whichever task needs DAPI first (wallet or identity sync, SPV bring-up in E0-05). The panic would land in a detached task, not in `platform_status`.
- Note also that tonic uses `RootCertStore::add_parsable_certificates`, which silently drops a block whose base64 decodes but whose DER is garbage. That gives a late `UnknownIssuer` rather than a panic, but it is still not caught at open.

Fix:
- Validate with the same code path the SDK uses. Dry-run it in `build_sdk`: `Endpoint::from_static("https://localhost").tls_config(ClientTlsConfig::new().ca_certificate(cert))`, reachable via `dash_sdk::dapi_grpc::tonic::transport`. Map the error to `InvalidConfig`.
- Or parse with `rustls_pki_types::pem` / `CertificateDer::pem_slice_iter` and require at least one certificate, plus one `RootCertStore::add` per certificate to catch bad DER.
- Add tests for a truncated PEM and a bad-base64 PEM (both now panic), alongside the existing "no block" test.
- Consider an upstream PR or issue to replace the `expect` in `create_channel` with an error.

### Minor

**m1: `ca_cert_path` adds to the system roots, but the docs say it replaces them.**
- `session.rs:114-115` ("instead of the system roots"), `rust/bin/dwcli/src/main.rs:41-43` ("DAPI TLS is checked against … instead of the system roots") and the commit message are wrong.
- `tonic_channel.rs:34-56` builds `ClientTlsConfig::with_webpki_roots()` plus native roots unconditionally. tonic 0.14.6 `service/tls.rs:83-106` builds the root store from `native + webpki + ca_certs`. `ca_certificate()` appends. I confirmed this empirically: with no `--ca-cert`, a self-signed server gives `UnknownIssuer`; with the file, the handshake succeeds.
- Is additive the right behaviour? For a dashmate devnet, yes. The self-signed leaf is only useful as an extra anchor. Public CAs cannot vouch for a devnet IP or hostname, so keeping the system roots costs nothing there. Replacement semantics are not available through `SdkBuilder`, and nothing in E0-02 needs them.
- Fix the three doc strings to say "added to". If anyone ever wants pinning (only this CA, no public roots), that needs an SDK or tonic change (`tls_config_with_verifier`), so record it as out of scope.

**m2: `platform_status` holds the close gate for the whole fetch, with no cancel path.**
- No deadlock. `platform.rs` takes the guard once, via `enter()`, inside the task spawned by `on_runtime`. It calls no other session method that enters the gate. A close that is waiting makes new readers queue (fair `RwLock`) and then get `NetworkNotOpen`, which the test covers.
- Cancellation: dropping the caller's future detaches the spawned task (the `JoinHandle` is dropped). The fetch keeps running and keeps its gate guard until it finishes.
- Close-while-running: `NetworkSession::close()` (`gate.close().await`) blocks until the fetch ends. With DAPI blackholed, that is `DEFAULT_TIMEOUT` 10 s per attempt times retries (and per address). I measured about 10 s with one address; N addresses give about N × 10 s or more. It always terminates, so it is not a hang.
- Harmless for the one-shot CLI. It matters once a UI or E0-05's "closing during bring-up is clean" calls it.
- Fix: wrap the fetch in `tokio::time::timeout` (say 30 s), or `select!` against a session close notification, so close is not held hostage.

**m3: options are validated late in `open`.**
- The CA and protocol-version checks run in `build_sdk` (`session.rs:243`), after the vault is opened, `app.sqlite` is created and the first quorum-context attempt is made.
- That first attempt spawns a detached `update_quorum_caches` task (`session.rs:229-234`) that keeps doing network I/O after `open` returns an error.
- The failed open leaves its directories behind.
- Fix: validate `ca_cert_path` and `initial_protocol_version` next to `network.validate()` and the peer parsing at the top of `open`, and build the `Certificate` there.

**m4: "every engine thread" is overstated.**
- The doc on `ENGINE_THREAD_STACK_SIZE` says "Stack size of every engine thread". Only the tokio workers and blocking pool get it.
- The `dw-engine-drop` thread (`engine.rs:190`, which runs `session.close()` under `block_on`) and the `dw-quorum-context` retry thread (`context.rs:139`) are plain `std::thread` with the 2 MiB default.
- `Engine::block_on` also polls its future on the caller's thread, so only `NetworkSession` methods that hop via `on_runtime` get the 8 MiB stack.
- Today none of these threads does proof descent, so this is not a defect. Say so in the comment, or set `.stack_size(ENGINE_THREAD_STACK_SIZE)` on the drop thread (it runs the long close). `platform_status` is correct to hop with `on_runtime`.

**m5: T-01 risk (not a code defect): the self-signed gateway cert may not verify.** The experiments below show:
- The cert's own name must match the URI host: `domain_name(host)` is the host from the URL. With `--dapi https://127.0.0.1:…` and a cert that has a DNS SAN only, the result is `certificate not valid for name "127.0.0.1"`.
- A self-signed leaf with `basicConstraints CA:TRUE` fails (`CaUsedAsEndEntity`). A leaf with `CA:FALSE` passes. This is rustls-webpki behaviour, and nothing in the engine can fix it.
- Table (openssl 3 `s_server`; "handshake ok" means the TLS handshake finished and the gRPC call then timed out, because `s_server` does not speak gRPC):

| cert | `--ca-cert` | DAPI host | result |
|---|---|---|---|
| self-signed `req -x509` default, CA:TRUE, SAN localhost + 127.0.0.1 | itself | 127.0.0.1 | `CaUsedAsEndEntity` |
| self-signed, CA:FALSE, SAN localhost + 127.0.0.1 | itself | 127.0.0.1 | handshake ok |
| self-signed, CA:FALSE, SAN localhost + 127.0.0.1 | itself | localhost | handshake ok |
| self-signed, CA:FALSE, SAN localhost only | itself | localhost | handshake ok |
| self-signed, CA:FALSE, SAN localhost only | itself | 127.0.0.1 | name mismatch |
| self-signed, CA:FALSE, SAN localhost + 127.0.0.1 | none | localhost | `UnknownIssuer` |

- Before T-01's acceptance run, check what dashmate's gateway actually serves (a CA plus a CA-signed leaf, or a CA:TRUE self-signed leaf) and give `ca_cert_path` the right file (the CA, not the leaf). If dashmate's cert is CA:TRUE self-signed, E0-02's acceptance cannot pass through this API without an SDK change. It is worth recording in the T-01 notes.

### Nits

- **n1:** the stack tests prove "more than 2 MiB" (5 MiB of frames), not 8 MiB. That matches the E0-02 acceptance text, but the names say `8_mib`. A 4 MiB setting would also pass.
  - Mutation check (scratch copy with `.thread_stack_size(...)` removed): the lib test binary aborts with `thread 'dw-engine' has overflowed its stack / fatal runtime error: stack overflow, aborting` (SIGABRT). Real regressions are caught.
  - The abort kills the whole `dw-engine` lib test process instead of failing one test. Consider moving these two tests into their own integration test file (`tests/engine_stack.rs`), so a regression does not take out the other lib tests.
  - If you want the 8 MiB bound, add a 7 MiB probe (about 112 frames); expect some headroom concerns in debug builds.
  - Because tokio uses one `thread_stack_size` for both pools, the two tests are redundant for the setting but a reasonable guard that the builder isn't bypassed.
- **n2:** commit bc50ce6's message says an unknown protocol version is `invalid_argument`. The code (and c5d12ca) use `InvalidConfig`. The code is fine; it only disagrees with the message. `build_sdk` is inconsistent about this (a bad DAPI list is `InvalidArgument`), but all user-input errors are reported before any I/O, so it does not matter.
- **n3:** `Certificate::from_pem(pem)` is given a `Vec<u8>` already read, so the file is read once and there is no TOCTOU between check and use. This is a good choice. It differs from the roadmap text (`with_ca_certificate_file`) only in that the roadmap function re-reads and has no validation hook; no action needed.
- **n4:** `SessionOptions` gained public fields with no `#[non_exhaustive]`. Every in-repo literal was updated (`..Default::default()` in `dw-ffi`, `dw-console` tests, `backup.rs`, `flow_tests.rs`, `dwcli`, `e1_offline`, `m3_keychain_offline`, `r1_tools_offline`, `r2_compat_offline`, `session_lifecycle`, `umask_002`). Fine.

## Questions asked

**1. Do the stack tests really prove 8 MiB for workers and blocking threads?** They prove "more than 2 MiB" for both pools.
- The worker test spawns with `tokio::spawn` from `block_on` and checks the thread name `dw-engine`. The blocking test goes through `run_blocking` (`spawn_blocking`).
- Both recurse 81 frames of 64 KiB, about 5 MiB, which is over tokio's default and under 8 MiB.
- I mutation-tested it: with the setting removed, the process overflows and aborts.
- That meets the E0-02 acceptance as written. It does not pin 8 MiB (n1).

**2. CA file: added or replacing?** Added. See m1 for the doc fix; it is the right behaviour for a self-signed devnet leaf. Replacement is not available through `SdkBuilder` and is not needed.
- **Error mapping:** a missing or unreadable file, a file without a CERTIFICATE block and an unknown protocol version all map to `invalid_config` (`EngineError::InvalidConfig`). That is reasonable and consistent with `spv_config().validate()`. The path appears in the message; no secrets.
- **Real gap:** M1.

**3. `platform_status` on the runtime?** Correct hop. The bare current-thread-runtime test shows tonic needs the engine's reactor, and the closed-session test passes. No deadlock (single `enter()`, no nesting). Cancellation and close-while-running are m2: they always terminate, but close is held for up to N × 10 s per blackholed address.

**4. UniFFI `SessionOptions` not updated: acceptable until E0-13?** Yes.
- The Swift side is frozen (`MacUI`/`CrossUI` freeze in ROADMAP U), `Generated/DashWalletCore.swift` is checked in, and `dw-ffi`'s `From` impl leaves a comment and uses `..Default::default()`.
- Adding record fields would regenerate the bindings for UIs that cannot use them.
- The Tauri path (`dw-app`) uses `dw_engine::SessionOptions` directly.
- The only gap is traceability: the E0-13 row in ROADMAP does not mention these two fields, and this diff touches no docs. Add "bind `ca_cert_path`/`initial_protocol_version` on the SessionOptions record (UniFFI and `dw-app`)" to the E0-13 row, so it is not forgotten.

## Suggested fix list (for the author)

1. M1: real PEM validation at open (dry-run `tls_config`, or parse the certificates), plus the two corrupt-PEM tests.
2. m1: fix the "instead of the system roots" wording in `session.rs`, `dwcli` help and the commit/PR text.
3. m2, m3, m4 as cheap fixes, or defer to E0-05 with a note.
4. Add the E0-13 note about the two new fields.
5. T-01 notes (m5): which dashmate cert and host name to use.
