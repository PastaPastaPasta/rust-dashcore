# P1 — Opus high review (independent second reviewer)

**Range:** `75c1b44e` → `3a6091e41` (24 commits, all with good signatures (`%G? = G`)), `node/p1-p2p-transport`, worktree `~/workspace/rdc-wt/p1`. The worktree stayed clean throughout. This report is uncommitted. I followed `briefs/REVIEW-COMMON.md`, with the prompt's overrides: TMPDIR `/work/scratch/p1-opus/tmp` (mode 700), `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and the worktree's own targets. PLAN §7 handling: codenames only, and I opened no private material.

**Prior reviews:** I read `P1-r1-sol.md` and `P1-r2-sol.md`. r1's F1–F3 and r2's R2-1 are closed (see "Prior findings").

## Method

I ported nothing from the implementation's comments. Each function was compared line by line with the pinned sources, extracted with `git show v24.0.0-rc.3:` from `~/workspace/dash`:

- `src/net.cpp` lines 649–882 (`ReceiveMsgBytes`, `V1Transport`) and 885–1690 (`V2Transport`);
- `src/bip324.cpp`;
- `src/version.h`, `src/serialize.h` and `src/net.h` constants;
- `src/test/net_tests.cpp`, `v2transport_test`.

I read base-sdk's `dash-p2p-core` at the patched revision `4b4e76b2` for `ShortId`, `CommandString::is_valid_v2`, `V1Header::check`/`version_prefix` and `limits`.

I also re-extracted Core's two short-ID arrays independently, resolving `NetMsgType` names through `protocol.cpp` with my own script. They match `tests/data/core_short_ids.rs` entry for entry: 33 + 41 entries, with 5 and 29–32 empty and 168 = `platformban`.

## Parity with Core (look-hardest items)

All checked items match Core. Details by area:

**Receive state machine** (`v2.rs:219–384` vs `net.cpp:1163–1494`):
- **Detection:**
  - The responder reads at most 16 bytes in `KEY_MAYBE_V1`.
  - The prefix is `magic ‖ "version\0\0\0\0\0"` (`V1Header::version_prefix`).
  - On a mismatch the responder starts sending at once. On a full match it feeds the 16 bytes to the fallback, clear-shrinks both buffers, and returns with the remaining input unconsumed.
  - The wrong-magic special case checks bytes 4–16 only once the buffer holds ≥ 16 bytes, so it needs a later `KEY` step, as in Core. `responder_wrong_network_needs_a_key_step` covers this.
- **Garbage:**
  - Garbage is read one byte at a time, and only the last 16 bytes are compared, so the first match wins.
  - The connection fails exactly at 4095 + 16 bytes.
  - Garbage minus the terminator becomes the AAD.
- **Packets:**
  - The length check against `1 + 12 + min(MAX_SIZE, 3 MiB)` happens at the 3-byte descriptor, before any body byte is accepted.
  - The AAD is consumed by the first authenticated packet, decoys included. A decoy never changes state.
  - The VERSION contents are ignored.
  - `APP_READY` blocks further input.
  - A decryption failure is terminal.
  - `GetInfo` reports `detecting` until the version packet verifies.
- **Message types:**
  - `message_type` equals `GetMessageType`. Empty contents are dropped. IDs outside `[0,33) ∪ [128,169)` are dropped. Slots 5 and 29–32 give the empty type. A long type allows 0x20–0x7f (0x7f included) up to the first NUL, then requires NULs only; 13 zero bytes give the empty type.
  - The receive side has no version gate, as in Core.
- **Send side:**
  - `ShortId::for_send` maps the empty type to 5. This is Core's `V2MessageMap` emplace order: the first empty slot wins.
  - The gate is `peer_version >= 70235` (`platformban`: 70240).
  - `m_peer_version` starts at 209.
  - Core's `SetPeerVersion(greatest_common_version)` (`net.h:1047`) is what `set_peer_version` documents.
  - The version packet is empty contents with our garbage as AAD, written after the terminator. No decoys are ever sent.
  - `GetBytesToSend` `more`, `MarkBytesSent`, the 24-byte `m_sent_v1_header_worth` flag and `ShouldReconnectV1` match exactly.
