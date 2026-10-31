# BSX: base-sdk leniency catalogue, API-shape review and upstream PR plan

| | |
|---|---|
| **Task** | BSX (DEC-86, DEC-91; PLAN.md:493). It feeds BSX-PR (PLAN.md:494) and the BS7 merge gate (DEC-86). |
| **Status** | Draft for manager review. Uncommitted. |
| **Written** | 2026-10-09 by the BSX research agent (Opus 5.5 xhigh) and five area sub-agents. |
| **Fork** | `node/base-sdk` a1d4dd3 = upstream 33fcc59 plus four merges: 098dafd (BS11a-t, 12 commits), 481f8c3 (BS4a, 38), 23f6fd7 (BS5, 4), a1d4dd3 (BS6a, 19). |
| **BS7** | `node/bsdk-bs7-p2p-core` b69fa55: 26 commits on a1d4dd3, not merged. |
| **Upstream** | dashpay/base-sdk `develop` 33fcc59, which is also the merge base. Fetched read-only. |
| **Core** | v24.0.0-rc.3 (da567d7dba) only (DEC-82). v23.1.8 is cited only to show that a pin difference does not exist. |

Two items are handled under PLAN §7. They appear here only as the stubs in §2.5, and nothing else in this file concerns them.

## 0. Summary

**Counts.** 88 items: 41 leniencies (13 class (a), 24 class (c), 3 class (b), 1 split between (a) and (c)) and 47 strictness, data or API items, 2 of them marked for removal. 12 items carry a (b) flag for pasta (§4). Two more items are handled under PLAN §7 (§2.5). The PR series is in §7.3 (30 units in three tiers) and the questions are in §9.

**Headlines:**

