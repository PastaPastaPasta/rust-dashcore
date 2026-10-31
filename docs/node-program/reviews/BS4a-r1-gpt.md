# BS4a independent review, round 1 (GPT)

**CHANGES-REQUIRED.** Two major decode-domain mismatches are reproducible at both Core pins: the new blanket payload bound rejects valid V2 short-ID messages, and short `pong` payloads are classified as decode failures instead of cancelling an outstanding ping. The scalar, transaction, optional-version and consumption changes otherwise passed the targeted comparisons below. There is also a minor omission in the sweep's handshake policy.

Reviewed on 2026-10-08 under [REVIEW-COMMON.md](/work/home/agent/workspace/node-program/briefs/REVIEW-COMMON.md), against PLAN BS4a and A1 §1.4 DDE #1–#9 / §2.14:

| Repository | Reviewed snapshot |
|---|---|
| base-sdk | `node/bsdk-bs4a-decode-domain`, `f8a088ff59fab92e674734ed0859dc98a6f16736`; all 19 commits above `33fcc59e1868c803f9608e0e78ee8bddf3abcee7` (`node/base-sdk`) |
| rust-dashcore | `node/bs4a-decode-domain`, `c5534013c2f6840c9bf1f302ac5c09b8e246163d`; `contrib/node/dde-probe/` and `docs/node-program/DDE-SWEEP.md` |
| Reference | Dash Core `v23.1.8` and `v24.0.0-rc.3`: tag source, private release-binary regtest nodes, and O1 transaction oracle |

The reviewed worktrees were left clean. Builds, copied probes and generated evidence live in `/work/scratch/bs4a-r1-gpt`; tests used scratch storage. No commits, pushes or GitHub messages were made. The private nodes used P2P/RPC ports 29611/29612 and 29621/29622, with datadirs under that scratch directory, and were stopped after testing.

## Findings

### F1 — Major: the blanket 3 MiB payload check rejects valid V2 short-ID messages

**Locations:** [bip324.rs:69](/work/home/agent/workspace/bsdk-wt/bs4a-decode-domain/pkgs/p2p_core/src/bip324.rs:69), [macros.rs:181](/work/home/agent/workspace/bsdk-wt/bs4a-decode-domain/pkgs/p2p_core/src/macros.rs:181), [macros.rs:14](/work/home/agent/workspace/bsdk-wt/bs4a-decode-domain/pkgs/p2p_core/src/macros.rs:14), [codec.rs:15](/work/home/agent/workspace/bsdk-wt/bs4a-decode-domain/pkgs/p2p_core/src/codec.rs:15).

`decode_v2()` strips the message type and calls `decode_with_policy()`. Its unconditional check rejects any remaining payload above 3,145,728 bytes. Core's V2 transport instead bounds the **entire decrypted contents** to `1 + 12 + MAX_PROTOCOL_MESSAGE_LENGTH = 3,145,741`. Consequently a short-ID message can carry 3,145,740 payload bytes, whereas a long-form message can carry only 3,145,728. Neither pin reapplies the smaller V1 payload limit after extracting a V2 short ID.

Concrete witness: decrypted V2 contents `12 || 7856240100000000 || 00…`, with payload length 3,145,729 (or 3,145,740). `0x12` is `ping`; the first eight payload bytes carry nonce `0x01245678`, and the remainder is ignored by Core's handler. The SDK returns `PayloadTooLarge { size: 3145729, max: 3145728 }`; both real encrypted Core connections answer with `pong` and remain open.

| V2 type encoding | Payload bytes | Core v23.1.8 | Core v24 rc.3 | SDK |
|---|---:|---|---|---|
| short | 3,145,728 | pong, stays open | pong, stays open | accepts, consumes 8 |
| short | 3,145,729 | pong, stays open | pong, stays open | rejects |
| short | 3,145,740 | pong, stays open | pong, stays open | rejects |
| short | 3,145,741 | packet-too-large disconnect | packet-too-large disconnect | rejects |
| long | 3,145,729 | packet-too-large disconnect | packet-too-large disconnect | rejects |

Core source: `src/net.cpp` v23.1.8:1303 / v24 rc.3:1297 defines `MAX_CONTENTS_LEN` and checks it immediately after decrypting the length; command extraction happens later. The new SDK bound therefore introduces a transport-dependent decode divergence and a peer fingerprint.

