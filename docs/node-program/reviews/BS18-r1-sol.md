# BS18 independent review, round 1 (Sol)

Scope: `c68b1c3e4..069d97bc8` on `node/bs18-spv-protocol`; seven commits, each verified `G` signed. Reviewed in a detached scratch worktree at `069d97bc8` against the `v24.0.0-rc.3` Dash Core tag (`0364ed626e`). This file records review findings only.

## Findings

1. **Medium — the advertised 32 MiB range is narrower on streaming vectors and on strings.** `dash/src/consensus/encode.rs:391,472,621-625,823-833`. `read_compact_size_len` accepts lengths through 32 MiB, but `Vec<u8>::consensus_decode` wraps the reader in a 4,000,000-byte `Take`; `String` delegates to that path even when called through `deserialize`. A complete 4,000,001-byte vector succeeds with `deserialize::<Vec<u8>>` (which deliberately calls the finite-reader method) but fails on the streaming method; the same length as a `String` fails even through `deserialize`. Core's `src/serialize.h:292-321,1047-1053,1133-1150` checks the 32 MiB range and reads the full contents in chunks. **Probe:** review-only integration test for both entry points at 4,000,001 bytes. **Fix:** separate the allocation safety cap from the total readable byte budget, and test complete vectors and strings at both boundaries. The P2P 3 MiB message cap is a separate limit.
2. **Medium — public weight prediction still counts BIP141 witness data.** `dash/src/blockdata/transaction/mod.rs:650-875`. `Transaction::weight()` now equals four times its serialization size, while `predict_weight` still adds `prediction.witness_size` and marker/flag bytes. **Core reference:** `src/primitives/transaction.h:258-290` serializes no witnesses. **Probe:** a transaction with one in-memory witness has `weight() == 4 * size()` but its `InputWeightPrediction` produces a different weight. **Fix:** make the predictor ignore witness lengths for Dash, or remove/deprecate the witness-based API and add a regression test. No in-workspace `key-wallet` or `dash-spv` fee caller uses this predictor; their fee calculations use explicit size estimates.
3. **Medium — the transaction fuzz target has an obsolete zero-input assertion.** `fuzz/fuzz_targets/dash/deserialize_transaction.rs:13-24`. It subtracts eight weight units for a presumed marker/flag. A Core-style empty-input transaction now has no marker/flag and `weight == 4 * serialized_len`, so the target reports a false crash. **Core reference:** `src/primitives/transaction.h:347-365` reads and writes `vin`, then `vout`, with no witness branch. **Probe:** the ten-byte `01 00 00 00 00 00 00 00 00 00` transaction is a valid serialization and the old assertion differs by eight. **Fix:** assert the new size/weight relationship for every successfully decoded transaction.
4. **Low — `String` remains narrower than Core's byte string.** `dash/src/consensus/encode.rs:621-626`. Rust decoding rejects `[01, ff]` as invalid UTF-8; Core's `src/serialize.h:1047-1053` reads the byte into `std::string`. This predates BS18 but is an observable protocol-domain gap, including string-bearing messages. **Probe:** review-only `deserialize::<String>(&[1, 0xff])`. **Fix:** use a byte-preserving wire string in Core-facing fields, then sanitize only for display.
5. **Low — the changelog and `wtxid()` docs contradict the code.** `CHANGELOG.md:102-111`, `dash/src/blockdata/transaction/mod.rs:175-191,295-304`. The changelog first says dash-spv's sent version changes to 70242 and later says it remains 70237. The txid and wtxid docs say both hashes are equal for all transactions, but stable-txid asset unlocks hash a zeroed quorum tail for `txid()` while `wtxid()` hashes the complete encoding; Core calls the latter `GetInstanceHash()` (`src/primitives/transaction.h:293-310`). **Probe:** `dash/tests/core_v24_vectors.rs` already verifies `instance_hash != txid` for version 2 unlocks. **Fix:** correct the changelog sentence and describe `wtxid()` as the full-serialization hash, or direct callers to `instance_hash()`.
6. **Low — a syntactically extra extraction row can escape the Rust table tests.** `dash/tests/core_v24_protocol_tables.rs:68-178`, `contrib/node/core-protocol-tables.py:183-196`. The checked-in table is authentic today (`--check` matched byte for byte), and all actual version, inventory, service-flag and opcode values are exercised. The tests do not assert the number of `inventory.commands` or `service_flags.names` rows, and do not compare extraction metadata or source line numbers. **Probe:** adding `["MSG_FAKE", "fake"]` to `inventory.commands` left `inventory_commands_and_strings_equal_core` green, while the extractor's `--check` failed; the file was restored. **Core reference:** `src/protocol.cpp:281-328,345-359`. **Fix:** check command/name row sets exactly and run the extractor's `--check` in a gate with the pinned Core source.
7. possible security note: see manager — `dash/src/bip152.rs:240-295`.
8. possible security note: see manager — `dash/src/network/message_headers2.rs:596-605`.