1. **Most of the fork is not leniency.** Many items are strictness fixes, Core-exact limits, v24 data or new protocol types that a strict design wants anyway. They can go upstream after reshaping to kwvg's patterns (Tier 1 in §7).
2. **The consensus-required leniencies are concentrated in two places:** Core's BLS wire rules (B-02, B-03, B-06) and Core's lossy NetInfo reader (A-07, B-10, B-11, B-14). Both are decoder-level, so neither can move entirely into dash-consensus. Upstream gets either named Core-compat functions (the #54 pattern) or nothing; that is kwvg's call, so ask in an issue first (§7.3, Tier 3).
3. **Most other leniencies are (c) under DEC-83.** They are P2P handler tolerances, reject-reason parity, fingerprint-only acceptance, and a Core command-line emulator. They should leave base-sdk. The few the node still wants (handler tolerances, Core error texts, argument parsing) move to rust-dashcore (`dash-p2p`/`dash-args`/`dash-consensus`).
4. **Four upstream bugs make the strict upstream decoder reject honest v24 traffic:**
   - `senddsq`/`qsendrecsigs` carry a flag (A-15);
   - SPV wallets send a 9-field `version` (A-13);
   - getheaders2 replies hold up to 8000 headers (A-24);
   - Core's `govsync` carries a bloom filter tail (A-17, BS9b).

   These are the strongest upstream PRs and carry no leniency.
5. **The BLS synthesis changes the fork.** Payload BLS fields go back to upstream byte bags, and the Core wrapper semantics move node-side (§5.2). If pasta agrees (Q-12), BS6a and the EV0 consumers need reshaping before the fork can converge with upstream.
6. **BS7 merge gate (DEC-86).** BS7's dash-types reads (a06876d, 021ba95, `BaseCodec::BYTE`) are (c): verdicts are unchanged, only the error class and position move, and no Core code observes those. Drop them, and the dependent cursor tests, before merging BS7 into the fork (§2.4).
7. **Correction for A1 §1.4.** Core does not mark a P2P or `submitblock` block with an out-of-range output `BLOCK_FAILED_VALID`; only `-loadblock`/reindex does (§2.1).

## 1. Method and taxonomy

**How items were found**
- Per-commit diffs of `33fcc59..a1d4dd3` and `a1d4dd3..b69fa55`: 99 non-merge commits, 103 with the four merges.
- The review records `reviews/BS{4a,5,6a,7,11at}-r*.md`.
- Five research areas, each run by an Opus sub-agent and triaged by the parent:
  - A: BS4a decode domain;
  - B: BS11a-t BLS and BS6a v24 shapes;
  - C: BS5 dash-params;
  - D: BS7 p2p-core;
  - E: upstream conventions and API shape.

**Where Core evidence comes from**
- Core source at the tag (`~/workspace/dash`).
- The Core-judged corpora already in the fork. They were produced by dde-probe and the oracle shim: `pkgs/primitives/corpus/{decode_domain,payload_core}.json5`, `pkgs/p2p_core/corpus/*.json5`, `pkgs/pkc/corpus/bls_wrapper.json5` and `pkgs/params/corpus/*.json5`.
- One new probe. Area A ran 55 P2P payloads through upstream `P2pMsg::decode_payload` and fork `P2pMsg::decode_with_policy`, and compared both with the Core verdicts recorded in `pkgs/p2p_core/corpus/decode_domain.json5`:
  - 52 vectors came from that corpus;
  - 3 are messages Core sends itself: its 42-byte `govsync`, `senddsq 01` and `qsendrecsigs 01`.
  - Inputs and outputs are in `/work/scratch/bsx/areaA/probe/`. That is scratch space, so copy anything worth keeping.

**Dependencies.** Each merge's commits were replayed onto `develop` in a scratch clone (§7.1).

**Read-only.** No push, PR or comment. Nothing under `~/workspace` was changed except this file.

**Taxonomy**
- **Kind:**
  - **L (leniency):** the fork accepts or represents bytes or states that a strict, invalid-states-unrepresentable design rejects, or adds Core-compat behaviour.
  - **S (strictness):** a strictness or Core-exact fix that a strict design wants as well.
  - **API:** shape only.
- **Effect of the strict-vs-Core difference:**
  - **C:** consensus verdict.
  - **P:** interop with honest peers.
  - **R:** RPC or functional-test reject-reason parity.
  - **E:** error class or reason only.
  - **F:** fingerprint only; it needs crafted input.
- **Class (DEC-86):**
  - **(a)** consensus-required: keep, node-side where possible.
  - **(b)** likely Core bug: goes to pasta.
  - **(c)** unnecessary: remove.

  S and API items get no class unless they enable an (a). An item can carry a (b) flag on top of its class.
- **Confidence:** H, M or L.
- **Paths:** base-sdk paths are at a1d4dd3 (or b69fa55 for BS7) unless marked `up:` (33fcc59). Core paths are under `src/` at the tag unless they start with `test/`.

## 2. Catalogue

### 2.1 Area A: BS4a decode domain (481f8c3)

**Correction for A1 §1.4.** A1-architecture.md:211 says Core marks a block with an out-of-range output `BLOCK_FAILED_VALID`. That is wrong for P2P and `submitblock`:
- `ProcessNewBlock` runs `CheckBlock` first. When it fails, `AcceptBlock` is skipped and nothing is marked (`validation.cpp:4867-4893`; the comment at :4880 says so).
- Only `-loadblock`/reindex marks such a block, through `LoadExternalBlockFile` → `AcceptBlock` (`validation.cpp:5508`, `:5576` → `:4819-4822`).

So for P2P blocks, the failed-index part of A1's argument for A-01 does not hold.

#### A-01 `TxOut.value` decoded as an unchecked signed `CAmount`: L · E+R · (c) · H (Core), M (class)
- **Fork:** fc30b05.
  - `pkgs/primitives/src/transaction.rs:97-100` (`pub value: i64`), `:110` `TxOut::new`, `:118` `amount() -> Option<Amount>`, `:130` `money_range`, `:146` `TxInvalid::OutputNegative`, `:296` the check order.
  - The `serialize::duffs` adapter.
- **Upstream:** decoded `Amount` (`up:transaction.rs:92-95`). So a tx with a negative output or one above `MAX_MONEY` failed to decode, and so did any block holding it.
- **Core:** `CTxOut::nValue` is read unchecked. `consensus/tx_check.cpp:41`, `:43` and `:46` reject with `bad-txns-vout-negative`, `-vout-toolarge` and `-txouttotal-toolarge`: TX_CONSENSUS, for every tx including coinbase. No path accepts such a value; AssetLock's own check (`evo/assetlocktx.cpp:67-73`) runs after `CheckTransaction`.
- **Difference:** the verdict is identical. What differs:
  - **Penalty:** Core gives +100 and a disconnect for a tx (`net_processing.cpp:2021-2045`) and BLOCK_CONSENSUS for a block (`:1975-1983`). A strict decoder gives no penalty.
  - **Block in flight:** with a strict decoder the block's in-flight entry stays until timeout; Core clears it after decoding (`:5372-5383`).
  - **RPC:** `sendrawtransaction`, `testmempoolaccept` and `submitblock` return `bad-txns-vout-*` in Core, a decode failure with a strict decoder.
  - **Functional tests that assert these reasons:** `test/functional/mempool_accept.py:239,248,257` and `test/functional/data/invalid_txs.py:179-197`, used by `p2p_invalid_tx.py:82` and `feature_block.py:165`.
- **Class:** (c) for base-sdk under DEC-83, unless reason and penalty parity count as protocol parity (Q-1).
  - **Node-side if parity is wanted:** a framing walker in dash-consensus. When strict decoding fails, it re-reads the outputs as i64 and emits Core's reason with TX_CONSENSUS. CS0's `decode_tx` already frames raw bytes.
  - **Cost of reverting the fork:** a CS0 change, because `dash-consensus/src/primitives/canonical.rs:92` reads `value` as i64.
- **Upstream:** only if it is reshaped: a `make_num!` signed amount decoded and then judged by `Checkable` (the #7 pattern), not a bare `i64`.

#### A-02 Special-payload length bound raised from 10,240 to `MAX_SIZE`: L · E · (c) · H
- **Fork:** the payload-cap part of 339d43d.
  - `transaction.rs:240` now reads `CompactSize::decode_size(data)?.into_len(data.len())`.
  - `MAX_SPTX_PAYLOAD_SIZE` (`up:codec.rs:10`) and the 10,240 bounds in `impl_payload`/`codec_payload` (`up:codec.rs:47`, `:94`) are removed. This is a breaking change.
- **Core:** `vExtraPayload` is bounded only by `MAX_SIZE` (`serialize.h:42`, `:319`). The verdict comes from `consensus/tx_check.cpp:35`: `bad-txns-payload-oversize` above 10,000, TX_CONSENSUS.
- **Difference:** the same as A-01, as reason and penalty only. Upstream's 10,240 matches nothing in Core. Upstream's `Checkable` already rejects payloads above 10,000.
- **Class:** (c). If reasons are wanted, the A-01 walker can cover this too.

#### A-03 Transaction `Checkable` in Core's order; the oversize check now fires: S · H
- **Fork:** 6762bbb, 8be6b9d, and the `Checkable` part of fc30b05. `transaction.rs:187` `ByteCount`; `:276-278` (serialized size above `MAX_LEGACY_BLOCK_SIZE`, checked before payload size); `:296`.
- **Upstream:** `TxInvalid::Oversize` existed but was never returned, so txs of 1–3 MiB passed. The check order also differed, which changes the reason for a tx with several faults.
- **Core:** `consensus/tx_check.cpp:28-46`.
- **Upstream fit:** a bug fix. The test needs reworking, because the fork's test uses the 339d43d corpus and payloads above 10,240.

#### A-04 Length prefixes range-checked against `MAX_SIZE`: S · H
- **Fork:** 5ce44ad. `pkgs/types/src/compact.rs:119` (`CompactSize::decode_size`), `pkgs/types/src/codec.rs:295` (`Vec<T>`), `pkgs/types/src/adapters.rs:14`.
- **Core:** `serialize.h:319`, where `ReadCompactSize(range_check)` throws "size too large".
- **Upstream fit:** as is.

#### A-05 CbTx `bestCLHeightDiff`, `DynBitset` and NetInfo lengths through `decode_size`: S · H
- **Fork:** the S part of 339d43d, plus test d4a1be3. `pkgs/primitives/src/payload/cbtx.rs:76`, `support.rs:99`, `types/netinfo.rs:183`, `:232`.
- **Core:** `evo/cbtx.h:46` and `:61` (a u32 read as COMPACTSIZE, range-checked), then `evo/specialtxman.cpp:183`.
- **Note:** upstream stored any u64 (`up:cbtx.rs:59`). That is a latent C hazard for a consumer that truncates the value to u32.
- **Upstream fit:** as is, after A-04.

#### A-06 Exact vector batch reservation: S · H
- **Fork:** 73303cd. `pkgs/types/src/codec.rs:17` and `:303`.
- **Core:** `serialize.h:45`.
- **Effect:** memory only.
- **Upstream fit:** as is.

#### A-07 NetInfoV2 keeps one group per purpose, in purpose order: L · C · (a) + (b) flag · H
- **Fork:** ecb74da (test 0c71f50, doc 3c971b1). `pkgs/primitives/src/types/netinfo.rs:431-443` (`groups.entry(purpose).or_insert(group)` in a `BTreeMap`); docs at :410-415.
  - The field type is still `Vec<(NIPurpose, Vec<NIEntry>)>`, but decoding no longer round-trips non-canonical input.
  - Area B's B-12 is the same behaviour, read through BS6a's f53ebe7.
- **Upstream:** kept every group in wire order (`up:netinfo.rs:296`, `:302-315`), and its `check()` rejected a repeated purpose as `Duplicate` (`up:netinfo.rs:341-347`).
- **Core:**
  - `ExtNetInfo::m_data` is a `std::map` (`evo/netinfo.h:401`), read by `UnserializeMap`. Its `insert` keeps the first entry per key (`serialize.h:1232-1243`), and nothing checks the count against the map size.
  - Everything downstream sees only the map:
    - `Validate()` (`evo/netinfo.cpp:588-633`);
    - the triviality checks (`evo/providertx.cpp:166-185`);
    - the ProTx sign string `::SerializeHash(*this)` (`providertx.cpp:390-391`; `specialtxman.cpp:1070-1094`);
    - the stored state and SML entries.
  - Only the funding inputs' sighash, which covers the raw `vExtraPayload` (`script/interpreter.cpp:1437-1438`), and the txid commit to the dropped or reordered bytes.
- **Difference:**
  - Upstream rejects v24-valid ProTxs that repeat a purpose.
  - For out-of-order purposes, upstream would hash and store wire order instead of Core's re-serialized order. That gives a different sig-hash verdict and different SML/state bytes.
- **Class:** (a).
  - **Node-side:** partly. A faithful wire decode plus a `core_groups()` accessor (BTreeMap, first wins) would let dash-evo use the Core view for `Validate`, the sign hash, the shared-reg hash and SML/state bytes, while base-sdk keeps upstream's round-tripping value.
  - Entry-level normalization (B-10, B-11) still needs a Core reader, so see §5.3.
- **(b) flag:** Core accepts a non-canonical map (repeated or out-of-order keys) whose dropped bytes only the creator's input signatures cover. Core should reject it, ideally before V24 activates on mainnet (Q-2). Not security-relevant: there is no crash, no split between Core nodes and no unbounded resource use.

#### A-08 `decode_payload` reports `consumed`/`len`/`trailing`: API · (c) · H
- **Fork:** 2ea2b13, 686db6a. `transaction.rs:394` returns `Option<Result<PayloadDecode, PayloadError>>`; `payload/mod.rs:386` has `PayloadDecode { pub payload, pub consumed, pub len }`, with `:398` and `:403`.
- **Upstream:** a strict `decode_payload` that rejects trailing bytes.
- **Core:** `GetTxPayload` requires `ds.empty()` (`evo/specialtx.h:18-28`), and every caller treats a throw and leftover bytes alike.
- **Difference:** none. Upstream already gave the same verdict and reason.
- **Class:** (c) as a leniency; what remains is API taste. CS0 uses it (`dash-consensus/src/primitives/tx.rs:5`, `:139-153`).
- **Upstream:** keep the strict name. A lenient report would need its own name and private fields.

#### A-09 LLMQ types 103 and 105 named: S · H
- **Fork:** cd1d38b. `pkgs/primitives/src/support.rs:19`, `:39`, `:43`.
- **Upstream fit:** as is.

#### A-10 params reuses `dash_primitives::LlmqType`: API (breaking) · H
- **Fork:** 9277360 and a258c7b; `pkgs/params/src/types.rs:15`. This renames `LlmqTestInstantSend` to `LlmqTestInstantsend`.
- **Upstream fit:** as is, squashed, after A-09.

#### A-11 `ProtocolVersion` is i32; `INCREASE_MAX_HEADERS2` added: S · H
- **Fork:** 47b93a7. `pkgs/p2p_core/src/version.rs:14`, `:37`.
- **Core:** the P2P protocol version is a signed `int` (`version.h`).
- **Probe:** `protocol_version_minus_1` decodes as 4294967295 upstream and as −1 in the fork, which a handler rejects as obsolete.
- **Upstream fit:** as is.

#### A-12 `CoreBool` (any non-zero byte is true) replaces the 0/1 `bool` codec: L · F · (c) · H
- **Fork:** dd859d9 and 0ba7bf5 (removes `impl BaseCodec for bool`). `pkgs/types/src/core_bool.rs:23`, `:26`.
- **Used in:** version `relay`/`fMasternode`, senddsq, qsendrecsigs, and SML `is_valid` (`msg/mn_list.rs:155`, `:175`).
- **Core:** `serialize.h:239` reads any byte. No consensus structure holds a bool. Honest Core, dashj and DashSync send 0 or 1.
- **Probe:** `version/relay_02`, `relay_ff`, `senddsq_02` and `qsendrecsigs_02` are accepted by Core and rejected by upstream.
- **Class:** (c). Keep the strict `bool`. Upstream #4's doc claim that the bool codec "matches Dash Core" is wrong and worth fixing.

#### A-13 `version` optional trailing fields read as Core reads them: L · P · (a) · H
- **Fork:** dd090ee, ec2ccd2, 389cd42, ad0c785 and a1dc00c.
  - `msg/version.rs:193-217`: seven `Option` fields.
  - `:222`, `:249`, `:266`, `:280`, `:296` (`has_prefix_tail`); `:98` caps `UserAgent` at 256.
  - `pkgs/types/src/serialize.rs:165` (`opt_str_u64`).
- **Upstream:** all 11 fields are mandatory.
- **Core:** `net_processing.cpp:3935-4015` reads each optional group `if (!vRecv.empty())`. dashj and DashSync send 9 fields, ending at `relay`.
- **Probe:** upstream rejects all six Core-accepted cuts. One of them, `cut_104_through_relay`, is exactly the dashj/DashSync shape.
- **Class:** (a). It is an upstream bug: SPV wallets cannot connect.
  - **Node-side:** no. It is the message's wire definition.
- **Upstream fit:** reshape to a nested `Option<Tail>` so only prefix forms are representable (BS4a r1 m-2), with strict bools.

#### A-14 `VersionTail { head, error }` keeps the version head when a later field fails: L · F · (c) · H
- **Fork:** 6f136d2. `msg/version.rs:326` (`MsgDecode`); `pkgs/p2p_core/src/error.rs:75`.
- **Core:** applies head checks (obsolete version, self-connection) before reading a broken tail. Probe: `obsolete_short_tail`.
- **Class:** (c). The node can decode the head itself and apply head checks first.

#### A-15 `senddsq` and `qsendrecsigs` carry one flag byte: S/L · P · (a) · H
- **Fork:** bfdc8ce moves them out of `stub_empty`; f652f44 types them. `msg/control.rs:17`, `:27`; `msg/mod.rs:199-214`.
- **Upstream:** these were in `stub_empty`, so any payload gave `PayloadNotEmpty` (`up:msg/mod.rs:189,191`; `up:macros.rs:36-42`).
- **Core:** sends `SENDDSQUEUE(true)` to every peer that is not blocks-only (`net_processing.cpp:4224-4226`), and `QSENDRECSIGS(true)` (`net.cpp:4385`, `evo/mnauth.cpp:171`). It reads one bool (`net_processing.cpp:4485-4505`).
- **Probe:** upstream rejects the `captured` senddsq that v23 and v24 nodes actually exchanged.
- **Class:** (a). It is an upstream bug that breaks every Core peer.
- **Upstream fit:** as typed structs with strict `bool` fields.

#### A-16 `Pong { nonce: Option<u64> }`: L · F · (c) · H
- **Fork:** 157e286. `msg/ping.rs:35-58`.
- **Core:** `net_processing.cpp:5505-5527` cancels the ping on a short pong, which it calls "most likely a bug in another implementation".
- **Class:** (c). The node can check `payload.len() < 8` before decoding.

#### A-17 Typed P2P messages ignore trailing bytes: L · P (through govsync) / F · (a) as handler policy · H
- **Fork:** bfdc8ce. `pkgs/p2p_core/src/macros.rs:190-214`; `DecodedMsg { msg, consumed }` at `:122`.
- **Core:** handlers read what they need.
- **Probe:** upstream rejects Core's own 42-byte `govsync`, because `GovSync { hash, bloom_filter }` lacks the filter's three trailing fields (`governance/net_governance.cpp:59`, `:93-97`, `:264`; `node/sync_manager.cpp:36`). The fork accepts it with `consumed=33`. Upstream also rejects `pong/matching_nonce_trailing`, `headers_trailing` and `version/trailing_5`.
- **Class:** (a) as policy, and it can live node-side: cursor-based `BaseCodec` decoding already leaves the remainder to the caller. The real fix for P is a complete `GovSync` (BS9b), which is an upstream bug fix in its own right.
- **Upstream fit:** a neutral "decode a prefix, report consumed" entry point, if kwvg wants one. Otherwise keep the strict `decode_payload`.

#### A-18 Empty-payload commands ignore their payload: L · F · (c) · H
- **Fork:** bfdc8ce (the `parsed_empty`/`stub_empty` arms of `macros.rs:190-214`). It removes `P2pDecodeError::PayloadNotEmpty`, which kwvg added on purpose in #18.
- **Probe:** `verack_with_payload` and `sendheaders2_with_payload`.
- **Class:** (c). Restore `PayloadNotEmpty` upstream and in the fork; the node can ignore it.

#### A-19 `P2pMsg::Unknown { command, payload }` instead of an `UnknownCommand` error: L · F · (c) as a leniency, FIT as API · H
- **Fork:** bfdc8ce. `macros.rs:112`, `:212`, `:229`.
- **Core:** logs "Unknown command" and keeps the peer.
- **Upstream fit:** the open-enum `Unknown(raw)` pattern (#11, semgrep `enum-bare-unknown`) and rust-bitcoin's `NetworkMessage::Unknown` are precedent. Optional.

#### A-20 `InvalidCommand`, with `CommandString::is_valid_v1`/`is_valid_v2`: S · H
- **Fork:** 99d090f. `pkgs/p2p_core/src/command.rs:65`, `:72`, `:77`; `error.rs:49`.
- **Core:** V1 checks at `protocol.cpp:232` and `net.cpp:804`, where Core drops the message and keeps the peer. V2 checks in `GetMessageType` (`net.cpp:1496-1540`), which allows 0x7F. The difference between the two is a harmless Core quirk, not (b).
- **Upstream fit:** the predicates as a standalone API.

#### A-21 3 MiB payload bound for every command: S · H
- **Fork:** f45f641. `pkgs/p2p_core/src/codec.rs:15`; `macros.rs:183`.
- **Core:** `net.cpp:737-738`; `net.h:84`.
- **Upstream fit:** as is.

#### A-22 V2 contents bounded by 1 + 12 + 3 MiB before typing: L/S · F · (c) · H
- **Fork:** 5cf3bd7. `codec.rs:21`; `bip324.rs:87`; `error.rs:40`.
- **Core:** `net.cpp:1297-1304`. A short-ID message can carry 12 more payload bytes than the V1 limit.
- **Class:** the extra 12 bytes are (c). The check order is S.

#### A-23 V2 typing split from payload decoding; reserved short IDs become `Unknown`: API/S, L (reserved IDs) · F · (c) for the reserved-ID rule · H
- **Fork:** 463429e and b24511e.
  - `bip324.rs:37` `is_core_short_id`; `:45` `V2Message`; `:69`; `:86` `split_v2`; `:123` `decode_v2(contents, &ProtoCtx)`.
  - `error.rs:54`, `:56`.
- **Core:** `GetMessageType` (`net.cpp:1496-1540`).
- **Upstream fit:** the split as a pure framing API. The `ProtoCtx` form stays node-side.

#### A-24 headers2 count limit of 8000 when the common version is at least 70235: S/L · P · (a) · H
- **Fork:** bfdc8ce. `msg/headers2.rs:289-291`; `msg/headers.rs:24`.
- **Upstream:** a fixed `into_len(2_000)` (`up:headers2.rs:19`, `:258`).
- **Core:** `GetHeadersLimit` (`net_processing.cpp:1210-1215`) applies to the getheaders2 reply (`:4819`). An honest v24 peer can send up to 8000 headers.
- **Class:** (a). It is an upstream bug.
- **Upstream fit:** drop the fixed codec cap and bound the count by the payload (kwvg #4, "drop premature limits"). Export the limits; the node applies the version-dependent limit.

#### A-25 `TooManyEntries`; header counts bounded by the payload: S/API · H
- **Fork:** bfdc8ce and 89f4635. `msg/headers.rs:21`, `:24`, `:27`, `:32`, `:40`; `msg/headers2.rs:23`; `error.rs:63`.
- **Core:** a count above the limit costs the peer 20 points (`net_processing.cpp:5322-5340`).
- **Upstream fit:** the payload-bounded allocation fits. `TooManyEntries` is handler policy and stays node-side (§5).

#### A-26 headers `tx_count` ≠ 0 accepted: L · F · (c) · H
- **Fork:** bfdc8ce. `msg/headers.rs:119`.
- **Upstream:** `into_len(0)` (`up:headers.rs:66`).
- **Core:** reads the count and ignores it. Honest peers send 0.
- **Class:** (c).

#### A-27 `ProtoCtx`, `MsgDecode`, `DecodedMsg`, `decode_with_policy`: API · node-side · H
- **Fork:** bfdc8ce. `pkgs/p2p_core/src/ctx.rs:20`, `:29`; `codec.rs:31`; `macros.rs:122`, `:177-187`.
- **Dependency:** `ProtoCtx` holds BS11a-t's `BlsCtx`, so it has a compile dependency on 6fe1011 (§7.1).
- **Upstream fit:** none. It is peer-negotiated state (#7: "Base SDK is a stateless library"), so it stays node-side.

**Which P2P items matter for honest v24 traffic** (from area A's probe):

| Item | Honest traffic that upstream mishandles |
|---|---|
| A-13 version tail | dashj/DashSync 9-field `version`: **yes** |
| A-15 senddsq/qsendrecsigs | every Core peer: **yes** |
| A-24 headers2 8000 | v24 peers answering getheaders2: **yes** (shown from code; the probe vector was truncated) |
| A-17 trailing bytes | Core's `govsync`: **yes**, through the incomplete `GovSync` (BS9b) |
| A-12, A-14, A-16, A-18, A-19, A-22, A-23, A-26 | none |

No behaviour change: e892086 (lint), 571e94e (test labels), a15c4e1 (corpus regeneration), f6b8693 and 3f02caa (changelogs).

### 2.2 Area B: BS11a-t BLS (098dafd) and BS6a v24 shapes (a1d4dd3)

**Upstream baseline.**
- At 33fcc59 every payload BLS field is an unvalidated byte bag, `BlsPkBytes<BlsScIetf>`/`BlsSigBytes<BlsScIetf>` (`pkgs/pkc/src/bls/public_bytes.rs:19-22`).
- So compared with upstream, the fork's BLS work adds strictness (the "malleable" throw) and precision (VALID or INVALID by scheme). The "L" marks in this section are measured against a strict design, not against upstream.
- Txids are safe: `Transaction` keeps the raw `extra_payload` (`transaction.rs:222`).

#### B-01 Run-time scheme selection (`BlsCtx`, `BlsSchemeKind`, `BlsSchemeSource`): API · (a)-enabling · H
- **Fork:** 6fe1011 (26c91c9 adds the payload sources).
  - `pkgs/pkc/src/bls/scheme_ctx.rs:18-48` (`BlsSchemeKind`, `from_legacy` :27, `opposite` :41), `:55-85` (`BlsCtx`), `:104-120` (`BlsSchemeSource { Version, Global(BlsCtx), AlwaysBasic }`).
  - Re-exported at `bls/mod.rs:26`.
- **Core:** picks the scheme in three ways:
  - the process flag (`bls/bls.h:180`, `:206-209`, switched at V19, `evo/specialtxman.cpp:983`, `:1008`);
  - the object's version (`evo/providertx.h:180`, `:301`, `:346`, `:419`; `llmq/commitment.h:110-113`);
  - always basic (`evo/mnhftx.h:49`, `providertx.h:533`).
- **Class:** (a)-enabling. Payload decoding needs a scheme chosen from the version, and a type parameter cannot express that.
  - **Node-side:** deriving the ctx already is node-side (`bls_ctx_for_block`, dash-evo API.md:27, :81; A1:354).
- **Shape:** `BlsSchemeSource::Global` has no production caller; it is used only in `scheme_ctx.rs:144-145` and a test. §5.2 moves `BlsCtx` and `BlsSchemeSource` node-side.

#### B-02 Legacy decode with no curve or subgroup check ("Exotic" points are VALID): L · C · (a) · H (rule), M (history)
- **Fork:** c2c6cba. `pkgs/pkc/src/bls/wrap_codec.rs:49-62` (`BlsPoint { Usable, Exotic }`), `:178-196`, `:223-240`, `:253-258`.
- **Core:** relic `SetBytes` (`bls.h:100`) and the `CheckMalleable` round trip (`bls.h:211`), with no curve or subgroup check.
  - The derived rule is proven equal to relic: DEC-40/46 and `reviews/BS11at-r1-opus.md` §2.
  - Vectors: `contrib/vectors/bls_wrapper/` (f9a942c), `pkgs/pkc/tests/bls_wrapper.rs` (20aac32).
- **Where legacy bytes occur:**
  - ProTx v1 operator keys and signatures (`providertx.h:180`, `:301`, `:346`, `:419`);
  - QC v1/v2 (`commitment.h:48-49`, `:110-113`), which a non-null QC can carry only before V19 (`commitment.cpp:131`).
  - CbTx CL and AssetUnlock signatures follow the flag (B-06). MNHF signatures and the ProUpSharedRegTx key are always basic.
- **Difference:** Core accepts a key once `pubKeyOperator.Get().IsValid()` holds (`providertx.cpp:300`, `:468`), Exotic keys included. A strict decoder rejects such a tx, which is C.
- **Class:** (a). The decoder must accept these bytes, so base-sdk has to carry at least "canonical legacy encoding, no point check".
  - **Node-side:** only partly. The node needs field arithmetic, which lives in dash-pkc behind its isolated `blst` use. §5.2 proposes an explicitly named Core wire-form classifier for dash-pkc.
- **History:** A1:318 and :2345 assume O6 scan (h) will find no Exotic legacy field in history. That scan has not been run. Q-6 asks to run it over blocks and the current MN list.

#### B-03 Eager three-outcome decode (VALID, INVALID or throw): L+S · C · (a) + (b) flag · H
- **Fork:** c2c6cba (type), 4e6bb33 (adoption).
  - `pkgs/pkc/src/bls/wrap_eager.rs:28`, `:41-51`, `:64-76`, `:108`; `wrap_value.rs:26-51`, `:74-90`.
  - `payload/bls.rs:59-77`.
  - Fields: `quorum.rs:99-111`, `proupservtx.rs:111`, `prouprevtx.rs:73`, `mnhftx.rs:47`.
- **Core (`bls.h:184-203`):**
  - bytes canonical in the chosen scheme give a valid object;
  - bytes canonical only in the opposite scheme give `Reset()`, an invalid object;
  - bytes canonical in neither throw "malleable BLS object" (:196), so `GetTxPayload` fails with `bad-*-payload`;
  - zeros give an invalid object.
- **Which outcome matters:**
  - **The throw is S:** upstream's byte bags accepted these bytes.
  - **Zeros are the null encoding:** a strict design models them as `None`.
  - **Opposite-scheme bytes are L/C:** Core treats them as null. `IsNull` requires every BLS field to be invalid (`commitment.h:118-131`; `llmq/blockprocessor.cpp:124`, `:369`), so Core accepts a null QC that carries opposite-scheme bytes, and a strict decoder would reject that block. For ProUpServTx and ProUpRevTx signatures, an invalid signature fails verification anyway, so only the reason differs (E).
- **Class:** (a).
  - **Node-side:** yes, if payload fields stay byte bags. The node classifies each field under its version's scheme with the §5.2 classifier, and turns "neither" into Core's payload error.
- **(b) flag:** the reset exists for a fork-transition race (`bls.h:191-200`). It also gives null QCs and the CbTx CL (B-06) a second null encoding made of arbitrary non-zero bytes. These structures are miner-made and txids commit to the bytes, so the harm is low (Q-7).

#### B-04 `DecodeError::MalleableBls`: API (breaking) · (a)-supporting · H
- **Fork:** 4bfb8fc. `pkgs/types/src/codec.rs:55`, `:78`, `:102`; mapped at `payload/bls.rs:57-63`, `:101`.
- **Core:** `bls.h:196`.
- **Note:** this is error precision only (E). Area E judges that BLS knowledge in dash-types breaks the layering (§5): carry it as `DecError(BlsError)` or a primitives payload error.

#### B-05 `BlsLazy` (lazy decode, captured scheme, Core's serialize and hash states): L · E (laziness), C (scheme-carrying serialization) · laziness (c), holder (a) · H/M
- **Fork:** c2c6cba, e45ca9e, b561560 and 390cef6; adopted in 4e6bb33.
  - `pkgs/pkc/src/bls/wrap_lazy.rs:21` (`spin::Mutex`), `:67-75`, `:89-110`, `:123-222`.
  - `payload/bls.rs:79-124`.
  - Fields: `proregtx.rs:144`, `:199`; `proupregtx.rs:90`, `:114`; `proupsharedregtx.rs:55`, `:65`.
- **Core:** `CBLSLazyWrapper` (`bls.h:392-575`). It stores bytes and the scheme and never throws on read (:467-475). Unique-property keys come from its serialization (`evo/deterministicmns.h:459-475`, `:526`, `:593-598`). A migration re-encodes the key under the new scheme (`specialtxman.cpp:71`).
- **Does laziness change verdicts?** No. Every consensus path calls `Get()` first: `IsTriviallyValid` fails with `bad-protx-key-null` (`providertx.cpp:300`, `:468`, `:579`). A key canonical in neither scheme is rejected either way; only the reason differs (E).
- **What the node does need:** a serialization that carries the scheme, and its hash, for unique-property hashes and SML bytes after a scheme migration. That needs bytes, the scheme and transcoding, not laziness.
- **Class:** laziness is (c) unless MN-list load benchmarks need it, which is Core's own reason for it (Q-8). The scheme-carrying holder is (a).
  - **Node-side:** yes. Put an eager holder in dash-evo: wire bytes, scheme and decoded value, rejecting canonical-in-neither at decode. Verdicts are identical, and Eq/Hash come back (B-21).

#### B-06 `BlsGlobal`; CbTx CL and AssetUnlock signatures decoded without a scheme: L+S · C · (a), flag deferral (c) unless regtest parity is needed · H
- **Fork:** ac9634d, 60d6090 and 7f3f483.
  - `pkgs/pkc/src/bls/wrap_global.rs:38-46`, `:56-64`, `:71-80` (`get(ctx)`), `:87`, `:92`.
  - `cbtx.rs:43`, `:77`, `:97`; `assetunlock.rs:40`, `:53`, `:63`.
- **Core:** plain `CBLSSignature` read under the global flag (`bls.h:206-209`). A CL signature that is not valid counts as null (`evo/cbtx.cpp:205-208`; `specialtxman.cpp:169-181`).
- **Difference:**
  - Decoding rejects bytes canonical in neither scheme (S).
  - Opposite-scheme bytes give a null CL, which Core accepts when the previous coinbase had no CL (L/C).
  - For AssetUnlock, an invalid signature fails verification, so only the reason differs (E).
- **Flag:** on mainnet and testnet the flag is always basic for these fields, so `get(ctx)` equals a basic eager decode. Only regtest/devnet activation orders such as V20 before V19 need the deferral (A1:354, :990).
- **Class:** (a) for "opposite scheme means null". The deferral is (c) unless pasta wants parity with V20-before-V19 regtest runs (Q-9).
  - **Node-side:** yes, the same as B-03. If parity is not needed, decode these fields with `AlwaysBasic` and drop `BlsGlobal`.

#### B-07 `BlsValue::from_set_bytes_basic`: L · P · (c) for now · M
- **Fork:** fcf26a9, 97096e7 and 3b1d38f. `wrap_value.rs:57-71`. No production caller.
- **Core:** `SetBytes(bytes, false)` with no round trip, for governance object and vote signatures (`governance/object.cpp:569`, `governance/vote.cpp:201`). The doc's mention of CoinJoin is unconfirmed.
- **Difference:** only for a G2 subgroup point with y in Fp and the sign bit set; none is known.
- **Class:** (c) now. Revisit in BS9b (Q-10).

#### B-08 ProTx and commitment scheme rules exposed: S · (a) · H
- **Fork:** 8685005 (sources in 26c91c9). `payload/bls.rs:20-49` (`protx_bls_scheme`, `commitment_bls_scheme`).
- **Core:** `providertx.h:180`, `:301`, `:346`, `:419`; `commitment.h:48-49`, `:110-113`.
- **Upstream fit:** stateless protocol data. Expose it as methods on the payload types (area E), returning the scheme marker id.

#### B-09 BLS encoders write under Core's write-time scheme: S/API · (a)-supporting · H
- **Fork:** 73ab4b0. `payload/bls.rs:104-124`; `cbtx.rs:97`; `assetunlock.rs:63`.
- **Core:** lazy serialization at `bls.h:446-457`; eager serialization writes `ToBytes` (`bls.h:180`), so an invalid value is written as zeros.
- **Deviation:** for a lazy field that failed `Get()`, Core writes the raw bytes until `Get()` runs, and zeros after. The fork always writes zeros. This affects only payloads Core rejects (E).
- **Round trip:** decode then encode is no longer the identity for such fields. That is F, and harmless to txids.
- **Class:** goes with B-05. Node-side with the holder. Q-11 asks whether a raw passthrough encoder should exist.

#### B-10 NetInfo `Invalid` entry (`ff`) for unknown tags and invalid entries: L · C (via A-07), E otherwise · (a) · H
- **Fork:** f53ebe7. `pkgs/primitives/src/types/netinfo.rs:153-173`, `:177-193`, `:208`, `:361`.
- **Upstream:** an unknown tag fails decoding (`up:netinfo.rs:164-167`). kwvg removed `NIEntry::Invalid` on purpose in #12.
- **Core:** `NetInfoEntry::Unserialize` (`evo/netinfo.h:243-263`) calls `Clear()` on an unknown type (:262) or an invalid domain (:261). `Serialize` writes a lone `ff` (:229-239, :197).
- **When it changes a verdict:** in a kept group the entry fails `Validate` (`netinfo.cpp:588-596`; `providertx.cpp:318`, `:422-430`), so only the reason differs (E). In a group dropped by A-07, Core accepts, so C.
- **Class:** (a). It is decoder behaviour, so it cannot be fully node-side (§5.3).

#### B-11 Caught Service read failures and the cursor after them: L · C (via A-07), E otherwise · (a) + (b) flag · H (rule), M (b)
- **Fork:** f53ebe7 (vectors b64d9f1). `netinfo.rs:213-222`, `:224-257`, including the CompactSize consumption rule at :228-229.
- **Core:** `netinfo.h:247-252` catches the Service read failure and calls `Clear()`. The stream keeps what was consumed and parsing continues. Domains propagate their failure (:254-261).
- **Core-judged vectors:** `netinfo_size_noncanonical`, `netinfo_size_513` and `netinfo_unknown_net` in `payload_core.json5`.
- **Class:** (a), decoder-level.
- **(b) flag:** Core resumes mid-field after a swallowed failure, so the wire bytes and Core's re-serialization (a lone `ff`) differ.

#### B-12 → see A-07
Duplicate purpose groups, first one wins. This is the same behaviour as A-07, introduced in ecb74da; f53ebe7 only rewrote the docs around it. Area B's evidence is consistent with A-07, and it adds the sign-string comment at `providertx.cpp:390` ("protection against malleability").

#### B-13 Domain length cap of 253: S · (a) · H
- **Fork:** f53ebe7. `netinfo.rs:29`, `:180-190`, `:300-306`.
- **Core:** `LIMITED_STRING(m_addr, 253)` (`netinfo.h:25`, `:178`). The domain read sits outside the try, so the whole payload fails.
- **Upstream fit:** as is, once carved out of f53ebe7.

#### B-14 Invalid legacy address re-encoded as zeros: L · C · (a) + (b) flag · H
- **Fork:** f53ebe7. `netinfo.rs:620-641`, `:684`.
- **Upstream:** kept the raw service (`up:netinfo.rs:482-485`).
- **Core:**
  - `MnNetInfo::Unserialize` wraps `NetInfoEntry{service}`, which stays invalid for a non-valid service (`netinfo.h:343-347`, `:212-216`). `Serialize` writes `CService{}` (:326-334), and `IsEmpty` follows the entry (:355).
  - ProRegTx accepts an empty NetInfo (`providertx.cpp:318`). So Core accepts a ProRegTx with a non-zero invalid address as a masternode with no address.
  - The SML entry, and with it `merkleRootMNList`, holds zeros, and so does the sign string (`providertx.cpp:390-391`).
  - ProUpServTx rejects an empty NetInfo (`:426-428`), so there the difference is only the reason (E).
- **v23 comparison:** the same at v23.1.8 (`netinfo.h:204-209`, `providertx.cpp:95`), so there is no difference between pins.
- **Class:** (a).
  - **Node-side:** a raw service in base-sdk plus a `core_effective()` accessor that the node uses for state and hashes (Q-13).
- **(b) flag:** Core keeps nothing of the wire address, and the creator's input signatures are the only thing covering it.

#### B-15 Unknown `ExtNetInfo` version reads nothing more: L · E · (c) · M-H
- **Fork:** f53ebe7. `netinfo.rs:422-433`, `:447-450`, `:467`, `:534-541`.
- **Core:** `netinfo.h:412-428`. `Validate` reports Malformed (`netinfo.cpp:590-591`), so the tx is rejected either way.
- **Class:** (c). base-sdk can reject an unknown `ExtNetInfo` version at decode.

#### B-16 Empty NetInfo reported as Core reports it: S · (a) · H
- **Fork:** 3c2b5ae (tests in 3613593). `netinfo.rs:387-391`, `:589-591`, `:684`.
- **Upstream:** treated NetInfo as empty when every group was empty, for any version (`up:netinfo.rs:451-453`). Upstream's ProRegTx path would therefore accept as empty a value Core rejects (reverse C).
- **Core:** `netinfo.h:442`, `:355`.
- **Upstream fit:** as is. It reuses f53ebe7's codecs, so it needs reshaping onto upstream's.

#### B-17 Transaction types 10–12: S · (a) · H
- **Fork:** 4e6bb33. `prodistx.rs`, `proupsharetx.rs`, `proupsharedregtx.rs`; dispatch at `payload/mod.rs:367-371`, `:531-533`.
- **Core:** `primitives/transaction.h:39-41`; `providertx.h:436-461`, `:499-502`, `:530-544`.
- **Upstream fit:** as is, with BLS fields as byte bags.

#### B-18 v3 payout and share lists: S · (a) · H
- **Fork:** 4e6bb33 and 26c91c9 (docs in c0a08c8). `shares.rs:9`, `:36-52`, `:68-90`, `:99-105`; `proregtx.rs:147-163`, `:202-208`.
- **Core:** `providertx.h:31-76`, `:184-208`, with share limits at :134-138.
- **Bends:**
  1. Counts above 255 wrap on encode, as in Core.
  2. **The encoder does not check `join_sigs.len() == shares.len()`** (`proregtx.rs:203-205`), whereas Core throws (:192-196). A mismatched value encodes to bytes that decode as something else.
  3. `CollateralShare.amount` is a raw `i64`, which is `CAmount`.
- **Upstream fit:** after fixing bend 2 (Q-14: fallible encode, or a `Checkable` rule).

#### B-19 CbTx v4 and an `i32` height: S/API · (a) for v4, (c) for the type change · H
- **Fork:** 4e6bb33. `cbtx.rs:32`, `:57`, `:79-80`, `:100`, `:153-158`.
- **Core:** `int32_t nHeight` (`evo/cbtx.h:43`, `:56`).
- **Difference:** the bytes are identical, and a negative height never matches a block. The type change is verdict-neutral (Q-15).

#### B-20 AssetLock and AssetUnlock v2, and `AssetLock::check`: S · (a) + node gate · H
- **Fork:** 4e6bb33 and 283ebc0. `assetlock.rs:65-92`; `assetunlock.rs:27`.
- **Core:** `evo/assetlocktx.h:35-36`, `:78-84`. v2 is accepted only with V24 (`assetlocktx.cpp:56-61`, `:207-212`). Credit scripts are checked at `:74-80`.
- **Bend:** `check()` has no context, so it accepts v2 before V24. The node needs a V24 gate in dash-consensus for both lock and unlock.

#### B-21 Lint exemptions: API/lint · H
All in `maint/codeql/rust/lib/policy.qll`.
- **6644f85 (:240-244) and 4bae81f (:133-145, :245-247):** exempt `BlsLazy`, and types holding one, from the Eq/Hash rule. As a result `ProRegTx`, `ProUpRegTx`, `ProUpSharedRegTx` and their wrappers lose Eq/Hash. These stand or fall with B-05, and they never go upstream: reshape the types, not kwvg's policy.
- **b497f65 (:52-73):** names the public `Share` types in the secret-type rule.
- **0621fcb (:147-151):** restores a doc comment.
- **Upstream:** b497f65 and 0621fcb as a separate `sdk%lint(codeql)` change, raised with kwvg.

#### B-22 Test and vector infrastructure: tests · H
- **Commits:** f9a942c, 20aac32, 3b1d38f, 28f68ee, b64d9f1, 550a249 and 3613593. These are Core-judged BLS wrapper vectors and payload vectors at both pins.
- **Upstream fit:** each test travels with its feature. Upstream PRs carry regenerated, tag-based vectors with the generator embedded and no node-program references (§8). The private scrub list applies (§8).

#### B-23 IES key from the basic encoding of the DH point under every scheme: S · P · H
Added by BS11a's design (rev7, D67). The code is upstream's, not a fork commit.
- **Fork and upstream:** `up:pkgs/pkc/src/bls/ies_ops.rs` (unchanged at a1d4dd3). `S::ies_seal` (:360, :386) and `S::ies_open` (:407, :424) derive the AES key from the scheme's own encoding of the shared point. The test `kdf_reads_the_scheme_own_encoding` (:536) asserts that, and the `bls_dh` "chia" KATs encode it.
- **Core:** `symKey` is `ToByteVector(false)` of `DHKeyExchange`, truncated to 32 bytes, on encrypt and both decrypts (`bls/bls_ies.cpp:43-47`, `:79-83`, `:96-100`). That is always the basic encoding, whatever the scheme or the global flag.
- **Difference:** the Chia arm's key differs from Core's in byte 0 of the encoded point, so legacy-scheme ciphertexts do not interoperate with Core.
- **Class:** none (S, a Core-exact fix).
- **Upstream fit:** U30. One KDF over the basic encoding; the scheme tag stays a pure wire-encoding tag for the ephemeral key; the `bls_dh` KATs are regenerated from Core at a tag. The fork takes the same change in BS11a-3.

### 2.3 Area C: BS5 dash-params (23f6fd7)

**Commits.**
- 0286d1a holds most of the source.
- fdda6b2 holds the spork options and the rule that refuses only the final negative height.
- ae304a1 and 8c23be5 hold the tests and corpora.

Paths are under `pkgs/params/` unless a full path is given.

| ID | Item | Kind · effect | Class | Core | Upstream fit |
|---|---|---|---|---|---|
| C-01 | **Release axis** (`Release {V23_1, V24_0}`, `deployments()`, `has_min_spork_keys`, `caps_llmq_size`). `src/network.rs:54-85`; `src/data.rs:54-61`, `:80-83` | API | dead (DEC-82) | one release per binary | **No.** Remove it from the fork too (~150 lines, the 4,642-line `chainparams_v23_1.json5`, the v23 half of `brr_payments.json5`). Q-16 |
| C-02 | **v24 data refresh:** V24 on bit 13, `evo_shares` on bit 14, 39 checkpoints, chain work, assumevalid, chainTxData, testnet start 1790208000, `require_standard: Option<bool>`. `src/data.rs:68-103`; `src/mainnet.rs:20-206`; `src/testnet.rs:70-77` | S | — | `chainparams.cpp:212-213`, `:221-222`, `:230-231`, `:306-357`, `:421-431`. `fRequireStandard` was removed in v24 (b1f31ecdb9, bitcoin#28354) | **As is**, onto upstream's static tables. Remove `require_standard` as its own `!` commit |
| C-03 | **Owned `ChainParams` and `NetworkKind`** replace `Network` and `&'static`. `src/network.rs:17-50`; `src/types.rs:458-563` | API (breaking) | — | `chainparams.cpp:1402-1414` | **Reshape.** Extend upstream `Network` with `Devnet` instead of adding a parallel enum. Keep an infallible static path for the three fixed networks; use owned values only for devnet and overridden regtest. This also fixes the bspcheck regression (C-22). Q-17 |
| C-04 | **New CChainParams members:** fixed seeds, platform HRP, spork address, policy flags, assumeutxo, devnet genesis. `src/types.rs:411-421`, `:458-536`; `src/mainnet.rs:209-258`; `src/testnet.rs:179` | S | — | `chainparams.cpp:273`, `:278`, `:302`, `:459`, `:477`, `:505`, `:660`, `:697`, `:904`, `:914-923`, `:943`; `chainparamsbase.cpp:13-16`, `:39-51` | **As is.** `data_dir()` and `mine_blocks_on_demand()` stay node-side (§5) |
| C-05 | **LLMQ tables:** `MAX_LLMQ_SIZE`, `LlmqParams`, `max_cycles`, `max_store_depth`, `AVAILABLE_LLMQS[14]`, per-network sets. `src/llmq.rs:13-328`; `src/data.rs:106-111` | S | — | `llmq/params.h:17`, `:50-130`, `:138`; `chainparams.cpp:132-143` | **Reshape.** Drop `name` (it duplicates `LlmqType`'s Display). Use plain arithmetic: `dkgInterval` is never overridable, so the `checked_div`/`wrapping_mul` cases are unreachable. Needs A-09 |
| C-06 | **Devnet with derived genesis.** `src/devnet.rs:26-144`; `src/genesis.rs:57`, `:79-98`, `:106-127` | S | — | `chainparams.cpp:43-65`, `:85-107`, `:550-713`; `validation.cpp:4314-4318`; `consensus/params.h:100` | **Reshape:** `hash_devnet_genesis_block: Option<BlockHash>` instead of a zero sentinel; remove the dead `Err` arm at `genesis.rs:95` |
| C-07 | **`-nodevnet` selects "devnet-0".** `src/builder.rs:303-310`; corpus case `corpus/chainparams_v24_0.json5:4102-4110` | L · C/P (devnet only) | **(b)** minor | `util/system.cpp:423-435`, `:518-541`, `:1020-1055`; used at `init.cpp:1832` and `net_processing.cpp:4034` | **No.** Upstream takes a name string. Node: dash-args refuses `-nodevnet` with a typed error (DEC-83 allows it). Q-18 |
| C-08 | **Core argument-occurrence model** (`ChainOption`, 21 variants; negation; last vs. all). `src/builder.rs:20-128`, `:138-161`, `:174-218`, `:229-247`, `:288-300` | L · E | (c) for a library | `util/system.cpp:103-108`, `:160-175`, `:423-435`, `:518-541`; `chainparams.cpp:1422-1445` | **No.** Moves to dash-args (A1:583, :2844), which calls typed setters |
| C-09 | **Replicas of Core's integer parsing** (`from_chars`, `strip_plus`, `parse_i64/i32`, saturating `atoi_i64`, wrapping `int_arg`, `interpret_bool`). `src/parse.rs:13-84`; copied in `tests/core_corpus.rs:357-389` | L · E, plus C for the devnet int wrap | **(b)** for the silent wrap (`-highsubsidyfactor=4294967297` → 1); (c) for the rest | `util/strencodings.h:120-140`; `strencodings.cpp:234-260`; `util/system.cpp:543-566`; `chainparams.cpp:1247-1256` | **No.** Moves to dash-args, with any lenient reader named as such (the `*_lossy` precedent in ab625bb) |
| C-10 | **Negative activation heights:** the BL-31 refusal; fdda6b2 refuses only the effective value. `src/lib.rs:7-14`; `src/builder.rs:354-418`, `:579-583`; `src/error.rs:112` | L · E (regtest/devnet) | **(b)**: inconsistent and harmless | Unchecked `int`: `chainparams.cpp:1085-1187` (ParseInt32 at :1108). Checked: `-testactivationheight` at :1051. Negative behaves like 0 (`versionbits.cpp:123`) | **Moot upstream** (typed `u32`). BL-31 refusal moves to dash-args. Q-19 |
| C-11 | **Devnet subsidy and difficulty integers unchecked** (i32). `src/types.rs:318-326`; `src/builder.rs:303-346` | L · C (devnet only) | **(b)**: a negative factor gives a negative subsidy; a huge one overflows int64 (UB). Local configuration only | `consensus/params.h:192-194`; `chainparams.cpp:635-642`, `:1247-1256`; `validation.cpp:1814`, `:1822`, `:1858-1861`; `pow.cpp:177` | **Reshape:** typed `u32`; refuse factor 0? Q-20 |
| C-12 | **LLMQ overrides.** (i) size/threshold not checked for 1 ≤ threshold ≤ size; (ii) the regtest ISDIP0024 role accepts a non-rotated quorum. `src/builder.rs:340`, `:529-575` | L · C (regtest/devnet) | (i) **(b)**/(c); (ii) (a): four functional tests rely on it (`feature_asset_locks.py:80`, `feature_llmq_singlenode.py:37`, `feature_notifications.py:34`, `p2p_instantsend.py:42`) | `chainparams.cpp:1019-1027`, `:1189-1245`, `:1258-1393` (rotation checks at :1271, :1296; cap at :1217) | **Reshape:** a typed setter validating 1 ≤ threshold ≤ size ≤ 400, keeping (ii) |
| C-13 | **Spacing above the timespan:** `difficulty_adjustment_interval()` returns `Option`. `src/types.rs:362-381`; `src/builder.rs:340-346` | S | Core side: **(b)**, a latent divide-by-zero; local configuration only | `chainparams.cpp:1352-1369`; `pow.cpp:140`, `:153`, `:162`; `rpc/mining.cpp:86` | **As is.** CS2's large-spacing devnets need it (`dash-consensus/src/params.rs:23-30`) |
| C-14 | **Spork options:** v24 uses the last `-sporkaddr` with a warning; NUL and HRP refusal; whitespace trim. `src/builder.rs:229-286`; `src/spork.rs:12-37`; `src/types.rs:384-408` | L · E/P | `-nosporkaddr` failing instead of meaning "default": **(b)** minor. Trim and NUL/HRP checks: (c). Mainnet `-sporkaddr`: node policy | `init.cpp:826`, `:1746-1752`; `spork.cpp:219-230`; `key_io.cpp:42-102`; `base58.cpp:38-75` | **Default address only** (C-04). Resolution moves to dash-args, which conflicts with DEC-72. Q-21 |
| C-15 | **HardCodedPins** (68589, 847000). `src/types.rs:434-451`; `src/data.rs:21-27`; `src/mainnet.rs:158-161` | L · C | (a) | Literals in validation code: `validation.cpp:4476`, `:6670`, `:6705` | **No.** Keep them as constants in dash-consensus (`pow/params.rs:54-55`). Q-22 |
| C-16 | **BRR payment table.** `src/types.rs:572-574`; `corpus/brr_payments.json5`; `tests/brr_payments.rs` | S | — | `masternode/payments.cpp:103-128` | Depends on where it lives. Q-23 |
| C-17 | **Core asserts become errors** (`V20BelowDip3`, `MnRrBelowV20`, `DevnetGenesis`). `src/error.rs:114-118` | S | — | `chainparams.cpp:45`, `:106`, `:828-829`, `:964-966` | **As is** |
| C-18 | **Core error texts** (`Display` "byte for byte"; `VbField`). `src/error.rs:14-169` | L · E | (c) | the `strprintf` throws in `chainparams.cpp:972-1393` | **Reshape** to typed errors with lowercase library messages (AGENTS.md:13-14). Core texts go to dash-args if anyone wants them |
| C-19 | **Corpora.** 178 v24 cases: about 67 pin quirks, 3 encode BL-31, 31 are tool-only. The bases are plain dumps | S (test data) | bases are clean; cases are not | header `corpus/chainparams_v24_0.json5:1-9` (non-tag commit 23769258c239) | **Reshape:** ship a fixture of bases plus sane override cases, generated from a Core tag with the generator embedded (as `pkgs/num/corpus/arith256.json5` does). Q-24 |
| C-20 | **`BuriedDeployment`/`DeploymentPos` helpers.** `src/types.rs:37-67`, `:140-200` | API | — | `consensus/params.h:24-47`; `deploymentinfo.cpp:11-56` | **Reshape:** fix the order in `ALL` (DERSIG before CLTV); drop or const-ify `gbt_force`; move `test_activation_name` to dash-args |
| C-21 | **`-fastprune`** sets the regtest prune-after height to 100. `src/builder.rs:354-360` | S | — | `chainparams.cpp:875` | **As is**, as a typed `fast_prune(bool)` |
| C-22 | **bspcheck adaptation.** `pkgs/dev/src/bin/bsdk_util/bspcheck.rs:113-128` | API | — | — | **Reshape:** `Network::from_magic`, as a separate `dev%refac` commit |

Area C found nothing that has to be private. The two closest cases are C-11 and C-13. Both need an operator to choose a devnet configuration (C-13 also needs authenticated RPC), so both are tracked here as (b).

### 2.4 Area D: BS7 p2p-core (b69fa55, unmerged)

Paths are under `pkgs/p2p_core/src/` unless a full path is given. `np` stands for `net_processing.cpp`.

**BS7 merge gate (DEC-86): the dash-types reads are (c), so drop them.** This covers a06876d (`Vec<T>` read until failure), 021ba95 (chunked byte vectors and strings, and the hidden `BaseCodec::BYTE`), and BS7's dependent cursor reads (D-09, and part of D-13).
- **Accept/reject results are unchanged.** Every element type base-sdk reads consumes at least one byte, so a count larger than the remaining bytes fails under both the old and the new read. What changes:
  - the error class, `CompactSizeExceedsLimit{limit = remaining}` versus `Eof`;
  - the failure position.
- **The chunking never matters.** `read_chunks` (`pkgs/types/src/codec.rs:178-187`) only differs above 5,000,000 bytes, which no P2P message (at most 3 MiB + 12) or block can reach.
- **No Core code observes the error class or position.**
  - Only two Core catches keep reading the same stream: the NetInfo Service entry (`evo/netinfo.h:248-252`) and `CQuorumDataRequest`'s `nError` (`llmq/quorums.h:124-127`). Neither reads a vector inside its try.
  - Every other catch penalises and returns: np:5536-5541, np:5570-5575, np:5664, np:5693, `governance/net_governance.cpp:98`, `:209`, `llmq/net_signing.cpp:72`, `:103`, `:121`, `:137`, and `llmq/net_dkg.cpp:181`, `:376`.
  - The outer catch only logs (np:5940-5943).
- **Cost:** an impossible count was rejected in O(1) and now costs O(n). A zero-byte element type would loop up to `MAX_SIZE` times; none exists today.
- **Consumers:** only tests. These are `handler_verdicts_are_cores` (`tests/core_corpus.rs:272`, with `our_class` at :114-129), `byte_reads_stop_where_core_does` (`types/src/codec.rs:484`) and `core_cursors.rs`. rust-dashcore's `dash-consensus/src/primitives/decode.rs:355-362` matches only `CompactSizeExceedsLimit{limit: MAX_SIZE}`, so it is unaffected.
- **Before merging BS7 into the fork:**
  - drop a06876d;
  - reduce 021ba95 to a fail-fast `read_byte_vector` helper (`into_len(data.len())`);
  - drop the core_cursors corpus and test, and the cursor parts of 6238c14 and a1d2cd2;
  - relax 7b93dbf's comparisons to accept/reject only, mapping a remaining-bound `CompactSizeExceedsLimit` to Core's "end of data".
- **Not measured.** Area D's revert build did not finish at load averages of 30–65, so the list of dependent tests above comes from reading the code.

| ID | Item | Kind · effect | Class | Core | Upstream fit |
|---|---|---|---|---|---|
| D-01 | **Core limits and protocol versions** (61bfbf2, 23d8738). `limits.rs:1-67` (19 constants). `version.rs`: V23_1, V24_0, INIT, ADDRV2, NO_LEGACY_ISLOCK, DSQ_INV, BIP324_BASELINE, EFFICIENT_QRINFO, ISDLOCK_CYCLEHASH_UPDATE, PLATFORM_BAN, QFCOMMIT_STALE_REPROP_BAN, PLATFORMBAN_V2_SHORT_ID, COINJOIN_REBALANCE, ASSET_UNLOCK_INV; `CURRENT` deprecated | S/API · P (gates use them) | (a), no node-side part | `net.h:80`, `:84`, `:86`; np:115–222; `validation.h:82-83`; `script.h:24`; `blockfilterindex.h:20`; `version.h:14-77` | **As is**, minus `limits::MAX_SIZE` (re-export `MAX_SER_SIZE`), `V23_1`, `V24_0` and `CURRENT` (release pinning is NODE). Q-25: add `MIN_MASTERNODE_PROTO_VERSION` (70242)? |
| D-02 | **V1 framing** (851071c). `v1.rs`: `V1Header` 26-35, `V1FrameError` + `disconnects()` 39-64, `check` 131-139, `check_payload` 148-156, `checksum` 170-173, `encode_v1` 180-187 | S/API · P (framing), F (drop vs. disconnect) | (a) | `net.cpp:731-741` (disconnect), `:797-807` (drop); `protocol.cpp:232-250` | **Reshape:** a `BaseCodec` header type; rename `check(magic)`, which collides with `Checkable`. `disconnects()` is NODE (peer policy; AGENTS.md:41) |
| D-03 | **V2 short-ID send gate** (9808e01). `bip324.rs:25` `encode_v2(msg, peer_version, buf)`; `short_id.rs:47`, `:60`, `:67`; `CommandString::EMPTY` | S/API · P (`platformban` at 70240: v22.x/v23.0.x peers have no slot 168), F (baseline gate) | (a) | `net.cpp:935`, `:989`, `:998-1006`, `:1579`, `:1601-1606`; `net.h:1042-1048` | **As is** (`!`: `encode_v2` signature) |
| D-04 | **Reserved, unassigned V2 short IDs decode as the empty type** (9808e01). `bip324.rs` `split_v2` arm, about :131-133; `short_id.rs:39-41` | L · E | (c), low stakes | `net.cpp:1028-1035`, `:1496-1509`. Both outcomes are "ignore, keep the peer" | Either. Q-26: strict `UnknownShortId`, documented as "drop, keep peer" |
| D-05 | **`decode_v2_payload` and the long-form bound** (e43da5e, e2f39fc). `bip324.rs:85-98` | S/API · E (`split_v2` already bounds it) | keep | `net.cpp:1297-1307`, `:1496-1530` (0x7F allowed in long form; inherited, harmless) | Optional. Q-27: does the node need a separate entry point? |
| D-06 | **Inventory types** (d3c22e9, 0fe352c). `msg/inv.rs`: 13 variants (6, 16, 21, 23–26, 28, 29, 31–34); `command`, `is_known`, `is_gen_tx`, `is_gen_blk`, `request_interval`; `AssetUnlockInv` | S/API · P | (a) for the names | `protocol.h:495-528`, `:557-565`; `protocol.cpp:281-305`; np:1595-1608 | **Reshape:** without `request_interval` (NODE). Q-28: drop `AssetUnlockInv::Unsupported` (it exists only for v23.1.8; DEC-82) |
| D-07 | **headers2 lenient uncompress** (40673b2). `msg/headers2.rs:172-199`: the first header keeps only its version (178-181); a past-list offset gives version 0 (185-188) | L · F (penalty parity: Core +100 via `BLOCK_INVALID_HEADER`, np:2000, np:3364; a strict failure gets "log, no penalty") | (c). The past-list zeroing is a benign (b), b-12 | `primitives/block.cpp:57-134`; `block.h:154-185`. Honest `Compress` never produces either input, and the result is always invalid (`bad-version` `validation.cpp:4513-4518`, np:3353, `high-hash` `validation.cpp:4311`) | **Strict reshape:** an error for a first header with compressed flags and for a past-list offset. If penalty parity is wanted (Q-1), dashd-p2p uncompresses itself (~40 lines) |
| D-08 | **headers2 wire image, Core-image encoders, hash once** (40673b2, 6238c14 encoder part, 0b31754 bench). `CompressedHeader` 49-59; `compress` 140-164; `reply` 297, `announce` 304, `block_headers` 309, `is_prev_hash_derived` 317, `decode_with_hasher` 337-349; capacity 353-367. Removes the fork's public `CompressionState` | S/API · F (byte parity) and E/perf | keep | `primitives/block.cpp:57-93`; np:4847-4856, np:6498-6504 | **As is** |
| D-09 | **Header counts that run past the data** (6238c14). `msg/headers.rs:37-47`: `into_len(max)` instead of `into_len(data.len())`, after `TooManyEntries` | L · E | (c) | np:5320-5343 | **Drop**, with D-17 |
| D-10 | **BIP37** (6285205). `msg/bloom.rs` (`FilterLoad` 24-54 with `read_limited_byte_vec(36000)`, raw `flags`; `FilterAdd` 63-81; `PartialMerkleTree`; `MerkleBlock`) | S · P/E | keep | `common/bloom.h:20-33`, `:78`; np:5536-5541, np:5570-5575 | **As is**, after Q-5. Keep `flags` raw: a strict enum would turn Core-accepted input into +100 |
| D-11 | **`sendcmpct` flag read as `CoreBool`** (6285205; CoreBool comes from BS4a) | L · F | (c), following A-12 | np:4274-4276; `serialize.h:238-239` | **Strict `bool`** (A-12) |
| D-12 | **cmpctblock, PrefilledTx, blocktxn, v24 short-ID keys** (6285205). `msg/compact.rs:87-95`, `:133-140`, `:157-183`, `:301-308` | S/API · P (wrong keys degrade compact-block reconstruction) | (a) | `blockencodings.h:73-80`, `:116-125`; `blockencodings.cpp:20-33`, `:47-50`; np:5061, np:5304 | **As is**, after Q-5. Needs BS6b's instance-hash keys |
| D-13 | **getblocktxn** (27e470f, a1d2cd2). `msg/compact.rs:192-295`: private fields, `checked_add` + `u16` overflow check, strictly increasing `new`, serde `try_from`, and a read-until-failure cursor (205-230) | S/API + L · P/F + E | overflow check and constructor: keep; cursor: (c) | `blockencodings.h:22-54`; np:4704-4749 | **Reshape:** no cursor read |
| D-14 | **BIP157 types; send-only `CFilter`/`CFHeaders`/`CFCheckpt`/`MerkleBlock` decode to `P2pMsg::Unread`** (6285205). `msg/cfilter.rs`; `msg/mod.rs`; `macros.rs` | L/API · E/F | (c) for the ignore policy | np:3489, np:3520, np:3563; Core has no receive handler for the four | **Reshape:** type all of them in both directions, because SPV clients (dash-spv) must decode them. "Core ignores this" moves to dashd-p2p. After Q-5 |
| D-15 | **block/tx typing; getblocks, ssc, sendtxrcncl, parsed-empty messages** (6285205). `msg/mod.rs:84`, `:86`, `:224-225`; `msg/headers.rs:71-86`; `msg/control.rs` | S/API · P | (a) | np:4639, np:4310-4341, np:5357, np:4878; `node/sync_manager.cpp:333-342` | Control messages: **as is** (U5). Block/tx: Tier 3 (U29) |
| D-16 | **`P2pDecodeError::Consensus(String)` becomes `Consensus(DecodeError)`** (e946aec) | API (breaking) | — | — | **As is** |
| D-17 | **dash-types `Vec<T>` read until failure, and `BaseCodec::BYTE`** (a06876d; `BYTE` from 021ba95). `pkgs/types/src/codec.rs`, about :323-347 and :225-228 | L · E | (c) | `serialize.h:292-320`, `:795-851` | **Drop** (see the merge gate above) |
| D-18 | **dash-types byte vectors and strings** (021ba95). `pkgs/types/src/codec.rs:165-187`, String decode about :357-363, `adapters.rs` | L · E | (c) | `serialize.h:1047-1053`, `:1092-1104`, `:1132-1146` | **Drop.** Keep `read_byte_vector` as a fail-fast helper (area E: FIT) |
| D-19 | **Corpora, tests, bench** (96a3aca, 0b31754, aa7f5ce, 649716c, 7b93dbf, a1d2cd2, b69fa55) | tests | — | — | **Split and trim.** Leniency-free: `core_messages.json5` (v24 rows), `core_headers2.json5`, `short_ids.json5`, `tests/decode_allocation.rs`. Not leniency-free: `core_verdicts.json5` (CoreBool vectors, the Unread policy, error-class checks, probe artefacts, and 86 of its 172 cases at v23.1.8 — DEC-82 drops them; nothing ran at 70242). `core_cursors.json5` goes with D-17 |

No behaviour change: b20c624 (refactor), 95f24ec (lint), 378f4cf and 3a80245 (changelog; dropped upstream).

### 2.5 Items under PLAN §7

- **P-1.** EMBARGO-1: see PLAN §7 handling
- **P-2.** EMBARGO-2: see PLAN §7 handling

## 3. Summary table and counts

There are **88 catalogued items**: A 27, B 21, C 22, D 18. Two further items are handled under PLAN §7 (§2.5) and are not counted here. B-12 duplicates A-07. Tests (B-22, D-19) and commits with no behaviour change are not counted.

| Area | Items | Leniency (L) | (a) | (c) | (b) as class | (a)/(c) split | Strictness/API (S/API) | (b) flags |
|---|---|---|---|---|---|---|---|---|
| A: BS4a | 27 | 15 | 5 | 10 | 0 | 0 | 12 | 1 |
| B: BS11a-t + BS6a (+ BS11a: B-23) | 21 | 9 | 6 | 2 | 0 | 1 | 12 | 3 |
| C: BS5 | 22 | 9 | 2 | 4 | 3 | 0 | 13 | 7 |
| D: BS7 | 18 | 8 | 0 | 8 | 0 | 0 | 10 | 1 |
| **Total** | **88** | **41** | **13** | **24** | **3** | **1** | **47** | **12** |

**Reading the table:**
- **(c) total.** Two S/API items are also marked (c) for removal: A-08 (`PayloadDecode`) and C-01 (`Release`). That makes **26 (c) items** in all.
- **The 13 (a) leniencies:**
  - A-07, A-13, A-15, A-17, A-24;
  - B-02, B-03, B-06, B-10, B-11, B-14;
  - C-12 (ii), C-15.
  - B-05 is split: the scheme-carrying holder is (a), the laziness (c).
- **Which (a) items can live node-side:**
  - fully: A-17, C-15, and B-03/B-06 if the §5.2 classifier lands;
  - partly: A-07 (accessor);
  - not at all: A-13, A-15 and A-24 are the wire definition, so they belong upstream as bug fixes; B-10, B-11 and B-14 are the decoder's parse position (§5.3); B-02 needs the classifier.
- **Leniencies with a consensus effect (C):** A-07, B-02, B-03, B-05, B-06, B-10, B-11, B-14, C-07, C-09, C-11, C-12 and C-15. The C items in area C affect devnet and regtest only.
- **Leniencies that matter for honest peers (P):** A-13, A-15, A-17, A-24, B-07, C-07 and C-14.
- **(b) flags (12)** become the 12 rows of §4 (b-1 to b-12). B-03 and B-06 share b-4; C-13 is S on the fork side but (b) on Core's side.
- **Every BS7 leniency is (c).** That includes the dash-types reads behind the DEC-86 merge gate (§2.4).

| ID | Item | Kind | Effect | Class | (b) | Upstream |
|---|---|---|---|---|---|---|
| A-01 | TxOut.value unchecked i64 | L | E+R | (c) |  | U22 if Q-1 |
| A-02 | payload length cap → MAX_SIZE | L | E | (c) |  | never |
| A-03 | tx Checkable in Core order | S | – | — |  | U4 |
| A-04 | decode_size MAX_SIZE check | S | – | — |  | U1 |
| A-05 | CbTx/DynBitset/NetInfo lengths via decode_size | S | – | — |  | U4 |
| A-06 | exact vector reservation | S | – | — |  | U1 |
| A-07 | NetInfoV2 first-wins purpose map | L | C | (a) | b | U21 |
| A-08 | PayloadDecode consumed/trailing | API | – | (c) |  | never |
| A-09 | LlmqType 103/105 | S | – | — |  | U2 |
| A-10 | params reuses LlmqType | API | – | — |  | U2 |
| A-11 | signed ProtocolVersion | S | – | — |  | U3 |
| A-12 | CoreBool replaces bool | L | F | (c) |  | never |
| A-13 | version optional tail | L | P | (a) |  | U13 |
| A-14 | VersionTail | L | F | (c) |  | never |
| A-15 | senddsq/qsendrecsigs flag | L | P | (a) |  | U5 |
| A-16 | Pong Option nonce | L | F | (c) |  | never |
| A-17 | trailing bytes ignored | L | P/F | (a) |  | node; U23 |
| A-18 | empty commands ignore payload | L | F | (c) |  | never |
| A-19 | P2pMsg::Unknown | L | F | (c) |  | U14 (API) |
| A-20 | InvalidCommand, is_valid_v1/v2 | S | – | — |  | U3 |
| A-21 | 3 MiB bound for every command | S | – | — |  | U3 |
| A-22 | V2 contents +12 bytes | L | F | (c) |  | never |
| A-23 | V2 split; reserved IDs → Unknown | L | F | (c) |  | U17 (split) |
| A-24 | headers2 limit 8000 at ≥70235 | L | P | (a) |  | U6 |
| A-25 | payload-bounded header counts | S | – | — |  | U6 |
| A-26 | headers tx_count ≠ 0 | L | F | (c) |  | never |
| A-27 | ProtoCtx/MsgDecode/decode_with_policy | API | – | — |  | never (node) |
| B-01 | BlsCtx/BlsSchemeKind/BlsSchemeSource | API | – | — |  | U20 (kind only) |
| B-02 | legacy decode, Exotic VALID | L | C | (a) |  | U20 |
| B-03 | eager VALID/INVALID/throw | L | C | (a) | b | U20 (classifier) |
| B-04 | DecodeError::MalleableBls | API | – | — |  | U20 (replaced) |
| B-05 | BlsLazy | L | E/C | (a)/(c) |  | never (node) |
| B-06 | BlsGlobal (CbTx CL, AssetUnlock sig) | L | C | (a) |  | never (node) |
| B-07 | from_set_bytes_basic | L | P | (c) |  | deferred (BS9b) |
| B-08 | ProTx/commitment scheme rules | S | – | — |  | U20 (methods) |
| B-09 | encoders under write-time scheme | S | F | — |  | never (node) |
| B-10 | NIEntry::Invalid | L | C/E | (a) |  | U21 |
| B-11 | caught Service failures, cursor | L | C/E | (a) | b | U21 |
| B-13 | domain cap 253 | S | – | — |  | U12 |
| B-14 | invalid legacy address → zeros | L | C | (a) | b | U21 |
| B-15 | unknown ExtNetInfo version | L | E | (c) |  | never |
| B-16 | empty NetInfo as Core | S | – | — |  | U12 |
| B-17 | tx types 10–12 | S | – | — |  | U11 |
| B-18 | v3 payouts/shares | S | – | — |  | U11 |
| B-19 | CbTx v4, i32 height | S | – | — |  | U11 |
| B-20 | AssetLock/Unlock v2 | S | – | — |  | U11 |
| B-21 | CodeQL exemptions | API | – | — |  | U25 (part) |
| B-23 | IES key from the basic encoding | S | P | — |  | U30 |
| C-01 | Release axis | API | – | (c) |  | never (remove) |
| C-02 | v24 data refresh | S | – | — |  | U7 |
| C-03 | owned ChainParams/NetworkKind | API | – | — |  | U9 (reshaped) |
| C-04 | new CChainParams members | S | – | — |  | U7 |
| C-05 | LLMQ tables | S | – | — |  | U8 |
| C-06 | devnet derived genesis | S | – | — |  | U9 |
| C-07 | -nodevnet → devnet-0 | L | C/P | (b) | b | never (dash-args) |
| C-08 | Core argument-occurrence model | L | E | (c) |  | never (dash-args) |
| C-09 | Core int-parsing replicas | L | E/C | (c) | b | never (dash-args) |
| C-10 | negative activation heights | L | E | (b) | b | moot (u32, U10) |
| C-11 | devnet subsidy ints unchecked | L | C | (b) | b | U10 (u32) |
| C-12 | LLMQ overrides | L | C | (a) | b | U10 (validated) |
| C-13 | spacing > timespan, Option interval | S | – | — | b | U10 |
| C-14 | spork options | L | E/P | (c) | b | default only (U7) |
| C-15 | HardCodedPins | L | C | (a) |  | never (dash-consensus) |
| C-16 | BRR table | S | – | — |  | U24 |
| C-17 | asserts → errors | S | – | — |  | U7/U9 |
| C-18 | Core error texts | L | E | (c) |  | never (dash-args) |
| C-19 | corpora | S | – | — |  | regenerated |
| C-20 | deployment helpers | API | – | — |  | U7 (reshaped) |
| C-21 | fastprune | S | – | — |  | U10 |
| C-22 | bspcheck adaptation | API | – | — |  | U9 |
| D-01 | limits and protocol versions | S | P | — |  | U15 |
| D-02 | V1 framing | S | P/F | — |  | U16 |
| D-03 | V2 short-ID send gate | S | P/F | — |  | U17 |
| D-04 | reserved V2 short IDs → empty type | L | E | (c) |  | Q-26 |
| D-05 | decode_v2_payload, long-form bound | S | E | — |  | U17 (optional) |
| D-06 | inventory types | S | P | — |  | U18 |
| D-07 | headers2 lenient uncompress | L | F | (c) | b | U19 (strict) |
| D-08 | headers2 wire image, encoders | S | F/E | — |  | U19 |
| D-09 | header counts past the data | L | E | (c) |  | never |
| D-10 | BIP37 types | S | P/E | — |  | U26 |
| D-11 | sendcmpct CoreBool | L | F | (c) |  | U27 (strict bool) |
| D-12 | BIP152, v24 short-ID keys | S | P | — |  | U27 |
| D-13 | getblocktxn (cursor part L) | L | E | (c) |  | U27 (no cursor) |
| D-14 | BIP157; send-only Unread | L | E/F | (c) |  | U28 (typed both ways) |
| D-15 | block/tx, control messages | S | P | — |  | U5, U29 |
| D-16 | P2pDecodeError::Consensus typed | API | – | — |  | U3 |
| D-17 | Vec read-until-failure, BYTE | L | E | (c) |  | never |
| D-18 | byte vector/string chunked reads | L | E | (c) |  | never (helper only) |

## 4. Likely Core bugs, class (b), for pasta

These are non-security items, judged so because none causes a crash, a split between Core nodes, or unbounded resource use. Items under PLAN §7 are reported separately.

| # | Item | What Core does | Why it looks like a bug | Harm |
|---|---|---|---|---|
| b-1 | A-07 (B-12) | It reads `ExtNetInfo`'s purpose map with `UnserializeMap`: the first entry per key wins, and neither count nor key order is checked. The sign string hashes the re-serialized map. | A non-canonical map is accepted. The dropped or reordered bytes are covered only by the creator's input signatures, not by the operator/owner signature the sign string was meant to provide (`providertx.cpp:390`, "protection against malleability"). | Low: creator-only malleability and a data carrier. Worth rejecting before V24 activates on mainnet (Q-2). |
| b-2 | B-11 | It swallows a failed Service read (`netinfo.h:247-252`) and resumes parsing wherever the failed read stopped. | Parsing resumes mid-field, so the wire bytes and Core's re-serialization (a lone `ff`) disagree. | Low. |
| b-3 | B-14 | It turns a non-zero but invalid legacy address into an empty NetInfo, and ProRegTx accepts that. | The wire address is discarded silently and nothing reports it. | Low: the masternode simply has no address. |
| b-4 | B-03, B-06 | Opposite-scheme BLS bytes `Reset()` to an invalid object (`bls.h:191-200`), and an invalid object counts as null in a QC and in the CbTx CL. | It creates a second null encoding made of arbitrary non-zero bytes, a leftover of the fork-transition race handling. | Low: miner-made structures, and txids commit to the bytes. |
| b-5 | C-07 | `-nodevnet` selects devnet with the name "devnet-0". | A negation turns the feature on. | Minor: local configuration. |
| b-6 | C-09 | `GetIntArg` results are narrowed to `int` silently (`chainparams.cpp:1247-1256`), so `-highsubsidyfactor=4294967297` becomes 1. | Silent wrap-around. | Minor: devnet only. |
| b-7 | C-10 | It accepts negative `-dip3params`, `-budgetparams` and `-vbparams` heights, while `-testactivationheight` is range-checked (`chainparams.cpp:1051`). | Validation is inconsistent across parameters. | None: a negative height behaves like 0. |
| b-8 | C-11 | The devnet `-highsubsidyfactor` is unchecked, so a negative factor gives a negative subsidy and a huge one overflows int64 (undefined behaviour, `validation.cpp:1858-1861`). | No range check. | Local devnet configuration only. |
| b-9 | C-12 (i) | The LLMQ size and threshold overrides do not check 1 ≤ threshold ≤ size. | No range check. | Local regtest/devnet configuration only. |
| b-10 | C-13 | `-powtargetspacing` above the timespan makes `DifficultyAdjustmentInterval()` zero, so `pow.cpp:140`, `:153`, `:162` and `rpc/mining.cpp:86` take a modulus by zero. | Divide by zero. | Local devnet configuration; the RPC path also needs authentication. |
| b-11 | C-14 | `-nosporkaddr` fails as an invalid address instead of meaning "use the default". | The negation is not handled. | Minor. |

| b-12 | D-07 | When `Uncompress` meets a headers2 version offset that points past its list, it leaves version 0 and carries on (`primitives/block.cpp:107-114`). It also zeroes compressed fields in a first header. | A robustness quirk: honest `Compress` never produces either input. | None: the resulting header is always invalid. Optional to report. |

## 5. API-shape review

### 5.1 Verdicts

**Source.** Area E (§6 has the conventions), reconciled with areas A to C.
- **FIT:** fits upstream as is.
- **RESHAPE:** fits after the change named.
- **NODE:** keep node-side (rust-dashcore).

Area E's general rule for splitting strict from Core-compat behaviour:
1. The existing name keeps the strict semantics. A stateless Core-lenient rule that matters for consensus gets its own explicitly named function (#54).
2. Anything needing peer, chain, global or release state goes node-side.
3. Node-side code composes upstream byte bags, predicates and strict codecs in a rust-dashcore wrapper crate.
4. Never edit kwvg's CodeQL or semgrep policy to admit a type. Reshape the type instead.

**dash-types**

| API | Commit | Verdict | Reason |
|---|---|---|---|
| `CompactSize::decode_size` | 5ce44ad | FIT | Core's stateless `ReadCompactSize` range check. Doc-reference `into_len`. |
| `serialize::opt_str_u64` | a1dc00c | FIT | Same pattern as the existing serde adapters. |
| exact vector reservation (`MAX_VECTOR_ALLOCATE`) | 73303cd | FIT | Memory bound. |
| `DecodeError::MalleableBls` | 4bfb8fc | RESHAPE | BLS knowledge in dash-types breaks layering. Carry it as a domain error (`BlsError`) through `DecError`, or drop it under §5.2. |
| `CoreBool` | dd859d9 | NODE | Keep the strict `bool` under its name. If a nonzero-is-true read is ever wanted upstream, it gets its own decode function (#54), not a newtype. |
| removal of `impl BaseCodec for bool` | 0ba7bf5 | NODE; keep it upstream | It breaks downstream for no gain. |
| BS7 dash-types reads (`read_byte_vector`, hidden `BaseCodec::BYTE`, read-until-fail `Vec<T>`) | 021ba95, a06876d | see §2.4 | Area E: the borrowed-slice helper fits; the hidden `BYTE` const does not (use a sealed internal helper). |

**dash-pkc** (see §5.2 for the reconciliation)

| API | Commit | Verdict | Reason |
|---|---|---|---|
| `BlsSchemeKind {Legacy, Basic}` | 6fe1011 | RESHAPE | Reuse the marker vocabulary (`BlsScChia`/`BlsScIetf`), for example a `BlsScheme::ID` const or an enum named after the markers. |
| `BlsCtx` | 6fe1011 | NODE | A one-field newtype for Core's global flag, which is chain state (#7). |
| `BlsSchemeSource` | 6fe1011, 26c91c9 | NODE | It records which Core code path picks the scheme. `Global` has no production caller. |
| `BlsValue`, `BlsPoint`, sealed `BlsWire`/`BlsWirePoint` | c2c6cba | RESHAPE | Into one named, pure wire-form classifier with a single `BlsError` (§5.2). The manual zeroize on `K::SECRET` dodges the CodeQL secret-name rules. |
| `BlsValue::from_set_bytes_basic` | fcf26a9 | NODE (defer to BS9b) | Emulates Core's governance `SetBytes`. |
| `BlsEager`, its aliases, unit `BlsDecodeError` | c2c6cba | NODE | Core's `CBLSWrapper` semantics. The unit error struct alongside `BlsError` breaks the single-error pattern. |
| `BlsLazy` | c2c6cba | NODE | `spin::Mutex`, a `PartialEq` that decodes, no Eq/Hash, a new dependency, and edits to kwvg's CodeQL policy. |
| `BlsGlobal` | ac9634d | NODE | `get(ctx)` is stateful. |
| `protx_bls_scheme`, `commitment_bls_scheme` | 8685005 | RESHAPE | Stateless protocol data. Expose as methods on the payload types, returning the scheme id. |
| policy.qll edits | 6644f85, 4bae81f, b497f65, 0621fcb | keep out of feature PRs | The first two disappear with §5.2. Raise the `Share` false positive with kwvg as its own `sdk%lint(codeql)` change. |

**dash-primitives**

| API | Commit | Verdict | Reason |
|---|---|---|---|
| `TxOut.value: i64`, `TxOut::new`, `amount()`, `TxInvalid::OutputNegative` | fc30b05 | RESHAPE, or keep `Amount` | If wanted (Q-1): a `make_num!` signed amount, decoded and then judged by `Checkable` (#7). A bare `i64` breaks "newtypes for distinct semantics" (`guide_rust.md:75`). |
| `PayloadDecode {payload, consumed, len}`; `Option<Result<…>>` | 2ea2b13, 686db6a | NODE, or RESHAPE under its own name | Keep the strict `decode_payload`. `len` duplicates `extra_payload.len()`. |
| v24 payloads `ProDisTx`, `ProUpShareTx`, `ProUpSharedRegTx`, `PayoutShare`, `CollateralShare`, `CompactSig`; TxType 10–12 | 4e6bb33 | FIT (fields reshaped) | New consensus types. BLS fields are upstream byte bags. |
| `SpecialPayload` loses Eq/Hash and gains `#[expect(clippy::large_enum_variant)]` | 4e6bb33 | RESHAPE | A side effect of `BlsLazy` fields. It disappears under §5.2. |
| `NIEntry::Invalid` (encodes `ff`) | f53ebe7 | RESHAPE, or NODE | It reverses #12. See §5.3. If kept, model it as `Unknown {code, data}`, which keeps the raw tag. |
| `NetInfoV2` first-wins decode (non-round-tripping) | ecb74da | RESHAPE | §5.3: a faithful wire value plus a Core view. |
| `CbTx` height `i32` | 4e6bb33 | keep `BlockHeight` unless Q-15 says otherwise | Verdict-neutral. |
| `LlmqType` 103/105 | cd1d38b | FIT | — |

**dash-p2p-core**

| API | Commit | Verdict | Reason |
|---|---|---|---|
| `ProtoCtx`, `MsgDecode`, `DecodedMsg`, `decode_with_policy` | bfdc8ce | NODE; restore the strict `decode_payload` upstream | Peer-negotiated state and handler policy. |
| `P2pMsg::Unknown {command, payload}` | bfdc8ce | FIT (optional) | Open enum with the raw payload (#11). |
| removal of `PayloadNotEmpty` | bfdc8ce | NODE; keep it upstream | Reverses #18. |
| `P2pDecodeError::Consensus(DecodeError)` (was `String`) | bfdc8ce, e946aec | FIT | A typed source. |
| `ContentsTooLarge`, `InvalidCommand`, `EmptyV2Contents`, `TruncatedV2Type` | bfdc8ce, 99d090f, 463429e | FIT | Framing errors. Use one `command` type. |
| `CommandString::is_valid_v1`/`is_valid_v2` | 99d090f | FIT | Pure predicates. |
| `TooManyEntries` | bfdc8ce | NODE | Handler limits. |
| `VersionTail` | 6f136d2 | NODE | Handler behaviour, with a weakly typed `String` error. |
| `Pong {nonce: Option<u64>}` | 157e286 | NODE; keep `u64` upstream | "We may *receive* invalid data, we won't *emit* it" (#18). |
| `Version` with seven `Option` fields, `has_prefix_tail()` | dd090ee, ec2ccd2 | RESHAPE | A nested `Option<Tail>` makes non-prefix forms unrepresentable (A-13). |
| `ProtocolVersion` as i32, plus constants | 47b93a7, 61bfbf2 | FIT; `V23_1`/`V24_0`/`CURRENT` are NODE | Core's `int`. Release pinning isn't SDK scope. |
| `SendDsq`, `QSendRecSigs`, `SendTxRcncl`, `Ssc` | f652f44, 6285205 | FIT with plain `bool` | A-15. |
| `V2Message`, `split_v2`, `decode_v2_payload` | 463429e, b24511e | FIT | Pure BIP324 framing. `decode_v2(contents, ctx)` stays NODE. |

**dash-p2p-core, BS7 additions**

| API | Commit | Verdict | Reason |
|---|---|---|---|
| `pub mod limits` (19 constants) | 61bfbf2 | RESHAPE | Re-export from the crate root (`use-pub-roots-only`). `MAX_SIZE` duplicates `MAX_SER_SIZE`. Wire maxima fit; handler, relay and depth limits are NODE. |
| `V1Header` (`Unencodable`; `check(magic)`, `check_payload`, `version_prefix`), `checksum`, `encode_v1` | 851071c | RESHAPE | Stateless framing fits, but as a `BaseCodec` type. Rename `check(magic)`, which collides with `Checkable`. |
| `V1FrameError`; `disconnects()` | 851071c | FIT for the enum; `disconnects()` NODE | Disconnecting is peer policy (AGENTS.md:41). |
| `encode_v2(msg, peer_version, buf)`; `ShortId::min_version`/`for_send`/`has_core_slot`; `CommandString::EMPTY` | 9808e01 | FIT (`!`) | Short-ID availability by protocol version is wire data. Without it, short-ID `platformban` reaches old peers. |
| `decode_v2_payload` | e43da5e, e2f39fc | optional | `split_v2` plus dispatch covers it (Q-27). |
| 13 `InvType` variants, `command`, `is_known`, `is_gen_tx`, `is_gen_blk` | d3c22e9 | FIT | Protocol data. |
| `AssetUnlockInv`, `request_interval() -> Duration` | d3c22e9 | NODE | A release switch (Q-28) and fetch-timing policy. |
| `CompressedHeader`, `GetHeaders2`, `Headers2::reply`/`announce`/`block_headers`/`is_prev_hash_derived`/`decode_with_hasher` | 40673b2 | FIT, with a strict uncompress | Core's lenient `Uncompress` (D-07) is NODE, if wanted at all. |
| BIP37 `FilterLoad`, `FilterAdd`, `PartialMerkleTree`, `MerkleBlock` | 6285205 | FIT after Q-5 | Size caps go in `Checkable`. |
| BIP152 `SendCmpct`, `CmpctBlock`, `PrefilledTx`, `GetBlockTxn`, `BlockTxn`, `ShortTxId` + `get`, `short_id_with_keys` | 6285205, 27e470f, a1d2cd2 | RESHAPE after Q-5 | `ShortTxId` through `make_num!`/`Numeric`, with no `get` (#41). `GetBlockTxn::new` returns a `Result`. Strict `bool` for `sendcmpct`. No cursor read. |
| BIP157 types (6) | 6285205 | FIT after Q-5, typed in both directions | Same #40 coordination question as BIP152. |
| `P2pMsg::Unread(CommandString)` and the `send_only` macro arm | 6285205 | NODE | It encodes "Core has no handler", a server-side view. An SDK client must decode these messages. |
| `Block`/`Tx` messages typed through dash-primitives | 6285205 | after Q-5 (U29) | — |
| `SendTxRcncl`, `Ssc`, `GetBlocks`; `FilterClear`/`Mempool`/`QWatch` consume nothing | 6285205 | FIT | Control messages (U5). |
| hidden `BaseCodec::BYTE`; read-until-failure `Vec<T>`; chunked byte vectors | 021ba95, a06876d | drop (D-17, D-18) | A specialization hack on a public trait. Keep `read_byte_vector` as a fail-fast helper. |


**dash-params**

| API | Commit | Verdict | Reason |
|---|---|---|---|
| `NetworkKind` replacing `Network` | 0286d1a | RESHAPE | Extend `Network` with `Devnet`. Keep the static fast path (C-03). |
| `Release` | 0286d1a | NODE (remove) | DEC-82 (C-01). |
| typed builder setters, `build() -> Result` | 0286d1a | FIT | Devnet and regtest parameters are data. |
| `ChainParamsBuilder::arg`/`negate`, `ChainOption`, `parse.rs` | 0286d1a, fdda6b2 | NODE (dash-args) | Emulates Core's ArgsManager (C-08, C-09). |
| `ParamsError` with Core texts; `VbField` | 0286d1a | RESHAPE / NODE | Library messages are lowercase (AGENTS.md:13-14). Core texts go to dash-args (C-18). |
| `data_dir()`, `mine_blocks_on_demand()` | 0286d1a | NODE | Filesystem layout and mining behaviour (AGENTS.md:41). |
| `LlmqParams`, `AVAILABLE_LLMQS`, `MAX_LLMQ_SIZE`, `DeploymentPos`, `SporkParams`/`SporkSigners`, `BRR_PERIODS_PERMILLE` | 0286d1a | FIT | Consensus data. Deployment state machines stay NODE. |
| `AssumeutxoData`, `HardCodedPins` | 0286d1a | NODE (likely) | Node bootstrap data. The pins are validation literals in Core (C-15). |
| `#[non_exhaustive]` on five params types | 0286d1a | RESHAPE | kwvg breaks freely at 0.x with `!`. Prefer exhaustive types. |

### 5.2 BLS: where Core's wrapper semantics live (recommendation; Q-12)

Areas B and E disagree:
- **Area B** keeps `BlsEager` (wire value plus scheme, the minimal "invalid" holder) in base-sdk, together with `BlsCtx` and `BlsSchemeSource`, because its decoders take a source.
- **Area E** moves every wrapper node-side and reshapes the rest into predicates over byte bags.

Both agree that `BlsLazy`'s laziness is (c), that `BlsGlobal`'s deferral is (c) unless Q-9 says otherwise, and that kwvg's CodeQL policy must not be edited.

**Recommendation:**
1. **Upstream dash-pkc gains one Core-compat surface:**
   - a runtime scheme id in the marker vocabulary;
   - one explicitly named, pure classifier of wire bytes as Core reads them. It returns null, canonical point (usable or Exotic, B-02), opposite-scheme only, or canonical in neither, with a single `BlsError`. It reuses the existing codec. The legacy no-curve-check rule is documented as Core compatibility.

   It stays in dash-pkc because `blst` use is isolated there (#19), and rust-dashcore's pure crates cannot link it (DEC-71).
2. **Upstream payload types keep their byte-bag BLS fields.**
   - The B-08 scheme rules become methods that say which scheme a field's bytes are in.
   - For version-dependent fields, consider a scheme-neutral bag, since `BlsPkBytes<BlsScIetf>` mislabels a v1 key. That is a kwvg-level choice.
3. **The node (dash-evo) builds an eager holder per field** (bytes, scheme, decoded value) with the classifier:
   - canonical in neither gives Core's `bad-*-payload`;
   - opposite-scheme or zeros give null;
   - a point gives valid.

   Scheme-carrying serialization, unique-property hashing and the migration re-encode (`specialtxman.cpp:71`) live there too. `BlsCtx` derivation is already node-side.
4. **Fork consequence (DEC-86 convergence):**
   - BS6a's payload BLS fields go back to bags (4e6bb33, 60d6090, 73ab4b0 reworked).
   - `BlsEager`/`BlsLazy`/`BlsGlobal`/`BlsSchemeSource`/`BlsCtx` move to rust-dashcore.
   - 6644f85 and 4bae81f are dropped, which gives the ProTx types back Eq/Hash.
   - `MalleableBls` gives way to the classifier's error.
   - The EV0 consumers re-pin.
   - Cost is medium to large; the benefit is that the upstream Core-compat surface becomes one named function.
5. **If kwvg declines even the classifier,** the fork carries it as a small permanent patch (dash-pkc only). Rewriting it outside dash-pkc would need a `blst` dependency in a non-pure rust-dashcore crate.

### 5.3 NetInfo: Core's lossy reader (recommendation; Q-4)

Core reads NetInfo lossily:
- the first group wins (A-07);
- unknown or invalid entries become `Invalid` (B-10);
- a failed Service read is caught and parsing continues from where it stopped (B-11);
- an invalid legacy address becomes empty (B-14).

All of this is (a), and it is decoder-level: where parsing resumes after a caught failure decides whether the rest of the payload parses.

**Upstream argument.** A strict decoder (kwvg's #12) fails to decode consensus-valid v24 ProTxs, for example a repeated purpose group holding garbage. So an SPV client or wallet built on base-sdk cannot parse every valid v24 block. That argument goes beyond node parity, and kwvg's own #3 cuts both ways.

**Options:**
1. **Issue first.** Propose that ProTx decoders read NetInfo as Core does, with the type recording both the wire form and the Core view (as `core_groups()`/`core_effective()` accessors). Its `check()` then reports what Core's `Validate` would.
2. **If kwvg declines,** keep ecb74da and f53ebe7 as a documented permanent fork patch.
3. **Not recommended:** a second ProTx decoder set in dash-evo, which duplicates four payload decoders.

## 6. Upstream conventions (dashpay/base-sdk, read-only survey)

Area E surveyed the repo read-only on 2026-10-09; its raw data is in `/work/scratch/bsx/areaE/` (scratch). The rules below bind every unit in §7.

**Commits and titles**
- **Format:** `namespace%type[(scope)][!]: description` (`maint/lint/lint_commit.py:30-43`, `maint/unconv.toml`).
  - Namespaces: `num`, `p2p_core`, `params`, `pkc`, `pow`, `primitives`, `script`, `sdk`, `types`.
  - Types: `build`, `chore`, `ci`, `doc`, `feat`, `fix`, `lint`, `misc`, `perf`, `refac`, `test`. There is no `merge` type, so fork merge commits never go upstream.
- **Commit size:** at most 73 columns and 5 lines, subject included.
- **Bisectability:** every commit must compile and lint on its own. `just comb` runs `contrib/git_filter.py` over `develop..HEAD`.
- **No trailers upstream.** kwvg's commits carry none, and bodies are rare. All 103 fork commits carry a `Co-Authored-By: Claude Opus 5.5` trailer, with pasta as author.
  - Whether upstream commits keep the trailer is pasta's call (Q-3); the global attribution rule asks for it.
  - With the trailer, a commit has room for at most one body line.
- **PR titles:** the `pkg%type(scope)[!]:` form with a comma-separated, lowercase, backticked list of changes, often 100–150 characters (for example #54 `num%fix!: match reference implementation in overloading, parsing, rounding and block proof checks, …`). `!` marks breaking changes. Only the pattern is linted.
- **Branches:** short topic names, `_pN` for a series (`bls_p1..p4`, `pkc_p0..p2`, `misc_p2/p3`). Bodies cross-reference each other ("follow-up base-sdk#N").

**Body.** Use `.github/PULL_REQUEST_TEMPLATE.md`:
- **Motivation** (kwvg often leaves it out).
- **Additional Information:** a bullet per change, citing Core source by path and line, as #44 and #54 do.
- **Breaking Changes:** "Refer to changelog", or `### Added` / `### Superseded` lists.
- **How Has This Been Tested?:** the literal `contrib/git_filter.py … -- bash -c '<clippy, test, lint_all>'` command.
- **Checklist:** tick everything except the milestone box, which is for collaborators.

**Labels.** One topic label from `.github/board.json`: `Build/CI`, `Codec` (dash-types, dash-num), `Crypto` (dash-pkc), `Documentation`, `FFI`, `P2P`, `Primitives`.
- dash-params has no precedent, so use `Primitives`.
- Bots add `stale`/`needs-rebase`. CodeRabbit (the "chill" profile) reviews every non-draft PR, and kwvg answers it inline.
- There are no CODEOWNERS or CONTRIBUTING files.

**CHANGELOGs.** Only released crates keep `[Unreleased]` entries: `dash-types`, `dash-num` and `dash-pkc`. kwvg said on #44 that unreleased crates record nothing until their first release, so the fork's p2p_core and params CHANGELOG commits (f6b8693 in part, 378f4cf, 3a80245) are dropped upstream.

**Gates**
- **`just test`:** clippy with no default features and with `full`, nextest, doc tests, `cargo doc`, `bench --no-run`.
- **`just lint`:** `just --fmt --check`, `lint_all.py`, and CodeQL with `rust-security-and-quality`. CodeQL must really run; exit 77 ("binary not found") is a skip, not a pass.
- **CodeQL policy** (`maint/codeql/rust/lib/policy.qll`):
  - types derive Clone/Debug/Eq/Hash/PartialEq plus serde or `Unencodable`;
  - impls follow a fixed order;
  - a name regex classes types as secrets;
  - foreign re-exports go only through `__deps`/`__private`.
- **Semgrep:** `enum-bare-unknown` (an Unknown variant carries the raw value), `style-no-get-prefix`, `use-pub-roots-only`, `types-macro-no-codec`.
- **AGENTS.md:**
  - `no_std` + `alloc`;
  - "No networking code in any crate" (`:41`);
  - only the default/std/full/serde features "unless there is a pressing justification" (`:45-55`), so a node-only cargo feature would go against the repo's stated rules;
  - lowercase `Display` with no trailing punctuation;
  - `#[expect]` rather than `#[allow]`.
- **Commit cap:** `MAX_PR_COMMITS=100` (`.github/ci.env`). Fork plus BS7 is 103 commits, so splitting is required anyway.

**Size profile.**
- Across 24 kwvg PRs the median is 13 commits, 26 files and +1,307/−408 lines.
- Small fix-up PRs exist (#14, #36, #43, #49). The outliers are corpus imports (#8, +53,587).
- No external contributor has opened a PR yet, so ours would be the first.

**Philosophy an upstream reviewer will apply**
- **Stateless scope.** README: "a parsing and stateless verification SDK". #7: "Base SDK is a stateless library … falls outside our scope" (when dropping `DeploymentContext`).
- **#54:** "Since the stricter of the two is already an existing API, lossy parsing is given its own definition."
- **#3:** "there's little benefit to being *more* permissive than the reference implementation." This is the lever for every Core-exact strictness fix. Its converse, that being stricter than the reference rejects valid chain data, is the lever for A-13, A-15, A-24 and §5.3.
- **#18:** "while we may *receive* invalid data, we won't *emit* it". kwvg added `PayloadNotEmpty` on purpose.
- **#12:** "`NIEntry::Invalid` has been removed. Unknown entry types now fail at decode time."
- **`guide_rust.md`:** "Make invalid states unrepresentable" (`:79`), "Limit input size" (`:468`), and "Reject non-minimal encodings … accepting non-minimal forms creates consensus divergence" (`:470-471`).
- **Issue #40:** BIP152/157 types have been stubbed since #39, pending the unreleased `bitcoin-p2p-messages`. Ask kwvg before typing them (Q-5).

## 7. Upstream PR series plan

### 7.1 Dependency facts

**How the fork branches were built** (first commit and its parent):
- BS11a-t (6fe1011…97096e7) sits directly on **33fcc59**, so it is already a develop-based series.
- BS4a (dd859d9…3c971b1) sits on the BS11a-t merge (098dafd).
- BS5 (0286d1a…8c23be5) sits on the BS4a merge (481f8c3).
- BS6a (4bfb8fc…7f3f483) sits on the BS4a merge (481f8c3).
- BS7 (61bfbf2…b69fa55) sits on a1d4dd3.

**Replays onto `develop` in a scratch clone** (cherry-pick of the whole series):
- BS11a-t applies 12/12.
- BS4a applies 38/38 textually but has a compile dependency on 6fe1011: `ProtoCtx` holds `BlsCtx`.
- BS5 needs BS4a (cd1d38b, 9277360, a258c7b: `LlmqType`, `support.rs`). It applies 42/42 on BS4a.
- BS6a needs BS11a-t and BS4a, not BS5. It applies 69/69.
- BS7 needs BS4a and BS6a textually. It applies 95/95 on BS11a-t + BS4a + BS6a without BS5, and on BS4a alone it fails only at the a06876d CHANGELOG hunk.

**Single commits picked alone onto `develop`:**
- Clean: 6fe1011, c2c6cba, dd859d9, 5ce44ad, fc30b05, d4a1be3, 9277360, 47b93a7, bfdc8ce, 0ba7bf5, ad0c785, a258c7b, ae304a1, 4bfb8fc and 28f68ee.
- Everything else conflicts alone, so units are rebuilt on `develop`, not cherry-picked.

### 7.2 Principles

- **DEC-91 (2):** leniency-free units go first. A unit shaped by this catalogue waits until the manager accepts its classification.
- **Tiers:**
  - **Tier 1:** S, API or data only, no leniency. Can open once the manager has reviewed this plan.
  - **Tier 2:** carries an item classified (a) or FIT here, reshaped into a strict representation of Core's protocol. Opens once the manager accepts the classification.
  - **Tier 3:** needs a pasta answer and/or a kwvg issue first.
- **Rebuild, don't transplant.** Each unit is rebuilt on `develop` in kwvg's shapes: strict names kept, no node-program text, tag-generated vectors with the generator embedded, and CHANGELOG entries only for released crates (types, num, pkc). Fork commits are the source, not the history.
- **Pace.** Keep one or two PRs open at a time, as kwvg's own `_pN` series do. Fold each review change back into `node/base-sdk` (DEC-91 (5)).
- **Scrub.** Run §8 before every push. "Private scrub" means the private steps also apply. Every unit that touches `pkgs/primitives` or carries transaction, block or payload bytes gets them.

### 7.3 Units

Sizes are estimates for the rebuilt unit (source + tests, then fixture), not the fork's numbers.

| # | PR title (`pkg%type`) | Branch | Label | From fork commits | Classification dependency | Size | Scrub | After |
|---|---|---|---|---|---|---|---|---|
| **Tier 1** | | | | | | | | |
| U1 | `types%fix: range-check length prefixes against MAX_SIZE, reserve vector batches exactly` | `codec_size` | Codec | 5ce44ad, 73303cd | none (A-04, A-06 are S) | ~+100/−5; 2–3 commits | low | — |
| U2 | `primitives%feat!: name LLMQ types 103 and 105, reuse LlmqType in dash-params` | `llmq_types` | Primitives | cd1d38b, 9277360 + a258c7b squashed | none (A-09, A-10) | ~+50/−50; 2 commits | **private scrub** | — |
| U3 | `p2p_core%fix!: make ProtocolVersion signed, bound every command by 3 MiB, add command validity predicates, carry the decode error in P2pDecodeError` | `p2p_frame` | P2P | 47b93a7, f45f641, 99d090f (predicates and `InvalidCommand` only), e946aec | none (A-11, A-20, A-21, D-16) | ~+120/−30; 4 commits | low | — |
| U4 | `primitives%fix: check transaction size before payload size, route CbTx, DynBitset and NetInfo lengths through decode_size` | `tx_check` | Primitives | 6762bbb, 8be6b9d, 339d43d (S part only; the payload cap stays), d4a1be3 | none (A-03, A-05; A-02 stays out) | ~+150/−40; 3–4 commits | **private scrub** | U1 |
| U5 | `p2p_core%fix!: type senddsq and qsendrecsigs as one bool each, type ssc, sendtxrcncl and getblocks` | `p2p_ctrl` | P2P | bfdc8ce and f652f44, reshaped with strict `bool`; the control-message part of 6285205 | none: an upstream bug fixed with strict types (A-15, D-15) | ~+150/−20; 2–3 commits | low | U3 |
| U6 | `p2p_core%fix!: drop fixed header count caps, bound counts by the payload, export Core's header limits` | `hdr_limits` | P2P | 89f4635, bfdc8ce (count parts), 47b93a7's `INCREASE_MAX_HEADERS2` | none (A-24 limit, A-25 allocation); `TooManyEntries` stays node-side | ~+120/−40; 2–3 commits | low | U3 |
| U7 | `params%feat!: track Dash Core v24 chain parameters, drop require_standard` | `params_v24` | Primitives | 0286d1a split (C-02, C-04 static parts, C-17), fixture regenerated from a tag | none (Q-24 picks the tag) | ~+450/−250; ~700–2,000 fixture lines; 6–9 commits | low (fixture header) | — |
| U8 | `params%feat: add LLMQ parameters and per-network quorum sets` | `llmq_params` | Primitives | 0286d1a split (C-05 reshaped) | none | ~+350, 100 test; 3–4 commits | low | U2, U7 |
| U9 | `params%feat!: add devnet with derived genesis, extend Network, keep the static path` | `params_devnet` | Primitives | 0286d1a split (C-03 reshaped, C-06), `dev%refac` bspcheck | none (Q-17) | ~+300/−100, 150 test, ~750 fixture; 5–6 commits | low | U7, U8 |
| U10 | `params%feat: add typed regtest and devnet overrides` | `params_ovr` | Primitives | 0286d1a and fdda6b2 reshaped (C-10 u32, C-11, C-12 validated, C-13, C-21) | none: stricter than Core only where Core is (b) (Q-20) | ~+450/−50, 300 test, ~400 fixture; 6–7 commits | low | U9 |
| U11 | `primitives%feat!: decode v24 special transactions, v3 payout and share lists, CbTx v4, asset lock v2` | `v24_payloads` | Primitives | 4e6bb33 split (types 10–12, v3 lists, CbTx v4, AssetLock/Unlock v2; BLS fields stay byte bags), 26c91c9 (shared ProTx helpers), c0a08c8, 283ebc0, 3613593 (asset-lock part), plus the B-18 bend-2 fix | none: S only (B-17 to B-20); CbTx height per Q-15 | ~+1,000/−150; 6–8 commits | **private scrub** | U2 |
| U12 | `primitives%fix: cap NetInfo domains at 253 bytes, report empty NetInfo as Core does` | `netinfo_strict` | Primitives | f53ebe7 (domain cap only), 3c2b5ae reshaped onto upstream's codecs, 3613593 (`is_empty` test) | none (B-13, B-16) | ~+100/−60; 2–3 commits | **private scrub** | U4 |
| U15 | `p2p_core%feat: add Core's protocol versions and message limits` | `p2p_limits` | P2P | 61bfbf2 (without `limits::MAX_SIZE`, `V23_1`, `V24_0`, `CURRENT`), 23d8738 | none (D-01; Q-25) | ~+110/−10; 2 commits | low | U3 |
| U16 | `p2p_core%feat: frame V1 messages, checking magic, size, checksum and command as Core does` | `p2p_v1` | P2P | 851071c, reshaped as a `BaseCodec` header type with no `disconnects()` | none (D-02) | ~+200; 2–3 commits | low | U15 |
| U17 | `p2p_core%refac!: split V2 typing from payload decoding, gate short IDs on the peer's version` | `p2p_v2` | P2P | 463429e, b24511e, 9808e01, 5cf3bd7 (the check order only), and optionally e43da5e/e2f39fc (Q-27) | none for the split and the gate (A-23, D-03, D-05); D-04 per Q-26 | ~+400/−100 plus the 346-line `short_ids` fixture (built from source; scrub its header); 4–6 commits | low | U15 |
| U18 | `p2p_core%feat: name every inventory type Core knows` | `p2p_inv` | P2P | d3c22e9 (without `request_interval` and `AssetUnlockInv`), 0fe352c | none (D-06; Q-28) | ~+90; 1–2 commits | low | U15 |
| U19 | `p2p_core%fix!: encode headers2 as Core does, decode compressed headers strictly, hash each header once` | `headers2` | P2P | 40673b2 (strict uncompress), the encoder part of 6238c14, 0b31754 (bench, scrubbed) | none (D-07 strict reshape, D-08) | ~+470/−260 plus the scrubbed `core_headers2` fixture; 3–5 commits | low (corpus header) | U6, U15 |
| U30 | `pkc%fix!: derive the BLS IES key from the basic encoding of the DH point under every scheme` | `ies_kdf` | Crypto | none yet (BS11a-3) | none (B-23 is S) | ~+40/−20 plus the regenerated `bls_dh` KATs; 1–2 commits | low | — |
| **Tier 2** | | | | | | | | |
| U13 | `p2p_core%fix!: read version's optional trailing fields as one nested tail` | `version_tail` | P2P | dd090ee, ec2ccd2, 389cd42, ad0c785, a1dc00c (reshaped: nested `Option<Tail>`, strict bool) | A-13 (a) | ~+250/−60; 3–4 commits | low | U3, U1 |
| U14 | `p2p_core%feat!: return unknown commands as P2pMsg::Unknown` (optional) | `p2p_unknown` | P2P | bfdc8ce (the `Unknown` arm only) | A-19 FIT | ~+60/−30; 1–2 commits | low | U3 |
| **Tier 3** | | | | | | | | |
| U26 | `p2p_core%feat: type BIP37 messages` | `p2p_bip37` | P2P | 6285205 (`msg/bloom.rs`) | Q-5; D-10 is S | ~+130; 1–2 commits | low | U15 |
| U27 | `p2p_core%feat!: type BIP152 messages, derive v24 short-ID keys` | `p2p_bip152` | P2P | 6285205 (`msg/compact.rs`), 27e470f, a1d2cd2 (no cursor read), strict `sendcmpct` bool | Q-5; D-12 (a), D-13. Needs BS6b's instance-hash keys | ~+420; 3–4 commits | **private scrub** | U1, U15 |
| U28 | `p2p_core%feat: type BIP157 messages in both directions` | `p2p_bip157` | P2P | 6285205 (`msg/cfilter.rs`), without send-only or `Unread` | Q-5; D-14 reshaped | ~+125; 1–2 commits | low | U15 |
| U29 | `p2p_core%feat: type block and tx messages` | `p2p_blocktx` | P2P | 6285205 (block/tx arms) | Q-5 | ~+60; 1 commit | **private scrub** | U15 |
| U20 | `pkc%feat(bls): name schemes at runtime, classify wire bytes as Core reads them` | `bls_wire` | Crypto | 6fe1011 reshaped, the codec part of c2c6cba, 8685005 as methods, a regenerated subset of the f9a942c/20aac32 vectors | Q-12 (§5.2) plus a kwvg issue; B-02/B-03 (a) | ~+400/−20 plus vectors; 4–6 commits | **private scrub** | U11 |
| U21 | `primitives%fix!: read NetInfo as Core does, keep the wire form` | `netinfo_core` | Primitives | ecb74da, f53ebe7 and b64d9f1, reshaped (§5.3) | Q-4 plus a kwvg issue; A-07, B-10, B-11, B-14 (a) | ~+300/−100; 3–5 commits | **private scrub** | U12 |
| U22 | `primitives%fix!: decode out-of-range output values, report them through Checkable` | `tx_amount` | Primitives | fc30b05 reshaped (signed-amount newtype) | only if Q-1 says reason parity counts; A-01 (c) otherwise | ~+150/−60 | **private scrub** | U4 |
| U23 | `p2p_core%fix: read govsync's whole bloom filter` | `govsync` | P2P | none yet (BS9b) | A-17's P part | small | low | U3 |
| U24 | `params%feat: add the BRR payment table` | `params_brr` | Primitives | 0286d1a (C-16), 8c23be5 (BRR corpus) | Q-23 | ~+80, 50 test | low | U7 |
| U25 | `sdk%lint(codeql): exclude payout share types from the secret name rule` | `cql_share` | Build/CI | b497f65, 0621fcb | kwvg owns the policy, so ask first | tiny | low | U11 |

**Order:**
1. U1, U2, U3, U7 and U30 have no dependencies and can go in any order.
2. Then U4–U6, U8 and U15.
3. Then U9–U12 and U16–U19.
4. Then Tier 2, then Tier 3.

If kwvg prefers fewer, larger PRs, merge U1+U4 (codec), U3+U5+U6+U15 (p2p framing) and U7+U8 (params data). Each merged unit stays under 20 commits, far below `MAX_PR_COMMITS`.

**Tests travel with their unit.** For example, `tests/decode_allocation.rs` goes with U1/U6, and the v24 rows of `core_messages.json5` go with the typed-message units. The trimmed verdict set goes with the unit whose handler rule it pins.

### 7.4 Never upstream (node-side homes)

**rust-dashcore `dash-p2p`/handler layer:**
- `ProtoCtx`, `MsgDecode`, `DecodedMsg`, `decode_with_policy`;
- `TooManyEntries` and handler count limits;
- `VersionTail` and the head-first checks;
- the short-pong rule;
- empty-payload tolerance (map `PayloadNotEmpty` to "ignore");
- trailing-byte tolerance (prefix decode);
- `V23_1`/`V24_0`/`CURRENT`;
- `disconnects()`, `request_interval()`, `AssetUnlockInv`;
- `CoreBool` reads;
- the send-only/`Unread` "Core ignores this" policy (D-14);
- the lenient headers2 uncompress, only if Q-1 wants penalty parity (D-07).

**rust-dashcore `dash-evo`/`dash-consensus`:**
- the BLS holders (§5.2);
- `BlsCtx`/`BlsSchemeSource`;
- the scheme-carrying hash;
- the AssetLock/Unlock V24 gate (B-20);
- the reason walker if Q-1 wants it (A-01, A-02);
- HardCodedPins (C-15);
- the payload-reason mapping for special transactions.

**rust-dashcore `dash-args`:**
- the ChainOption occurrence model;
- the atoi/parse replicas;
- negation and `-nodevnet` policy;
- `-sporkaddr` resolution;
- BL-31;
- Core error texts and `VbField`.

**Dropped entirely:**
- `Release` and the v23 data;
- `BlsValue::from_set_bytes_basic` (until BS9b);
- the reserved-short-ID and +12-byte V2 tolerances;
- headers `tx_count` ≠ 0;
- `PayloadDecode`, unless CS0 keeps it as API taste;
- policy.qll exemptions 6644f85 and 4bae81f.

### 7.5 Fork actions (DEC-86 convergence)

Do these per unit as upstream lands, not as one refactor. Each removal needs its node-side replacement first. Before moving anything, check consumers with `rg` across the rust-dashcore worktrees.
1. **Remove (c) items from `node/base-sdk`:**
   - BS4a: A-12 (restore `bool`), A-14, A-16, A-18 (restore `PayloadNotEmpty`), A-22's extra 12 bytes, A-23's reserved IDs, A-26.
   - BS6a and BS5: B-07, B-15, C-01, and C-08/C-09/C-14/C-18 (moved to dash-args).
   - BS7, before its merge: D-09, D-13's cursor, D-17, D-18; D-07 becomes strict; D-11 follows A-12; D-14 is typed both ways; D-04 follows Q-26.
   - **Known consumers:** CS0 reads `TxOut.value` as i64 (`dash-consensus/src/primitives/canonical.rs:92`) and uses `PayloadDecode` (`dash-consensus/src/primitives/tx.rs:5`, `:139-153`). rdc `dash-consensus/tests/common/mod.rs:186-200` uses `ChainOption::from_name`.
2. **Move node-side:** the §7.4 lists, and the BLS wrappers if Q-12 is accepted.
3. **Reshape in place to the upstream form** once each unit merges upstream. Rebase the fork onto `develop` unit by unit (DEC-91 (5); BASE-SDK-FORK.md table).
4. **BS7 merge gate (DEC-86):** see §2.4 for the dash-types read classification.

## 8. Scrub list

**Before every push of every unit**, over full branch history (content and messages), the PR body and every comment (DEC-91 (3)):
1. **Internal references:** `grep -nE 'rust-dashcore|node[- ]program|\bBS[0-9]|DEC-[0-9]|A1 §|A1-architecture|O17|EVO-[0-9]|DDE-SWEEP|dde-probe|dash-oracle|pins\.txt|contrib/node|ORACLE\.md|PLAN|dash-evo|dash-consensus|\br[0-9] review|\bOpus\b|\bGPT\b|node/integration'`. Every hit must go.
2. **Paths:** no `~/private`, `/work/`, `/home/`, or scratch paths.
3. **Private scrub steps** on every unit marked "private scrub" (PLAN §7.2 readers have them).
4. **Commit lint:** 73 columns × 5 lines. Trailers per Q-3.
5. **Vectors:** regenerated from a Core tag with the generator embedded; no oracle-shim or pinned-archive provenance.

**Known hits at a1d4dd3** (file:line, category):
- `contrib/vectors/payloads/README.md:9`, `:22`: rust-dashcore conformance path.
- `contrib/vectors/payloads/gen.py:89`, `:233`: internal task and plan references.
- `pkgs/params/corpus/brr_payments.json5:3-5`: node program, pinned archive and dash-oracle-sys.
- `pkgs/params/corpus/chainparams_v23_1.json5:2-12`, `chainparams_v24_0.json5:2-12`: the same, plus `contrib/node/bs5-chainparams`.
- `pkgs/params/tests/core_corpus.rs:96`: oracle.
- `pkgs/pkc/corpus/bls_wrapper.json5:8`: oracle shim.
- `pkgs/primitives/corpus/decode_domain.json5:4`, `:63`: oracle; `contrib/node/dde-probe`.
- `pkgs/primitives/corpus/payload_core.json5:8`: internal task.
- `pkgs/primitives/corpus/proregtx.json5:546-547`: O17, BS6a and BS6c.
- `pkgs/primitives/corpus/v24.json5:3`: rust-dashcore `node/integration`.
- `pkgs/primitives/src/payload/mod.rs:473`, `proregtx.rs:227`, `proupregtx.rs:127`: "dash-evo's".
- `pkgs/primitives/tests/decode_allocation.rs:84`: review reference.
- `pkgs/primitives/tests/payload_core.rs:30-45`, `:482`, `:633`: BS6a, BS6c, O17, A1 and review references.

**Additional hits on BS7 (b69fa55):**
- `pkgs/p2p_core/bench/main.rs:22`, `:25`: oracle.
- `pkgs/p2p_core/corpus/core_cursors.json5:3-4`, `core_headers2.json5:8-16`, `core_messages.json5:2-9`, `core_verdicts.json5:5-26`, `short_ids.json5:2`: rust-dashcore `contrib/node/dde-probe`, `bs7_*.py`, `gen_bs7_corpus.py`, `ORACLE.md`.
- `pkgs/p2p_core/tests/core_corpus.rs:163-165`: DDE-SWEEP and BS9b.
- `pkgs/p2p_core/tests/core_corpus.rs:263-265`: oracle.
- `pkgs/p2p_core/tests/v2_payload.rs:83`: BS4a, `probe_v2.py`.

**Also scrub:** fork-only commit-message wording, and the `docs/node-program/` tree if it exists on the branch.

## 9. Questions for pasta

Each question gives a recommendation and the units it blocks.

**Policy**
- **Q-1:** Under DEC-83, do reject reason, misbehaviour score, RPC reason, and penalty parity against hostile peers count as protocol parity (A-01, A-02, D-07)? *Rec: no for base-sdk. If yes, a node-side reason walker and a node-side headers2 uncompress.* Blocks U22 and the A-01/A-02 fork removal.
- **Q-3:** Keep the `Co-Authored-By` trailer on upstream commits? kwvg's commits have none. *Rec: pasta's call; keeping it costs one body line per commit.* Blocks all units.
- **Q-12:** Accept §5.2: byte bags upstream plus one named Core wire-form classifier, with the BLS wrappers moved to rust-dashcore (a BS6a/EV0 rework)? *Rec: yes.* Blocks U20 and the fork's BLS convergence.
- **Q-4:** NetInfo (§5.3): open a kwvg issue proposing a Core-reading decoder with wire and Core views, and fall back to a permanent fork patch? *Rec: yes.* Blocks U21.

**Core reports, class (b)**
- **Q-2:** Report b-1 (non-canonical `ExtNetInfo` map) to Core before V24 activates on mainnet? *Rec: yes, as a normal issue.*
- **Q-7:** Report b-4 (second null encoding) to Core? *Rec: low priority, together with b-1.*
- The other (b) items (b-2, b-3, b-5 to b-11) are minor or affect only local configuration. *Rec: batch them in one low-priority Core issue, or drop them.*

**BLS**
- **Q-6:** Run O6 scan (h) now for Exotic legacy fields over blocks and the current MN list? *Rec: yes; A1:318 rests on it.*
- **Q-8:** Keep laziness only if MN-list load benchmarks need it? *Rec: start eager; measure in EV.*
- **Q-9:** Is parity with V20-before-V19 regtest runs required (the `BlsGlobal` deferral)? *Rec: no. Decode the CL and AssetUnlock sigs as basic.*
- **Q-10:** Defer `from_set_bytes_basic` to BS9b? *Rec: yes.*
- **Q-11:** Add a raw-passthrough encoder for byte-faithful re-encoders? *Rec: not needed once fields are bags (§5.2).*

**Primitives**
- **Q-13:** B-14: zero the address at decode, or keep the raw value plus a `core_effective()` accessor? *Rec: raw plus accessor.* Part of U21.
- **Q-14:** B-18: make the encoder fallible for a join-sig/share count mismatch, or catch it with a `Checkable` rule? *Rec: a `Checkable` rule, the #7 pattern.* Blocks U11.
- **Q-15:** CbTx height: keep `i32` or go back to `BlockHeight`? *Rec: `BlockHeight` upstream, since it is verdict-neutral.* Blocks U11.

**Params**
- **Q-16:** Remove `Release` and the v23 data from the fork as well? *Rec: yes.*
- **Q-17:** Keep a static fast path for the three fixed networks? *Rec: yes.* Blocks U9.
- **Q-18:** Refuse `-nodevnet` in dash-args (a DEC-83 deviation) or keep Core's "devnet-0"? *Rec: refuse.*
- **Q-19:** Move the BL-31 refusal to dash-args and use `u32` upstream? *Rec: yes.* Blocks U10.
- **Q-20:** Should upstream be stricter than Core for devnet subsidy and difficulty (non-negative values, factor ≥ 1)? *Rec: yes.* Blocks U10.
- **Q-21:** Amend DEC-72 so dash-args owns `-sporkaddr` resolution and upstream params carries only the default? *Rec: yes.*
- **Q-22:** Keep HardCodedPins as dash-consensus constants? *Rec: yes.*
- **Q-23:** Where should the BRR table live: params upstream, or dash-consensus? *Rec: params; it is consensus data like the LLMQ tables.* Blocks U24.
- **Q-24:** Generate upstream fixtures from v24.0.0-rc.3 now, or wait for the final v24.0.0 tag? *Rec: rc.3 now, regenerated at final.* Blocks U7.

**kwvg coordination**
- **Q-5:** Ask kwvg on #40 whether to type BIP152/157 ourselves or wait for `bitcoin-p2p-messages`? *Rec: ask before opening U26–U29. The answer may also cover the block and tx stubs.*

**BS7, decidable by the manager** (recommendations given)
- **Q-25:** add `MIN_MASTERNODE_PROTO_VERSION` (70242, `version.h:23`), and drop `CURRENT` upstream rather than setting it to 70242? *Rec: add it; drop `CURRENT` upstream, and the node defines its own.* Blocks U15.
- **Q-26:** should reserved V2 short-ID slots decode as the empty type (Core's model) or as a strict `UnknownShortId` (D-04)? *Rec: the strict error upstream, documented as "drop, keep the peer".* Blocks U17.
- **Q-27:** keep a separate `decode_v2_payload` entry point? *Rec: only if dashd-p2p needs one; otherwise `split_v2` plus dispatch.* Part of U17.
- **Q-28:** drop `AssetUnlockInv::Unsupported`, which exists only for v23.1.8 (DEC-82)? *Rec: yes; type 34 is always known.* Blocks U18.

## Appendix A: commit → item map

"never" means the commit stays out of upstream; see §7.4 for where its content goes instead.

**BS11a-t (098dafd)**

| Commit | Items | Upstream |
|---|---|---|
| 6fe1011 | B-01 | U20 (reshaped) |
| c2c6cba | B-02, B-03, B-05 | U20 (codec part only); the wrappers stay node-side |
| 6644f85 | B-21 | never |
| f9a942c, 20aac32 | B-22 | U20 (regenerated subset) |
| 70fb331 | changelog | with U20 |
| e45ca9e, b561560, 390cef6 | B-05 | never (node-side) |
| 3b1d38f, fcf26a9, 97096e7 | B-07 | deferred (BS9b) |

**BS4a (481f8c3)**

| Commit | Items | Upstream |
|---|---|---|
| dd859d9, 0ba7bf5 | A-12 | never |
| 5ce44ad | A-04 | U1 |
| 73303cd | A-06 | U1 |
| fc30b05 | A-01, A-03 | `Checkable` order → U4; value type → U22 (Q-1) |
| 339d43d | A-02, A-05 | S part → U4; the payload cap stays as upstream has it |
| d4a1be3 | A-05 | U4 |
| 6762bbb, 8be6b9d | A-03 | U4 |
| 2ea2b13, 686db6a | A-08 | never |
| cd1d38b | A-09 | U2 |
| 9277360, a258c7b | A-10 | U2 |
| 47b93a7 | A-11 | U3 (its constant → U6) |
| bfdc8ce | A-15, A-17, A-18, A-19, A-24, A-25, A-26, A-27 | parts → U5, U6, U14; the rest never |
| f652f44 | A-15 | U5 |
| f45f641 | A-21 | U3 |
| 99d090f | A-20 | U3 |
| dd090ee, ec2ccd2, 389cd42, ad0c785, a1dc00c, 3f02caa | A-13 | U13 |
| 6f136d2 | A-14 | never |
| 157e286 | A-16 | never |
| 89f4635 | A-25 | U6 |
| 5cf3bd7 | A-22 | never (the +12 bytes); ordering with the V2 unit |
| 463429e, b24511e | A-23 | V2 unit (§2.4) |
| ecb74da, 0c71f50, 3c971b1 | A-07 | U21 |
| f6b8693 | changelogs | the types part with U1/U13; the p2p_core and params parts are dropped |
| e892086, 571e94e, a15c4e1 | lint, test labels, corpus regeneration | folded into each unit |

**BS5 (23f6fd7)**

| Commit | Items | Upstream |
|---|---|---|
| 0286d1a | C-01 to C-22 | split into U7, U8, U9, U10 and U24; C-01 is dropped; C-07 to C-09, C-14, C-15 and C-18 go node-side |
| fdda6b2 | C-08, C-10, C-14 | the u32 parts → U10; the rest → dash-args |
| ae304a1 | C-19 | regenerated in U7, U9 and U10 |
| 8c23be5 | C-14, C-16, C-19 | BRR → U24; the rest node-side |

**BS6a (a1d4dd3)**

| Commit | Items | Upstream |
|---|---|---|
| 4bfb8fc | B-04 | replaced by the classifier's error (U20) |
| 4bae81f | B-21 | never |
| f53ebe7 | B-10, B-11, B-13, B-14, B-15 | domain cap → U12; the rest → U21 (B-15 dropped) |
| 4e6bb33 | B-03, B-05, B-17 to B-20 | v24 types → U11; the BLS fields stay byte bags |
| 26c91c9 | B-01, B-18 | the shared helpers → U11 |
| 283ebc0, c0a08c8 | B-20, B-18 | U11 |
| 3c2b5ae | B-16 | U12 |
| 3613593 | B-16, B-20 | U11, U12 |
| 28f68ee, b64d9f1, 550a249 | B-22 | not carried; each unit carries its own tag-generated vectors |
| 0621fcb, b497f65 | B-21 | U25 |
| ac9634d, 60d6090, 7f3f483 | B-06 | never (node-side) |
| 8685005 | B-08 | U20 (as methods) |
| 73ab4b0 | B-09 | never (node-side) |

**BS7 (b69fa55, unmerged)**

| Commit | Items | Upstream |
|---|---|---|
| 61bfbf2, 23d8738 | D-01 | U15 |
| 851071c | D-02 | U16 |
| 9808e01 | D-03, D-04 | U17 |
| e43da5e, e2f39fc | D-05 | U17 (optional) |
| d3c22e9, 0fe352c | D-06 | U18 |
| 40673b2 | D-07, D-08 | U19 (strict uncompress) |
| 6238c14 | D-08, D-09 | encoder part → U19; the count read is dropped |
| 0b31754 | D-08 bench | U19 (scrubbed) |
| 6285205 | D-10, D-11, D-12, D-14, D-15 | U5 (control), U26, U27, U28, U29 |
| 27e470f, a1d2cd2 | D-13 | U27 (no cursor) |
| e946aec | D-16 | U3 |
| a06876d | D-17 | never (drop before the fork merge) |
| 021ba95 | D-17, D-18 | fail-fast `read_byte_vector` only; the rest is dropped |
| 96a3aca, aa7f5ce, 649716c, 7b93dbf, b69fa55 | D-19 | split and trimmed into each unit |
| b20c624, 95f24ec, 378f4cf, 3a80245 | no behaviour change | folded or dropped |
