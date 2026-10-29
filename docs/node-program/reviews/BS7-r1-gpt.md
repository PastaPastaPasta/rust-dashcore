# BS7 review, round 1 — GPT

Reviewed base-sdk `481f8c3..ab017ae` (15 commits, `pkgs/p2p_core`) and rust-dashcore `e7cd6055` (BS7 probes and DDE-SWEEP evidence). Detached worktrees: `/work/scratch/bs7-gpt/base-sdk` and `/work/scratch/bs7-gpt/rdc`. Core sources were read with `git show` at **v23.1.8** and **v24.0.0-rc.3**; the independent C++ reproductions use headers archived from those tags.

The opaque PLAN §7 allowlisted class was excluded from investigation. No branch edits, commits, pushes, or posts. Only the assigned review file and private scratch were written.

## Findings

### F1 — major: malformed `getblocktxn` has a different error cursor

**Location:** base-sdk `pkgs/p2p_core/src/msg/compact.rs:205`.

`CompactSize::decode_size(data)?.into_len(data.len())?` treats the bytes remaining as a count limit and rejects before attempting any index. Core instead reads the declared count and attempts entries until a field fails. For the 34-byte payload `00` × 32 followed by `03 00`, Core reads the block hash, the count of three, and the first index difference, then throws when the second difference is absent. Base-sdk rejects immediately after the count.

Independent reproduction:

- v23.1.8 Core formatter: 2 bytes consumed from the index-vector suffix, hence **34 bytes** including the hash; end-of-data exception.
- v24.0.0-rc.3 Core formatter: the same **34 bytes** and exception.
- Actual base-sdk `GetBlockTxn::decode`: **33 bytes** consumed; `CompactSizeExceedsLimit { limit: 1, value: 3 }`.

This fails the explicit malformed-input reader requirement, even though both implementations reject and the current handler corpus considers them equivalent. Reproductions are `/work/scratch/bs7-gpt/cursor.cpp`, `cursor23`, `cursor24`, `cursor.rs`, and `cursor-rust`.

**Core citations:** v23.1.8 `src/net_processing.cpp:4661-4663`, `src/blockencodings.h:35-40,50-52`, `src/serialize.h:778-792,824-827`; v24.0.0-rc.3 `src/net_processing.cpp:4704-4706`, `src/blockencodings.h:35-40,50-52`, `src/serialize.h:801-814,847-850`. Both handlers deserialize `BlockTransactionsRequest` before checking the requested block, and both vector readers attempt the entries in order.

**Fix:** retain bounded allocation, but separate the declared count from the allocation bound and attempt each available difference in Core order. Add an exact error-cursor regression for this payload and other partial prefixes. Do not fix the allocation issue by reserving the untrusted declared count upfront.

### F2 — major: the differentials do not establish the claimed malformed reader equivalence

**Locations:** rust-dashcore `contrib/node/dde-probe/bs7/src/fuzz.rs:92-100,144-146`; `bs7/src/main.rs:93-96`; `bs7_diff_headers2.py:245-266`; base-sdk `pkgs/p2p_core/tests/core_corpus.rs:232-255`.

The tx/block helper preserves `consumed` only for successful decodes. On failure it reduces Rust to a string, and treats any Rust error paired with an oracle `Decode` error as a match. Thus its reported bytes-read equality applies to successful decodes only. The command corpus checks broad `read` / `throw` / `catch` categories, without error cursors. F1 is a concrete malformed reader difference that passes the committed test suite.

The headers2 differential also records `core_closed` but never checks it, never checks `pred.consumed`, and compares only the logged hash subset on rejected messages. An independent invocation of its actual `Node.check` accepts `core="read", core_closed=true` with `pred={ok:true,consumed:0,hashes:[],prevs:[]}` as `(true,"empty")`. Consequently a decoder that returns an empty message for a nonempty payload, or consumes trailing data, can evade this comparison. A truncated message is accepted as equivalent whenever both sides are broadly labelled `throw`, regardless of where reading stopped. Reproduction: `/work/scratch/bs7-gpt/harness-check.py`.

**Core citations:** v23.1.8 `src/net_processing.cpp:5246-5268,5795` and v24.0.0-rc.3 `src/net_processing.cpp:5323-5345,5941`: count checks occur before the header loop, each header is read and uncompressed in order, and decode exceptions are caught at the outer message boundary. For the tx/block reader boundary: v23 `:4823,5280`; v24 `:4878,5357`.

**Fix:** expose and compare the cursor on both success and failure through an independent reader oracle; check ordered decoded headers rather than only membership in logged hash sets; explicitly validate the connection/penalty observations relevant to each test state. Add harness negative controls proving that wrong consumption, empty successful decodes of nonempty messages, and changed connection outcomes fail. If some checks must await O1c/P2a, accurately qualify the existing evidence and leave those acceptance gates pending. Huge case counts do not cover fields the comparator discards.