**Suggested fix:** separate envelope validation from handler decoding. Retain the V1 3 MiB payload limit; check V2 total contents against 3,145,741 before command/short-ID resolution; permit the extra 12 bytes for short-ID payloads. Do not reapply the V1 bound in V2 dispatch. Test both representations at every boundary, including oversize input with an unknown short ID so the size error precedes type resolution.

Evidence: [v2-core.jsonl](/work/scratch/bs4a-r1-gpt/v2-core.jsonl), [focused-sdk.jsonl](/work/scratch/bs4a-r1-gpt/focused-sdk.jsonl) (`v2_ping/*`), [V2 reproduction script](/work/scratch/bs4a-r1-gpt/probe/v2_boundary.py).

### F2 — Major: short `pong` is a successful state event in Core, not an EOF failure

**Locations:** [msg/mod.rs:200](/work/home/agent/workspace/bsdk-wt/bs4a-decode-domain/pkgs/p2p_core/src/msg/mod.rs:200), [ping.rs:33](/work/home/agent/workspace/bsdk-wt/bs4a-decode-domain/pkgs/p2p_core/src/msg/ping.rs:33), [DDE-SWEEP.md:83](/work/home/agent/workspace/rdc-wt/bs4a-decode-domain/docs/node-program/DDE-SWEEP.md:83).

The default `MsgDecode` for `Pong` reads a mandatory u64, so every payload of length 0–7 returns `Consensus(Eof)`. The sweep calls this equivalent because it “also only logs.” Core also **cancels the outstanding ping**: it checks `vRecv.size()` before attempting a read, sets `bPingFinished = true` for a short payload, and clears `peer->m_ping_nonce_sent`. No nonce bytes are consumed and no successful round-trip measurement is recorded.

Concrete reproduction at each pin: complete the handshake but leave Core's initial `ping` unanswered. Verify `getpeerinfo` exposes `pingwait`; send `pong` with an empty payload. `pingwait` disappears, `pingtime`/`minping` stay unset, and Core logs `Short payload`. The same happens for every length 1–7 (tested `2a` followed by zero padding). An eight-byte nonzero mismatched nonce leaves `pingwait` pending, providing the control case. The SDK instead rejects all eight short inputs before a handler event can be delivered.

Core source: `src/net_processing.cpp` v23.1.8:5397, short branch at 5432, nonce clearing at 5446; v24 rc.3:5474, short branch at 5509, clearing at 5523. This falls directly under DDE #9 and the per-state policy requirement: mapping EOF to a log does not reproduce the handler transition and changes ping timeout behaviour.

**Suggested fix:** represent a short pong explicitly (for example an optional nonce or a short-payload variant) in the handler-facing decode result. Return success with consumed length 0 when fewer than eight bytes are available; let the actor cancel the pending ping without recording latency. Preserve normal u64 parsing and trailing-byte leniency for length ≥8. Correct the sweep claim and add a state regression that contrasts short, zero-nonce and nonzero-mismatched pongs.

Evidence: [pong-state.jsonl](/work/scratch/bs4a-r1-gpt/pong-state.jsonl) (nine lengths × both pins), [focused-sdk.jsonl](/work/scratch/bs4a-r1-gpt/focused-sdk.jsonl) (`pong/*`), [state reproduction script](/work/scratch/bs4a-r1-gpt/probe/pong_state.py).

### F3 — Minor: the sweep omits conditional handshake gates

**Locations:** [DDE-SWEEP.md:80](/work/home/agent/workspace/rdc-wt/bs4a-decode-domain/docs/node-program/DDE-SWEEP.md:80), [DDE-SWEEP.md:109](/work/home/agent/workspace/rdc-wt/bs4a-decode-domain/docs/node-program/DDE-SWEEP.md:109).

The combined no-payload row omits `sendaddrv2`'s version/handshake gate. At both pins the order is: common version <70223 → ignore; otherwise after verack → disconnect; otherwise enable addrv2, ignoring all payload bytes. With payload `dead`, versions 70221/70222 stay connected before and after verack; versions 70223/70240 disconnect only after verack. A1 already records the post-verack behaviour, but the delivered per-command sweep does not.

