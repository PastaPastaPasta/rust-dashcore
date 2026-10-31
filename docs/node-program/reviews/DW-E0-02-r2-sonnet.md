# DW-E0-02 review r2 (sonnet)

Branch `dw/e0-02-runtime-hygiene` in /work/home/agent/workspace/dw-wt/e0-02-runtime-hygiene. New commits: d87dbbb (CA validation at open), d84f970 (stack tests moved, drop thread, 7 MiB), fa22ebb (ROADMAP). Round 1: DW-E0-02-r1-sonnet.md.

**Verdict: APPROVE-WITH-NITS.** M1 is closed. No blocker or major remains. m1, m3, m4, n1 and the ROADMAP notes are closed. The m2 item from round 1 (close held for the length of a `platform_status` fetch) was not in this round's list and is unchanged.

## Gates (rust/, TMPDIR=/dev/shm/e0-02-r2-tmp mode 0700, umask 022)

- `cargo fmt --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: exit 0, 55 "test result: ok" lines, 0 failed.
  - The new tests ran and passed: `session::tests::{a_valid_ca_certificate_is_accepted, a_damaged_ca_file_is_a_config_error, missing_ca_certificate_is_a_config_error, initial_protocol_version_seeds_the_sdk}`, `session_lifecycle::bad_options_fail_open_before_anything_is_created`, and `engine_stack::{engine_workers_have_an_8_mib_stack, engine_blocking_threads_have_an_8_mib_stack}`.
- Tooling note, not a branch problem: the first `cargo test` died because the shared sccache server had been started by another thread with a TMPDIR that was later deleted (`/dev/shm/e0-03-fix3-tmp`). I re-ran with my own `SCCACHE_SERVER_PORT` and it was green.

## M1: closed

Re-ran my round-1 repro with the built `dwcli` (`--dapi https://localhost:1 --ca-cert FILE platform-status`, fresh empty datadir each time). Every damaged file now fails at open with `error: invalid configuration: ...` (`EngineError::InvalidConfig`, code `invalid_config`, `error.rs:142`), exit 1, no panic. A `--datadir` listing afterwards is empty in every rejected case.

| file | result |
|---|---|
| PEM cut off at 300 bytes (round-1 repro) | `truncated: a CERTIFICATE block has no END line` |
| BEGIN line + `Zm9vYmFy`, no END (round-1 repro) | same truncated message |
| BEGIN/END with `!!!!not base64!!!!` | `a CERTIFICATE block is not base64: InvalidCharacter(33)` |
| BEGIN/END around valid base64 of garbage (`Zm9vYmFyZm9vYmFy`) | `certificate 1 of the file: TrailingData(SignedData)` |
| empty file | `no -----BEGIN CERTIFICATE----- block` |
| DER file | same "no block" message |
| missing file | `No such file or directory` |

The implementation is `load_ca` (`session.rs`, new in d87dbbb) with three layers:
1. `CertificateDer::pem_slice_iter` (the parser tonic uses) for each block.
2. `webpki::anchor_from_trusted_cert` for each DER (the function `RootCertStore::add_parsable_certificates` uses, which is what silently dropped garbage DER before).
3. A dry run of `Endpoint::tls_config(ClientTlsConfig::ca_certificate(..))`, the call whose `expect` panicked in rs-dapi-client.

I checked tonic 0.14.6: `channel/service/tls.rs:95,104` calls `convert_certificate_to_pki_types` and then `add_parsable_certificates`, so the validation matches the SDK's path. The unit test also covers "good cert followed by a truncated one".

### Valid real-world files are not rejected

Each of these was fed to `dwcli`. All passed validation, and the error moved on to the expected transport error (`tcp connect error`, port 1):

- a plain three-certificate chain (EC root, RSA intermediate, Ed25519 leaf);
- a decorated chain with `Bag Attributes` lines, `subject=` lines, comments, `openssl x509 -text` output between the blocks and trailing prose;
- a CRLF file with no trailing newline;
- a certificate followed by its private key in one file;
- the system bundle, `/etc/ssl/certs/ca-certificates.crt` (121 certificates);
- a self-signed `CA:FALSE` leaf (`b.pem`), the dashmate case;
- a certificate with an X.509 v1-style trust-anchor path is handled by webpki's v1 fallback (checked in the source, `trust_anchor.rs:39`); openssl 3 here could not mint a real v1 certificate to test with, so that one is source-reviewed only.

(Two runs hit my 120 s timeout while the box was at load average 70 to 80; the re-runs finished in 0.5 s and 11 s with the same result.)

Two behaviours to know about, both acceptable:
- `-----BEGIN TRUSTED CERTIFICATE-----` files (openssl `-trustout`) are rejected as "no CERTIFICATE block". tonic would also see no certificate, so nothing is lost, and the round-1 check rejected them as well.
- The check is all-or-nothing and stricter than tonic: a bundle in which one member does not parse as a trust anchor is now rejected, where tonic would drop that member and keep the rest. This is arguably better for a file the user chose on purpose, and the system bundle passes. See nit N1.

