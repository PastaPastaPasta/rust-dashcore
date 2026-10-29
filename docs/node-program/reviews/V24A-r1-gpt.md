# V24A independent review, round 1

Reviewed `node/v24a-dashcore-wire` at `a22c7138ada57acb8b4dd634d7a3968e3791a57a`, using `git diff node/integration...HEAD`. Read `PROCESS.md` and the review brief; inspected upstream PRs #1137, #1136 and #1138 through GitHub REST. Reference: Dash Core `v24.0.0-rc.3` (`da567d7dba945143eee4d2ce1621c21b32089989`). The relevant Core source was also compared with the available compiled v24 checkout before using its libraries for a C++ probe.

No branch files were modified and no commit was made. Review probes and logs are under `/tmp/V24A-r1-gpt-probe` and `/tmp/V24A-r1-gpt-*.log`.

## Findings

### Major M1 — NetInfo preserves wire map order in the provider signing hash, unlike Core

**Locations:** `dash/src/blockdata/transaction/special_transaction/provider_update_service.rs:175`, `dash/src/blockdata/transaction/special_transaction/provider_registration.rs:440`, and `dash/src/sml/masternode_list_entry/net_info.rs:213` / `:244`.

Core deserializes `ExtNetInfo::m_data` into a `std::map`. `UnserializeMap` inserts entries, retaining the first value for a duplicate key; serialization sorts the surviving keys. Its provider payload signing hash therefore uses this canonical map. Rust keeps every purpose pair in a `Vec` and writes the original order, including duplicate keys, both for transaction preservation and `base_payload_hash()`. The transaction-level re-encoding check compares Rust with itself, so it does not catch this disagreement.

**Concrete scenario:** reorder the three purpose records of the committed `rpc_netinfo@269` ProUpServTx, or append a duplicate of its first purpose record. A transaction using Core's canonical provider signature and appropriately signed funding inputs has the same effective service configuration and provider signing digest as before; Rust's typed provider digest changes. This can make signing or signature verification disagree with Core. Preserving the original transaction bytes/txid does not require changing the semantic payload signing digest.

**Observed proof:** a C++ probe calling `GetTxPayload<CProUpServTx>` and `SerializeHash(*payload)` produced the same hash for the original, reordered and duplicated maps:

`87aff09b931033881a09c6c51ad3bcbdc2aabd2bacdfdc7f9948589101228c01`

Rust returned typed payloads for all three, but its reordered digest was `721783929ccd152be38bdc4a29ec96359b4ce6eec5b5e087a90bbb9127f951b8`, and its duplicate-key digest was `0d228e74c12218455ff7de5df6a6921c0bfadc69d8f09104c27d6f82dad8a63c`. This was a payload/hash differential probe, not a rebroadcast of the mutated transactions.

**Fix:** distinguish raw transaction preservation from Core's effective typed map and signing serialization. Match Core's key ordering and first-key-wins behavior when hashing/signing provider payloads. Preserve raw bytes separately when canonical serialization differs. Add differential tests for both cases; simply rejecting all noncanonical maps as consensus-invalid would also disagree with Core.

### Major M2 — The newly reused NetInfo decoder omits Core's address normalization and domain size limit

**Locations:** `dash/src/sml/masternode_list_entry/net_info.rs:132` and `:139`, reached by the new v3 paths at `dash/src/blockdata/transaction/special_transaction/provider_update_service.rs:231` and `provider_registration.rs:514`.

The Rust decoder checks service address lengths but accepts their contents, and reads domains with the generic `String::consensus_decode` without the 253-byte domain bound. Core uses `LIMITED_STRING(..., 253)`, then checks `DomainPort::IsValid()` and `CService::IsValid()` in `NetInfoEntry::Unserialize`. It clears semantically invalid entries to `Invalid`; oversized domain bodies throw. The helper predates this branch, but these new transaction decoders now depend on it for v24 payload semantics.

**Concrete scenarios and proof:** mutate the same Core vector to contain an uppercase domain, an underscore in a domain label, IPv4 `0.0.0.0`, or a 254-byte domain. The release `dash-tx` marks the first three entries `[invalid entry]`; its typed payload extraction fails for the 254-byte domain. Rust returns a typed `ProviderUpdateServicePayload` containing the supplied domain/service in every case, and its re-encoding equality check passes. Core's later NetInfo validation rejects the invalid entries; the oversized case fails provider payload extraction. IPv6 mapped/unspecified/documentation addresses and invalid CJDNS prefixes also need Core-equivalent handling, beyond length checks.

**Fix:** enforce the 253-byte domain bound before allocation and reproduce Core's entry validation/normalization, including domain syntax, lowercase and port requirements and the service address rules. Keep original bytes through the raw payload path when normalization changes serialization. Add negative Core differential vectors. This is separate from implementing all contextual ProTx consensus validation.

### Major M3 — Raw known provider payloads break the FFI tag/pointer contract