The `sendtxrcncl` row says “after verack: disconnect” without its prerequisite: Core first returns if transaction reconciliation is disabled. The default private nodes ignored the same post-verack inputs in that state. Source confirms the enabled check precedes the handshake check (`net_processing.cpp` v23:4265 / v24:4310); `sendaddrv2` is v23:4247 / v24:4292.

**Suggested fix:** record these conditions and their order in the sweep, with before/after-verack and version-bucket evidence. Connection-state gates appropriately remain outside the SDK codec; this finding concerns completeness of the delivered policy rather than requiring them in `ProtoCtx`.

Evidence: [handshake-state.json](/work/scratch/bs4a-r1-gpt/handshake-state.json), [handshake probe](/work/scratch/bs4a-r1-gpt/probe/handshake_state.py), and `sendtxrcncl/*` in [sweep-core-enriched.jsonl](/work/scratch/bs4a-r1-gpt/sweep-core-enriched.jsonl).

## DDE #1–#9 results

| DDE row | Adversarial checks and result |
|---|---|
| #1 signed output value | i64 extrema, negative values, `MAX_MONEY` boundaries and 256 deterministic random values; raw output round-trip and soft `amount()` accessor. O1 accepts the same transaction bytes and consumes the same length; range rejection remains a later transaction rule. No new disagreement found. |
| #2 Core bool | All 256 input bytes decode and normalize to 0/1; real `senddsq` and `qsendrecsigs` handlers were exercised with all 256 values plus a trailing `ff` at each pin. Version relay/MN booleans also tested. No disagreement found. The strict Rust `bool` codec was deliberately removed. |
| #3 extra-payload size | Payload lengths 10,000, 10,001, 10,240, 10,241, 1 MiB, 32 MiB and 32 MiB+1 through O1. At exactly 32 MiB SDK/Core consume 33,554,611 transaction bytes; above it all reject. Non-minimal CompactSize and other length prefixes retain canonicality/range checks. The 1 MiB case also confirms total-size rejection precedes payload-size rejection. |
| #4 LLMQ types | All 256 discriminants tested; all 14 named values compared against both Core tags, including 103/105 and the `ff` sentinel. Other bytes remain representable as `Unknown`. The params re-export is the same type. No disagreement found. |
| #5 unknown commands | Unknown valid command decodes to `P2pMsg::Unknown`, preserves raw bytes and reports their retained length. Core logs/ignores it. Oversize unknown V1 input is rejected before dispatch. Command-string syntax belongs to the later transport owner; see scope below. |
| #6 consumed payload length | Mainnet ProRegTx/CbTx payloads with and without trailing bytes; RPC payload object disappears when Core's `GetTxPayload` cannot consume everything, while SDK exposes `is_complete() == false`. CbTx `bestCLHeightDiff` at 32 MiB passes, above it fails. Shape/version/BLS divergences already assigned to BS6a remain. |
| #7 protocol context | Context introduced and exercised across 70230/70234/70235 headers2 count boundaries; signed `ProtocolVersion` tested at i32 boundaries. Version-dependent overlay layouts are still assigned to BS8–BS10, so this review does not certify their current placeholder codecs. |
| #8 optional version | Every cut of the 137-byte captured version, field-boundary suffix omission, incomplete groups, subver 256/257, nonzero bools, signed/obsolete versions, broken tails and trailing bytes. Core's mandatory 46-byte head and optional grouped reads match the SDK's success/`VersionTail` results. An obsolete head still disconnects before a broken suffix can mask that decision. |
| #9 per-command empty/trailing | All 74 commands exercised; V1 envelope check order passed. Typed/no-read trailing-byte behaviour matched the tested handlers apart from **F2**. V2 envelope handling fails **F1**. Raw stubs and known owner divergences remain explicitly deferred. |

[focused-sdk.jsonl](/work/scratch/bs4a-r1-gpt/focused-sdk.jsonl) contains 698 focused cases. The original [P2P probe results](/work/scratch/bs4a-r1-gpt/p2p-original.json) and [payload RPC results](/work/scratch/bs4a-r1-gpt/payload-original.json) were reproduced at both pins; the corpus generator completed successfully. There were 960 O1 transaction cases (11 original, 949 extended mutations/truncations/value/version cases), with **zero decode-verdict or consumed-length disagreements at either pin**: [original results](/work/scratch/bs4a-r1-gpt/oracle.jsonl), [extended results](/work/scratch/bs4a-r1-gpt/oracle-extra.jsonl), [extended inputs](/work/scratch/bs4a-r1-gpt/oracle-extra-requests.jsonl).

