# BS4a review, round 2 (Opus, focused)

**Scope.**
- base-sdk `node/bsdk-bs4a-decode-domain` at c887c50: the 14 fix commits f8a088f..c887c50.
- rust-dashcore `node/bs4a-decode-domain` at 00aab98c, on node/integration c8a6ee50: e171f049, db0d76b8, 2e406e47, 00aab98c. That is DDE-SWEEP.md §4 rewritten as "settled", plus `probe_gates.py` and `probe_v2.py`.
- Round 1: `BS4a-r1-gpt.md` (F1–F3) and `BS4a-r1-opus.md` (M-1..M-5, m-1..m-8, nits). GPT is unavailable (DEC-42), so this review covers both.

**What I did.**
- Rebuilt the round-1 before/after probes from exported snapshots of f8a088f and c887c50.
- Compared base-sdk's V2 splitter against Core's short-ID table for all 255 IDs.
- Re-ran the live probes (`probe_p2p`, `probe_v2`, `probe_gates`, `gen_corpus`) on my own regtest nodes at both pins, plus extra V2 cases.
- Checked every §4 row against Core source at v23.1.8 and v24.0.0-rc.3. Three independent source passes covered all 74 commands (A1: transport, gates and inventory/headers/addr; A2: blocks, bloom, filters and control; B: LLMQ and mnauth; C: governance and CoinJoin). I re-read every discrepancy reported below in source myself.
- Ran the base-sdk gate, CodeQL and the commit lint. Re-ran the bench. Scanned for consumers of the `LlmqType` re-export.

Scratch: `/work/scratch/bs4a-r2-opus/`. Live-probe outputs: `/work/scratch/bs4a-r2-opus/live/out/`. The private nodes used ports 29911/29912 and 29921/29922 with datadirs under `/dev/shm`; they were stopped and deleted. Both worktrees under review are clean.

## Verdict: CHANGES-REQUIRED

Every round-1 finding is closed, and the code fixes are correct. F1, F2 and M-4 reproduce exactly before and after. The new V2 split matches Core for every short ID, both size boundaries, and empty or truncated contents, at both pins. The gate passes (dash-pow excluded, as before), CodeQL passes, and the −2% bench result is real.

Two things block approval:

1. **N-1 (major): the §4 table is not "settled" as written.** The transport rows, gates 1–8, the "handler reads" and "decode failure" columns, the catch-and-score list and the trailing-byte rule are now right; I found no wrong pre-read gate, read shape or catch site. But the **"After the read, in order"** column is incomplete or wrong in 14 rows, at one or both pins: penalties, disconnects and early returns that `dashd-p2p::policy` would get wrong. Two items also affect decode policy itself: N-4 (invalid command bytes) and the DKG intake shapes in N-1(i).
2. **N-2 (major): a new memory-amplification path.** It is the same class as r1 M-4, reached through `NetInfoV2` once the special-payload cap became `MAX_SIZE`. Decoding the payload of one 3 MiB P2P transaction reserves about 100 MB.

**Can §4 be marked "settled"?** Not as written. It can be once both of these are done:
- N-4 and the DKG intake-shape item N-1(i) are corrected;
- the "After the read" column is either corrected per N-1 or explicitly labelled *informative*. In the second case, the post-decode rules belong to `dashd-p2p`/P2a, which re-check them against source when they implement them.

The decode-policy columns can then be called settled.

## Round-1 closure

