# Node SDK integration

This branch, `node-sdk-integration`, carries the base-sdk changes our Rust Dash full node uses. It is rebuilt on
`develop` at `d9e0d24` (MSRV 1.89, `thiserror` for the error enums it extends), so it is the working base for bringing
these changes upstream. It is not a pull request: the units in section 5 become small pull requests, paced after your v24
series. It supersedes `node-sdk-preview`, which stays pushed, frozen and unchanged.

Each commit is one reviewable step and builds on the one before it. Most carry one planned unit (section 5); where units
could not be separated without writing new code, one commit carries several, and section 1 says which. Every commit
passes `just test lint`, CodeQL included, with one deviation: on our x86_64 build host, `dash-pow` fails clippy with
warnings denied on unmodified `develop` (unused AES-NI items in `util/aes/amd64.rs` under the repository's `x86-64-v3` +
`aes` target flags), so we ran the Justfile's clippy lines with `--workspace --exclude dash-pow`. No commit touches
`pkgs/pow`. Your CI runs on arm and does not see this.

A few hunks, tests and corpus rows from our fork are withheld from this branch. They follow later.

Throughout, "Core" means Dash Core v24.0.0-rc.3, with v23.1.8 checked wherever the two differ. "Evidence" gives counts;
the cases are in the named corpus files, each produced by Core and not written by hand.

## 1. The commits

| # | Commit | Crate | Unit (section 5) | Size | Breaks API |
|---|---|---|---|---|---|
| 1 | `6d16114` | `dash-types` | `codec_size` | +85/−4 | no |
| 2 | `f55ccbe` | `dash-types` | feeds `v24_protx`, `version_tail`, `p2p_ctrl` | +141 | yes |
| 3 | `8b1ef2d` | `dash-pkc` | `bls_wire` | +5,319 | no |
| 4 | `ad3b48a` | `dash-primitives` | `llmq_types` | +45 | yes |
| 5 | `e645b5a` | `dash-primitives` | `netinfo_strict`, `netinfo_core` | +219/−50 | yes |
| 6 | `b99abb4` | `dash-primitives` | `tx_check`, `tx_amount` | +599/−81 | yes |
| 7 | `aff37a2` | `dash-primitives` | `v24_protx`, `cql_share` | +1,532/−321 | yes |
| 8 | `f3e8af8` | `dash-primitives` | `v24_cbtx_assetlock` | +1,296/−8 | yes |
| 9 | `5b3e49b` | `dash-params` | `params_v24`, `llmq_params`, `params_devnet`, `params_ovr` | +12,817/−713 | yes |
| 10 | `c7784d3` | `dash-params` | `params_brr` | +548 | no |
| 11 | `f08b0d7` | `dash-p2p-core` | `p2p_frame`, `p2p_limits`, `p2p_v1`, `p2p_v2`, `hdr_limits`, `headers2`, `version_tail`, `p2p_unknown`, `p2p_bip37` | +2,521/−375 | yes |
| 12 | `c5c175c` | `dash-p2p-core` | `p2p_inv` | +89/−2 | yes |
| 13 | `bc92e13` | `dash-p2p-core`, `dash-types` | `p2p_ctrl` | +584/−41 | yes |
| 14 | `8abddb4` | `dash-p2p-core` | `p2p_bip152`, `p2p_bip157`, `p2p_blocktx` | +8,111/−25 | yes |
| 15 | `9010dc5` | `dash-p2p-core`, `dash-types` | `overlay` | +9,621/−187 | yes |

Sizes are `git diff --shortstat` against the previous commit, corpora included.

## 2. What each change does

Each entry says what the change does and why the node needs it, the Core behaviour it matches and the evidence, its
status, what it depends on, the unit it maps to, and the API changes that matter for your v24 work.

"Status" is one of: **merged on our fork** (reviewed, in the node's base-sdk today), or **open upstream** (a pull
request against `dashpay/base-sdk`). Everything on this branch is merged on our fork; three units are also open
upstream.

### 2.1 `dash-types`: length prefixes and vector reservation (`6d16114`)

- **What and why.** `CompactSize::decode_size`, and length prefixes of vectors, strings and scripts range-checked
  against `MAX_SER_SIZE` even when enough bytes remain. `Vec<T>` reserves each allocation batch exactly instead of
  growing past it. A decoder that skips the range check accepts a message Core refuses, and the node must refuse it
  where Core does.
- **Core behaviour.** `ReadCompactSize` with its default range check, and `reserve(allocated)` in Core's vector
  deserializer. Evidence: unit tests on each boundary, and the length cases of the P2P decode-domain corpus (2.11).
- **Status.** Merged on our fork; open upstream as #57, rebuilt there in your shape.
- **Depends on.** Nothing. **Unit.** `codec_size`.
- **API.** Nothing removed. A length prefix over `MAX_SER_SIZE` now fails where it used to read.

### 2.2 `dash-types`: `CoreBool`, `MalleableBls` and `opt_str_u64` (`f55ccbe`)

- **What and why.** `CoreBool`, a wire boolean that reads any non-zero byte as `true` and writes `0x00` or `0x01`, as
  Core's `bool` deserializer does. `DecodeError::MalleableBls`, Core's "malleable BLS object". `serialize::opt_str_u64`.
  The payload and message commits after this one use them.
