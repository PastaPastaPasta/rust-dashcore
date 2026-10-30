# BS2: differential conformance results (dashcore vs base-sdk vs dashd)

**Date:** 2026-10-08
**Harness:** `conformance/` (crate `dash-conformance`, branch `node/bs2-conformance-harness`). See
its README for the checks, CLI and output formats.
**Libraries under test:**
- rust-dashcore `dashcore` at `node/integration` 83247e70 (= `dev` 67b307c6), built with
  `core-block-hash-use-x11`.
- base-sdk `dash_primitives` at develop `33fcc59` (pinned for the harness crate only).

**Oracle:** Dash Core v24.0.0-rc.3 (`docs/node-program/ORACLE.md`).

**Fix round 1 (after the independent review `reviews/BS2-r1-gpt.md`).** The harness changed
substantially (§5, items 12 and 13): the gate now covers every unit of the requested range,
checkpoints are bound to the code, oracle, policy and chain, and the `getblock 2` comparison
now classifies every field dashd prints. The coverage numbers in §1 and §2 were produced
**before** that round, by a harness whose `getblock 2` comparison skipped addresses,
endpoints, payouts and shares. The refresh timer re-checks both chains under the new harness;
until a pass completes, the old results show up as `unverified` in each `summary.json`.

**Update after V24a (branch `node/conformance-v24-payloads`).** dashcore now decodes every v24
format, and the harness compares it with dashd: the v24 payload objects of types 10–12
(`proDisTx`, `proUpShareTx`, `proUpSharedRegTx`), ExtAddr netinfo, DIP-0026 payouts, shares and
early-dissolution terms, and CbTx v4's `merkleRootAssetUnlocks`. On the v24 regtest fixtures,
**RDC-2 to RDC-8 are fixed**: dashcore matches dashd on every block, txid and `getblock 2` field
(`tests/v24_findings.rs`), and its rendering of every transaction in dashcore's own
Core-produced v24 vectors matches Core's JSON (`verbose::tests`). Every remaining v24 finding
is a base-sdk gap (BSDK-1 to BSDK-6). Types 10–12 have no library-vs-library payload
comparison, because base-sdk has no decoder for them (BSDK-2). A payload dashcore keeps as
raw bytes (`UnknownPayloadType`: no decoder for the type, or a payload its decoder does not
consume exactly, which Core's `GetTxPayload` refuses too) is now `payload.unsupported`
against dashcore, as for base-sdk. Core prints no payload object for such a payload, and
the harness emits none for it either. The findings below are kept as found by BS2.

## TL;DR

- **Every block checked so far decodes identically in both libraries and in dashd** (by the
  pre-fix-round harness; see the note above):
  - mainnet heights 0–648,307 (to 2017-04);
  - testnet heights 0–642,846 (to 2022-01; includes DIP3 ProTx, CbTx and QcTx history).

  Block hashes, txids, merkle roots, header fields and payload fields all agree. dashcore's
  header proof-of-work check, and base-sdk's much broader `Block::check()` (proof of work,
  merkle root, coinbase and transaction rules, payload checks), accept every best-chain block.
  There are **0 library mismatches** on either network.
- **The full chains are not done yet.** The oracles are still syncing on a saturated disk (§1).
  A systemd timer keeps extending the runs every 2 hours until the chains are complete, so the
  numbers below are a snapshot (2026-10-08 06:19 UTC). Live numbers:
  `/work/conformance/<net>-<mode>/summary.json` and `/work/conformance/passes.log`.
- **Every failure found is on Dash Core v24 formats.** Testnet has not activated v24 (its BIP9
  window opened 2026-09-24), and mainnet has no v24 deployment yet. The v24 cases were
  therefore exercised on regtest chains produced by Core rc.3's own functional tests. On those
  chains, **both libraries fail on every post-v24 block**: 8 rust-dashcore bugs and 6 base-sdk
  bugs (§3).