### Validation happens before anything is created

- `NetworkSession::open` runs `network.validate()`, then the SPV peer parse, then `SdkOptions::validate(&opts)` (CA and protocol version), and only then `create_owned_dir`, the vault, `app.sqlite` and the quorum-prefetch task. `Engine::open_network` only computes `network_dir`, creating nothing.
- Black-box result above: the datadir is empty after every rejection.
- `bad_options_fail_open_before_anything_is_created` (session_lifecycle.rs) asserts `InvalidConfig` and `!root.join("regtest").exists()` for a truncated CA and for `initial_protocol_version = u32::MAX`. It passes.
- `build_sdk` now takes the already-validated `SdkOptions`, so there is no second read of the file (no check/use race) and no late failure.

## Minors from round 1

- **m1 (docs say "replaces the system roots"): closed.** `SessionOptions::ca_cert_path` docs, the `dwcli --ca-cert` help text and the `grep` of the tree (`*.rs`, `*.md`, `*.ts`) now say "in addition to the system roots"; no "instead of the system" is left. The `session.rs` doc also records that the file may be a CA or a `CA:FALSE` self-signed leaf, that the host name must match, and that pinning needs an SDK change.
- **m3 (late validation, leftover directories, detached task): closed.** See above. The `update_quorum_caches` task can no longer outlive a failed open that was caused by bad options.
- **m4 ("every engine thread" overstated): closed.** The doc on `ENGINE_THREAD_STACK_SIZE` names the async workers and the blocking pool, says `dw-engine-drop` gets 8 MiB too, says `dw-quorum-context` keeps the platform default, and says a future polled by `Engine::block_on` runs on the caller's thread so Platform calls must hop with `on_runtime`. The drop thread now sets `.stack_size(ENGINE_THREAD_STACK_SIZE)` (`engine.rs:198`).
- **n1 (stack tests prove only > 2 MiB, and abort the lib test binary): closed.**
  - The two tests moved to `rust/crates/dw-engine/tests/engine_stack.rs` and probe 112 frames of 64 KiB (7 MiB). The blocking test now uses `tokio::task::spawn_blocking` directly and also asserts the thread name `dw-engine`.
  - Mutation run against the moved file (a scratch copy of HEAD; the branch was not touched). Result of `cargo test -p dw-engine --test engine_stack`:

    | `thread_stack_size` | outcome |
    |---|---|
    | removed | `thread 'dw-engine' has overflowed its stack`, SIGABRT |
    | 4 MiB | same overflow, SIGABRT |
    | 6 MiB | same overflow, SIGABRT |
    | 8 MiB (control) | both tests ok |

  - The abort now kills only the `engine_stack` binary; the other `dw-engine` lib tests are in their own process and unaffected.
  - Residual: a 7.5 MiB setting would still pass. That is fine; the intent is "8 MiB, not tonic's 2 MiB", and a debug-build 7 MiB probe is already close to the limit.
- **ROADMAP (fa22ebb): closed.**
  - The E0-13 row now says it adds `ca_cert_path` and `initial_protocol_version` to the SessionOptions record (UniFFI `dw-ffi` and `dw-app`) and why E0-02 left them out.
  - A "T-01 notes from the E0-02 review" block sits after the T table. It covers additive CA semantics, giving the CA rather than the leaf, host name vs SAN, the `CA:TRUE` self-signed leaf failing with `CaUsedAsEndEntity`, and the measured `openssl s_server` results.
  - Those notes match my round-1 table. One small inconsistency, not worth a change: the E0-02 row at ROADMAP.md:64 still says `SdkBuilder::with_ca_certificate_file`, while the code uses `with_ca_certificate(Certificate)`; the notes explain why.

## Nits (no change required)

- **N1: the validation is stricter than tonic and the failure message for garbage DER is terse.** `TrailingData(SignedData)` is a webpki enum, not something a user can act on. Suggest mapping `anchor_from_trusted_cert` errors to "certificate N of the file is not a valid X.509 certificate (webpki: ...)". Also note in the doc that one bad member rejects the whole file (all-or-nothing), if that is intended.
- **N2: dependency pins are loose.** `rustls-pki-types = "1"` and `rustls-webpki = "0.103"` are direct deps (Cargo.lock only gains two edges; both crates were already in the graph at 1.14.1 and 0.103.13). If tonic later moves to webpki 0.104, the second check could diverge from tonic's, though the third (dry-run) layer still catches a damaged file. Cheap safeguard: keep the dry run, as it is.
- **N3: `m2` from round 1 is still open** (not in this round's list): `platform_status` holds the close gate for the duration of the fetch, with no timeout or cancel path. It terminates, so it is not a hang; it matters for E0-05's "closing during bring-up is clean". Worth a one-line note on the E0-05 row.

## Cleanup

TMPDIR, the scratch copy, the private sccache server and the 11 GB scratch target dir were removed. `/work/scratch/e0-02-r2/` keeps the test certificates (`certs/`) and the clippy/test logs. The branch worktree is unmodified.

**Verdict: APPROVE-WITH-NITS.**
