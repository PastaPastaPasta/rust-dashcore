# BS7 closure review r1 — Opus (sequential pass after Sol, DEC-92)

| | |
|---|---|
| Under review | base-sdk `node/bsdk-bs7-p2p-core` **3229f62b** (33 commits on a1d4dd3); rust-dashcore `node/bs7-p2p-core` **1aa86c7b** |
| Core | v24.0.0-rc.3 and v23.1.8, plus v20.0.0 and v22.0.0 for the older-sender checks (`git show <tag>:…`) |
| Scratch | detached worktrees under `/work/scratch/bs7-closure-opus/`; `TMPDIR=/dev/shm/bs7-closure-opus-tmp`, `RUSTC_WRAPPER=`, `EIO_BACKEND=posix`, 12 jobs. Both removed afterwards |
| Read first | REVIEW-COMMON.md, `BS7-closure-r1-sol.md` (F1–F3 not repeated here), `BS7-r1-opus.md`, DEC-86/91/98, BSX catalogue §2.4/§8 |
| Verdict | **APPROVE-WITH-NITS** |

**On Sol's F1.** It is no worse than minor. The only automated consumer of the fuzz targets is `contrib/node/check-private-tests.sh:130-172`. It checks `done[-1].mismatches == 0`, the absence of `why` rows, the classifier verdict and the exit status, so the mismatch-counter gap cannot slip through there. `bs7_diff_headers2.py` fails on mismatches. `bs7_diff_framing.py` has no caller apart from the README recipe. F1 matters only once these commands become unattended O3 or CI gates, and that is Sol's own caveat.

## 1. Interop: does any strictness refuse what an honest sender emits?

I found no case. The checks, by area:

- **headers2, every encoder Core has shipped.** `primitives/block.cpp` has not changed since #4497 (`git log v24.0.0-rc.3 -- src/primitives/block.cpp`). The v23.1.8 → v24 diff of `block.h`, `blockencodings.h` and `blockfilter.h` is copyright lines, `Time()` and a `CBlockLocator` move: no serialization changes.
  - **Same send paths at every tag.** The reply path (`if (!v_headers.empty()) Compress(…)`) and the announcement path (`Compress(vHeadersCompressed, …)`) are textually identical at v20.0.0 (np:3525/4859), v22.0.0 (4341/5890), v23.1.8 (4804/6359) and v24 (4854/6504).
  - **The first header is always whole.** The constructor sets offset 0 plus the PREV/TIME/NBITS bits (0x38), and `Compress` returns early when there are no previous blocks. So the strict first-header rule (`headers2.rs:186-196`) never fires on Core output.
  - **Every offset `Compress` emits is in the decoder's list.** Each offset comes from `find` over the encoder's list. In reply mode the encoder never saves the first header's version, but the decoder does (`Uncompress`, empty-previous branch). The decoder's list is therefore always the encoder's list with one extra entry at the back, until eviction makes them equal. Positions line up and the decoder's length is never shorter.
  - **Core never sends a negative time offset.** `nTime - last_block.nTime` is a `uint32_t` subtraction, so a backward step wraps above `INT16_MAX` and goes whole. I confirmed this on the corpus: none of the 13 captures or the four 300-header oracle samples carries a negative offset. 15 headers after the first carry a whole time: the backward steps, or forward gaps above 32,767 s.
  - **Unused flag bits** pass through untouched.
  - **The 8000 limit** follows `GetHeadersLimit(common ≥ 70235)` (np:1210). It is mutation-checked: see §3, B.
- **sendcmpct.** Every Core send site writes a C++ `bool` literal (v24 np:1399, 1409, 4221; v23.1.8 np:1376, 1386, 4190). `Serialize(bool)` writes a `uint8_t` 0 or 1. The `bool` has been there since Bitcoin's original BIP152 code, so no older Dash release sends another byte. Strictness changes the outcome only for crafted flags (DDE-SWEEP §5 records the node-side policy).
- **cfilter, cfheaders, cfcheckpt.**
  - `cfilter` matches `BlockFilter::Serialize` (u8 type, block hash, `GetEncoded()` vector).
  - `cfheaders` matches `filter_type_ser, stop hash, prev_header, filter_hashes`, and `cfcheckpt` the same without `prev_header` (v24 np:3509/3550/3591).
  - No content checks are added. Generic vectors keep `into_len(data.len())`.
- **merkleblock.** Matches `CMerkleBlock`/`CPartialMerkleTree` at both tags (`merkleblock.h:88-96,151`): `nTransactions`, then `vHash`, then the `BitsToBytes` vector.
- **getblocktxn.** Matches `DifferenceFormatter::Unser` (`blockencodings.h:22-33`): `ReadCompactSize` is range-checked, then the wrap, u64-sentinel and u16 checks run. The count bound `into_len(data.len())` gives the same reject verdict as Core reading to EOF. Core's own requests (np:5203/5213) come from `PartiallyDownloadedBlock` and strictly increase, so `GetBlockTxn::new` and the encoder always accept them. The wire form has not changed since BIP152. Older Bitcoin-era manual loops wrote the same bytes, and every Dash release at or above `MIN_PEER` 70221 uses this formatter.
- **cmpctblock and prefilled.** `COMPACTSIZE(uint16_t)` is checked against MAX_SIZE and then the u16 range, and the 65,535 total is checked after both lists are read. Both match the corpus boundaries (`cmpctblock_prefilled_index_65535/65536`, `cmpctblock_total_65535/65536`).