## Coverage of all 74 commands and limits of the evidence

The SDK command set equals `allNetMessageTypes` at both tags: exactly 74, with no missing or extra commands. The broad sweep made 528 SDK requests and 752 real Core requests (376 per pin across the 73 non-version commands); `version` was tested separately with the captured-payload probe. Shared inputs include empty, `00`, nonminimal `fd0000`, above-MAX_SIZE `fe01000002`, and available command-specific seeds/trailing variants.

The table below records **empty-input observations in the broad sweep's ordinary-peer/initial-node state**, not a proof of every command's decode domain. “No rejection observed” means precisely that; it may reflect an IBD, synchronization, session or masternode gate. Raw stubs retain bytes for later owners and therefore accept inputs Core might reject when those handlers run. `synced` in the broad JSON records successful peer synchronization via ping, not masternode-sync completion. Additional governance tests ran after mining one block and advancing masternode sync, outside IBD. No authenticated masternode or active CoinJoin session was constructed.

For all 74 commands, plus unknown and malformed commands, a separate established-V1 test advertised body length **3,145,729 without sending a body**. Both pins disconnected in all 152 cases, confirming the envelope limit precedes command validation/handler parsing; SDK dispatch rejects above-cap inputs for every command. SDK boundary requests at exactly 3 MiB proceed to command policy. The V2 difference is F1.

Evidence: [requests](/work/scratch/bs4a-r1-gpt/sweep-requests.jsonl), [SDK outcomes](/work/scratch/bs4a-r1-gpt/sweep-sdk.jsonl), [Core outcomes](/work/scratch/bs4a-r1-gpt/sweep-core-enriched.jsonl), [V1 cap/order outcomes](/work/scratch/bs4a-r1-gpt/cap-v1-core.jsonl). Use the **enriched** Core file: Core's exception log lines lack a peer ID; these were recovered from sequential `received:` records rather than treating their omission from the initial filter as acceptance.

