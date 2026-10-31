# BS7 review r1 (Opus): base-sdk `dash-p2p-core`

| | |
|---|---|
| Under review | base-sdk `node/bsdk-bs7-p2p-core` 481f8c3..**ab017ae** (15 commits, `pkgs/p2p_core`); rust-dashcore `node/bs7-p2p-core` **e7cd6055** (`contrib/node/dde-probe` BS7 tooling, DDE-SWEEP.md) |
| Core pins | v23.1.8, v24.0.0-rc.3 (trees extracted with `git archive` to `/work/scratch/bs7-opus/core/<tag>`) |
| Reviewer | Opus, 2026-10-09 |
| Scratch | `/work/scratch/bs7-opus/` (worktrees `bsdk/`, `rdc/wt/`; API probe `apiprobe/`; differential outputs `diff-*.json`) |
| Verdict | **CHANGES-REQUIRED** (two majors: one API fix, one harness fix; both small) |

## 1. What I checked and how

**Source equivalence, both pins, line by line:**

- **V1 framing.** `V1Header::check` follows `V1Transport::readHeader`: magic, then `MAX_SIZE`/3 MiB, both disconnect (`net.cpp:731-741` / `:732-742`). `check_payload` follows `GetReceivedMessage`: checksum, then `IsCommandValid` (0x20–0x7e before the first NUL, then all NUL); both drop the message and keep the peer (`:796-807` / `:797-808`). Signed `char` makes 0x80+ invalid in V1, and `is_valid_v1` matches. `version_prefix` is magic ‖ 12-byte `version`.
- **V2 receive.** `split_v2` follows `GetMessageType`: empty contents, a short long-form type, an unknown ID, and a bad long-form byte (`uint8_t`, 0x7f allowed) all drop. Reserved slots 29–32 and 169–255 behave as Core's `IsValidV2ShortID` (`net.cpp:1028-1035`, `:1491-1525`). The contents limit equals `MAX_CONTENTS_LEN` (`:1297-1299`).
- **V2 send gate.** `ShortId::for_send` is `V2MessageMap` (emplace keeps the first, so the empty type maps to slot 5, after FEEFILTER's empty slot) plus `m_peer_version >= GetMessageMinVersion`, with `platformban` at 70240 and `m_peer_version` starting at `INIT_PROTO_VERSION` 209. `V2_BITCOIN_IDS`, `V2_DASH_IDS`, `IsValidV2ShortID` and `V2MessageMap` are identical at both tags. The command set (74) equals `allNetMessageTypes` at both pins, and both pins have the same set.
- **headers2.** `CompressedHeader`'s serialization, `Compress` and `Uncompress` match `primitives/block.cpp:57-134` and `block.h:70-190`, identical at both tags:
  - flag semantics: a set bit means the field is on the wire;
  - the time delta is computed in `uint32_t`, so a backwards step goes whole, and `nTime + time_offset` wraps modulo 2^32;
  - the first header restores nothing and saves its (possibly zero) version;
  - an offset past the list leaves version 0 and does not touch the list;
  - the MRU list keeps duplicates and evicts at 7.
  
  The handler path (`np:5246-5266` / `5323-5343`) checks `nCount > GetHeadersLimit(pfrom, true)` (8000 only at common ≥ 70235) before any read, then reads past the data and throws. Hashing happens only inside `Uncompress`. The reply mode (`np:4804` / `4854`: `if (!v_headers.empty()) Compress`, with `send_headers` pushing each header before the next) and the announcement mode (`np:6359` / `6504`) are `Headers2::reply`/`announce`. I confirmed the past-the-list offset, unused flag bits and wrap-around arithmetic with a scratch decode (`apiprobe` case 3: version 0, `time = 100 + (−3)`, bits inherited, one hash, byte-identical re-encode).
- **Inventory.** `InvType` with `command`/`is_known`/`is_gen_tx`/`is_gen_blk` equals `CInv::GetCommandInternal`/`IsKnownType`/`IsGenTxMsg`/`IsGenBlkMsg` at each pin. The only pin difference is `MSG_ASSET_UNLOCK` = 34 → `tx` at v24 (`protocol.cpp:303-305`, `protocol.h:527,557`), which `AssetUnlockInv` selects. `request_interval` equals `GetObjectInterval` at both pins (60 s default).
- **BIP152.**
  - `ShortTxId` is `CustomUintFormatter<6>`. `PrefilledTx.index` is `COMPACTSIZE` into `uint16_t` (range-checked, then u16).
  - `CmpctBlock` throws once the two lists together exceed 65,535, after reading both, as `blockencodings.h` does.
  - `GetBlockTxn` follows `DifferenceFormatter::Unser`; the checked add and the u16 conversion are equivalent to Core's three overflow conditions, because `n ≤ MAX_SIZE`.
  - The keys are single SHA256(header ‖ nonce) words 0 and 1; the short ID is SipHash-2-4 & 2^48−1.
  - The v24 instance-hash change (`blockencodings.cpp:29-33`) is documented on `CmpctBlock::short_id`.
- **BIP37/157.** `filterload` reads `LIMITED_VECTOR(vData, 36000)`, u32, u32, u8. `filteradd` reads `LIMITED_VECTOR(520)`. Both pins catch in the handler (+100, `np:5458-5465`/`5535-5542`, `5492-5498`/`5569-5575`). The BIP157 requests and the send-only `cfilter`/`cfheaders`/`cfcheckpt`/`merkleblock` layouts match. The four have no handler and are silent (`np:5769-5829`), so `P2pMsg::Unread` is right.
- **Small control messages.** `ssc` (i32, i32 after the `IsSynced` gate, `sync_manager.cpp:337-346` / `333-342`), `sendtxrcncl` (u32, u64), `sendheaders2`/`qwatch`/`mempool`/`filterclear` (nothing read).
- **Limits.** Every constant in `limits.rs` equals Core's at both pins. `ProtocolVersion` constants equal `version.h`.

**Runs (my worktree at ab017ae, `TMPDIR=/dev/shm/bs7-opus-tmp`, ≤ 8 jobs):**

- `cargo nextest -p dash-p2p-core --all-features`: 67/67. `--no-default-features` (no_std + alloc): 14/14. `cargo clippy -p dash-p2p-core --all-features --tests --benches -D warnings`: clean. `cargo fmt --check`: clean. `lint_commit.py -r 481f8c3..ab017ae`: 15/15 PASS (`p2p_core%type:`, ≤ 73 columns, ≤ 5 lines). `lint_semgrep.py`: 0 findings. CodeQL (`lint_codeql.py run --lang=rust --with-suite=rust-security-and-quality`, CLI 2.27.2): exit 0, 244/244 Rust files extracted, 0 results from the 6 policy queries and the suite (the CLI was on `PATH`, so this was not the silent exit-77 skip).
- **Independent differentials** on my own regtest v23.1.8/v24.0.0-rc.3 pair (ports 29711/29721, datadirs in `/dev/shm`), a fresh 40-block chain, and `bs7-tool` built from e7cd6055 at base-sdk ab017ae: `bs7_diff_headers2.py` (600 s, seed 4242) ran 1,092 cases at v23.1.8 and 1,072 at v24 (accepted 470/445, rejected 433/432, throw 157/167, too_many 12/11, empty 20/17) with **0 mismatches and 0 errors**. `bs7_diff_framing.py` (250 cases per pin, seed 4343) had **0 mismatches** across deliver, drop and disconnect for ping, unknown and invalid commands.
- **API probe** (`/work/scratch/bs7-opus/apiprobe`, a path dependency on the worktree): see M-1.

**Not re-run:** `bs7_probe.py` (the implementer's probe JSON is gone from `/work/scratch/bs7`; the committed corpora are its output), the oracle header walks, and `fuzz-tx`/`fuzz-block`. I read their code instead (§3).

## 2. Findings

### Major

**M-1. V2 messages can't be decoded under V2 rules once the contents are split; the only owned-payload decoder applies V1 rules, and P2a's `decode_v2_payload` doesn't exist.**
`pkgs/p2p_core/src/bip324.rs:41,65,82`; `src/macros.rs:205-215`.

- **The API gap.** `V2Message` borrows the decrypted contents, has private fields, and only `split_v2` can build one. A1 §2.14 has the reader (P1) do transport work and queue an owned `RawMsg { cmd, payload: Bytes }`; the actor (P2a) decodes later. Once the payload is owned, the only public decoder is `P2pMsg::decode_with_policy`, which applies V1 transport rules:
  - the 3 MiB V1 payload bound (`PayloadTooLarge`, documented as "on which Core disconnects");
  - the V1 command rule (0x20–0x7e, `InvalidCommand`, documented as "Core drops").
- **Core's V2 path differs on both counts.** It accepts payloads up to 3 MiB + 12 after a short ID (`MAX_CONTENTS_LEN`, `net.cpp:1297-1306`). A long-form type containing 0x7f is valid there, and the command then reaches `ProcessMessage` as an unknown command ("Unknown command", processed, not dropped).
- **Reproduced** (`apiprobe` cases 1 and 2):
  - `split_v2(00 "bs7\x7f…")` then `.decode()` → `Unknown`, but `decode_with_policy(cmd, payload)` → `Err(InvalidCommand)`;
  - a short-ID `tx` with a payload of 3 MiB + 5 bytes: `.decode()` → reads the tx, but `decode_with_policy` → `Err(PayloadTooLarge)`.
- **Failure scenario.** P2a follows A1 and the error docs, so a V2 peer sending a legal 3 MiB + 1..12-byte message gets disconnected where dashd keeps it, and a 0x7f long-form type is dropped where dashd processes it as unknown. Both are fingerprintable. The second also changes the masternode-probe "first message" and "prior to verack" accounting.
- **Design mismatch.** P2a's design (§2 table, line 119) already plans on `split_v2`/`decode_v2_payload`, but BS7 doesn't export `decode_v2_payload`.
- **Fix (small).** Add `pub fn decode_v2_payload(cmd: &CommandString, payload: &[u8], ctx: &ProtoCtx)`. It would check `payload.len() ≤ MAX_V2_CONTENTS_LEN − 1` and `cmd.is_valid_v2()` (or trust `split_v2`'s check), then call `dispatch`. Alternatively give `V2Message` an owned form or `from_parts`. Fix `decode_with_policy`'s doc so it says "V1 only". Add a test pinning the two cases above.

**M-2. `diff_p2p_tx`/`diff_p2p_block` count every value divergence as allowlisted, so "0 mismatches outside one allowlisted class" holds only for verdict and bytes read, not for decoded values.**
rust-dashcore `contrib/node/dde-probe/bs7/src/fuzz.rs:143-162`.

- **What the predicate does.** A case is a mismatch only when the verdict or `consumed` differs (`:145`). For every input both sides accept, `rre != cre` (base-sdk's re-encoding differs from the oracle's) goes straight into `allowlisted` (`:160-161`). The allowlist predicate is therefore "any re-encode difference", not a test for the one class PLAN §7 handles.
- **Failure scenario.** A future (or present) base-sdk bug can read the same number of bytes but produce a different value: a field read with the wrong width but the same total, a sign or endianness slip, or a mis-assigned special-payload field. Such a bug would be counted silently under `allowlisted_plan7`, and the run would still report 0 mismatches. The README and DDE-SWEEP §6 present the result as a clean value differential.
- **Fix.** Make the allowlist predicate specific: a classifier for the one approved class, agreed with the manager, so I don't describe it here. Count every other re-encode difference as a mismatch, and re-run the 2 × ~2.8 CPU-h. Until then, word the evidence as "verdict and bytes read; value equality not checked beyond the allowlist".

### Minor

**m-1. The Core-verdict corpus test doesn't compare bytes read, and `gated` cases check nothing.**
`pkgs/p2p_core/tests/core_corpus.rs:249-253`; `gen_bs7_corpus.py:29,56-74`.

- For `read` cases the test only asserts that base-sdk decoded and that the result isn't `Unknown`. It doesn't compare `consumed` with what Core's handler read. A decoder that stopped short of a field Core reads would pass every `*_trailing` case and most valid cases, and be caught only by a matching `*_cut` case.
- `GATED = {sendtxrcncl, ssc}` cases are labelled and skipped. Core never read an `ssc` in the probe, because regtest mnsync had finished. The `sendtxrcncl` on-path read was Core-verified by BS4a's `probe_gates.py` (`-txreconciliation=1`); `ssc` was not.
- **Fix.**
  - Record Core-implied `consumed` for fixed-layout commands; the generator can compute it from the case.
  - Assert `consumed` for `read` cases.
  - Add an `ssc` case on a node whose mnsync isn't finished (a fresh node before `mnsync next`, or `-regtest` with the sync reset).

**m-2. `diff_headers2` accepts every Core "rejected" outcome without a value check when Core's rejection logs no hash.**
`contrib/node/dde-probe/bs7_diff_headers2.py:245-266`.

- When Core reads the message and rejects it, the script compares only the hashes Core logged against base-sdk's decoded hashes. Some rejections log no hash: "non-continuous headers sequence" (+20), more than 8 unconnecting headers (+10), and the every-10th unconnecting +20. Those cases pass whatever base-sdk decoded.
- In my run 433/1,092 (v23) and 432/1,072 (v24) cases were `read/rejected`, so a sizeable share of the differential doesn't check decoded values.
- **Fix.** For rejected cases, also check base-sdk's prediction against the rejection reason. For example, "non-continuous" must coincide with a predicted continuity break, and "> 8 unconnecting" with `headers[0].prev` not being in Core's index. Or send each rejected payload's decoded headers back as a plain `headers` message and compare Core's verdicts.

**m-3. Per-pin inventory (type 34) and the V2 send gate are source-checked only.**
The 80 handler cases have no `inv`/`getdata`/`notfound` case with type 34. Its handling differs observably by pin: v24 tracks and requests it, v23 skips it as unknown. The V2 gate deviation is acceptable (see §4); the inventory one is cheap to close.
**Fix.** Add `inv` with type 34 at each pin and record whether a `getdata` follows. That makes `AssetUnlockInv` Core-judged.

### Nits

- **n-1.** `ProtocolVersion::CURRENT = 70240` (`src/version.rs:23`) is now ambiguous next to `V23_1`/`V24_0`. Nothing in base-sdk uses it, and a consumer on testnet would get the wrong value. Remove it or deprecate it in favour of the release constants.
- **n-2.** `impl BaseCodec for Headers2` (`src/msg/headers2.rs:272-276`) is public and differs from the handler path: no 2000/8000 limit, and it rejects a count the data can't hold before hashing. Document it as "not Core's handler read; use `decode_with_policy`/`decode_with_hasher`". `Headers` (`headers.rs`) has the same issue.
- **n-3.** `CmpctBlock::short_id` (`src/msg/compact.rs:173`) recomputes SHA256 for every transaction. P3 should use `short_id_keys` + `short_id_with_keys`; consider a `ShortIdKeys` value type. There is also no Core-style constructor from a `Block` (coinbase prefilled, random nonce, txid at v23 and instance hash at v24). That's P3/P4's, but worth a line in the changelog's "not provided".
- **n-4.** Doc drift: P2a §3.4.10 item 2 names `encode_reply`/`encode_announce`, and line 119 names `decode_v2_payload`. BS7 ships `Headers2::reply`/`announce`, and `decode_v2_payload` is missing (M-1). Update P2a's design text when M-1 lands.
- **n-5.** Note for P1, no BS7 change needed. Core applies the short-ID gate in `SetMessageToSend`, at dequeue, with the `m_peer_version` current then (`net.cpp:1579`), not when the message is queued. P1 should call `encode_v2` at dequeue with the current common version. Today only Core's own `version` precedes `SetCommonVersion`, so nothing observable depends on this yet.

## 3. Probe harness rigour (summary)

Good:

- Both releases must agree on every case or the generator fails.
- Labels come from Core's own log lines and connection state, not expectations.
- The framing cases include byte-wise delivery.
- The oracle walks cover every mainnet and testnet header via Core-built replies.
- The short-ID table is derived from source, with code-shape assertions that fail if Core's gate changes.
- `diff_headers2` covers the version-dependent count limit (70234 vs 70242) and the compression edge cases (first-header flags, offsets past the list, unused bits, cuts, counts past the data).

Weak spots: M-2, m-1, m-2. Also, `diff_headers2` only covers fresh low-work regtest headers, so the "accepted" path is exercised only for chains Core will index. That's fine for decode equivalence.

## 4. Deviations

| Deviation | Ruling | Why |
|---|---|---|
| Short-ID send gate source-checked only | **Accept** | Deterministic table. `bs7_short_ids.py` asserts the exact code shapes at both tags and fails on drift. P1's M2 interop with dashd exercises it on the wire (common ≥ 70240); the 70235–70239 band stays source-only, which is acceptable |
| Breaking API changes (`CompressedHeader`, `encode_v2(common_version)`, `InvType`/`AssetUnlockInv`) | **Accept** | No consumer in rust-dashcore `node/integration` yet (grep). They're recorded in CHANGELOG, and BS7 re-pins separately (DEC-72) |
| `sendtxrcncl`/`ssc` state gating left to P2a | **Accept** | The gates depend on node configuration and sync state (P2a §3.2 rows). `decode_with_policy`'s doc says so. P2a's table has both rows |
| `govsync` trailing-bytes check skipped (BS9b) | **Accept** | Owner is BS9b (DDE-SWEEP §5); the test lists it in `OTHER_OWNERS` |
| No BIP37 serving logic | **Accept** | Serving is P4's (A1 §2.14 "Serving"); BS7 provides the types |
| Fuzz below 24 CPU-h per target (BL-39) | **Accept as tracked** | PLAN M1 exit 3 still needs ≥ 24 CPU-h clean. M-2's re-run should count toward it once the allowlist is narrowed |

## 5. API fitness for P1, P2a, P3

- **P1:** `V1Header` (`from_bytes`, `check`, `check_payload`, `version_prefix`), `encode_v1`, `split_v2`, `encode_v2`, `ShortId::for_send`, `limits::MAX_V2_CONTENTS_LEN`: sufficient, apart from M-1's hand-off to the actor.
- **P2a:** `decode_with_policy`, `ProtoCtx`, `InvType` + `AssetUnlockInv` (`P2pRules.msg_asset_unlock`), `limits`.
  - The hash-once rule is supported. `Headers2::decode_with_hasher` calls the hasher exactly where `Uncompress` does: in order, only for headers read before a failure, and after the count check. `is_prev_hash_derived(i)` tells the actor that `headers[i].prev_hash` is `X11(headers[i-1])`, so the continuity loop can reuse it and hash only what remains.
  - A `limits::headers_limit(common, compressed)` helper mirroring `GetHeadersLimit` would remove a copy in P2a (optional).
- **P3:** `CmpctBlock`, `short_id_keys`, `short_id_with_keys`, `PrefilledTx`, `GetBlockTxn`, `BlockTxn` are complete for decoding. The v24 instance hash isn't in base-sdk yet (BS6b, DEC-71), and n-3 covers the constructor.

## 6. Verdict

**CHANGES-REQUIRED.** The codec work is careful and, as far as I could verify against both pins (source plus my own differential runs), byte- and reader-equivalent. Two fixes before merge:

1. **M-1:** add a V2-rule decoder for an owned (command, payload), as P2a's design expects, and correct `decode_with_policy`'s V1-only documentation.
2. **M-2:** narrow the `fuzz-tx`/`fuzz-block` allowlist predicate to the approved class, count the rest as mismatches, and re-run.

m-1 to m-3 should land with the fix round, or be recorded as tracked follow-ups. Nits at the implementer's discretion.