- **Accounting:**
  - `send_memory_usage` = 24 + `MallocUsage(capacity)`, with libstdc++'s `_M_check_len` growth (`size + max(size, n)`).
  - I traced both handshake orders: the buffer fully drained before the peer's key, giving capacities 16 then 36; and a partly sent key+garbage buffer, giving 2·(64+G).
  - V1's charge is `56 + MallocUsage(payload capacity)`, matching `CSerializedNetMsg::GetMemoryUsage`. The receive side reports `raw_size`, which is what Core's `m_msg_process_queue_size` charges.
- **V1** (`v1.rs` vs `net.cpp:694–882`):
  - The header is read in one step per call, magic before size.
  - The size limit is checked at the header.
  - The 256 KiB reserve-ahead is capped at the message size.
  - Checksum comes before command validity, and both drop the message while keeping the peer.
  - `Reset` runs on error and after every message.
  - `GetBytesToSend` `more` and `MarkBytesSent` clear-shrink as in Core.
  - `Transport::receive` is `CNode::ReceiveMsgBytes`'s loop.

**Crypto** (`cipher.rs` vs `bip324.cpp` and Core's `FSChaCha20`/`FSChaCha20Poly1305`):
- The ECDH argument order and party match `ComputeBIP324ECDHSecret`, using secp256k1's BIP324 hash and the static context. The salt is `"bitcoin_v2_shared_secret" ‖ magic`.
- The HKDF PRK is HMAC(salt, ikm), and expand computes HMAC(prk, info ‖ 0x01). The six labels match, and so does the terminator split: the initiator sends the first 16 bytes.
- The length is 3 bytes little-endian through `FSChaCha20`. Its rekey takes the next 32 keystream bytes, then sets nonce `{0, ++rekey_counter}`.
- The packet AEAD uses nonce `{packet_counter LE32, rekey_counter LE64}`. Its rekey uses block 1 at `{0xffffffff, rekey_counter}`.
- Both ciphers rotate after 224 calls, and a failed decrypt still advances the counter.
- The tag comparison is constant time (RustCrypto's `subtle`), as Core's `timingsafe_bcmp` is. The terminator comparison is a plain `==` in both.
- Core's BIP324 and FSChaCha vectors pass, and live interop passes against v24.

**DoS:**
- No allocation is driven by a peer-controlled length beyond received + 256 KiB, capped at 3 MiB + 33 bytes for V2 and 3 MiB for V1.
- The key/garbage stage reserves 4111 bytes once.
- Fragment-count independence (r1 F1) is restored by the capacity guard at `v2.rs:353`, which matches `net.cpp:1424`.
- **No panics from peer bytes:** I walked every index, `unreachable!`, `expect` and subtraction on the receive path. The descriptor guarantees `n ≥ 17` for `decrypt`; `max_bytes_to_process` cannot underflow because the buffer never exceeds `recv_len + 20` without being consumed; the `keys()` expects are reached only after `initialize`; and `initialize` runs once, because the state leaves `Key`.
- The remaining panics are documented caller contracts: `received_message` without a complete message, an oversize `mark_bytes_sent`, `with_keys` with garbage > 4095, and an outbound payload ≥ 2^24.

**`test-peer` isolation:**
- `encrypt_with_header` is `#[cfg(any(test, feature = "test-peer"))]` (`cipher.rs:408`).
- The feature is non-default.
- Its only enabler is node-fuzz, which is a separate workspace (`node-fuzz/Cargo.toml:11`) with its own lockfile.
- The root uses resolver 2, so a dev-dependency could not leak the feature into a normal build anyway.
- With default features, the `compile_fail` doctest runs and passes (`test-doc-default.log`).
- A default-build consumer cannot otherwise set reserved bits: `Keys` and the send ciphers are private, and `encrypt`/`encrypt_in_place` take only the ignore bool.

**L5:**
- The crate starts no threads: getrandom is a syscall, and secp256k1 uses a thread-local or static context.
- It has no globals and no network statics; the magic is passed per instance.
- `check-pure.sh` passes, with the crate in `node-pure`, which is stricter than L5.

## Deviations (judgement)

- **Core's sans-io `Transport` instead of #264's async handshake: accepted, and the better choice.** It gives byte-exact state transitions and removes #264's class of leftover-bytes desyncs after the handshake (memory note `dash-spv-v2-transport-pr-264`). It is also the interface SB6 drives.
- **RustCrypto ChaCha20 and ChaCha20-Poly1305, secp256k1 0.33 ElligatorSwift and dash-sha256 HKDF: accepted.** Core's AEAD is RFC 8439 exactly, and the vectors cover rekeys at 224 and the nonce layout.
- **The local `message_type`: accepted and necessary.** Core delivers 5 and 29–32 as the empty type; I verified this above.
- **opt-level 3 dev profiles: accepted.** They are dev only; release is unaffected.
- **Best-effort key wipe: accepted.** `SecretKey` is `Copy`, so stack copies remain. The ECDH secret, PRK, rekey material and the RustCrypto cipher states are zeroized (`chacha20`/`chacha20poly1305` with `zeroize` implement `Drop`). This is no weaker than Core's `memory_cleanse` coverage in practice.
- **v24-only interop: accepted under DEC-82.** Sol r1 also passed against 23.1.

## Findings

No blocker or major findings.

### Minor M1 — The `node-pure` CI group runs the dashd interop tests without provisioning a v24 dashd

**Location:** `.github/ci-groups.yml:36` (the crate is in `node-pure`), `.github/workflows/build-and-test.yml:56–85` (dashd setup only for `spv`/`ffi`, and v24 only for `node-app`), and `dash-p2p-transport/tests/dashd_interop.rs:46–58`.

**Scenario:**
- Once the base-sdk pin is public and the GitHub Cargo jobs run again (BASE-SDK-FORK.md, "leaving agentbox"), the `node-pure` job runs `cargo test -p dash-p2p-transport --all-features`.
- That job has no `DASHD_V24_PATH`, no setup-dashd cache and no `SKIP_DASHD_TESTS`.
- `dashd_binary()` reaches `.expect("no dashd: …")`, and all 5 interop tests fail.
- Since `3a6091e41` there is also no `DASHD_PATH` fallback, although node-pure never exported one either.
- This is latent today: the agentbox gate's `cargo test --workspace` finds `~/.rust-dashcore-test/dashcore-24.0.0-rc.3`, and the GitHub jobs are skipped.

**Fix:** extend the `node-app` v24 setup step (`DASHVERSION: 24.0.0-rc.3`, exporting `DASHD_V24_PATH`) to `node-pure`, or move the crate to a group that has it. Then a missing dashd stays a loud failure where it is expected.

### Nit N1 — The isolation guard is invisible to `--all-features` and disables itself under feature unification

**Location:** `cipher.rs:373–390`.

**Scenario:**
- The `compile_fail` doctest exists only under `not(feature = "test-peer")`.
- `ci_config.py run-group` uses `cargo test -p <crate> --all-features`, so GitHub CI never runs it; with `--all-features` only one doctest is reported.
- More importantly, suppose a future root-workspace crate (say SB6) declared `dash-p2p-transport = { features = ["test-peer"] }`. Then the gate's `cargo test --workspace` would unify the feature on, the guard would silently disappear, and nothing would fail. That is exactly the leak it exists to catch.

**Fix:** add a cheap metadata check, for example in `check-pure.sh` or `ci_config.py verify-groups`, that no root-workspace package enables `dash-p2p-transport/test-peer` in any dependency kind. Optionally also run `cargo test -p dash-p2p-transport --doc` with default features as an explicit step.

### Nit N2 — Driver contracts SB6 must carry, in addition to Sol r1's list

Neither item is a P1 defect.

- **(a) Messages completed before a `TransportError`.** `Transport::receive` hands these out ("Messages completed before the bad bytes have been passed on"). Core also queues them (`notify` survives a failed `ReceiveMsgBytes`, `net.cpp:2608–2617`), but `CloseSocketDisconnect` sets `fDisconnect`, and `ProcessMessages`/`ThreadMessageHandler` skip such nodes. So they are never processed. SB6 must drop them, or a peer can get one last message processed after it violates the protocol, which dashd would not do.
- **(b) When to call `set_message_to_send`.** Call it at socket-send time (`SocketSendData`), not at push time. Then short-ID gating sees the common version that applies when the message is encoded, as in Core, where `verack` pushed during `version` processing is encoded after `SetCommonVersion`.

### Nit N3 — The garbage-length KS test only catches gross deviations

**Location:** `dashd_interop.rs:455–468`.

With N = 1000 and a 1e-6 false-failure rate, the one-sample bound is D ≤ 0.085. For example, a range off by about 300 values (0…3800, D ≈ 0.072) would pass. The generator is evidently uniform by inspection: the low 12 bits of a uniform `u16`, at `v2.rs:85`, so this is acceptable as an interop smoke test. A deterministic unit test pinning the mask and the 4095 bound would catch a regression the KS test cannot. This run's statistics were 0.0420 / 0.0497 / 0.0200, within bounds.

## Prior findings

- **r1 F1:** closed. The guard matches `net.cpp:1424`, and `large_packets_grow_the_buffer_in_steps` and `large_packet_reserves_ahead_of_what_arrived` pass.
- **r1 F2:** closed. `sanitize_command` is `SAFE_CHARS_DEFAULT` up to the first NUL, and `Debug` escapes.
- **r1 F3:** closed per r2. The model's 7 unit tests pass on the rebased head, which dropped `Release` in the RP-V24 adaptation.
- **r2 R2-1:** closed. See "`test-peer` isolation" above.

## Validation (this review)

- `DASHVERSION=24.0.0-rc.3 setup-dashd.py`, `DASHD_V24_PATH` set, then `cargo test --locked -p dash-p2p-transport --all-features -- --nocapture`: **42 passed / 0 failed** (7 lib, 3 Core vectors, 5 interop, 8 V1, 19 V2), plus 1 doctest. The interop ran against dashd **240000**, with KS **0.0420 / 0.0497 / 0.0200**. Log: `/work/scratch/p1-opus/test-all.log`.
- `cargo test --locked -p dash-p2p-transport --doc` (default features): **2 passed**, including `encrypt_in_place (line 376) - compile fail ... ok`. Log: `test-doc-default.log`.
- `cargo clippy --locked -p dash-p2p-transport --all-targets -- -D warnings`, with default and all features: **clean**. `cargo fmt --check -p dash-p2p-transport`: **clean**.
- `contrib/node/check-pure.sh`: **4 crates ok**, `EXIT 0` (`check-pure.log`).
- node-fuzz `cargo test --locked --lib models::p2p_transport_model::tests` with `RUST_MIN_STACK=33554432` (the debug-stack note from r2): **7 passed**, including `scripted_faults_and_cuts` and `scripted_large_packet_in_fragments` (`fuzz-model-tests.log`).
- I did not rerun the fuzz campaign or the PF0 benches. Sol's r1/r2 runs cover the campaign. Since r2's head, the transport sources changed only by the hook gating and its moved unit test.
- Independent short-ID table extraction: **74/74 match**.
- No dashd was left running, and the scratch TMPDIR is empty.

## Verdict

**APPROVE-WITH-NITS.** The V1 and V2 state machines, the cipher suite and the short-ID handling are byte-for-byte faithful to Core v24 on every peer-observable decision I checked. Peer bytes cannot drive unbounded allocation or a panic, and `test-peer` cannot reach a default build. M1 is a latent CI-provisioning gap, not a transport defect. Fix it before the GitHub Cargo jobs are re-enabled. N1–N3 are optional hardening and SB6 hand-off notes.