- **Core behaviour.** Core's `bool` deserializer and its BLS wrapper's malleability check.
- **Status.** Merged on our fork. Not proposed as is: upstream keeps a strict 0/1 `bool` for the messages that carry a
  flag, and `MalleableBls` gives way to the classifier's error if `bls_wire` goes ahead.
- **Depends on.** 2.1. **Unit.** None on its own; it feeds `v24_protx`, `version_tail` and `p2p_ctrl`.
- **API.** `DecodeError` gains a variant, so exhaustive matches need a new arm.

### 2.3 `dash-pkc`: BLS fields as Core's wrappers read them (`8b1ef2d`)

- **What and why.** `BlsSchemeKind`, `BlsCtx` and `BlsSchemeSource` select the legacy or basic scheme at run time, as
  Core picks it per field: from the payload version, always basic, or from its process-wide flag. `BlsEager`,
  `BlsLazy` and `BlsGlobal` model Core's `CBLSWrapper`, `CBLSLazyWrapper` and the flag-scheme fields. An eager read
  gives a valid object, the invalid object (serialized as zeros) or Core's malleability error. Payload BLS fields
  decide transaction and block verdicts, and Core's rules for them are not the IETF rules.
- **Core behaviour.** Core's BLS wrappers at both releases. Evidence: `pkgs/pkc/corpus/bls_wrapper.json5`, from a
  seeded generator and a small C++ program linked against Core's own BLS code (`contrib/vectors/bls_wrapper/`): 104
  public-key, 157 signature and 15 secret-key inputs read eagerly, and 1,008 public-key and 1,008 signature
  pairs, each with Core's outcome at v24.0.0-rc.3 and v23.1.8. The inputs cover 42 public-key, 57 signature and 11
  secret-key input classes.
- **Status.** Merged on our fork. Not proposed as is: `bls_wire` keeps byte bags upstream plus one named classifier,
  and the wrapper types move to our node (question 2).
- **Depends on.** Nothing on this branch. **Unit.** `bls_wire` (issue first).
- **API.** Additive in `dash-pkc`. The `bls` feature now depends on `spin`. The CodeQL policy gains exemptions for the
  wrapper types. The new `BlsDecodeError` has hand-written `Display` and `Error` impls, not `thiserror`; it converts
  when `bls_wire` is rebuilt.

### 2.4 `dash-primitives`: LLMQ types 103 and 105 (`ad3b48a`)

- **What and why.** Names the two LLMQ types `develop` lacks (`llmq_test_dip0024` and `llmq_devnet_dip0024`), so all
  14 types Core knows decode by name.
- **Core behaviour.** Core's `Consensus::LLMQType`. Evidence: the chain-params corpus (2.9) uses every type.
- **Status.** Merged on our fork; open upstream as #58, which also has `dash-params` reuse `LlmqType`. Here that reuse
  is in 2.9.
- **Depends on.** Nothing. **Unit.** `llmq_types`.
- **API.** `LlmqType` gains two variants, so exhaustive matches need arms, and types 103 and 105 no longer decode as
  `Unknown`.

### 2.5 `dash-primitives`: NetInfo as Core reads it (`e645b5a`)

- **What and why.** Unusable entries decode as `NIEntry::Invalid`; each purpose keeps its first group, as Core's map
  does; a legacy address Core does not keep reads as the empty service; domains are capped at 253 bytes; an empty
  NetInfo is reported as Core reports it. A strict reader fails to decode some ProTx transactions Core accepts in
  blocks, so an SPV client or wallet on base-sdk cannot parse every valid v24 block.
- **Core behaviour.** Core v24's `ExtNetInfo` and `MnNetInfo` readers and their `Validate`. Evidence: the 21 NetInfo
  cases of `payload_core.json5` (2.8), each judged by Core.
- **Status.** Merged on our fork. **Depends on.** 2.1. **Units.** `netinfo_strict` (Tier 1) and `netinfo_core`
  (issue first, question 3).
- **API.** `NetInfo` keeps invalid entries as `Invalid`, and ProTx decoders return them instead of failing.

### 2.6 `dash-primitives`: transaction checks in Core's order (`b99abb4`)

- **What and why.** `TxOut`'s value decodes as Core's signed `CAmount`, and `check()` reports
  `bad-txns-vout-negative` and the money-range errors as Core does; special payloads are bounded by `MAX_SER_SIZE`;
  the serialized size is checked before the payload size, as in Core's `CheckTransaction`; `decode_payload` reports
  how much it read (`PayloadDecode`). A node must reject a transaction for the reason Core gives and accept every
  transaction Core accepts.
- **Core behaviour.** `CheckTransaction` and `MoneyRange`. Evidence: `pkgs/primitives/corpus/decode_domain.json5`, 12
  cases with Core's verdicts, and allocation tests for hostile lengths.