| Command | SDK form / empty input | Core v23 empty input | Core v24 empty input | Scope / follow-up |
|---|---|---|---|---|
| `version` | typed: error | mandatory head: error; suffix: see DDE #8 | mandatory head: error; suffix: see DDE #8 | BS4a policy |
| `ping` | typed: error | read exception | read exception | **F1** (V2 cap) |
| `pong` | typed: error | cancels pending ping | cancels pending ping | **F2** |
| `addr` | typed: error | read exception | read exception | BS10 address values |
| `addrv2` | typed: error | read exception | read exception | BS10 address values |
| `inv` | typed: error | read exception | read exception | BS4a policy |
| `getdata` | typed: error | read exception | read exception | BS4a policy |
| `notfound` | typed: error | read exception | read exception | BS4a policy |
| `getheaders` | typed: error | read exception | read exception | BS4a policy |
| `headers` | typed: error | read exception | read exception | BS4a policy |
| `getheaders2` | typed: error | read exception | read exception | BS4a policy |
| `headers2` | typed: error | read exception | read exception | BS7 header codec |
| `govsync` | typed: error | no rejection observed | no rejection observed | BS9b |
| `govobj` | typed: error | no rejection observed | no rejection observed | typed; state gates |
| `govobjvote` | typed: error | no rejection observed | no rejection observed | BS9b |
| `getmnlistd` | typed: error | read exception | read exception | BS4a policy |
| `mnlistdiff` | typed: error | +100 / disconnect | +100 / disconnect | BS8 |
| `senddsq` | typed: error | read exception | read exception | BS4a policy |
| `qsendrecsigs` | typed: error | read exception | read exception | BS4a policy |
| `verack` | no reads: accept | no rejection observed | no rejection observed | BS4a policy |
| `getaddr` | no reads: accept | no rejection observed | no rejection observed | BS4a policy |
| `sendaddrv2` | no reads: accept | disconnect | disconnect | **F3** (handshake gates) |
| `sendheaders` | no reads: accept | no rejection observed | no rejection observed | BS4a policy |
| `sendheaders2` | no reads: accept | no rejection observed | no rejection observed | BS4a policy |
| `block` | raw stub: accept | read exception | read exception | BS7 |
| `blocktxn` | raw stub: accept | read exception | read exception | BS7 |
| `cmpctblock` | raw stub: accept | read exception | read exception | BS7 |
| `filteradd` | raw stub: accept | +100 / disconnect | +100 / disconnect | BS7 |
| `filterload` | raw stub: accept | +100 / disconnect | +100 / disconnect | BS7 |
| `getblocks` | raw stub: accept | read exception | read exception | BS7 |
| `getblocktxn` | raw stub: accept | read exception | read exception | BS7 |
| `merkleblock` | raw stub: accept | no rejection observed | no rejection observed | BS7 |
| `sendcmpct` | raw stub: accept | read exception | read exception | BS7 |
| `tx` | raw stub: accept | no rejection observed | no rejection observed | BS7 |
| `getcfilters` | raw stub: accept | read exception | read exception | BS7 |
| `cfilter` | raw stub: accept | no rejection observed | no rejection observed | BS7 |
| `getcfheaders` | raw stub: accept | read exception | read exception | BS7 |
| `cfheaders` | raw stub: accept | no rejection observed | no rejection observed | BS7 |
| `getcfcheckpt` | raw stub: accept | read exception | read exception | BS7 |
| `cfcheckpt` | raw stub: accept | no rejection observed | no rejection observed | BS7 |
| `sendtxrcncl` | raw stub: accept | no rejection observed | no rejection observed | BS7; **F3** |
| `spork` | raw stub: accept | +100 / disconnect | +100 / disconnect | BS8 |
| `dsa` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `dsi` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `dsf` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `dss` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `dsc` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `dssu` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `dstx` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `dsq` | raw stub: accept | no rejection observed | no rejection observed | BS9b |
| `ssc` | raw stub: accept | read exception | read exception | BS7 |
| `qfcommit` | raw stub: accept | read exception | read exception | BS8 |
| `qcontrib` | raw stub: accept | +10 (gate) | +10 (gate) | BS9a |
| `qcomplaint` | raw stub: accept | +10 (gate) | +10 (gate) | BS9a |
| `qjustify` | raw stub: accept | +10 (gate) | +10 (gate) | BS9a |
| `qpcommit` | raw stub: accept | +10 (gate) | +10 (gate) | BS9a |
| `qsigsesann` | raw stub: accept | no rejection observed | no rejection observed | BS9a |
| `qsigsinv` | raw stub: accept | no rejection observed | no rejection observed | BS9a |
| `qgetsigs` | raw stub: accept | no rejection observed | no rejection observed | BS9a |
| `qbsigs` | raw stub: accept | no rejection observed | no rejection observed | BS9a |
| `qsigrec` | raw stub: accept | read exception | read exception | BS8 |
| `qsigshare` | raw stub: accept | no rejection observed | no rejection observed | BS9a |
| `qgetdata` | raw stub: accept | +10 (gate) | +10 (gate) | BS8 |
| `qdata` | raw stub: accept | +10 (gate) | +10 (gate) | BS8 |
| `clsig` | raw stub: accept | no rejection observed | no rejection observed | BS8 |
| `isdlock` | raw stub: accept | no rejection observed | no rejection observed | BS8 |
| `mnauth` | raw stub: accept | no rejection observed | no rejection observed | BS8 |
| `getqrinfo` | raw stub: accept | read exception | +100 / disconnect | BS8 |
| `qrinfo` | raw stub: accept | +100 / disconnect | +100 / disconnect | BS8 |
| `platformban` | raw stub: accept | no rejection observed | no rejection observed | BS8 |
| `filterclear` | stub, no reads: accept | no rejection observed | no rejection observed | BS7 |
| `mempool` | stub, no reads: accept | no rejection observed | no rejection observed | BS7 |
| `getsporks` | stub, no reads: accept | no rejection observed | no rejection observed | BS4a policy |
| `qwatch` | stub, no reads: accept | +10 (gate) | +10 (gate) | BS7 |