**Locations:** `dash/src/blockdata/transaction/special_transaction/mod.rs:555`, `key-wallet-ffi/src/special_payload.rs:339` / `:360`, and `key-wallet-ffi/src/managed_account.rs:950`.

The new fallback can produce `UnknownPayloadType(2, bytes)` for a known but truncated, trailing or future-version ProUpServTx. The FFI converter exposes `payload_type == 2` with every provider pointer null. The exported struct explicitly promises that `provider_update_service` is non-null iff the tag is 2 (`special_payload.rs:328`), and its module tells consumers to key off the tag. `FFITransactionRecord` publishes this object unconditionally for any `Some(payload)`.

**Concrete scenario:** a wallet record containing a raw known-type provider transaction reaches a C/Swift consumer that switches on the documented tag and dereferences the corresponding pointer. It can null-dereference. The converter probe confirmed `payload_type=2, provider_update_service_is_null=true`; an actual foreign-client crash was not exercised. The C struct sizes remain unchanged, but that does not preserve this semantic contract. The older non-special raw-type variant could already violate it for colliding values; this branch makes it directly reachable through known provider payload decoding.

**Fix:** keep raw/unparsed provider payloads out of the existing typed FFI view, or introduce an explicit unparsed state with checked accessors and a documented consumer migration. Raw bytes are already available through transaction data. Preserve `repr(C)` layouts and add a regression test through `FFITransactionRecord`, not just a typed payload constructor.

### Minor m1 — Special transaction size/weight still counts witness bytes that encoding now omits

**Locations:** `dash/src/blockdata/transaction/mod.rs:485` and `:625`.

**Scenario/proof:** add a three-byte witness item to the input of the committed v3 ProRegTx at `feature_masternode_payout_shares@121`. Encoding correctly stays at 506 bytes, while `size()` reports 513 and `weight()` reports 2031 instead of `506 * 4 = 2024`. Network-decoded special transactions have empty witnesses, so this affects constructed objects rather than the supplied valid vectors.

**Fix:** make size/weight use the same witness-framing eligibility rule as encoding, and add one special-transaction witness regression test.

### Minor m2 — Coverage does not require qrinfo or the full set of address/layout cases

**Locations:** `dash/tests/core_v24_vectors.rs:425` and `dash/contrib/core-vectors/p2p_capture.py:64`.

The coverage guard requires type/version pairs, but not each address family, Evo/regular form, share/signature-count form, or valid/invalid boundary. It would remain green if regeneration lost Tor/I2P/domain or Evo coverage. There is no Core-produced `qrinfo` response vector, although that response embeds the changed CbTx and masternode-entry formats. The `getqrinfo` cap has a useful Rust boundary test, but no Core differential vector.

**Fix:** assert the required layout/address coverage explicitly; capture and compare a Core `qrinfo` response, ideally with both extra-share settings, and add malformed-payload/address fixtures covering M1/M2.

### Minor m3 — Regeneration can succeed after failed functional tests and mislabel the binary

**Locations:** `dash/contrib/core-vectors/run-functional-tests.sh:45` and `dash/contrib/core-vectors/extract.py:191`.

**Scenario:** one background functional test fails, or the caller supplies binaries from another release. Bare `wait` does not aggregate child failures, and the extractor hardcodes the rc.3 generator string without checking the running binary. A partial or differently generated fixture can consequently carry the expected provenance label. This is a tooling issue; the current fixture's provenance was independently corroborated below.

**Fix:** retain child PIDs and propagate failed statuses; verify/record the actual binary version and source tag/commit instead of hardcoding the generator label.

## Required behavior checks and compatibility

**Malformed known payloads:** Core transaction/block deserialization reads `vExtraPayload` as a length-prefixed byte vector; it does not deserialize every typed payload there. `src/evo/specialtx.h:18` catches typed parse exceptions and requires complete consumption. Later, `GetValidatedPayload` in `specialtxman.cpp:1100` rejects missing provider payloads as `bad-protx-payload`; CbTx has `bad-cbtx-payload`, and other known types have their corresponding validation paths. `ProcessSpecialTxsInBlock` propagates transaction failures as block consensus failures. Thus the whole block can deserialize successfully and still be invalid. A missing/truncated outer byte vector remains a deserialization failure.

Core's `GetTxPayload` does **not** require re-encoding equality. That distinction matters for canonicalizing maps/addresses (M1/M2). It also short-circuits unknown provider versions after reading the version; such an object can extract successfully from a two-byte payload but subsequently fails version/trivial validation. The changelog's assertion at line 44 that these versions fail decoding “as in Dash Core” should distinguish extraction from validation.

The raw fallback is reasonable for an SPV/wallet **parser**: it preserves transaction boundaries and hashes, and allows future payloads to coexist with ordinary wallet inputs/outputs. It is not proof of transaction/block validity. The future node, which will use base-sdk types, must separately reproduce Core's strict validation and rejection reasons; neither an `UnknownPayloadType` nor a successfully decoded typed payload establishes consensus acceptance. Truncated and trailing inner payload probes correctly stayed raw and byte-identical in this branch.