- **Predictions confirmed:**
  - R1 "special-tx length prefix ignored": a CbTx v4 misaligns dashcore's whole block.
  - R1 "BlockHash is sha256d by default".
  - R3 "base-sdk v24 gaps": CbTx v4, types 10–12, DIP-26 ProTx v3, AssetUnlock v2 txid.
- **Most serious finding (RDC-5):** dashcore *silently* misdecodes a DIP-26 ProRegTx v3. It
  returns wrong field values and re-encodes byte-identically, so only the oracle cross-check
  catches it.
- **The harness had bugs of its own.** The review rounds and the runs found false negatives,
  attribution and accounting bugs, including a resumed gate that could turn a recorded failure
  into success, and one that could pass on results from two chains after a reorg. All found so
  far are fixed and covered by regression tests (§5).

## 1. Coverage

Snapshot at 2026-10-08 06:19 UTC.

| Network / source | Heights | Blocks | Txs | Special txs | `getblock 2` blocks | Library mismatches |
|---|---|---|---|---|---|---|
| mainnet, RPC | 0 – 648,307 | 648,308 | 2,600,262 | 0 (DIP3 is at 1,028,160) | 649 | 0 |
| mainnet, blk files | 0 – 648,881 | 648,882 | 2,605,738 | 0 | n/a | 0 |
| testnet, RPC | 0 – 642,058 | 642,059 | 5,342,277 | 779,884 (ProReg 2,337, ProUpServ 4,274, ProUpReg 830, ProUpRev 115, CbTx 635,059, QcTx 137,269) | 3,093 | 0 |
| testnet, blk files | 0 – 642,846 | 642,847 | 5,343,345 | 780,877 | n/a | 0 |
| regtest v24 chains (5), RPC with `getblock 2` on every block | all | 3,797 | 10,361 | 10,076 | 3,797 | see §3 |

How each mode checks blocks:
- **RPC mode:** every block is compared with dashd's `getblock <hash> 1`, which gives the hash,
  txids, merkle root and header fields.
- **blk-file mode:** every best-chain block is compared with its child's raw `prev_hash`, and
  the tip is compared with the oracle's best hash.
- **`getblock 2` cross-check (RPC mode only):** for every block containing a ProTx, MnHF,
  AssetLock/Unlock or type 10–12 special tx, plus every 1000th block. It runs in both
  directions.
- The blk-mode runs also check stale blocks (`--include-stale`). None have been stored so far
  in the synced range.

**Not covered yet:**
- testnet 642,847 – tip (~1,568,400). This range includes v19 BasicBLS/Evo, v20, mn_rr and
  withdrawals: AssetLock/Unlock, MnHF, CbTx v3 and QcTx v3/v4 on a real chain.
- mainnet 648,882 – tip (~2,551,700). This range includes all DIP3+ special transactions.

The same formats appear on the v24 regtest chains, where both libraries agree with dashd on
everything except the v24 changes.

**Oracle sync at the snapshot:**
- mainnet: block 650,974 of 2,551,717 headers. `verificationprogress` is 0.037 (it is
  weighted by tx count).
- testnet: block 647,722 of 1,568,398 (0.42).

**Sync rate and ETA:**
- Current rates are about 1.3K blocks/min on mainnet and 1.7K/min on testnet.
- The `/work` disk is at about 96% utilization from other workloads, including B1's timed IBD
  benchmark.
- ETA: testnet about 9–14 h, mainnet about 24–36 h, assuming blocks get heavier toward the
  tip.

**Transient `blk.orphan` records.** blk-mode summaries currently also show 4 (testnet) and 5
(mainnet) of these. They are blocks downloaded ahead of their parents while the oracle syncs.
Each pass re-indexes and replaces them. Once synced, any that remain are real.

## 2. Mismatch counts by class