| r1 finding | Status | Evidence |
|---|---|---|
| F1 / m-6: V2 bound 12 bytes too strict | **Closed** (a81261f, 53c0763) | Before: short-ID payloads of 3,145,729 and 3,145,740 bytes gave `PayloadTooLarge`. After: `Ok(Ping)` with 8 bytes consumed. 3,145,741-byte payloads (3,145,742 bytes of contents) give `ContentsTooLarge`. Long-form 3,145,728 is accepted and 3,145,729 gives `ContentsTooLarge`. Unknown short ID 0xfe at 3,145,742 bytes of contents gives `ContentsTooLarge`, so the size check runs before typing. My rebuild matches `bs4a-fix1-evidence/evidence-{before,after}.txt` byte for byte. Live at both pins (my nodes): every one of the 10 `probe_v2` outcomes matches |
| F2 / M-1: short pong | **Closed** (35ba33b) | Before: lengths 0–7 gave `Consensus(Eof)`. After: `Pong{None}` with `consumed=0`; 8 and 9 bytes give `Some(42)` with 8 consumed. Core v24 np:5480-5512 is identical at v23 (5403-5435). Live at both pins: 0, 1 and 7 bytes and a zero nonce cancel the ping (`pingwait` disappears); a mismatched nonce leaves it pending; a match with a trailing byte is measured |
| F3: handshake gates | **Closed** | §4 rows for `sendaddrv2` (DDE-SWEEP:143) and `sendtxrcncl` (:165) are correct about the gates before the read. Live at both pins: `sendaddrv2` at 70221, 70222, 70223 and 70240, before and after verack. One remaining error is in `sendtxrcncl`'s after-read order: N-1(m) |
| M-2: generic rule and gates | **Closed** | Gates 1–8 and their order are correct at both tags (np 3907/3936 … 4815/4865). The blocks-only set has 26 commands (`protocol.cpp:185-212`, the same at both tags), and the `RejectIncomingTxs` conditions are right. The catch-and-score list is complete: filterload, filteradd, govsync filter, govobjvote, spork, getqrinfo (v24), sig shares, DKG. The DKG and `qgetdata` trailing-byte rules (v24) are the only ones |
| M-3: misstated rows | **Closed for the rows listed** | The 11 r1 rows are now right. New errors were found in other rows and columns: N-1 |
| M-4: header-count allocation | **Closed** (f124561) | Largest single allocation for a 3 MiB `fe f8ff2f00…` payload drops from 251,657,600 bytes (80×) to 92 bytes, for both `Headers::decode` and `Headers2::decode`. The test is `header_counts_are_bound_by_the_payload`. The same class of bug remains in `NetInfoV2`: N-2 |
| M-5: CodeQL | **Closed** | `lint_codeql.py check` and `run --lang=rust --with-suite=rust-security-and-quality` with CodeQL 2.27.2 both exit 0, and the analysis really ran (229 files, not the exit-77 skip) |
| m-1: `check` re-encodes | Closed | c3440b3 adds a `ByteCount` `EncodeBuf`, and the >1 MB test skip is gone |
| m-2: `Version` invalid states | Partly closed | `has_prefix_tail()` and docs were added. The `debug_assert` was removed again in 370b893 because serde can produce a gappy `Version`. Invalid states are still representable: nit N-7 |
| m-3: evidence labels | Closed | 712a55a labels the source-derived cases. `compact.rs`'s `u64` case now uses Core's `unserialize_vector_with_max_size` bytes |
| m-4: probe classifier | Closed, with residue | e171f049 attributes log lines by peer id. Some remaining assertion gaps: N-5 |
| m-5: tooling pins | Closed | `oracle/` and `bench/` pin by `rev` and commit their lockfiles; §6 names `o1-oracle-sys` 36c2b987. The URLs are still `file:///work/...`, so the tooling only reproduces on this machine. Acceptable |
| m-7: §2/§4.1 gaps | Closed | VARINT, the `COMPACTSIZE` type range, `qbsigs`' outer count, addrv2 length per network, the version gates and the loading-blocks gate are all added |
| m-8: version throw class | Closed | The test now asserts `VersionTail` past 46 bytes and `Consensus` inside the first 46 |
| Nits (clone, `trailing()`, MsgDecode docs) | Closed | — |

## The V2 split (question 2)

`split_v2` (`p2p_core/src/bip324.rs:50-72`) compared with Core's `V2Transport::GetMessageType` (`net.cpp` v23 1496-1540 / v24 1490-1534). The short-ID tables `V2_BITCOIN_IDS` and `V2_DASH_IDS` are identical at both tags:

