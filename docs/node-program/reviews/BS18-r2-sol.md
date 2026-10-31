# BS18 independent review, round 2 (Sol)

Scope: `069d97bc8..c27748295` (seven commits, all `G` signed), reviewed at `c27748295` in a detached scratch worktree. Core reference: peeled `v24.0.0-rc.3` at `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. This is review only; the implementer's branch and worktree were untouched.

## Closure of round 1 findings

| Finding | Result | Evidence |
|---|---|---|
| F1, 32 MiB vector and string range | Closed | Both `deserialize` and streaming decode accept complete 4,000,001-byte and 32 MiB `Vec<u8>` and UTF-8 `String`; both refuse 32 MiB + 1 at the length. `Vec<T>`, `Box<[u8]>`, and `Cow<str>` follow the same range. Independent probe and `core_v24_protocol_tables` pass. See memory analysis below. |
| F2, witness weight | Closed | `predict_weight` ignores witness lengths and returns four times the serialized size. Independent witness regression and library test pass. |
| F3, fuzz assertion | Closed | The ten-byte empty transaction has size 10 and weight 40; the fuzz target's zero-input assertion passes with `--cfg fuzzing`. |
| F5, docs | Closed | The changelog distinguishes library 70242 from dash-spv's advertised 70237; `wtxid()` is documented and tested as the full-serialization/instance hash, including the asset-unlock exception. |
| F6, exact table rows | Closed | Inventory command and service-flag name rows are compared exactly; extra-row probes fail. Provenance and JSON shape are checked. The pinned extractor `--check` passes. |
| F7, inbound BIP152 indexes | Closed | possible security note: see manager — `dash/src/bip152.rs:78-91,181-196,307-337`. The outbound encoder has a separate finding below. |
| F8, headers2 count | Closed | possible security note: see manager — `dash/src/network/message_headers2.rs:596-608`. |

F4 is deferred to BL-113 as ruled and is not reconsidered here.

## F1 memory analysis

The 32 MiB CompactSize limit bounds each announced vector/string length. `Vec<u8>` extends in 128 KiB chunks. Generic `Vec<T>` initially reserves at most `MAX_VEC_SIZE / 4` bytes (about 1 MiB) regardless of the announced count, then grows only as elements decode. A 32 MiB announcement with no body caused a maximum single allocation request of 131,072 bytes for `Vec<u8>` (5 bytes supplied), 999,984 bytes for `Vec<Vec<u8>>` (10 bytes, including an inner 32 MiB announcement), and 999,960 bytes for `Vec<Transaction>` (14 bytes, including an input-vector announcement). All returned an I/O error on both entry points. Nested generic vectors can hold their per-level reservations at once (about 2 MiB in the transaction example); a short stream cannot force an allocation proportional to every announced 32 MiB length. Fully supplied sequences can have in-memory element overhead greater than their wire bytes.

Core's `MAX_VECTOR_ALLOCATE` reserves up to 5 MB per generic vector batch, and its byte-vector reader grows by at most 5 MB per read; Rust's first allocation requests are smaller. Core's `std::string` resizes to the announced size at once, whereas Rust's `String` uses the chunked byte-vector path. P2P message payloads are independently capped at 3 MiB before decoding. No new unbounded short-stream allocation was found.

## Implementer note rulings

| Note | Ruling |
|---|---|
| (a) Version round-trip | Accepted. Core v24 `CTransaction::Serialize` and `CMutableTransaction::SERIALIZE_METHODS` write the version and type as one signed 32-bit word (`src/primitives/transaction.h:271,357-360`), and our encoder now does the same. The fuzz target still checks unchanged length, low version bytes, remaining bytes, size, and weight. |
| (b) Error text | Accepted. Rust rejects at Core's wire boundaries; exact `std::ios_base::failure` wording is not a protocol requirement. |
| (c) Streaming budget and chunks | Accepted for direct vector/string entry points. The 4 MB `Take` is removed there, 128 KiB byte chunks bound speculative allocation, and the P2P message layer remains at 3 MiB. |
| (d) Deprecated segwit predictors | Accepted. They are documented as inapplicable to Dash and deprecated while preserving source compatibility; the current predictor does not count witness data. |

## New finding

1. **Low — outbound `getblocktxn` permits an index Core cannot represent.** `dash/src/bip152.rs:281-303`. `BlockTransactionsRequest::indexes` is public `Vec<u64>`, but Core's field is `std::vector<uint16_t>` (`src/blockencodings.h:44-52`). The changed encoder rejects decreasing indexes and `u64::MAX` but accepts 65,536 and `u64::MAX - 1`. An independently encoded request with `[65_536]` is rejected by the Rust/Core-compatible decoder as `differential value overflow`; `[u64::MAX - 1]` is rejected at Core's CompactSize range. Check `idx <= u16::MAX` when encoding (or narrow the field), and test 65,535 versus 65,536 on the write path. This is an outbound correctness gap, with no newly accepted inbound value.

No new security-relevant finding was established.

## Mutation checks

Each mutation was applied alone in the detached worktree; its test failed by assertion. The source was restored from a saved copy and touched before the next mutation. `git diff --exit-code` and the extractor `--check` passed afterward.

| Mutant | Kill |
|---|---|
| Restore the 4,000,000-byte `Vec<u8>` streaming cap | **Killed** by `byte_vectors_and_strings_are_read_to_the_core_range`. |
| Count witness lengths in `predict_weight` | **Killed** by `predicted_weight_equals_weight_whatever_the_witness`. |
| Drop the 16-bit getblocktxn overflow check | **Killed** by `getblocktxn_absolute_index_is_bounded_by_u16`. |
| Read the headers2 count as an unchecked `VarInt` | **Killed** by `headers2_count_is_read_with_the_core_range`. |
| Re-add the getblocktxn count-by-transaction-size heuristic | **Killed** by `getblocktxn_accepts_the_request_core_accepts_with_50000_indexes`. |
| Add an extra service-flag name row | **Killed** by `service_flags_equal_core`. |
| Round 1 control: add an extra inventory command row | **Killed** by `inventory_commands_and_strings_equal_core`. |

## Validation

- Independent review-only tests: four passed, including both entry points, 32 MiB boundaries, short-stream allocation requests, negative-version asymmetry, BIP152 boundaries, and the outbound encoding gap.
- `cargo test -p dashcore --test core_v24_protocol_tables`: 13/13 passed. BIP152 library tests: 11/11 passed. Targeted headers2 and weight tests passed.
- Fuzz target `zero_input_transaction_has_no_marker_or_flag` with `RUSTFLAGS='--cfg fuzzing'`: passed.
- `core-protocol-tables.py --core ~/workspace/dash --check`: up to date.
- The default `dash-spv` run passed 626 unit tests but seven concurrent `dashd_masternode` cases hit the harness's 30-second dashd readiness timeout. A one-thread rerun also had a startup timeout; dashd logs show block-index/filter-index initialization continuing after the deadline. With a review-only 180-second readiness wait, all 10 `dashd_masternode` tests passed and `dashd_sync` passed 35/37. Its two coinbase cases failed while generating blocks with JSON-RPC `SocketError(WouldBlock)` under load, then both passed when rerun alone. The original test file was restored by copying its saved contents back and touching it after each run.
- `cargo test -p dash-spv-ffi --test dashd_sync -- --test-threads=1`: 7/7 passed. `cargo test -p key-wallet-ffi`: passed, including 243 library tests and its integration targets.
- All seven mutations/control tamper checks were killed by test assertions; the tracked worktree diff was empty after restoration.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