| Class | mainnet (0–648K) | testnet (0–643K) | regtest v24 |
|---|---|---|---|
| rust-dashcore bug | 0 | 0 | 8 distinct bugs (RDC-2…RDC-8; RDC-1 is static): 2,082 `block.decode` + 2,122 `tx.decode` + 4 `verbose` records |
| base-sdk bug | 0 | 0 | 6 distinct bugs (BSDK-1…BSDK-6): 2,113 `payload.decode`/`payload.unsupported` + 2,078 `block.check` + 5 `block.merkle_root` records |
| both | 0 | 0 | 5 `tx.txid` (RDC-8 = BSDK-5) |
| harness bug | 0 open (all fixed, §5) | 0 open | 0 open |
| informational / transient | 5 transient orphans (blk) | 4 transient orphans (blk) | none: the 5 v2 asset unlocks were informational `tx.txid_rule` records before fix round 1; the txid reference now implements Core's stable-txid rule |

## 3. Findings

Repros are given as (chain, height, txid). Each block, with dashd's `getblock 1` and
`getblock 2` views, is in `conformance/tests/data/regtest-<chain>-<height>.json`. The
`tests/v24_findings.rs` snapshot pins every finding below. To regenerate the chains, run
`conformance/scripts/regtest-v24.sh ~/workspace/dash v24.0.0-rc.3 /work/chains/bin/current/bin <dir>`.
The test's chains are deterministic.

### rust-dashcore bugs

**RDC-1. `BlockHash` is sha256d unless the non-default feature `core-block-hash-use-x11` is on.**
- This is a static finding; the harness enables the feature.
- Minimal repro: a crate that depends on `dashcore` with default features reports
  `genesis_block(Mainnet).block_hash()` = `089fc444b06edd0f70d9fda85f9a3b2e22e549b354a1bdb210ce7804c69eb0a4`.
  The correct mainnet genesis hash is `00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6`.
- Fix: make X11 unconditional.

**RDC-2. The special-tx `vExtraPayload` length prefix is read and ignored**
(`special_transaction/mod.rs`, `TransactionType::consensus_decode`).
- dashcore parses the payload by its own idea of the format, so any payload longer than
  expected leaves bytes behind.
- For a standalone tx, the result is `N trailing bytes not consumed`.
- Inside a block, the leftover bytes are read as the start of the next transaction. The whole
  block then fails with `unsupported segwit version`, `unknown special transaction type` or
  `trailing bytes`, depending on the leftover bytes.
- One new payload version therefore makes **every** block it appears in undecodable. In the
  regtest data that is every post-v24 block: 2,082 `block.decode` failures across the 5
  chains, plus the `rpc_netinfo` blocks with v3 ProTxs.
- Fix: read exactly `len` bytes and decode the payload from that slice. Reject or keep raw any
  trailing bytes, as Core's `GetTxPayload` does.

**RDC-3. CbTx v4 is unsupported.** v4 adds `merkleRootAssetUnlocks`, which dashcore never reads.
- Combined with RDC-2, this makes every v24 block undecodable.
- Repro: `feature_asset_locks` h=750, coinbase `f505b861432bf770603c9f0302969023d407c46583eb3b4904262129940ed0f3`
  (338 bytes). Payload: `cf` (len 207) `0400` (v4) `ee020000` (h=750) …
  ```
  03000500010000000000000000000000000000000000000000000000000000000000000000ffffffff016affffffff0398958a05000000001976a914d1e414e99c4cf3160d099aeb70438a3ca92dad1a88ac374e3b0600000000016a8e72640a000000001976a9140dfad9a7b1bc5816a8a3211a158fd059f6b9651988ac00000000cf0400ee020000d116eebdcb8052370593c9b6fc396661b059dbe41c16bdd379a14ca545489b1a0b75f2526ac94900e91fe3cbd909c3a978ce40f98dcebaf77ca95f03dea155a400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ba0b6be2a60000000000000000000000000000000000000000000000000000000000000000000000
  ```
- Open PR: #1137.