| Input | Core (both pins) | base-sdk | Match |
|---|---|---|---|
| Short IDs 1–32 and 128–168 (all 255 IDs checked against Core's table) | the named command, or `""` for 5, 29, 30, 31, 32 | the same command, or the all-zero type giving `Unknown` | yes, 0 mismatches |
| Short ID 0 (long form) with 0–11 type bytes | dropped, peer kept (live, 1–12 bytes of contents) | `TruncatedV2Type` | yes |
| Short IDs 33–127 and 169–255 (33, 127, 169, 0xfe, 0xff spot-checked live) | dropped, peer kept | `UnknownShortId` | yes |
| Empty contents | dropped, peer kept (live) | `EmptyV2Contents` | yes |
| Contents of 3,145,742 bytes, any first byte | disconnect before typing (`MAX_CONTENTS_LEN`, v23 1303-1310 / v24 1297-1304) | `ContentsTooLarge`, before the type is read | yes |
| Short IDs 5, 29 and 32 with a payload | `Unknown command ""` (live, at both pins) | `Unknown`, all-zero command, payload kept | yes |
| Long form `ping` with an empty payload | dispatched, handler throws | `split_v2` gives `ping`, then `decode_v2_payload` gives `Consensus` | yes |
| Long form with a byte outside 0x20–0x7f, or a non-zero byte after the first NUL | **dropped, peer kept** (live) | **`Ok(P2pMsg::Unknown)`** | **no: N-4** |

## Findings

### N-1. Major: the "After the read" column of DDE-SWEEP §4 is incomplete in 14 rows

I re-read every item below in source at both tags. Line numbers are v23.1.8 / v24.0.0-rc.3; `np` is net_processing.cpp.

| Row (DDE-SWEEP line) | Doc says | Core |
|---|---|---|
| (a) `govobjvote` (134) | "a rejected vote +20 once governance is synced" | The penalty is `exception.GetNodePenalty()` (`net_governance.cpp` 267-269 / 266-268). It is +20 only for an unknown masternode, an unsupported signal or a bad signature (`governance/object.cpp`). Already-known, obsolete, too-frequent and expired-parent rejections score 0. **The pins differ:** v23 adds +20 for a cached invalid vote (`governance.cpp:804-809`) and never penalises an unknown parent. v24 has no invalid-vote cache and scores an unknown parent +20 only when `!IsValidForUnknownParent` (`governance.cpp:821-838`) |
| (b) `govsync` (135) | "a repeated sync request +20" | Only tracked requests are limited: `track_request = full_sync \|\| (!object_fetch && HaveSyncableObjectForHash(nProp))` (ng v24:120-131, the same at v23). An object fetch (non-zero `nProp` with an empty filter) and a vote sync for an object we can't sync are never limited. The key is `peer.addr` (netfulfilledman), so the limit survives a reconnect |
| (c) `dstx` (185) | "bad structure +10 (v24 +1 …); unknown MN +1; bad signature +10; then the transaction's penalties" | There are no-penalty returns the row leaves out, and both stop processing: "already have" and an MN over the rate limit (`!IsValidForMixingTxes`, np 3677-3681 / 3698-3702). **The order differs:** v23 checks structure, then already-have (np ~3632-3639); v24 checks already-have, then structure (np 3642-3667). The v24 +1 requires `fPossiblyValidPostV24 && IsPromotionDemotionActive(fNextBlock=true)`; otherwise it is +10 |
| (d) `cmpctblock` (156) | "invalid compact block +100" | After the read: unknown previous block sends getheaders with no penalty (4992 / 5069). A header that fails validation goes through `MaybePunishNodeForBlock(via_compact_block=true)` (5008 / 5085): consensus and mutated results score 0 there, invalid header, checkpoint or invalid previous block +100, ChainLock conflict +10. `READ_STATUS_INVALID` +100 is reached only when the block is close to the tip and the in-flight slot conditions hold; otherwise reconstruction is optimistic and a failure scores 0. Then the `ProcessCompactBlockTxns` +100 paths (3749/3757 / 3770/3778) and the headers fallback (5186 / 5263) |
| (e) `getblocktxn` (158) | "index out of range +100" | Only when the request is for the most recent block, or for one we have within `MAX_BLOCKTXN_DEPTH` (10) of the tip (4668-4705 / 4712-4749). An unknown block is ignored; a deeper one is served as a full block, with no index check |
| (f) `headers`/`headers2` (123-124) | +100 invalid header, checkpoint or invalid prev; +10 missing prev or ChainLock | Also `BLOCK_CACHED_INVALID` ("duplicate" of a known-invalid header): +100 for an **outbound** peer, 0 for an inbound one (np 2006-2020 / 1984-1998, verified). `BLOCK_TIME_FUTURE` scores 0, and its clock differs: v23 uses adjusted time, v24 `NodeClock`. Devnet-genesis consensus +100. `nUnconnectingHeaders` resets when a headers message connects (3291 / 3275) |
| (g) `block` (154) | "mutated block +100" | Then `ProcessBlock` and `BlockChecked`, which call `MaybePunishNodeForBlock(via_compact_block=false)` (2333 / 2326) with the full table: consensus or mutated +100, cached-invalid +100 for an outbound peer, invalid header, checkpoint or invalid prev +100, missing prev or ChainLock conflict +10 |
| (h) `getdata` (127) | "> 50,000: +20" | `ProcessGetBlockData` disconnects with no penalty in three cases: the outbound target is reached and the peer asks for a historical or filtered block without the `download` permission (2759-2766 / 2786-2793); a `NODE_NETWORK_LIMITED` node is asked for a block below its limit by a peer without `noban` (2768-2775 / 2796-2803); **v24 only**, a block that can't be read (2820-2827), where v23 asserts |
| (i) DKG (178) | v23: "typed decode of a copy, **catch** +100"; v24: "framing walk … must consume" | Both intake checks also enforce shape rules (+100 on failure), and **the shapes differ by pin**. v23 `CheckDKGMessageStructure` (`dkgsessionmgr.cpp:82-117`, verified): complaint `badMembers.size() == size && complainForMembers.size() == size`; pcommit `validMembers.size() == size`; contrib `vvec == threshold`, blobs in `[minSize, size]`; justification `≤ size`. v24 `net_dkg.cpp:99-159` (verified): bitsets `≤ size` (`ReadBoundedDynBitset` throws above), complaint counts equal, pcommit `≤ size`; `== size` is enforced only later, in `PreVerifyMessage`, after the unrequested +10 (640-646). Effect: an unrequested complaint or pcommit with short bitsets is +100 at v23 and +10 at v24. This is BS9a's decode-intake policy, not an informative rule |
| (j) `qgetdata` (174) | "v24: unknown type +100; v23: replies 'invalid type'. Rate limit +25" | v23 registers every request in the rate-limit map before the type and block checks, so a repeated invalid request gets +25. v24 (`net_quorum.cpp:121-151`, verified): invalid type replies and then +100. An unknown block or one off the active chain replies with no penalty and no registration (never rate-limited). `RequesterLimitExceeded` (64) gives +25 with no reply; `CapacityExhausted` drops silently. +25 applies only to non-`*_MISSING` errors |
| (k) `qfcommit` (171) | "… too old … ; failed `Verify` +100" | Before `Verify`, at both tags: `HasMinedCommitment` returns with no penalty, and so does a local minable commitment with at least as many signers (v23 `blockprocessor.cpp:123-143`, v24 :173-190, verified). An invalid commitment for an already-mined quorum scores 0 in Core, not +100 |
| (l) `mnauth` (172) | "v24: at the per-identity cap, disconnect without a penalty" | **At both tags** (`evo/mnauth.cpp:104-156`, verified at v23): a valid MNAUTH on an outbound probe connection disconnects. If another connection is already verified with the same proRegTxHash, a non-masternode drops the new peer; a masternode uses `DeterministicOutboundConnection` to drop one side or mark the inbound one as a probe. The v24 cap comes on top of these |
| (m) `sendtxrcncl` (165) | "already registered, or version 0: disconnect; a peer we never pre-registered: ignored" | The order is `NOT_FOUND` (ignored), then `ALREADY_REGISTERED`, then `min(peer, ours) < 1` gives `PROTOCOL_VIOLATION` (`txreconciliation.cpp:102-117`, identical at both tags, verified). So version 0 from a peer we never pre-registered is ignored |
| (n) `addr`/`addrv2` (129-130) | after the read: relay gate, > 1000 +20 | Also an addr-fetch connection is disconnected after a message with more than one address (4433-4436 / 4478-4481, verified) |

**Minor rows** (same fix):
- `version` (118) step 2: the desirable flags depend on IBD (`GetDesirableServiceFlags`). After IBD, `NODE_NETWORK_LIMITED` is accepted.
- `inv` (126): the ISDLOCK version rule applies only when `!fAlreadyHave`.
- `isdlock` (169): it omits the asynchronous +20 (`OLD_ACTIVE_SET_FAILURE_MISBEHAVIOR_SCORE`, v23 `net_instantsend.cpp:164-167` / v24 :195-198).
- `dssu`/`dsf`/`dsc` (184): missing gates are CoinJoin client enabled, blockchain synced, `CheckDiskSpace`, and wallet builds only. Every matching session re-reads the same `vRecv`, so a second matching session reads the next bytes or throws.
- `dsa` (180): `ERR_VERSION` is any `nFlags` with bits 0 and 1 both set, not just 3. Promotion and demotion flags also give `ERR_VERSION` below 70241, before V24, or with legacy participants.
- `dsq` (181): read by any non-masternode wallet build not in `-blocksonly`; no loaded wallet is needed.
- `filteradd` (162): the condition is that a `TxRelay` exists, which includes fRelay=0 peers.
- bloom rows (161-162): NODE_BLOOM is per peer, and the `bloomfilter` permission grants it under `-peerbloomfilters=0`.
- `getcfilters` (164): "unknown stop block" also covers `!BlockRequestAllowed`.
- `blocktxn` (157): an unrequested message is ignored first.
- `tx` (155): an already-known transaction is ignored (or relayed under `forcerelay`) before validation.
- `qdata` (175): a quorum missing from the cache returns silently.
- Penalties paragraph (90): `DisconnectNode(addr)` drops every connection from the address, discouragement fires only when the score crosses 100, and the v23 lines (5689-5727) are missing.

**Suggested fix.** Correct these rows, or relabel the "After the read" column as informative: post-decode rules owned by `dashd-p2p`/P2a, re-verified against source when implemented. Rows (i) and N-4 must be corrected in either case, because they are decode-intake policy.

### N-2. Major: `NetInfoV2::decode` reserves about 32× the payload, newly reachable through a transaction

- **Where:** base-sdk `pkgs/primitives/src/types/netinfo.rs:304-309`. `purpose_count` and `entry_count` are bounded only by `data.len()` before `Vec::with_capacity`.
- **Why it's new:** at 33fcc59 the special payload was capped at 10,240 bytes (`MAX_SPTX_PAYLOAD_SIZE`), so the reservation stayed under about 330 KB. BS4a row #3 raised the payload cap to `MAX_SIZE`, so a ProUpServTx or ProRegTx v3 can carry about 3 MiB over P2P (32 MiB in principle).
- **Witness** (`/work/scratch/bs4a-r2-opus/probe-netinfo-{old,new}`, a counting global allocator): a 3,145,593-byte type-2 transaction whose ProUpServ v3 payload declares `purpose_count = 3,145,528`.
  - 33fcc59: `Transaction::decode` rejects it at the payload cap; largest allocation 256 bytes.
  - c887c50: it decodes. `decode_payload()` then makes one **100,655,552-byte** allocation (32.0× the payload) before failing.
- **Exposure:** `Transaction::decode_payload` is the path `Checkable` and EV1's `bad-*-payload` rule use, so one P2P message costs about 100 MB of transient memory. Core batches vector allocation (`MAX_VECTOR_ALLOCATE`), so it doesn't do this.
- **Fix:** bound both counts by the smallest encoded entry, as f124561 does for headers: `into_len(data.len() / 2)` for purposes (u8 purpose plus a count byte) and the minimum `NIEntry` size for entries. Add a test like `header_counts_are_bound_by_the_payload`. Also check the remaining `with_capacity(count)` sites in decoders reachable from a `MAX_SIZE` payload.

### N-3. Minor: dropping the `LlmqType` re-export breaks CS2's tests on re-pin; nothing else breaks

- base-sdk: no in-repo consumer imports `dash_params::LlmqType`. `params/src/regtest.rs` and `types.rs` import `dash_primitives::LlmqType`, and `docs/samples` builds.
- rust-dashcore: `node/integration` doesn't depend on `dash-params`. `node/cs0-node-primitives` uses only `Network`, `ChainParams` and `BuriedDeployment`.
- **`node/cs2-pow-subsidy` breaks:** `dash-consensus/tests/common/mod.rs:15-17` has `use dash_params::{…, LlmqType, Network}` and matches `LlmqType::Llmq50_60` and `LlmqType::Llmq60_75` (lines 122-126). After the re-pin to BS4a this fails to compile. The variant names survive, so the fix is a one-line import from `dash_primitives` (already a dependency there).
- `dash-params`' CHANGELOG has no entry. That follows the repo's "Initial release" convention, but the break should go in the re-pin or merge note, next to the `node/bs4a-conformance-adapt` companion change.

### N-4. Minor: invalid command bytes decode as `Unknown`, but Core drops the message, which matters for gate 4

- **Where:** `bip324.rs:62-63` (`CommandString::from_bytes(*cmd)` with no check) and `decode_with_policy` for V1.
- **What Core does:** live at both pins, a V2 long-form type with a byte outside 0x20–0x7f, or with a non-zero byte after the first NUL, is dropped with the peer kept. V1 is the same (`reject_message`, 0x20–0x7e).
- **What base-sdk does:** returns `Ok(P2pMsg::Unknown)`.
- **Why it matters:** DDE-SWEEP §4.0.1 maps `Unknown` to "on a probe connection gate 4 disconnects". Core never reaches `ProcessMessage` for a dropped message, so it doesn't set `nTimeFirstMessageReceived` either. A probe peer that sends such a message and then `mnauth` stays connected in Core and is disconnected under the table.
- §5's BS7 bullet (DDE-SWEEP:202), "decodes such a command as `Unknown`, which the node also ignores, so the outcome matches", is therefore wrong. So is `command.rs:56-57`, "validated on decode".
- **Fix:** validate in `split_v2` and return a drop-class error, as is already done for `TruncatedV2Type`. Or state in §4.0.1 and §5 that the transport owner must check command bytes before the gates run, and correct the comment.

### N-5. Minor: some "verified" claims are not asserted by the probes

All the outcomes reproduce. These checks would still pass for the wrong reason:
- `probe_gates.py:185-186` asserts only "accept" for the default-node controls. Neither "`qsendrecsigs` throws" nor "`qwatch` +10 on a plain node (verified)" (DDE-SWEEP:79, :148) is asserted, although the log contains both.
- `:174-175` asserts that `sendtxrcncl` with reconciliation off is accepted, which would also pass if the read threw. Assert the "does not have txreconciliation enabled" line and no `Exception`.
- `:171-177` don't check the disconnect reasons. `:180` matches any `(0 -> 100)` line, not the reason.
- `probe_v2.py:80` filters the log with `"peer=%d" % id in l`, so peer=4 matches peer=40–49. Use `p2p.py`'s exact match. `:88` doesn't check for the empty `""` in `Unknown command ""`.

### Nits

- N-6: `P2pMsg::decode_v2_payload` (macros.rs:195) is public and applies no bound; a 50 MB payload decodes. The docs say "for a payload `split_v2` returned". Consider taking the `split_v2` output type, or note it in the docs.
- N-7: `Version` still represents gappy tails (r1 m-2). The assertion was dropped in 370b893 because serde can produce one. Either reject non-prefix tails in serde, or keep the docs as they are.

## Gate (question 5)

I ran it on an exported c887c50 snapshot with `TMPDIR` in `/dev/shm`. Logs: `gate.log`, `gate2.log`, `codeql.log`, `lint_all.log`.

| Step | Result |
|---|---|
| `cargo clippy --all-targets --no-default-features` (`-D warnings`) | pass |
| `cargo clippy --all-targets --features full` (`-D warnings`) | **fail in `dash-pow`** (10 dead-code and unused-import errors). The same failure occurs at 33fcc59, so it predates BS4a and is not caused by it |
| the same two with `--workspace --exclude dash-pow` | pass, pass |
| `cargo nextest run --profile=ci --workspace --features full` | **1117/1117 passed** (without `RUSTFLAGS=-D warnings`, so dash-pow builds) |
| doc tests, `cargo doc` with `RUSTDOCFLAGS=-D warnings`, `bench --no-run`, `build --no-default-features`, `docs/samples` check, `fmt --check` | all pass |
| `lint_all.py --exclude lint_codeql` (uv env with `--extra dev`) | pass, nix skipped |
| `lint_codeql.py check` and `run` (CodeQL 2.27.2) | pass; the analysis ran |
| `lint_commit.py run -r 33fcc59..c887c50` | 33/33 PASS |
| per-commit (author's `percommit.log`); I also spot-checked 53c0763 | p2p_core 31/31 at 53c0763 |

So the gate passes with the known dash-pow workaround; it doesn't pass verbatim. That matches round 1 and the team memory.

## Bench (question 6)

The −2% is real for user-space instructions on block decode.

- `bench/` built from its committed `Cargo.lock` (revs 33fcc59 and c887c50, with an identical `blocks.json5`). 34 blocks × 200 iterations, `perf stat -e instructions:u`, three runs per configuration, the `ITERS=0` load run subtracted.
- Combined binary: old 911.88 M, new 893.88 M, **−1.974%**. The spread across runs is under 0.0001%.
- To rule out a layout artifact of one binary linking both revisions, I also built a separate binary for each revision: old 911.81 M, new 893.82 M, **−1.974%**.
- This matches DDE-SWEEP §6 (911.9 M / 893.9 M) and r1. It is decode-only and instruction-based; as §6 says, wall time on this box can't be compared.