## Verified behavior

- `core-protocol-tables.py --core ~/workspace/dash --check` regenerated the checked-in JSON byte for byte. The script pins the peeled tag commit and extracts all 21 `version.h` constants, all 20 `GetDataMsg` members, all seven service flags (six named bits plus `NODE_NONE`), both limits and sites, and all 256 `GetOpName` byte results. Rust tests compare all values, every known and unknown inventory rendering (including `0x%08x`), all 64 service bits and all 256 opcode names. An unused extra command/name row or metadata edit is the exception noted above.
- Core's `CMutableTransaction::SERIALIZE_METHODS` reads `vin`, `vout`, lock time, then optional special payload. In the reviewed code, `00 00` after the version means empty inputs and outputs; `00 01` means empty inputs and one output, not a BIP144 marker/flag. A complete former BIP144 encoding is interpreted through Core's ordinary fields, rather than as witness data.
- `RawNetworkMessage` reads and rejects an announced payload above 3 MiB before payload bytes (`dash/src/network/message.rs:497-500`, `dash/src/consensus/encode.rs:875-887`), matching Core's v1 header check at `src/net.cpp:738`. The existing integration tests accept exactly 3 MiB and reject 3 MiB plus one. Plain `headers` counts use `read_compact_size_len`; the separate compressed count is noted above.
- `Inventory` preserves unknown `u32` values and hashes, and `Display` renders unknowns as `0x%08x` plus reversed hash bytes. `ServiceFlags` renders every unknown bit as `UNKNOWN[2^N]` in ascending bit order. The table tests cover every named case.
- All `Transaction` encoders reached from dash-spv, key-wallet, both FFI crates, RPC, and fuzz use the no-witness serializer. `HeaderAndShortIds::from_block` accepts only compact-block version 1. Core transaction vectors in `dash/tests/core_v24_vectors.rs` cover at least 60 special transactions and compare `size()` with Core's size; their serialized bytes also round-trip exactly. `Block::weight()` sums `4 * base_size` and transaction weights. `key-wallet` fee calculations use byte estimates, and no dash-spv fee calculation assumes a witness discount.

## BL-112 disposition

| Item | Disposition |
|---|---|
| `NetworkMessage::WtxidRelay`, dash-spv's `MessageType::WtxidRelay` | Stale control-message surface, with no witness payload or active dash-spv send path. Core ignores an unknown command. Remove later for API fidelity; no BS18 wire-witness path. |
| `UnsupportedSegwitFlag` | Unreachable error variant now that transaction decoding has no flag branch. Deferred API cleanup. |
| Ignored `segwit_block_test` | Historical Bitcoin fixture, ignored by default and incompatible with Dash's wire format. Remove or replace later; no production path. |
| Segwit commitment helpers | Public helpers can inspect in-memory witness fields, but a decoded Dash block never gets witnesses from the wire. Deferred API/documentation cleanup. |
| Standalone `Witness` codec and `fuzz/fuzz_targets/dash/deserialize_witness.rs` | Still produce and consume a witness *blob*, but no transaction or block serializer calls that codec. Keep only as explicit legacy API/fuzz coverage or remove it later; it cannot put BIP144 on the Dash wire. |
| BIP152 raw `VarInt` | possible security note: see manager — `dash/src/bip152.rs:240-295`. |

