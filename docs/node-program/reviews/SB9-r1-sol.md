# SB9 implementation review — Sol r1

Reviewed `node/sb9-dash-filters` at **4848ea92f65e63592fbed69f99b679e16569515c**, the five commits above integration **300721627**. Contract: `briefs/REVIEW-COMMON.md`, PLAN SB9, SB0 rev6.1 / DEC-119, and DEC-112 O2. PLAN §7 handling: codenames only. The branch was not edited, committed, pushed, or commented on externally.

The shared crate's filters and headers agree with Core v24 in every parity check below. The blocking finding is in the compatibility adapter: preserving an `io::Read` signature while imposing EOF changes how existing valid streams behave.

## Findings

### R1-M1 — major: the stream readers require EOF and consume subsequent objects

**Location:** `dash/src/bip158.rs:332`, especially `:338`; both `GcsFilterReader::match_any` at `:344` and `match_all` at `:354`, and their `BlockFilterReader` wrappers.

`read_to_end` buffers the entire underlying reader before inspecting the count or matching anything. Previously the reader consumed only the bytes necessary for the match. A complete, valid matching filter on an open connection now waits for connection closure, or returns a subsequent read error. A reader containing multiple filters loses the later filters. The count also no longer limits reading or allocation: even a zero-element filter followed by arbitrarily much input is accumulated before matching.

I compiled the pre-SB9 reader/writer from integration alongside the adapter and reproduced:

```text
complete valid filter then WouldBlock: old=Ok(true) new=Err(Io(Kind(WouldBlock)))
two concatenated filters (4 bytes each): old consumed 4 new consumed 8
empty query then WouldBlock: old=Ok(false) new=Err(Io(Kind(WouldBlock)))
```

The first reader supplies the complete encoded one-element filter for `hello`, then returns `WouldBlock`. The second is a cursor containing two complete copies of that filter. This is valid-input compatibility loss, not a malformed-filter difference needed for Core parity. Documenting the new EOF requirement on the methods does not preserve the old interface's behavior.

**Fix:** retain incremental matching in the public stream adapters. Give the shared codec a pure byte-source/cursor abstraction and wrap `io::Read` in the adapter, preserving early returns and read boundaries. Keep the direct slice-backed matching path. Add regressions for a matching filter without EOF, concatenated filters, and an empty query with a body that cannot be read. A caller-supplied finite slice or `Take` can buffer safely, but making every existing caller supply one is an API contract change.

**Scope:** current in-repository SPV and wallet matching use `BlockFilter`'s slice-backed methods, and RPC conversion simply retains the filter bytes. I found no production in-repository call to these stream reader types, so this finding is about the exposed compatibility API; I did not reproduce it through the current peer processing path.

### R1-m1 — minor: the generic GCS adapters reject previously supported parameters

**Location:** `dash/src/bip158.rs:306`, used by the public `GcsFilterReader::new` at `:321` and `GcsFilterWriter::new` at `:372`; `dash-filters/src/gcs.rs:39`.

The old constructors accept `m: u64` and `p: u8`. The new adapter narrows `m` to `u32` and rejects `p > 32`. Both restrictions reject valid, previously working generic GCS filters. With one element `hello` and keys `(0, 0)`:

```text
generic params m=784931 p=33: old=Ok(6) new=Err(InvalidInput)
generic params m=4294967296 p=19: old=Ok(567) new=Err(InvalidInput)
```

These cases do not overflow or require unreasonable encoded sizes. Restricting basic Dash filters to Core's parameters is correct; silently reducing the old generic adapter's supported domain is separate. No in-repository caller uses these non-basic parameters, which limits the severity.

**Fix:** preserve the legacy generic parameter domain for values whose shifts and range multiplication are valid, using checked arithmetic where necessary. Core's basic constructors should continue to select `P=19, M=784931`. Alternatively, obtain an explicit compatibility exception and document the reduced domain on the public constructors; it is not currently among the stated deviations.

### R1-n1 — nit: PF0's SipHash rows do not measure the new filter implementation

**Location:** `dash-filters/src/siphash.rs:20`; `contrib/node/perfgate/workloads.toml:19` and `:20`.

Both `siphash-*-16k` rows still call `dashcore_hashes`, whereas filter encoding and matching now call the copy in `dash-filters`. The rows remain meaningful for W consumers, but a future slowdown introduced only in the new copy will not appear in those rows. `kwm-filter-scan-2000` does exercise the new matcher, so this is not a claim that the current filter path has no performance coverage.

**Fix:** add or route direct SipHash workloads to `dash-filters`, retaining W coverage where needed, and record the retirement/migration work in the program rather than relying solely on a memory note. The present copy is behaviorally identical; this is not a merge blocker by itself.

## Core parity and deviation judgments

**Elements.** Compared against tag `v24.0.0-rc.3`'s `src/blockfilter.cpp`, `src/evo/specialtx_filter.cpp`, and provider/asset payload serializers. The extraction has the correct collateral serialization (32 hash bytes in memory order, then a little-endian index), bare owner/voting key IDs, ProTx hashes, legacy owner script versus v3 payout lists, operator payouts, and shared collateral refund/reward/owner fields. Empty reward scripts correctly fall back to the refund script. Provider payout/refund/reward scripts exclude empties but retain OP_RETURN; ordinary outputs and asset-lock credit outputs additionally exclude a leading OP_RETURN. Asset unlock, coinbase, quorum commitment and MNHF add no payload elements. The spent scripts are obtained from the supplied undo-equivalent lookup, exclude empties, and do not apply the output OP_RETURN exclusion. Deduplication happens before determining N. Interleaving input extraction with each transaction differs from Core's traversal, but the resulting set and sorted hash sequence are order-independent for a lookup representing the block's undo data.