### F3 — minor: `GetBlockTxn::new` does not enforce the serializer invariant

**Location:** base-sdk `pkgs/p2p_core/src/msg/compact.rs:196,233,244`.

`indexes` remains public and mutable, and serde can construct it without `new`. A caller can build or mutate the list to `[5,5]`. `encode` silently wraps the second subtraction and writes vector suffix `02 05 ff ff ff ff ff ff ff ff ff`, a message its own decoder cannot accept. Core refuses to serialize that index list.

**Core citations:** v23.1.8 and v24.0.0-rc.3 `src/blockencodings.h:28-32,50-52` (identical): `DifferenceFormatter::Ser` throws when an index is less than its running shift.

**Fix:** keep the indexes private, provide a validated constructor/setter and read-only accessor, and validate deserialization; alternatively expose a fallible checked encoder. The checked constructor alone is insufficient while unrestricted construction and mutation remain available.

## Scope decisions and deviations

- **Source-only short-ID send gate:** acceptable for this pure codec task. I reran `bs7_short_ids.py`; it derives both pins' tables and asserts the actual gate/map/init-version code shapes. Crate tests check all receive bytes and send-version bands. Actual encrypted transport negotiation remains P1's live interoperability gate. Core citations: v23 `src/net.cpp:1572` and v24 `:1566` (`SetMessageToSend`), plus their `V2MessageMap` and `GetMessageMinVersion` definitions.
- **Breaking APIs:** keeping wire flags/delta in `CompressedHeader` is justified for byte preservation; requiring the peer's common version for `encode_v2` prevents bypassing the send gate; explicit `AssetUnlockInv` properly distinguishes a release capability from the remote peer's negotiated version. The changes are recorded in the changelog. F3 is the separate unchecked-construction issue.
- **Caller state gates:** accepted as an architectural boundary. `sendtxrcncl` and `ssc` depend on configuration/sync state, not merely `ProtoCtx.version`. Their docs explicitly require the caller to gate first. The same boundary applies to bloom service permissions, import/reindex/IBD, and post-read scoring/locator limits. The corpus's gated rows do not verify the enabled reader path.
- **govsync trailing-byte skip:** accepted only as the named BS9b ownership exception. It must not be described as complete parity for every captured command.
- **No BIP37 serving logic:** accepted; BS7 provides message codecs, while P2a owns filter state and serving behavior. Limits and Core's inner catch behavior are documented in DDE-SWEEP.
- **Fuzz budget:** **not satisfied**. BL-39 records approximately 2.8 CPU-h each for tx/block, 45 minutes for headers2 and 50 minutes for framing. PLAN's M1 exit and A1's green-target criterion require the remaining 24 CPU-h per target. Deferring the remaining runs to O3/O1c is tracked work, not completed evidence. Resolve or explicitly waive the milestone gate; rerun after repairing the comparisons in F2.

## Independent validation

- `cargo test -p dash-p2p-core --all-features`: **67 tests passed**, including the Core corpus, all short-ID tests and the allocation tests.
- `cargo clippy -p dash-p2p-core --all-features --all-targets -- -D warnings`: **passed**.
- Built the committed `bs7-tool` release helper, pinned to `ab017ae`.
- `bs7_short_ids.py` and `check_sources.py` against both Core tags: **passed**.
- Compiled and ran both pinned Core vector-reader reproductions and the actual Rust codec reproduction for F1; invoked the actual headers2 comparator for F2.
- Live differential sample: **60 framing cases** (30 per pin) and **82 headers2 cases** (42 v23, 40 v24), **0 mismatches and 0 harness errors**. Invoked the committed differential functions sequentially, seeds 731/732 for framing and 941/942 for headers2, 25 seconds per pin for headers2. Results: `/work/scratch/bs7-gpt/sample.json`; driver: `sample.py`. Private datadirs under `/dev/shm/bs7-gpt-tmp/nodes`, P2P/RPC ports 32711/32712 and 32721/32722, one node at a time, `-par=2 -dbcache=64`. Build jobs capped at 8 per Cargo invocation. Both private nodes stopped; RAM test scratch removed after validation. No shared chain or benchmark data accessed.

The inspected V1 checks follow Core's magic/length/checksum/command order; V2 limits include the type bytes and reserved slots map to the empty type; inventory names and release capability handling match the two source tables. Headers2 preserves unused flags, first-header defaults, out-of-range offsets, duplicate MRU entries, modular time arithmetic, and the reply/announcement distinction. The committed allocation tests pass, but allocation bounds must preserve the read cursor as F1 illustrates.

The original full-history oracle walks, large fuzz runs, CodeQL and 1210-test nextest claim were not independently repeated. The local results above provide their actual verification scope.

Final verdict: **CHANGES-REQUIRED**.