- **Status.** Merged on our fork. The size-order part is built upstream and held (`tx_check`, after #57). The signed
  value is the optional `tx_amount` (question 6).
- **Depends on.** 2.1, 2.5. **Units.** `tx_check` and `tx_amount`.
- **API.** `TxOut::value` changes from `Amount` to `i64`, with `TxOut::new` and `TxOut::amount()` added;
  `TxInvalid::OutputTooLarge` and `OutputTotalTooLarge` carry `i64`; `TxInvalid::OutputNegative` is new;
  `Transaction::decode_payload` returns `Option<Result<PayloadDecode, PayloadError>>` and no longer fails on trailing
  bytes. This touches any v24 code that builds or checks outputs or decodes payloads.

### 2.7 `dash-primitives`: ProTx types 10 to 12 and the v3 lists (`aff37a2`)

- **What and why.** The new types `ProDisTx`, `ProUpShareTx` and `ProUpSharedRegTx`; the ProRegTx and ProUpRegTx v3
  owner payouts and share lists (`PayoutShare`, `CollateralShare`, `CompactSig`); each BLS field read under the scheme
  Core gives it (`protx_bls_scheme`, `commitment_bls_scheme`) and written back under Core's write-time scheme. Without
  them every ProRegTx v3 fails to decode (gaps 2 to 4 in section 4.1). The CbTx length check through `decode_size`
  is also here, because it shares the BLS field code.
- **Core behaviour.** Core v24's provider payloads. Evidence: the crafted payloads and v24 transactions of 2.8.
- **Status.** Merged on our fork. **Depends on.** 2.3, 2.6. **Units.** `v24_protx`, and `cql_share` for the CodeQL
  policy hunk on the payout-share names (question 5).
- **API.** `SpecialPayload` and `TxType` gain three variants each, so exhaustive matches need arms; `ProRegTx` and
  `ProUpRegTx` change shape at v3. Every BLS field in the ProTx, MnHF, CbTx, asset-unlock and commitment payloads
  changes type, from `BlsPkBytes`/`BlsSigBytes` to the 2.3 wrapper types. `CoinbaseCommitment::height` changes from
  `BlockHeight` to `i32`. `MAX_SPTX_PAYLOAD_SIZE` is removed and `PayloadDecode` exported. `dash-primitives` now
  always enables `dash-pkc`'s `bls` feature, which brings `blst` and `spin` to every user. This is squarely in your
  v24 area: see question 1.

### 2.8 `dash-primitives`: CbTx v4, AssetLock and AssetUnlock v2, and the payload corpora (`f3e8af8`)

- **What and why.** CbTx v4 with `merkleRootAssetUnlocks`; v2 asset-lock credits that may pay P2SH, with
  `bad-assetlocktx-script-pubkey`; AssetUnlock v2. Without them `develop` cannot decode a post-v24 coinbase (gap 1).
- **Core behaviour.** Core v24's `CCbTx`, `CAssetLockPayload` and `CAssetUnlockPayload`. Evidence:
  `pkgs/primitives/corpus/payload_core.json5`, 74 crafted payloads (NetInfo, BLS fields per scheme, CbTx, the old v3
  layout) judged by Core's `decoderawtransaction` on both releases under both BLS flags; and
  `pkgs/primitives/corpus/v24.json5`, 38 special transactions from the regtest chains Core's v24 functional tests build
  and two testnet blocks, each with Core's JSON. The generator is `contrib/vectors/payloads/`.
- **Status.** Merged on our fork. **Depends on.** 2.4, 2.7. **Unit.** `v24_cbtx_assetlock`.
- **API.** `CoinbaseCommitment` gains a field; `AssetLockInvalid::CreditBadScript` is new. Squarely in your v24 area:
  see question 1.

### 2.9 `dash-params`: chain params per network and release (`5b3e49b`)

- **What and why.** Chain params built per network kind (main, test, regtest, devnet) and release by
  `ChainParamsBuilder`, with v24 data (deployments, checkpoints, chain work, assumevalid, LLMQ tables, devnets with a
  derived genesis) and Core's command-line overrides (`-testactivationheight`, `-vbparams`, LLMQ overrides, devnet
  options, spork addresses). `dash-params` reuses `dash_primitives::LlmqType`. The node needs exactly the params Core
  selects, including the regtest and devnet setups Core's functional tests use.
- **Core behaviour.** Core's `CChainParams` and its argument handling. Evidence:
  `pkgs/params/corpus/chainparams_v24_0.json5` and `chainparams_v23_1.json5`, 178 cases each, dumped by Core itself.
- **Status.** Merged on our fork. Reshaped for upstream: no release axis, no v23 data, no Core error texts. The units
  could not be split here without writing new code, so this one commit covers four of them.
- **Depends on.** 2.4. **Units.** `params_v24`, `llmq_params`, `params_devnet` and `params_ovr`.
- **API.** `Network` and its static tables give way to `NetworkKind`, `Release` and an owned `ChainParams`;
  `dash_params::LlmqType` is removed (use `dash_primitives::LlmqType`). The new `ParamsError` has hand-written
  `Display` and `Error` impls. This touches any code that reads chain params.

### 2.10 `dash-params`: the BRR payment table (`c7784d3`)