**RDC-4. Special tx types 10/11/12 (ProDisTx, ProUpShareTx, ProUpSharedRegTx) fail with
`unknown special transaction type`**, and the whole block is rejected.
- Repros:
  - type 10 at `feature_masternode_shares` h=327, tx 5, `b2b25be3ed86f1d03a8467cbcbf45828ecad82991e18b73d987862c29179cf69` (287 bytes):
    `03000a00019784a3b4296f6361217d65e496163bcbb7c9699046a917b5074c8ed536c073750100000000ffffffff0200743ba40b0000001976a91458974c609a635de43fd1efe763fd38d7b762427188ac60ed39a40b0000001976a9141911a6ec416466ca71a31d079e8f31faab249ad888ac00000000a701009784a3b4296f6361217d65e496163bcbb7c9699046a917b5074c8ed536c073750000021f4bd2d22c1249eb5b56128cbf14f33c2afd6fa6e99b53c76c417f477c68c291e5534db765d23ebb55a41d454832d2528b91250a99d3d94ae08f39294f5cbc61a41f8740c069f6ae9205803ee0e7915c670b344efff4c352f214d7203a753e314b4c61dc16aa59fd2d5e1e4ec08850e5e4f246a2f619d3e2c42b6c89183d005ee9a6`
  - type 11 at h=330, tx 6, `6a16a908d2e87800c5be8af85becb55e29a4dbf9b071a9e3ac4f36e7b1ac2945`
  - type 12 at h=335, tx 1, `299ea6bc6924f384d969e6bb0356c3f1725ce6338166d3b7d9faf72ee6543bce`
- Open PRs: #1131, #1136 (unknown types as raw).

**RDC-5. ProRegTx v3 is decoded with the v1/v2 layout.** In Core rc.3, every v3 ProRegTx
carries ExtAddr NetInfo. It also replaces `scriptPayout` with a u8-counted payout list,
u8-counted shares, and one join signature per share (`src/evo/providertx.h`, `CProRegTx`
`SERIALIZE_METHODS`).
- **Silent case:** `feature_masternode_shares` h=273, tx 1, `45574307d12b8ebd87493f6844e7732a718202716bd9b8765d4b481557989ee4`
  (936 bytes).
  - dashcore decodes the tx without error and re-encodes it byte-identically. The wrong
    offsets happen to end at the payload boundary, and the leftover bytes land in `vchSig`.
  - The field values are wrong:
    - `operatorReward` = 18264 (dashd: 0);
    - `pubKeyOperator` shifted by 5 bytes (dashd: `83d7356e…`, dashcore: `c7bf71cd…`);
    - `earlyPeriodBlocks` and `earlyPenalty` are missing.
  - Round-trip tests cannot catch this; only the oracle cross-check does.
- **Loud case:** `feature_masternode_payout_shares` h=121, tx 1, `5bb21f874f50967df6faf711521c13d0723991d8d1e8c00669c9f49eea248a01`
  (507 bytes) fails with `IO error`.
  - dashcore reads past the payload into the next transaction.
  - The error carries no context.
- Open PR: #1132.

**RDC-6. ProUpServTx v3 (ExtAddr) is rejected: `unsupported ProUpServTx version`.** base-sdk
decodes it correctly.
- Repro: `feature_masternode_payout_shares` h=125, tx 2, `ae738cc6547cf27fab740924adb65af5b71496c77cd4dd76c078a0300c562085`.
- Open PR: #1138.

**RDC-7. ProUpRegTx v3 (DIP-26 payouts) fails to decode.**
- Repros:
  - `feature_masternode_payout_shares` h=124, tx 2, `16298ea559e62b310fd293cf85af916dd333cea7d0dfc073ccfd14e64eba48e0`
    (`174 trailing bytes`);
  - `feature_protx_version` h=384, tx 1, `11394a49704f57a63ba119742074ce43e4fcb13d97eece242de7be94c263aad5`
    (`IO error`).
