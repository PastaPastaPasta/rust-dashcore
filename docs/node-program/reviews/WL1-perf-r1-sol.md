# WL1 performance fix — narrow review

Reviewed `7b085b79a`, `cce2a77e5`, and `a3d93a078` at `a3d93a078`; excluded the follow-merge `7a4c88b3e` as requested.

**Verdict: ACCEPT**

## Findings

No actionable findings in the reviewed commits.

## Review evidence

1. **Mnemonic behavior and wiping:** `key-wallet/src/mnemonic.rs:100-117,279-302` returns the same 2048 words as bip39's `Language::word_list`; `words_by_prefix("")` includes every word in order and `try_into` only checks length. bip39 2.2.2's `Mnemonic::Display` iterates those word indices and writes a single ASCII space between words, matching both the original `inner.words().join(" ")` and the intermediate helper. Parsing, language selection, NFKD handling, whitespace and case handling are unchanged by these commits. The new count pass writes only to a `usize`; the second pass writes directly into the exact-capacity `Zeroizing<String>`, with no additional phrase string. The deprecated `phrase()` remains an ordinary `String`, as before. Core-mode checking receives the same English word-list contents.
2. **Segment format and errors:** `dash-spv/src/storage/segments.rs:600-645` changes only the two current fixed-size `Persistable` types: `HashedBlockHeader` (112 bytes) and `FilterHeader` (32 bytes). Header fields consume 80 bytes and the cached `BlockHash` consumes 32; `FilterHeader` consumes 32. The new `HashedBlockHeader::consensus_decode_from_finite_reader` reads these fields in the original order. A complete record decodes from precisely its former bytes; EOF or a partial last record ends loading as before, while other read or decode errors are propagated. The buffer slice is bounded by a compile-time `ENCODED_SIZE <= 112` check. Variable-size types retain the old loop. No new panic path from truncated or malformed records is apparent.
3. **Clippy and performance:** The workspace search found no remaining direct calls to the three banned bip39 methods. The root `clippy.toml` covers key-wallet, and no other workspace crate currently calls those methods. The seed path now formats through bip39's `Display`, which uses its parser-side word list, making the measured single table set plausible. Each fixed-size item makes one `BufReader::read_exact` call before slice decoding, removing per-field reads through `Take<&mut BufReader<File>>`. D143's PF0 numbers are consistent with those changes; this review did not remeasure them.

## Validation and scope

The existing GATE-FAST tier-1 run for `a3d93a078` reports passing Clippy and tests (`/work/scratch/gate-fast/logs/wl1-fix-t1b.log`, `GATE EXIT 0`). Local `cargo test -p key-wallet` passed (723 unit tests plus integration and doc tests), and `cargo test -p dash-spv --lib storage` passed (81 tests).

An overfull segment file containing more than `ITEMS_PER_SEGMENT` complete records can reach the pre-existing `debug_assert!` in `Segment::new` (`dash-spv/src/storage/segments.rs:555-557`) in debug builds. Both the old and new load loops allow that case; it is outside these commits' regression scope.

**Verdict summary:** ACCEPT. The reviewed commits preserve current mnemonic and segment behavior, retain phrase wiping on the zeroizing path, and plausibly remove the measured word-list duplication and per-field buffered reads. No change is required for this performance fix.