- **What and why.** The block-reward reallocation table (`BRR_PERIODS_PERMILLE`, Core's `vecPeriods`), which the node
  needs to check masternode payments in that period.
- **Core behaviour.** Core's table. Evidence: `pkgs/params/corpus/brr_payments.json5`, 80 masternode payments Core
  makes around the boundaries of the 19 reallocation periods (40 heights, at each release).
- **Status.** Merged on our fork. **Depends on.** 2.9. **Unit.** `params_brr`.
- **API.** Additive.

### 2.11 `dash-p2p-core`: versions, limits, framing and headers (`f08b0d7`)

- **What and why.** Core's protocol versions and message limits; V1 framing with Core's header checks (magic, then the
  3 MiB length) and payload checks (checksum, then the command bytes), saying whether Core disconnects; the V2
  short-ID send gate (short IDs from common version 70235, 70240 for `platformban`) and V2 typing apart from payloads
  (`decode_v2_payload`); command validity as Core's transports check it; header counts bounded by the payload, with
  Core's limit of 8,000 `headers2` from common version 70235; `headers2` decoded and encoded as Core's `Compress` does
  (`Headers2::reply`, `Headers2::announce`), keeping each header's wire form and hashing each header once; `version`'s
  optional fields read one by one, as Core reads them; unknown commands as `P2pMsg::Unknown`; and the BIP37 messages.
  These sit in one commit because they share the message-decoding trait this commit introduces.
- **Core behaviour.** `net.cpp`'s V1 and V2 transports, `version.h`, and the `headers2` and `version` handlers.
  Evidence: `pkgs/p2p_core/corpus/short_ids.json5`, Core's send map; `core_headers2.json5`, 10 captures from 4 networks;
  57 cases of `decode_domain.json5` (`version`, headers, ignored payloads, V2 contents, V1 commands), including exactly
  what each release sent.
- **Status.** Merged on our fork; the signed-version, bounds and command-validity part is open upstream as #59.
- **Depends on.** 2.1, 2.2. **Units.** `p2p_frame`, `p2p_limits`, `p2p_v1`, `p2p_v2`, `hdr_limits`, `headers2`,
  `version_tail`, `p2p_unknown` and `p2p_bip37`.
- **API.** `ProtocolVersion` is signed; `ProtocolVersion::CURRENT` is removed (use `V24_0`); `encode_v2` takes the
  peer's common version; `P2pDecodeError::Consensus` carries the `DecodeError`; `UnknownCommand` and
  `PayloadNotEmpty` give way to new variants; `P2pDecodeError::PayloadTooLarge`'s `command` is a `CommandString`;
  `Headers2` keeps `CompressedHeader`s with private fields, replacing `CompressionState`; `Version`'s optional fields
  are `Option`s, its relay and masternode-connection flags `Option<CoreBool>`; `P2pMsg::Unknown` is new. The new
  `V1FrameError` has hand-written `Display` and `Error` impls.

### 2.12 `dash-p2p-core`: inventory types (`c5c175c`)

- **What and why.** `InvType` names every inventory type Core knows, with Core's `command`, `is_known`, `is_gen_tx`,
  `is_gen_blk` and request interval; type 34 (`MSG_ASSET_UNLOCK`) is known and carried in a `tx`, as at v24.
- **Core behaviour.** `protocol.h`'s inventory types. Evidence: unit tests per type, and the captured messages of
  2.14.
- **Status.** Merged on our fork. **Depends on.** 2.11. **Unit.** `p2p_inv`.
- **API.** `InvType` gains variants, so exhaustive matches need arms.

### 2.13 `dash-p2p-core`: control messages (`bc92e13`)

- **What and why.** `senddsq` and `qsendrecsigs` carry their flag; `ssc`, `sendtxrcncl`, `getblocks`, `mempool` and
  `qwatch` are typed; `pong` reads its nonce as Core does; the `senddsq` and `qsendrecsigs` flags are `CoreBool`, read
  as Core reads a `bool`; `BaseCodec` for `bool` is removed from `dash-types`. `develop` rejects `senddsq` from every
  Core peer that relays transactions, and `qsendrecsigs` from masternodes on quorum-relay connections (section 4.2).
- **Core behaviour.** `net_processing.cpp`'s handlers. Evidence: the `control` and `pong` cases of
  `decode_domain.json5`, with exactly what each release sent.
- **Status.** Merged on our fork; the control-message part is built upstream and held (`p2p_ctrl`, after #59).
- **Depends on.** 2.11. **Unit.** `p2p_ctrl`.
- **API.** `SendDsq` and `QSendRecSigs` gain a field; `Pong`'s nonce is an `Option`; `BaseCodec for bool` is gone.

### 2.14 `dash-p2p-core`: `block`, `tx`, BIP152 and BIP157 (`8abddb4`)

- **What and why.** Typed `block` and `tx`; `sendcmpct`, `cmpctblock`, `getblocktxn` and `blocktxn` with v24 short-ID
  keys; the BIP157 messages both ways. Each typed message is checked against what Core sent and how its handlers
  judged crafted payloads, and decode allocations are bounded. The corpus test allows `govsync`'s tail, which the
  message does not read yet (section 4.2).
- **Core behaviour.** Core's handlers and serializers. Evidence: `pkgs/p2p_core/corpus/core_messages.json5`, 708
  messages two Core nodes sent each other and 107 compact blocks; `core_verdicts.json5`, 87 crafted payloads and 6
  framing cases with Core's verdict on each (kept, disconnected, or the exception it logged);
  `core_getblocktxn.json5`, 432 cases.
- **Status.** Merged on our fork. **Depends on.** 2.11, 2.13. **Units.** `p2p_bip152`, `p2p_bip157` and
  `p2p_blocktx` (all ask first, question 4).
- **API.** `P2pMsg` gains variants; `SendCmpct::high_bandwidth` is a strict `bool`; `GetBlockTxn` has private fields
  and checks its index order.

### 2.15 `dash-p2p-core`: overlay messages (`9010dc5`)

- **What and why.** Typed `clsig`, `isdlock`, `qsigrec`, `qfcommit` (versions 1 to 4), `mnauth`, `spork`,
  `platformban`, `qgetdata`, `qdata`, `getqrinfo` and `qrinfo` (with quorum snapshots); `mnlistdiff` in each layout
  Core has used (`MnListFormat`), and simplified masternode-list entries with their version and NetInfo; the hashes
  Core keys these objects by; Core's handler limits (`HandlerPayloadTooLarge`, and `TooManyEntries` for `qdata`). BLS
  keys and signatures are bytes, except a commitment's, which decode through the 2.3 wrappers. The flags here
  (`SimplifiedMnListEntry::is_valid`, the `qrinfo` and `getqrinfo` extra-share flags) are strict: a byte other than 0
  or 1 fails with `DecodeError::NonCanonicalBool`. `dash-types` also gains `BaseCodec` for `Box<T>`. These are the
  messages a node needs for ChainLocks, InstantSend, quorums and masternode lists.
- **Core behaviour.** Core's serializers and handler checks. Evidence: `pkgs/p2p_core/corpus/core_overlay.json5`, 433
  crafted payloads read by a test judge linked against a Core v24.0.0-rc.3 build (test-only additions), each with
  Core's outcome (bytes read, write-back, the object's hash, or the exception); the `mnlistdiff` fixture.
- **Status.** Merged on our fork. **Depends on.** 2.5, 2.7, 2.11, 2.13. **Unit.** `overlay` (new; issue first,
  question 10).
- **API.** `MnListDiff` holds its fields directly, replacing `MnListDiffPayload`, and records its format;
  `SimplifiedMnListEntry` carries `version` and `NetInfo` instead of `service`, and its hash is Core's `CalcHash()`;
  `QuorumClSig::index_set` is a `BTreeSet<u16>`; `P2pMsg::QfCommit` and `P2pMsg::QrInfo` are boxed; `DecodeError`
  gains `NonCanonicalBool`.

## 3. In review on our fork (not on this branch)

**Special-payload signature hashes and the AssetUnlock v2 stable txid.** `dash-primitives`, about +1,270/−80 over six
commits.

- What: the message each special payload's signature covers (`sig_hash_bytes()` per payload, as Core's
  `CheckProRegTx`, `CheckProUpServTx` and the other checks build it), refusing versions Core refuses; and the txid rule
  for v2 asset unlocks that Core adds at v24, so `Transaction::hash()` and `Block::merkle()` match Core for blocks that
  carry them. This is gap 5 of section 4.1: without it, `Block::check()` rejects valid v24 blocks with
  `bad-merkle-root`.