**Asset unlock hashing:** the stable predicate matches Core: signed transaction version at least 3, raw type 9, exactly 145 payload bytes and first payload byte at least 2. Typed unlock payloads always serialize to 145 bytes; the raw branch explicitly checks that size. Zeroing the final 132 bytes, full `instance_hash()`, and the block-order asset-unlock instance-hash merkle root match Core and the supplied vectors. Inventory 34 and the two protocol thresholds match the reference. Keeping advertised `PROTOCOL_VERSION` at 70237 is intentional.

**Platform dependency:** `platform/packages/rs-drive-abci/src/platform_types/withdrawal/unsigned_withdrawal_txs/v0/mod.rs:164` still uses `tx.txid()` as the threshold vote-extension message. With a v2 payload that would sign the stable hash, whereas Core `assetlocktx.cpp:222` verifies the full serialization with only `quorumSig` zeroed. Platform must use the unsigned transaction's `instance_hash()` before enabling v2 withdrawals. Its inspected withdrawal builders currently emit payload version 1, so this is a conditional downstream migration requirement, not a current v1 regression or a reason to revert the correct txid implementation. The changelog already warns signing callers; an integration test is still needed before v2 rollout.

**i16 parsing:** reading one 32-bit header, interpreting its lower 16 bits as signed for special-version gating, retaining the upper 16-bit type, and reproducing Core's signed `nVersion | (nType << 16)` serialization matches Core's struct model.

**Persistence/ABI:** existing `TransactionPayload` and `TransactionType` variants retain their order; new variants are appended. The hand-written native bincode implementations retain the legacy field layout below the new version gates. External probes compared legacy provider/CbTx enum-and-field encodings with the current native bincode bytes and decoded them with exact consumption; sampled legacy version-2 payloads passed. Old JSON maps with the added fields removed also decoded successfully (the ProUpServ JSON probe used zero IP to avoid its pre-existing `u128`/JSON-Value limitation). No complete historical wallet backup corpus or arbitrary binary serde format was tested. FFI `repr(C)` field layouts are unchanged, but M3 remains; its v3 views also omit extended addresses/payout lists/shares, so legacy zero/empty fields must not be interpreted as the new values.

**Positive wire coverage:** supported ProReg/ProUpServ/ProUpReg/ProUpRev version gates, payout/share u8 counts, fixed 65-byte signatures, signature exclusions in the added update signing hashes, Evo node IDs/port removal, CbTx v4 and asset lock/unlock layouts agree on the valid Core vectors. No additional mismatch was found in types 10–12 or the compact-size payload prefixes. Wallet payout matching scans the new payout/share scripts and deduplicates them; relevant functional tests passed.

**Vector provenance:** all 69 committed transaction hex strings were found byte-for-byte in the retained functional-test Core `blk*.dat` files under `/tmp/v24a/ft`; debug logs identify `v24.0.0-rc.3 (release build)`. All 69 transactions and 35 blocks also match the retained raw RPC extraction. The fixtures have strong positive coverage, including field comparisons, payload ECDSA signature recovery, txids, instance hashes, block merkle roots, asset-unlock commitments and mnlistdiff roots. They do not establish adversarial acceptance parity or exercise all signature verification paths.

## Validation and timing-test recommendation

- `cargo test -p dashcore`: passed.
- `cargo test -p dashcore --test core_v24_vectors -- --nocapture`: all 5 tests passed.
- `cargo test -p dashcore --all-features`: passed, including 612 unit tests, all 5 vector tests, the other integration tests and doc tests.
- `cargo test -p key-wallet -p key-wallet-ffi --all-features`: key-wallet had 662 passing unit tests and 3 timing-only failures: wallet recovery, wallet serialization and worst-case derivation. This failed before the FFI suite ran.
- `cargo test -p key-wallet --all-features performance_tests -- --test-threads=1`: all 8 passed.
- `cargo test -p key-wallet --all-features -- --skip tests::performance_tests`: 657 unit tests and the integration/doc suites passed.
- `cargo test -p key-wallet-ffi --all-features`: passed, including 245 unit tests and its integration/doc suites.
- `git diff --check node/integration...HEAD`: passed. The reviewed branch remained clean at the same HEAD.

The brief reports four performance failures under load; this run reproduced three. The fixed elapsed-time assertions at `key-wallet/src/tests/performance_tests.rs:151`, `:261`, `:300` and `:328` depend on scheduling/host contention. Keep their workload/correctness coverage, but move timing thresholds to dedicated controlled benchmarks or an explicitly opt-in performance job. Do not keep wall-clock thresholds as mandatory parallel correctness gates or repeatedly raise them to accommodate contention. These tests are pre-existing and are not evidence of a v24 parser regression.

## Verdict

Blockers: none identified. Resolve M1–M3 before approval; the remaining findings and conditional Platform rollout dependency are described above.

**CHANGES-REQUIRED**