## `PROTOCOL_VERSION` use audit

| Use | Value to use and reason |
|---|---|
| `dash/src/network/constants.rs:46`; `dash/tests/core_v24_protocol_tables.rs:35,65` | Library 70242; definition and Core parity test. |
| `dash/src/network/message_network.rs:80` (`VersionMessage::new`) | Library 70242 for generic callers. dash-spv builds `VersionMessage` directly and overrides this field. |
| `dash/src/network/message_blockdata.rs:369,382` (`getblocks`, `getheaders` constructors); `dash-spv/src/network/mod.rs:114,126` callers | Library 70242 is harmless: Core's `CBlockLocator` reads the field but `net_processing.cpp:4632,4756` uses only the locator hashes and stop hash. |
| `dash-spv/src/network/handshake.rs:23,59,250-300` | Advertised 70237 is used by the sole production `version` sender, per BL-111. |
| `dash-spv/tests/header_dispatch_order.rs:9,31` | Library value represents the fake peer in a test, not SPV's outgoing version. |
| `masternode-seeds-fetcher/src/main.rs:270-282`, `src/probe.rs:59` | Generic `VersionMessage::new` uses library 70242; this is a separate client and is not the dash-spv exception. |
| `dash-p2p-transport/tests/v2_transport.rs:78,147,691-692,790` | Local Core-v24 test fixture 70242; not an SPV advertisement. |
| `node-l1/src/overlay.rs`, `node-l1/tests/support/overlay_corpus.rs`, `dash-dml/src/unique.rs`, `dash-oracle-sys/src/mnlist.rs` | Documentation/test prose referring to library serialization version; no handshake write. |

No dash-spv production code gates on the library `PROTOCOL_VERSION`. Headers2 is negotiated through `NODE_HEADERS_COMPRESSED`; Core's increased 8000-header gate is 70235, QRInfo's efficient gate is 70236, mnlistdiff gates are at most 70230, and ISDLOCK's cycle-hash gate is 70237. Advertising 70237 remains internally consistent for those paths. Core may still announce ordinary balanced DSTX as `MSG_DSTX` at 70237; that is an existing mempool limitation, while the exception prevents the new rebalance DSTX and version 2 asset-unlock announcements.

## Mutation checks

| Mutant, applied alone and restored | Kill |
|---|---|
| 3 MiB → 5,000,000 | **Killed** by `message_size_limits_equal_core` (3,145,728 expected). |
| Accept BIP144 marker/flag again | **Killed** by `bip144_encoding_is_not_read_as_segwit`. |
| `MSG_ASSET_UNLOCK` 34 → 35 | **Killed** by `inventory_type_table_equals_core`. |
| Drop `P2P_V2` service-flag name | **Killed** by `service_flags_equal_core`. |
| `OP_CHECKDATASIG` → `OP_CHECKSIGADD` | **Killed** by `opcode_names_equal_core_for_every_byte`. |
| `SPV_ADVERTISED_PROTOCOL_VERSION = PROTOCOL_VERSION` | **Killed** at compile time by the handshake test's constant assertion. |
| `size()` counts an in-memory witness | **Killed** by `witness_is_never_serialized` (100 vs 61 bytes). |

## Validation

- Baseline `cargo test -p dashcore`: passed.
- Baseline `cargo test -p dash-spv` with `eval "$(python3 contrib/setup-dashd.py)"`: passed.
- Baseline `cargo test -p key-wallet-ffi`: passed.
- Baseline `cargo test -p dash-spv-ffi --test dashd_sync`: passed (7/7).
- Review-only decoding and weight probes: 7/7 passed. The first probe expectation was corrected after observing that `deserialize::<Vec<u8>>` deliberately bypasses the streaming 4 MB `Take`; both entry points were then tested.
- Seven mutants were applied one at a time; each was killed, then the original file restored and touched. `git diff --exit-code` and the extractor's `--check` passed after restoration. The implementer's worktree and branch were not changed.
- Extra table-tamper control: an unused command row survived the Rust test but failed the extractor's `--check`; the JSON was restored.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