- Evidence: a generator in `contrib/vectors/sighash/` and tests that verify Core's own payload signatures over the
  hashes; a differential campaign against Core reached its coverage plateau with no divergences.
- Status: reviewed; one finding is open, and it waits for a fuzzing-infrastructure fix on our side before the last
  review round. Estimate: it joins this branch in the first re-push after it merges on our fork.
- Depends on: the v24 payload commits here. Unit: `v24_sighash` (new, section 5). API: adds methods; nothing breaks.

**Decode-domain evidence.** No base-sdk code today.

- What: a differential campaign that decodes transactions, blocks, headers and every special payload (types 1 to 12,
  every version, both BLS flags) with base-sdk and with Core, and compares verdicts.
- Status: in review. If it finds a divergence, the fix lands here as its own commit.

## 4. Conformance findings

These are carried over from the preview, checked again against `d9e0d24`. Your commits since the preview (section 6)
do not change them.

We decoded the first ~650,000 blocks of mainnet and of testnet, and the regtest chains Core's own v24 functional tests
build, with `develop` and with Core side by side. Every block checked before v24 agreed. Every failure was on a v24
format.

### 4.1 Seven v24 gaps in `develop`

1. **CbTx v4 is not supported.** `decode_payload()` fails with "trailing bytes: 32 remaining", and
   `CoinbaseCommitment::check()` rejects version 4. Fixed here (`v24_cbtx_assetlock`).
2. **Types 10, 11 and 12 have no decoder.** They decode as `SpecialPayload::Unknown` and `Transaction::check()` reports
   `bad-txns-type`. Fixed here (`v24_protx`).
3. **Every ProRegTx v3 fails to decode**, including a plain ExtAddr registration with one payout and no shares.
   `develop` models v3 as v2 plus ExtAddr NetInfo, the layout before the payout and share lists. Fixed here
   (`v24_protx`).
4. **ProUpRegTx v3 fails to decode** (payout lists). Fixed here (`v24_protx`).
5. **The AssetUnlock v2 stable-txid rule is missing**, so `Transaction::hash()` is wrong for v2 unlocks,
   `Block::merkle()` does not match the header, and `Block::check()` rejects the valid block with `bad-merkle-root`.
   In review (section 3).
6. **`Block::check()` rejects every valid post-v24 block**: at the coinbase (gap 1) or earlier (gap 5). Gap 1 is fixed
   here; the rest follows with gap 5.
7. **AssetLock and AssetUnlock v2 fail their checks.** `AssetLock::check()` and `AssetUnlock::check()` reject version
   2. Fixed here (`v24_cbtx_assetlock`).