Additional confirmed owner backlog, already disclosed by `DDE-SWEEP.md` and not newly charged to BS4a:

- **BS7 headers2:** the first-header empty-version-cache witness is accepted by Core and rejected by SDK. [owner outcomes](/work/scratch/bs4a-r1-gpt/owner-core.jsonl), [SDK comparison](/work/scratch/bs4a-r1-gpt/owner-sdk.jsonl).
- **BS9b governance:** SDK accepts signature lengths 0, 1 and 97 for `govobjvote`; synced Core scores +100, while lengths 65/96 pass decoding. SDK accepts the incomplete 33-byte `govsync` bloom request that Core scores +100, and consumes only 33 bytes of the complete 42-byte empty-bloom request. See [synced Core outcomes](/work/scratch/bs4a-r1-gpt/synced-core.jsonl) and [SDK outcomes](/work/scratch/bs4a-r1-gpt/synced-sdk.jsonl).
- **BS6a/BS8–BS11a:** special-payload version early returns, BLS eager/lazy interpretation, overlay layouts and address canonicalization remain owner work. Unknown BIP155 address lengths through 512 and above 512, and u64-max addrv2 services without a MAX_SIZE check, were tested without a new verdict mismatch.

Thus this is an attack on the delivered BS4a shapes/policy and a command-by-command coverage record, not certification that today's raw stubs implement the complete node. It does not replace BS4b's ≥24 CPU-hour differential fuzz gate or CS0's canonical transaction/block re-encoding gate.

## Consumer/API compatibility

This series intentionally breaks public source APIs. Existing consumers need these adaptations:

| API change | Required consumer change |
|---|---|
| `TxOut.value: Amount → i64` | Construct through `TxOut::new` when starting with an `Amount`; use `amount()` when a bounded amount is required and handle failure. Do not cast an unchecked wire value to unsigned. |
| `Transaction::decode_payload() → PayloadDecode` | Access `.payload`, inspect consumed length / `.is_complete()`, and preserve the owning rule's leftover-byte rejection. |
| `P2pMsg::decode_payload` removed | Use `decode_with_policy(cmd, payload, ctx)` and handle `DecodedMsg.msg` / `.consumed`, including `VersionTail` and handler-specific errors. |
| `decode_v2` now needs context and returns `DecodedMsg` | Pass peer protocol/BLS context and adapt result access; fix F1 before relying on its envelope bound. |
| `ProtocolVersion` uses i32; version suffixes are `Option`; bool fields use `CoreBool` | Update literals/conversions, supply Core's defaults in the handler, and distinguish absence from truncation. |
| typed `senddsq`/`qsendrecsigs`; `P2pMsg::Unknown` | Adapt construction and exhaustive matches; unknown messages must be ignored by policy rather than penalized. |
| params `LlmqType` unified | Update import/type identity and renamed `LlmqTestInstantSend → LlmqTestInstantsend`. |
| Rust `bool: BaseCodec` and public `MAX_SPTX_PAYLOAD_SIZE` removed | Use `CoreBool` for Core wire fields and the proper serialization/rule limit. |

Both in-repo sample consumers in `docs/samples` were adapted and **build successfully**. Workspace tests also compile the in-repo callers. These are source breaks mandated by BS4a's representation changes, not evidence that the old public API remains compatible. A downstream upgrade must adapt callers; the separate rust-dashcore conformance-adaptation worktree is outside this reviewed branch and is not covered by the sample-build claim. A consolidated migration note would help external SDK consumers.

The convenience buffered `Decode` API remains strict about trailing bytes and its aggregate object buffer. The node design uses cursor-based `BaseCodec` plus per-command policy, so that convenience API is not used as evidence of a DDE regression.

## Build, tests and performance

Validation ran on a frozen copy of the reviewed SDK snapshot:

- `cargo test --workspace --all-features`: **1,113 passed, 0 failed**, two ignored doc tests; [log](/work/scratch/bs4a-r1-gpt/workspace-test.log). This includes compilation of the all-feature workspace.
- `cargo fmt --all -- --check`: passed.
- Clippy with warnings denied on `dash-types`, `dash-primitives`, `dash-p2p-core`, `dash-params`, `dash-pkc`, with `--all-targets --features full` and separately `--no-default-features`: passed; [full-feature touched-crate log](/work/scratch/bs4a-r1-gpt/clippy-touched.log), [no-default log](/work/scratch/bs4a-r1-gpt/clippy-nostd.log).
- `cargo build --workspace --no-default-features`: passed; [log](/work/scratch/bs4a-r1-gpt/build-nostd.log).
- `cargo check --workspace` in `docs/samples`: passed; [log](/work/scratch/bs4a-r1-gpt/samples-build.log).
- Whole-workspace full/all-feature Clippy with `-D warnings` is **not green**: unchanged `dash-pow` x86 AES imports/dead-code warnings stop it. This is outside the diff; targeted touched-crate lint passed. [workspace lint log](/work/scratch/bs4a-r1-gpt/clippy-full.log).

The mainnet block-decode instruction gate passed on all 34 corpus blocks (642,893 bytes), 200 iterations, release build, subtracting the `ITERS=0` load from `perf stat -e instructions:u`: old **912,149,106**, new **894,149,885**, **−1.9733%**. [Counter evidence](/work/scratch/bs4a-r1-gpt/bench-evidence.json). Shared-box wall times differed (~462 / 521 ms) and do not establish wall-time parity; the reported gate evidence is the instruction count.

## O1 provenance and replay

O1 was copied from committed `node/o1-oracle-sys` snapshot `54221217e5e0dfd6f0bc8e9b09c1eb065c8cb589`. Its committed Rust API needs newer oracle entry points than its committed archive pins provide. The live O1 worktree contains another worker's uncommitted pin adjustment; it was not modified or used as an implicit override. In the **scratch copy only**, pin metadata was adjusted to the available API-compatible archives, with manifest, SHA256 and release-tag ancestry verified. Changes from the older shims are confined to `src/test/oracle/oracle.cpp/.h`; the underlying release source is unchanged. Successful runs did not use an unpinned escape hatch.

| Pin | Oracle shim commit | Verified archive SHA256 |
|---|---|---|
| v23.1.8 | `350ce242a59c19782dc9eebc8e23dead0a2e16ee` | `22f00737694ab069e22d01d2cee3fc749b8c5c3814023a0a086d230c03829463` |
| v24.0.0-rc.3 | `23769258c239e69282569e9f9e219ce9e2c4ee2e` | `c9cbd75604c6deef3158854ebeb674b0ff12cf21fc5dba13b38a04ffa99fe29b` |

[Archive evidence](/work/scratch/bs4a-r1-gpt/oracle-archive-evidence.json), scratch O1 source in `/work/scratch/bs4a-r1-gpt/o1`, archives in `/work/scratch/o1/final/plain/a`. This provenance adjustment matters when reproducing with the committed tool; an unchanged O1 checkout does not link against its older pinned archive pair.

SDK/oracle inputs can be replayed without running a node:

```bash
export TMPDIR=/work/scratch/bs4a-r1-gpt/tmp
/work/scratch/bs4a-r1-gpt/sdkprobe/target/debug/bs4a-review-probe \
  < /work/scratch/bs4a-r1-gpt/focused-requests.jsonl
/work/scratch/bs4a-r1-gpt/probe/oracle/target/debug/dde-oracle wire \
  < /work/scratch/bs4a-r1-gpt/oracle-extra-requests.jsonl
```

The saved state probes use this private-node setup (scripts write the named scratch evidence files, so copy those outputs first if preserving this run):

```bash
cd /work/scratch/bs4a-r1-gpt/probe
export TMPDIR=/work/scratch/bs4a-r1-gpt/tmp
export DDE_PROBE_DIR=/work/scratch/bs4a-r1-gpt/nodes
export DDE_DASHD_V23=/work/scratch/o1/rel/dashcore-23.1.8
export DDE_DASHD_V24=/work/scratch/v24a-dashcore-wire/bin/dashcore-24.0.0-rc.3
python3 nodes.py start
python3 pong_state.py
python3 v2_boundary.py
python3 handshake_state.py
python3 nodes.py stop
```

**Verdict: CHANGES-REQUIRED.** Fix F1 and F2 and replace the incorrect short-pong equivalence claim; add the missing handshake gate qualifications in F3. Existing BS6a–BS11a backlog remains with its named owners.