All **149** committed special-tx block filters and headers pass through both the pure crate and dashcore adapter. I independently decoded their payload coverage: ProReg versions 1–3; service/registrar/revoke versions 2–3; asset-lock/unlock versions 1–2; shared types 10–12 version 1; coinbase versions 2–4; quorum version 1; MNHF version 1. The stored oracle suite does **not** contain v1 service/registrar/revoke payloads. I supplemented these with synthetic v1 update payloads and compared **720** real/mutated transactions with Core's extraction, including truncated and excess payloads, duplicate keys/scripts, empty scripts, OP_RETURN payout scripts, credit outputs, and shared reward fallbacks. All matched. The extraction function was also compiled directly from the tag's `specialtx_filter.cpp`; its 720 outputs matched the linked Core implementation used by the probe. Synthetic mutations establish extraction behavior, not that those transactions would pass block validation. Promote the version coverage assertions and missing legacy cases into permanent tests when convenient.

The documented two-byte unknown-provider-version exception is outside connected, validated blocks. I did not treat those invalid versions as supported oracle coverage.

**Fresh oracle.** Started a private release-binary v24 regtest with `-blockfilterindex=1`, mined 103 blocks, and exported genesis plus two adversarial blocks. These contain duplicate P2PKH and OP_TRUE scripts, empty outputs, leading OP_RETURN, OP_RETURN later in a script, and a subsequent transaction spending both OP_TRUE and an empty script. Both the crate and adapter exactly match Core's filter bytes and headers. The private node stopped normally. Reusing kept functional-test chains for the committed oracle vectors is acceptable: extraction reads `getblockfilter` from Core itself, the binary digest matches the provenance, and these additional fresh-chain checks support the output/undo edge cases. A fresh recreation of every special-tx functional chain was not necessary for this review.

**Codec.** `P=19`, `M=784931`, little-endian SipHash keys from the first 16 hash bytes, and the high 64 bits of the `u128` product agree with Core. Direct Core GCS encoding matches **19** cases with exact distinct N values `0, 1, 2, 252, 253, 254, 65535, 65536`, P values `0, 19, 32`, duplicate inputs, and hash-range collisions. A separately compiled Core decoder agrees with Rust for **21,548** boundary/malformed encodings. Another **30,037** mutated/random malformed cases exercised Rust strict decoding and both lazy matchers without panic. Strict padding/excess-byte behavior and wrapping delta arithmetic follow Core. The slice decoder does not allocate from a peer's declared element count; matching allocation depends on the caller's query. Oversized and noncanonical counts are bounded/refused. The fixed `skip_decode_check=true` path still checks CompactSize before retaining the bytes.

**Header chain.** Core's SHA256d(filter) and SHA256d(filter hash || previous header) match across L0, all 149 oracle blocks, consecutive runs, and the fresh genesis vector with the all-zero previous header. Wrong bytes, predecessor, batch length and interior links are covered. The adapter's peer header computation is byte-identical to the old computation. Adding special-tx elements changes locally constructed filters to the correct v24 filters; it does not change the header acceptance result for the same received filter bytes. Broader CANDIDATE-SPV work remains outside this review.

**SipHash copy.** The **473** production lines from the scalar SipRound through the AVX2/NEON batch kernels are byte-identical to the original. Official vectors and comparisons against the original/upstream engine pass for 20 key pairs, lengths including 0/1/7/8/9/20/23/25/36/256/257, and counts spanning the vector block/tail boundaries through 257. AVX2 ran on this host; NEON was checked by source identity, not executed. Copying rather than depending on W is an acceptable layer-preserving deviation, with R1-n1's coverage follow-up.

**Purity.** No crate filesystem/network I/O, threads, runtimes, clock calls or custom allocator. Targeted dependency bans and forbid-level pure Clippy checks pass. Scratch probe I/O and Core initialization are outside the crate. SB9's CI group registration is correct.

## Validation and limits

All commands used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, the worktree's own target artifacts, and disk-backed `/work/scratch/sb9-r1-sol/tmp` with mode 700. No `/dev/shm`, shared target directory, protected chain instance, or branch edits were used. Temporary datadir removed after the node stopped; scratch sources, corpus files and logs are retained under `/work/scratch/sb9-r1-sol/`.

- `cargo test -p dash-filters --all-features`: **19 tests passed** (12 unit, 7 integration).
- `cargo test -p dashcore --all-features bip158`: **3 tests passed**.
- `cargo test --locked -p dashcore --all-features --test bip158_core_v24_special_tx`: **1 test passed**, covering all 149 adapter vectors.
- `cargo test --locked -p key-wallet-manager --all-features --lib matching::tests`: **8 tests passed**.
- `cargo test --locked -p dash-spv --lib validation::filter`: **8 tests passed**.
- `cargo clippy --locked -p dash-filters --all-targets --all-features -- -D warnings`: passed.
- Targeted `cargo deny ... check bans` and pure forbid-level Clippy: passed.
- Scratch codec, Core extraction, fresh regtest and old/new adapter probes: results above. Reproducers include `adapter_probe.rs`, `elements_probe.rs`, `encode_probe.rs`, `core_codec.cpp`, and `fresh_core.py`; logs include `adapter-probe.log`, `elements-probe.log`, `encode-probe.log`, `codec-differential.log`, and `vector-probe.log`.

The implementer's 3,319-test gate and BL-20 rerun were not repeated; review constraints call for targeted validation. The +0.011% performance figure is reported evidence, not an independent measurement by this review. No runtime NEON validation was possible on this x86 host.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