A non-Core data point: rust-dashcore's existing `dash` crate encoder emits **signed** time deltas (`dash/src/network/message_headers2.rs:322-329`, `delta as i16` over `[i16::MIN, i16::MAX]`). So negative offsets are a real wire form, not only an attacker's. base-sdk accepts them and rebuilds them modulo 2^32, as Core does (D-08 kept). That makes their arithmetic interop-relevant, which bears on m-1 below.

## 2. Upstream shape

The names (`Headers2::reply`/`announce`/`decode_with_hasher`, `CompressedHeader`, `decode_v2_payload`, `V1Header`, `limits`) fit the crate's existing style. Release-specific constants equal `version.h` (all 21 checked at v24). Breaking changes are in the changelog and carry `!` commits.

No penalty, score or reject-reason string appears in base-sdk. Errors are typed, and doc comments describe what Core does ("on which Core disconnects"), which is compatible with DEC-98 Q-1. The remaining policy-shaped helpers (`V1FrameError::disconnects`, `InvType::request_interval`) and the reshaping items are Sol's and the catalogue's BSX-PR actions, so I don't repeat them.

The branch history goes back and forth: for example, 6238c14 "read header counts past the data" is undone by 5f7b3ff, and D-17/D-18 were added and then dropped. The series is not shaped for kwvg as it stands. That is expected: BSX-PR rebuilds it into U17/U19/U27/U29. One gap in that process is n-1.

## 3. Test strength (mutations in the scratch copy)

The baseline `cargo nextest run -p dash-p2p-core --all-features` gives 74/74 passed. I applied each mutation alone and then reverted it:

| # | Mutation | Result |
|---|---|---|
| A | `CFHeaders` codec: swap `stop_hash` and `prev_header` (`cfilter.rs:101`) | **survives** (74/74) |
| B | headers2 limit: `ctx.version >= INCREASE_MAX_HEADERS2` → `>` | caught (`decode_domain::headers_limits_follow_core`) |
| D | headers2 uncompress: `wrapping_add(i32::from(off) as u32)` → `wrapping_add(off as u16 as u32)` (a sign bug for negative offsets, `headers2.rs:214`) | **survives** (74/74) |
| F | getblocktxn decode: `shift = index` (drop the `+ 1`) | caught (3 tests, including Core's cursor and verdict corpora) |
| I | headers2 uncompress: drop `mark_most_recent` | caught (unit test plus the Core corpus) |
| J | sendcmpct: accept 1..=255 as `true` | caught (`handler_verdicts_are_cores`, `sendcmpct_hb02_v1`) |

The out-of-repo differentials would catch D, because `bs7_diff_headers2.py:129,155` generates offsets in −5..5 and compares against Core's logged hashes. So D is a gap in the base-sdk suite, the one upstream CI and kwvg run. It is not a gap in BS7's evidence as a whole. Nothing out of repo catches A.

## 4. Hostile-input bounds

- **headers2 handler path.** The count is checked against 2000 or 8000 before any read, then against the remaining bytes (`headers.rs:36-46`). The reservation is at most 8000 `CompressedHeader`s (under 1 MB). Each read header is hashed at most once, in Core's order, never more than Core does. The allocation test covers count 8000 against a 64-byte payload. There is no quadratic path: `VersionList` is a fixed array of 7.
- **headers2 codec path** (`BaseCodec`/`Decode`). The count is bounded only by `len/39`. A 3 MiB payload can make it hash about 80,000 headers, roughly 10× the handler's cap. This is documented at `headers2.rs:294-297` as "not Core's handler read" (my r1 n-2). That is acceptable for a library, but P2a must not route peer bytes through it. Already recorded; no new finding.
- **V2.**
  - `split_v2` checks the contents length before anything else and slices without allocating.
  - `decode_v2_payload` uses `saturating_add` and the shortest-type bound, then dispatches.
  - Only `Unknown` and the stub arms copy the payload, which is bounded by `MAX_V2_CONTENTS_LEN`.
  - V1 framing is O(n) (a single SHA256d).
- **Generic vectors** (dash-types, unchanged against a1d4dd3): the count is bounded by the remaining bytes, and reservations are batched at 5 MB. getblocktxn allocates at most 2 bytes per payload byte. Short IDs allocate in 5 MB batches. Both match Sol's allocation conclusion.

## 5. Findings

### Minor

**m-1. The base-sdk suite doesn't pin decoded values where re-encoding cannot reveal a mistake.** This covers negative headers2 time offsets and the two adjacent 32-byte fields of `cfheaders`.
`pkgs/p2p_core/src/msg/headers2.rs:214` (tests `:459-560`); `src/msg/cfilter.rs:101-106`; `tests/core_corpus.rs:121-137,162-208`.

- **Scenario D.** A refactor that sign-extends wrongly (mutation D) decodes a header with offset −5 as `prev + 65531`. The header hash then differs from Core's, so the node disagrees with Core on a chain sent by any signed-delta encoder, rust-dashcore's own `dash` crate included.
  - `cargo nextest -p dash-p2p-core` stays green. Core never emits negative offsets, so the corpus has none.
  - `CompressedHeader::encode` writes the stored wire offset, so round trips hide it.
  - The one unit test with −5 (`:494-513`) stops at the wire parse.
- **Scenario A.** A field reorder in `CFHeaders` (mutation A) swaps the stop hash and the previous filter header. The corpus only round-trips, so this also stays green, and the first consumer, a BIP157 client, would chain filter headers from the wrong value.
- **Fix.**
  - Add a unit test that decodes a two-header message with offsets −1 and `i16::MIN` and a `prev.time` near 0. Assert the absolute `time` values and the derived hash against a hand-computed header. A Core-judged case is better still: `bs7_diff_headers2.py` already generates such messages, so one case with Core's logged hash can be committed into `core_headers2`.
  - For the send-only replies, record what `bs7_probe.py` requested (stop hash, filter type, start height) next to each `cfilter`/`cfheaders`/`cfcheckpt`/`merkleblock` in `core_messages`. Assert the decoded `stop_hash`/`block_hash`/`header` against it. For `cfheaders`, also assert `prev_header` against the previous reply's last header, or zero from genesis.

### Nits

**n-1. The BSX §8 scrub regex misses internal references that BS7 added to base-sdk.**
`designs/BSX-leniency-catalogue.md:1049`; hits in base-sdk: `pkgs/p2p_core/CHANGELOG.md:59` ("P3 builds it"); `tests/core_corpus.rs:213-218` ("BSX", "D-11", "D-14"); `corpus/core_messages.json5:2` and `corpus/decode_domain.json5:85` (`bs7_probe.py`, `probe_v2.py`, on lines without `contrib/node`).

- **Scenario.** The §8 grep matches `\bBS[0-9]` and `DEC-[0-9]`, but not `BSX`, `D-NN`, `Q-NN`, `P[0-9]` task IDs or the `bs7_*`/`probe_*` script names. A unit cut from these commits passes the scrub and still carries node-program jargon. The CHANGELOG line, which kwvg reads, is the visible one.
- **Fix.**
  - Extend the §8 regex with `\bBSX\b|\b[DQ]-[0-9]{2}\b|\bP[0-9][a-z]?\b|\bbs7_|probe_[a-z0-9]+\.py|gen_bs7`.
  - Reword CHANGELOG:59 to "a caller builds it from `short_id_keys` and `short_id_with_keys`".

**n-2. `CompressedHeader`'s public fields can disagree with what `encode` writes.**
`headers2.rs:52-59,90-108`.

- **Scenario.** A caller edits `header.time` on a header whose flags compress the time, or builds `Headers2 { headers }` by hand. `encode` writes `time_offset` and silently drops the edit.
- **Context.** Decoded and `reply`/`announce`-built values are always consistent, so this is API hygiene against DEC-86's "invalid states unrepresentable", not a bug.
- **Fix.** For the U19 reshape, keep the fields private behind accessors, with `reply`/`announce`/decode as the only constructors. Alternatively, document that `header`'s compressed fields are derived and ignored by `encode`.

## 6. Independent validation summary

- Baseline p2p_core nextest, all features: **74/74**. Six mutations: four caught, two survived (m-1).
- Source comparison for every BS7 decoder and encoder was done against v24.0.0-rc.3 and v23.1.8. For headers2 and sendcmpct send paths I also checked v20.0.0 and v22.0.0.
- I didn't rerun Sol's live differentials or the fuzz runs. Sol ran fresh v24 pairs, and the decoders are unchanged between bfb7c93 and 3229f62. I opened nothing under `~/private`.
- No branch edits, commits or pushes. Scratch worktrees, the target directory and `/dev/shm/bs7-closure-opus-tmp` are removed.

## 7. Verdict

**APPROVE-WITH-NITS.** No blocker or major finding. Nothing BS7 makes strict refuses bytes any Dash Core release at or above `MIN_PEER` sends. The API carries no node policy beyond what Sol and the catalogue already route to BSX-PR. Allocation and CPU are bounded at or below Core on the handler paths.

m-1 should go to the implementer with Sol's F1–F3. It is cheap, and it is the only place the upstream suite would let a parity regression through. n-1 belongs to whoever maintains the BSX §8 scrub. Sol's F1 stays **minor**.