- Open PR: #1138.

**RDC-8. The v24 AssetUnlock v2 "stable txid" rule is not implemented.**
- dashcore's `txid()` is sha256d of the full serialization. dashd's txid for a v2 asset unlock
  is not.
- Repro: `feature_asset_locks` h=969, tx 1. dashd's txid is
  `ed6d78c04f2e9ba2f1c81536cde6885725448ae7083b7580b21031cf565590b8`; sha256d of the bytes is
  `9471462b6675e0f770c0844aeffc323b22a7b49a98a344def73026d2c55af7ec` (200 bytes):
  `03000900000190cff4050000000023210208757bbcd988510d37a86a8c703f9b57543fa343bcf5bd583d444f213b08df03ac000000009102210300000000000070110100c80300005eefda1aa736089dea8f24bc123625405d35aac52a27d4806aff193cf881055895c2eb3e11355fcbd86e5494ceb2d207e5e47d95a9da9b2d3d968c321935993773cd9cea6c831b43f2a09f92944fc62f11d29adb607774915e7dd26c01ecf912e9cd0e793ab66b5fb61145cd80e9830822d904d375562a705f35e65d9ceed33d`
- The same thing happens in all 5 v2 unlocks (heights 969–976).
- Open PR: #1134.

**RDC-9 (minor, API).**
- Decode errors from a short read surface as a bare `IO error`, with no field or offset.
- `QuorumCommitmentPayload` keeps `version`, `height` and `finalization_commitment` private,
  so a consumer cannot read the commitment without re-serializing it.

### base-sdk bugs

**BSDK-1. CbTx v4 is unsupported.** `decode_payload()` fails with `trailing bytes: 32 remaining`.
- `CoinbaseCommitment::check()` also rejects `version >= 4`.
- Unlike dashcore, base-sdk keeps the payload as length-delimited bytes, so the block and every
  other transaction still decode.
- Repro: same as RDC-3.

**BSDK-2. Types 10/11/12 have no decoder.** They decode as `SpecialPayload::Unknown`, and
`Transaction::check()` reports `bad-txns-type`.
- Repro: same as RDC-4.

**BSDK-3. Every ProRegTx v3 fails to decode,** including ones without shares.
- Failures:
  - `compact size value 136 exceeds limit` at payout-shares h=121 tx 1;
  - `trailing bytes: 22 remaining` at masternode-shares h=273 tx 1;
  - at `rpc_netinfo` h=261, `078c37e8e6ca903805aff192f1f34c03cbff84a4074e158b5a3c46fa546857e9`, a
    plain ExtAddr registration with a single payout and no shares.
- base-sdk models v3 as v2 plus ExtAddr NetInfo, which is the pre-rc layout. Its v3 vectors are
  synthetic (R3).
- dashcore fails the same txs (RDC-5, `IO error` at `rpc_netinfo` h=261).

**BSDK-4. ProUpRegTx v3 (DIP-26 payouts) fails to decode.**
- Failures: `trailing bytes: 174` at h=124, and `compact size … exceeds limit` at h=384.
- Repro: same as RDC-7.

**BSDK-5. The AssetUnlock v2 stable-txid rule is not implemented,** so `Transaction::hash()` is
wrong for v2 unlocks.
- As a consequence, `Block::merkle()` does not match the header (`block.merkle_root:base-sdk`),
  and **`Block::check()` rejects the valid block with `bad-merkle-root`.**