Repro: the transactions are in `pkgs/primitives/corpus/v24.json5`, each with its source chain and height. On `develop`,
decode the CbTx v4, type 10 to 12, ProRegTx v3, ProUpRegTx v3 and asset lock and unlock v2 entries with
`Transaction::decode` and `decode_payload`, then run their `check()`. To rebuild the chains, run Core v24.0.0-rc.3's
`feature_asset_locks.py`, `feature_masternode_payout_shares.py`, `feature_masternode_shares.py` and
`feature_protx_version.py` with `--nocleanup` and save `getblock <hash> 2` for each block;
`contrib/vectors/payloads/gen.py v24` turns those into the corpus.

### 4.2 Four strict-decoder bugs on honest traffic

These reject messages that honest Core peers or wallets send. Each is a wrong message definition, and each planned
upstream fix stays strict. This branch reads the `senddsq`, `qsendrecsigs` and `version` flags as `CoreBool` (any
non-zero byte is `true`, as Core reads a `bool`); the rebuilt `p2p_ctrl` and `version_tail` units read them
strictly (0 or 1) unless you prefer Core's reading.

1. **`senddsq` and `qsendrecsigs` carry one flag byte.** `develop` expects an empty payload, so it rejects `senddsq`
   from every Core peer that relays transactions, and `qsendrecsigs` from masternodes on quorum-relay connections.
   Fixed here (`p2p_ctrl`).
2. **The `version` tail.** Core reads the optional trailing fields one by one and accepts a message that stops between
   them. SPV wallets send such a message, and `develop` rejects all six cuts Core accepts. Fixed here (`version_tail`).
3. **The headers2 cap.** `develop` caps `headers2` at 2000 headers; Core sends up to 8000 when the common version is at
   least 70235. Fixed here (`hdr_limits`).
4. **The `govsync` tail.** Core's own `govsync` carries a whole bloom filter after the hash; `develop` reads only the
   filter's data and rejects its last 9 bytes as trailing. Not fixed on this branch: only the corpus test allows the
   tail. The complete message is planned (`govsync`).

## 5. Pull-request series

Units are rebuilt on `develop` in your shapes; this branch's commits are a source, and most map to one unit. Sizes are
estimates for the rebuilt unit. "After" means the unit is cut from that unit's branch. "On branch" names the commit in
section 1 that carries the unit's content today. The branch's commit order differs from the unit order (the
`bls_wire` content comes first, and `v24_protx` before `v24_cbtx_assetlock`) because each commit must build and pass
on the ones before it.

**Tier 1: no leniency.**

| Branch | Title | After | Size | On branch | Status |
|---|---|---|---|---|---|
| `codec_size` | `types%fix`: range-check length prefixes, reserve vector batches exactly | | ~+90/−5 | 1 (`6d16114`) | open, #57 |
| `llmq_types` | `primitives%feat!`: name LLMQ types 103 and 105, reuse `LlmqType` in `dash-params` | | ~+55/−35 | 4 (`ad3b48a`) | open, #58 |
| `p2p_frame` | `p2p_core%fix!`: signed `ProtocolVersion`, command and V2 bounds, command validity | | ~+300/−60 | 11 (`f08b0d7`) | open, #59 |
| `tx_check` | `primitives%fix`: size before payload size; lengths through `decode_size` | `codec_size` | ~+150/−40 | 6 (`b99abb4`) | built, held |
| `p2p_ctrl` | `p2p_core%fix!`: `senddsq`/`qsendrecsigs` flags; `ssc`, `sendtxrcncl`, `getblocks` | `p2p_frame` | ~+150/−20 | 13 (`bc92e13`) | built, held |
| `hdr_limits` | `p2p_core%fix!`: header counts bounded by the payload; Core's header limits | `p2p_frame` | ~+120/−40 | 11 (`f08b0d7`) | planned |
| `params_v24` | `params%feat!`: v24 chain params | | ~+450/−250 + fixture | 9 (`5b3e49b`) | planned |
| `llmq_params` | `params%feat`: LLMQ parameters and per-network quorum sets | `llmq_types`, `params_v24` | ~+350 | 9 (`5b3e49b`) | planned |
| `params_devnet` | `params%feat!`: devnet with a derived genesis, static path kept | `llmq_params` | ~+300/−100 | 9 (`5b3e49b`) | planned |
| `params_ovr` | `params%feat`: typed regtest and devnet overrides | `params_devnet` | ~+450/−50 | 9 (`5b3e49b`) | planned |
| `v24_cbtx_assetlock` | `primitives%feat!`: CbTx v4, AssetLock and AssetUnlock v2 | `llmq_types`, `tx_check` | ~+300/−60 | 8 (`f3e8af8`) | planned |
| `cql_share` | `sdk%lint(codeql)`: payout-share types and the secret-name rule | | tiny | 7 (`aff37a2`) | needs your call |
| `v24_protx` | `primitives%feat!`: types 10 to 12, v3 payout and share lists | `v24_cbtx_assetlock`, `cql_share` | ~+800/−120 | 7 (`aff37a2`) | planned |
| `v24_sighash` | `primitives%feat`: special-payload sighashes, AssetUnlock v2 stable txid | `v24_protx` | ~+1,000/−80 | not yet (section 3) | in review |
| `netinfo_strict` | `primitives%fix`: 253-byte domains, empty NetInfo as Core reports it | `tx_check` | ~+100/−60 | 5 (`e645b5a`) | planned |
| `p2p_limits` | `p2p_core%feat`: Core's protocol versions and message limits | `p2p_frame` | ~+110 | 11 (`f08b0d7`) | planned |
| `p2p_v1` | `p2p_core%feat`: V1 framing with Core's checks | `p2p_limits` | ~+200 | 11 (`f08b0d7`) | planned |
| `p2p_v2` | `p2p_core%refac!`: V2 typing apart from payloads; short-ID send gate | `p2p_limits` | ~+400/−100 | 11 (`f08b0d7`) | planned |
| `p2p_inv` | `p2p_core%feat`: every inventory type Core knows | `p2p_limits` | ~+90 | 12 (`c5c175c`) | planned |
| `headers2` | `p2p_core%fix!`: headers2 as Core encodes it, strict decode, one hash per header | `hdr_limits`, `p2p_limits` | ~+470/−260 | 11 (`f08b0d7`) | planned |

**Tier 2: a strict reshape of an interoperability rule.**

| Branch | Title | After | Size | On branch | Status |
|---|---|---|---|---|---|
| `version_tail` | `p2p_core%fix!`: `version`'s optional fields as one nested tail | `p2p_frame`, `codec_size` | ~+250/−60 | 11 (`f08b0d7`) | planned |
| `p2p_unknown` | `p2p_core%feat!`: unknown commands as `P2pMsg::Unknown` (optional) | `p2p_frame` | ~+60/−30 | 11 (`f08b0d7`) | planned |

**Tier 3: needs your answer or an issue first.**

| Branch | Title | After | Size | On branch | Status |
|---|---|---|---|---|---|
| `p2p_bip37` | `p2p_core%feat`: BIP37 messages | `p2p_limits` | ~+130 | 11 (`f08b0d7`) | ask first (#40) |
| `p2p_bip152` | `p2p_core%feat!`: BIP152 messages, v24 short-ID keys | `codec_size`, `p2p_limits` | ~+420 | 14 (`8abddb4`) | ask first (#40) |
| `p2p_bip157` | `p2p_core%feat`: BIP157 messages both ways | `p2p_limits` | ~+125 | 14 (`8abddb4`) | ask first (#40) |
| `p2p_blocktx` | `p2p_core%feat`: `block` and `tx` messages | `p2p_limits` | ~+60 | 14 (`8abddb4`) | ask first (#40) |
| `bls_wire` | `pkc%feat(bls)`: run-time schemes, a Core wire-form classifier | `v24_protx` | ~+400/−20 | 3 (`8b1ef2d`) | issue first |
| `netinfo_core` | `primitives%fix!`: NetInfo read as Core reads it, wire form kept | `netinfo_strict` | ~+300/−100 | 5 (`e645b5a`) | issue first |
| `tx_amount` | `primitives%fix!`: out-of-range output values decoded, reported by `check()` | `tx_check` | ~+150/−60 | 6 (`b99abb4`) | only if you want Core's reasons |
| `govsync` | `p2p_core%fix`: `govsync`'s whole bloom filter | `p2p_frame` | small | not yet | not started |
| `params_brr` | `params%feat`: the BRR payment table | `params_v24` | ~+80 | 10 (`c7784d3`) | planned |
| `overlay` | `p2p_core%feat!`: overlay messages (ChainLocks, InstantSend, quorums, MN lists) | `p2p_v2`, `bls_wire` | ~+2,000 + fixture | 15 (`9010dc5`) | issue first |

If you prefer fewer, larger pull requests, `codec_size` + `tx_check`, the p2p framing units, and `params_v24` +
`llmq_params` merge naturally. We will not open more until your v24 series lands, and #57 to #59 stay open for review.

### 5.1 Planned on our fork

These are not written yet. Timing comes from our project model, counted from 2026-10-08 with no allowance for work
already done; it is an estimate, not a commitment.

| Work | Crate | Depends on | Estimate | Becomes |
|---|---|---|---|---|
| Overlay messages II: the remaining LLMQ messages, including signature shares | `dash-p2p-core` | `overlay` | early January 2027 | an `overlay` follow-up |
| Overlay messages III: CoinJoin (with `dsq`'s hash and sighash) and governance (with the full `govsync`) | `dash-p2p-core` | `overlay` | early January 2027 | `govsync`, an `overlay` follow-up |
| All 74 commands typed, with an `Unknown` passthrough; a capture corpus from every peer-version bucket round-trips byte for byte | `dash-p2p-core` | the two above | late January 2027 | completes `p2p_unknown` and the overlay units |
| The legacy BLS scheme's decode rule completed and checked against Core's BLS library for every input class | `dash-pkc` | `bls_wire` content | late November to mid December 2026 | `bls_wire` |
| `dash-pkc` API completion: the remaining BLS operations Core's LLMQ code uses | `dash-pkc` | the item above | end of December 2026 | new unit, after `bls_wire` |
| A BLS vector corpus with provenance for those operations, and a serial BLS verify with no thread pool | `dash-pkc` tests | none here | mid December 2026 | new unit |
| Performance helpers: `encoded_len`, streaming SHA-256d, decoding with byte ranges | `dash-types`, `dash-primitives` | `codec_size` content | late November 2026 | new unit (no behaviour change) |

Two more pieces of work use base-sdk from our side and need nothing new upstream. One is a bridge from our existing
Rust Dash library's types to base-sdk's (about August to October 2027). The other moves that library onto base-sdk in
four steps: segwit and taproot removal; ECDSA through `dash-pkc`; network, LLMQ and payload types; then hash newtypes,
transactions, blocks, scripts and the SPV client's messages. The first three steps start from about June 2027 and the
last after October 2027. If they turn up a gap in base-sdk, it comes to you as a unit like the ones above.

## 6. Overlaps with your commits from 2026-10-12

Your series (#60, `d9e0d24`) landed after the preview, so this branch was rebuilt on it.

- **`thiserror` error enums** (`types`, `pkc`, `primitives`, `p2p_core`). Our new variants (`DecodeError::MalleableBls`
  and `NonCanonicalBool`, `TxInvalid::OutputNegative`, `AssetLockInvalid::CreditBadScript`, the new
  `P2pDecodeError` variants) use `#[error]` with the texts our hand-written `Display` impls had, which are gone. Three
  new error types (`V1FrameError`, `ParamsError`, `BlsDecodeError`) keep hand-written impls for now; they convert when
  their units are rebuilt.
  `P2pDecodeError::Consensus` now carries the `DecodeError` instead of its text, so it uses `#[error("{0}")]`;
  `UnknownCommand`, its formatter and `PayloadNotEmpty` are removed (section 2, `p2p_frame`).
- **`TxInvalid` value fields.** Your series keeps `OutputTooLarge { value: u64 }` and `OutputTotalTooLarge
  { total: u64 }`; ours makes them `i64`, Core's signed `CAmount`, because a decoded output value can be negative.
- **`serde` decoupled from `codec`.** No conflict; our new types follow your feature split.
- **CHANGELOGs.** Your MSRV and `portable` lines and ours sit side by side in `types` and `pkc`.
- **MSRV 1.89, `portable`, `cargo hack`, `nextest` config.** Every commit here passes the feature powerset and the moved
  nextest config.
- **`pow%build: modify aes_hw to imply simd`.** Untouched here, but see the `dash-pow` note at the top: the x86_64
  clippy failure is on `develop` itself.

Beyond the error-enum conversion above, nothing else overlaps: your series does not touch the payload, params or
message code these commits change.

## 7. Questions for you first

These decide the Tier 3 units and the shape of several Tier 1 units. Nothing in those units will be opened before you
answer.

1. **Which v24 pieces are you doing yourself, and which would you like from us?** Section 4.1 lists what `develop`
   needs. If you are writing CbTx v4, the asset-lock v2 payloads or types 10 to 12 yourself, we will diff against
   yours rather than open ours.
2. **BLS: one Core wire-form classifier in `dash-pkc`?** Core reads BLS bytes by rules that differ from the IETF codec:
   the legacy scheme's decode rule follows Core's, bytes canonical only in the other scheme become an invalid object,
   and bytes canonical in neither are a decode error. We propose one named, pure function that classifies wire bytes
   as Core reads them, with payload types keeping byte-bag fields. Is a scheme-neutral byte bag acceptable for
   version-dependent BLS fields? `BlsPkBytes<BlsScIetf>` mislabels a version-1 key. This decides `bls_wire` and the
   overlay unit.
3. **NetInfo with a Core view?** Core reads NetInfo lossily (first group per purpose wins, unknown or invalid entries
   kept as markers, a failed service read caught), so a strict decoder fails to decode some ProTx transactions Core
   accepts in blocks. We propose that the ProTx decoders keep both the wire form and Core's view. Yes, no, or
   something else? This decides `netinfo_core`.
4. **BIP37, BIP152 and BIP157, and `block`/`tx`: should we type them here, or wait for `bitcoin-p2p-messages` (#40)?**
   This decides `p2p_bip37`, `p2p_bip152`, `p2p_bip157` and `p2p_blocktx`.
5. **The payout-share types and the CodeQL secret-name rule: rename the types, or amend the rule?** It is your policy
   (`cql_share`).
6. **Out-of-range output values:** do you want `check()` to report Core's exact reason (`bad-txns-vout-negative`), which
   needs the decoder to accept the value first (`tx_amount`), or is a decode error enough?
7. **Fixtures:** generate them from v24.0.0-rc.3 now and regenerate at the final tag, or wait for the tag?
8. **Pull-request size:** the small units of section 5, or the merged groups?
9. **Protocol constants:** `MIN_MASTERNODE_PROTO_VERSION` (70242) upstream, and drop `ProtocolVersion::CURRENT` rather
   than move it?
10. **The overlay messages:** typed in `dash-p2p-core` with their BLS fields as bytes (as here), or kept outside
    base-sdk until the classifier question (2) is settled?

## Agent disclosure

The code, vectors and this document were generated with Claude Code using Claude Opus 5.5 (medium to extra-high
effort) and Claude Sonnet 5.5 (high effort), directed by pasta. They were reviewed with Claude Code using Claude Opus
5.5 (high effort) and with Codex using GPT-6.1-Sol and GPT-6-Sol (high effort). The commits carry no agent trailer,
per this repository's commit lint.