- Repro: same as RDC-8.
- Fixed by BS6b (`Transaction::hash()` zeroes the quorum info, as Core's `GetHash()` does).

**BSDK-6. `Block::check()` rejects every valid post-v24 block.** It fails at the coinbase
(`bad-txns-payload-decode` from BSDK-1), or earlier from BSDK-5.
- This matters if base-sdk's `check()` is used as a validation gate.
- Repro: any post-v24 fixture.

**Not observed.** No AssetLock v2 (base-sdk#40) and no LLMQ types 103/105 appear in any chain
checked. The harness covers both: `payload.decode` and `verbose.field` would flag them.

### Agreement worth recording

- **ProUpServTx v3 (ExtAddr):** base-sdk decodes it and matches every field of dashd's JSON,
  including the endpoint (`addresses.core_p2p`) and the operator payout address, which have
  been compared since fix round 1; dashcore rejects it (RDC-6).
- **Pre-v24 payloads:**
  - ProRegTx v1/v2 (legacy and BasicBLS, Regular and Evo), ProUpServ v1/v2, ProUpReg v1/v2,
    ProUpRev, CbTx v1–3, QcTx v1, v3 and v4 (legacy and BasicBLS commitments, rotation indices,
    member bitsets), MnHF v1, AssetLock v1, AssetUnlock v1.
  - Both libraries agree field by field with each other and with dashd. This covers all regtest
    chains plus every mainnet/testnet payload checked.
  - QcTx v2 (legacy, indexed) is not in any chain checked so far. It first appears at testnet
    769,700.

## 4. Classification method

- **Oracle comparisons attribute the mismatch to the library that differs from dashd.** dashd
  decides block hash, txids, merkle root, header fields and `getblock 2` values.
  - `both` means the two libraries are wrong in the same way.
- **Library-vs-library comparisons are `undecided`.** These are payload fields and tx fields
  with no oracle. None occurred.
- **Every field of dashd's `getblock 2` JSON is classified** (since fix round 1): compared with
  each library's decoded value rendered in dashd's format (`verbose.field`, and
  `verbose.missing` when a library that decoded the payload lacks it); a rendering (script asm,
  descriptor, address, type, decimal amount) recomputed from dashd's own source field
  (`verbose.render`, a harness finding); chain or index data on an explicit list; or a
  `harness-gap` finding, which fails the gate. No field is skipped silently.
- **Decode, re-encode and `check()` failures on blocks dashd accepted are bugs in the library
  that failed.** Every best-chain block is valid by definition.
- **I triaged each distinct group by hand against Dash Core source** (`src/evo/*.h`,
  `src/primitives/transaction.h`, `src/evo/core_write.cpp`) before listing it.

## 5. Harness bugs found and fixed during BS2

1. **(review H1)** In blk mode, the block-hash reference was derived from base-sdk's own hash,
   so base-sdk could never be flagged. A base-sdk X11 bug would also have silently cut the run
   short.
   - Fixed: blocks are now linked through raw `prev_hash` bytes, accepting either library's
     hash. Orphans are reported.
2. **(review H2)** blk-mode `--resume` marked whole files as done, regardless of the height
   range or file growth.
   - Fixed: units are keyed by a selection signature, and the latest run per file wins.
3. **(review M2)** The `getblock 2` comparison only went from library to oracle. It also
   skipped LLMQ bitsets and legacy BLS values.
   - Fixed: the comparison is now two-way.
4. **(review M3)** base-sdk payloads of unknown type passed silently.
   - Fixed: they are now reported as `payload.unsupported`.
5. **(review M1/M4/M6/L1–L9)** Assorted fixes:
   - exit codes;
   - mixing networks in one output dir;
   - duplicate lines on resume;
   - the live-tip race;
   - attribution of `tx.txid_rule`;
   - lib-vs-lib header fields without an oracle;
   - the merkle reference;
   - `mn_type` presence;
   - truncated NDJSON;
   - genesis check;
   - header-only indexing;
   - an output cap.
6. **The harness compared Evo platform ports with the top level of `getblock 2`.** dashd v24
   prints them there only with `-deprecatedrpc=service`, so both libraries were flagged on every
   Evo ProTx.
   - Fixed: they are no longer compared with the JSON. Their raw values are still compared
     between the libraries.
7. Capped mismatch groups reported truncated height ranges. Resuming RPC runs at the tip
   double-counted the last chunk. Both are fixed.
8. Mismatch records of superseded runs stayed in the counts. For example, orphans while the
   oracle syncs piled up across passes.
   - Fixed: every record is tagged with its unit and run, and the summary counts only the
     current run of each unit. It also reports any `unaccounted` difference.
9. **(review round 2, H1)** A narrower re-check (`--from/--to` for debugging) in an existing
   output dir replaced the wider earlier result and hid its findings.
   - Fixed: a unit group keeps the latest run that checked at least as many blocks.
   - RPC units are aligned to the chunk size.
   - `meta.json` pins the chunk size and blocks directory.
   - A lock file prevents concurrent writers.
10. **(review round 2, H2)** In blk mode, `--fail-on-mismatch` ignored chain-index findings, and
    a requested range the chain did not reach.
    - Fixed: index findings count towards the gate, and `blk.range` is reported.
    - Anchoring to the oracle is on by default.
    - Genesis hashes come from Core's chainparams, not from dashcore.
11. **(review round 2, M/L)** Assorted fixes:
    - Old output dirs are refused.
    - A torn last line is cut back on reopen, so later appends stay parseable.
    - Duplicate block copies are no longer reported as orphans.
    - `getblock 2` exemptions match exact top-level paths. `vin` txids, script hex and
      `assetUnlockTx.fee` were previously exempt by leaf name.
    - Informational checks no longer fail the gate.
    - A payload missing on the base-sdk side is reported.
    - A zero hole in a blk file followed by records is rescanned.
    - The blk unit signature covers changes to best-chain membership and hash references.
    - Added an end-to-end blk-mode test.

12. **(independent review r1, `reviews/BS2-r1-gpt.md`)** Fix round 1:
    - **Blocker:** `--resume --fail-on-mismatch` decided the exit status from the units the run
      checked itself, so resuming turned a recorded failure into success. The gate now covers
      every unit of the requested range, whichever run checked it (exit 1), and exits 2 when a
      unit of the range is not covered.
    - Checkpoints were not bound to the code, the policy or the chain. They now carry an
      identity (a source hash of the harness, `dashcore` and their path dependencies plus every
      external package's source from `Cargo.lock`, a digest of the executable, the oracle's
      version, and the `getblock 2` sampling or blk policy) and their coverage (RPC: the height
      range and the hash of its last block, re-validated every run so a reorg invalidates it;
      blk: a signature over every selected block's identity and reference hash plus the file's
      generation).
    - The summary chose one run per unit group by block count, dropping findings of ranges that
      were never re-checked. A unit now replaces another only when its coverage includes it;
      results of other identities that nothing re-checked under the same policy are reported
      as `unverified`.
    - The `getblock 2` comparison exempted every address, endpoint, payout and share subtree,
      and silently ignored oracle objects no library emitted anything for. Every field is now
      classified (§4), with independent renderers for dashd's formats.
    - The blk scanner dropped terminal garbage and short trailing bytes silently; every
      discarded byte range is now reported, and such findings make the gate incomplete.
    - The blk-mode txid reference was sha256d of the bytes, wrong for v24's stable asset-unlock
      txid; it now follows Core's rule (and is checked against the oracle in RPC mode).
    - An inverted range checked nothing and succeeded; it is now an error before anything is
      written. blk mode reads one block at a time and writes its mismatches immediately, so
      memory is bounded by the worker count; the default is at most 8 workers.
13. **(internal review of fix round 1)** The identity resolved `Cargo.lock` dependencies by
    version only and missed `dashcore`'s own base-sdk pin; the gate trusted the mismatch log
    over the progress counts; a weaker-policy run cleared a stronger policy's findings; the
    oracle's version was not part of the identity. All fixed, each with a regression test.
14. **(independent review r2, `reviews/BS2-r2-gpt.md`)** Fix round 2:
    - **Blocker:** a reorg between the separate `getblockhash` reads of the unit ends let a
      resumed RPC gate reuse a checkpoint of the old chain, check new chunks on the new one,
      and pass (exit 0) on the mixture, never checking the new chain's replaced blocks. RPC
      runs now pin the oracle's block at the top of the scope at the start. A gate passes only
      if every result it counts covers exactly its unit's heights and end hash, the results
      link to each other unit by unit through each unit's recorded parent hash, the last ends
      in the pinned block, and the pinned block is still the oracle's when the gate is decided
      (exit 2 otherwise). Tip growth does not fail it. The checkpoint
      format changed (format 2 records the parent hash); older checkpoints are not reused.
      Regression tests: the reviewer's scenario, a boundary read answered from another chain
      (caught by the links alone), and a shallow and a deep reorg after a run's last block
      versus tip growth.

## 6. Performance

Both measurements were taken on the oracle datadirs while they synced, with the disk about 96%
utilized by other workloads.

- **blk-file mode:** all of mainnet to height 651K (2.33 GB, 2.62M txs) took 73 s wall at idle
  I/O priority with 32 threads: 28 s of indexing (headers only) and about 45 s of checking. That
  is about 52 MB/s, and 383 s of CPU.
  - Extrapolating by bytes to the full mainnet chain (tens of GB), this is on the order of
    10–20 minutes on a quiet disk.
  - The CPU cost is about 165 s per GB, so 32 cores need about 5 s per GB. The disk read is the
    limit.
- **RPC mode:** testnet 500K–600K (100K blocks, 138 MB) took 11.5 s, about 8.7K blocks/s. It
  is limited by dashd's JSON-RPC (`getblock 0` and `getblock 1` per block, batched 50 per
  request, plus `getblock 2` for sampled blocks).

## 7. Running and refreshing

- The systemd user timer `dash-conformance-refresh.timer` runs `/work/conformance/bin/oracle-pass.sh`
  every 2 hours at idle CPU and I/O priority. That is a copy of `conformance/scripts/oracle-pass.sh`
  and of the harness binary built at this branch.
- Each pass resumes RPC and blk mode for both networks into `/work/conformance/<net>-<mode>`
  with `--fail-on-mismatch`, and appends one line per run to `/work/conformance/passes.log`
  with its exit status (0 covered and clean, 1 failing mismatches, 2 not fully covered) and
  gate line.
- A new build of the harness, a library or the oracle has a new identity, so the next pass
  re-checks everything; the first pass after fix round 1 does so too.
- Once the oracles reach their tips, the next pass completes the full-chain results. Read them
  with `dash-conformance summarize --out /work/conformance/<net>-<mode>`.
- To stop: `systemctl --user disable --now dash-conformance-refresh.timer`.
- To regenerate the v24 regtest chains: `conformance/scripts/regtest-v24.sh` (§3).

## 8. Follow-ups suggested

- **Fix RDC-2 first:** read payloads by length. It turns every unknown or new payload from "the
  block is undecodable" into "this one payload is undecodable".
- **Implement v24 formats once, preferably in base-sdk** (R3 §4.2 task 2.3), and let dashcore
  consume them:
  - CbTx v4;
  - types 10–12;
  - ProRegTx/ProUpRegTx v3 with DIP-26;
  - the AssetUnlock v2 txid.
  - `tests/v24_findings.rs` is the acceptance test.
- **Finalize the numbers in this document** once the timer's passes reach both tips: each of
  the four latest `passes.log` lines must show exit 0 with 0 units not covered, and each
  `summary.json` 0 `unverified` units, all under the deployed build. Note that the default
  `getblock 2` sampling leaves the fields of unsampled blocks compared only between the
  libraries and against `getblock 1`; a full-field pass needs `--verbose-every 1` (slower).
- **Run the harness against testnet once v24 activates there,** and against mainnet at
  activation.
- **Wire the harness into CI** (`--fail-on-mismatch`) with the committed fixtures, and run it
  nightly against the oracles.
