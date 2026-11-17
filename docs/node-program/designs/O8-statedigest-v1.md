# O8 — StateDigest v1: specification

| | |
|---|---|
| Row | PLAN §4.7 O8 (to be rewritten by §15 of this file); O9a consumes it; A1 §4.6, §2.10.1; R4 §2.3.1–2.3.2; PRD NFR-CEQ-06 |
| Status | **rev0, for review** (Opus high, then Sol on the whole O8 series). Checkpoint 1 of O8 |
| Date | 2026-10-12 |
| Tier | impl O-m; rev Sol (spec: Opus high first, manager ruling) |
| Inputs | the approved O8/O9a plan (`/work/scratch/o8/plan.md`, manager rulings of 2026-10-12); A1 §4.6, §2.10.1; R4 §2.3; EV2 §6.3 item 3; DEC-16, DEC-82, DEC-177, DEC-199 |
| Core pin | "Core" means **v24.0.0-rc.3** (`0364ed626e`). Every `file:line` below is at that tag |
| Codes | the patch is Core branch `node/state-digest-v24.0`; the record crate is `dash-statedigest`; the extractor is `dash-conformance digest extract`; the rig is `contrib/node/statedigest/` |

## 0. Summary

StateDigest v1 is a set of line-oriented files that describe, block by block, the state a node derives from the chain, so that rdashd's state can be compared with Dash Core's without running either node's RPC at comparison time.

- **One encoding.** Every file is canonical JSON Lines (§3): fixed key order, no whitespace, integers only (amounts in duffs), hashes in Core's display order. Two conforming writers produce byte-identical lines for the same content, so equal state means equal bytes, and `cmp` is a valid first check.
- **Four record families.** The patched dashd writes the **digest log** (`-statedigestlog`: `H`, `C`, `R`, `D` records, §4) and the **decisions file** (`-recordstatedecisions`: `H`, `A` records, §7). The extractor writes the **golden file** (merged `C` records plus sparse `S` records, §9).
- **Field classes** (A1 §4.6): every field is **semantic** (equal on any two nodes on the same chain), **observation** (equal only for the same sequence of operations) or **schedule** (never compared). The class is structural: the three classes live in three sub-objects `s`, `o` and `x`, so a comparator cannot mistake one for another (§5).
- **What the patch records.** Every `ConnectBlock` attempt (success, failure, `fJustCheck`), every `DisconnectBlock`, the non-transactional effects each one left behind (A1 §2.10.1), and the answers every chain-helper query returned during the attempt.
- **What the golden pass adds.** UTXO-set statistics, including the per-block MuHash, come from `gettxoutsetinfo muhash <h>` on a frozen copy, not from the patch (erratum E-O8-3, §15.2). The extractor merges them into the patch's records, so one golden `C` record per block holds every semantic field.
- **Comparison** (built by O9a): `linear` mode projects both sides onto the final active chain and compares semantic fields; `stream` mode compares the whole record sequence including observations. A key one side lacks is **uncovered**, reported, never passed (§10).

## 1. Scope and non-goals

**In scope (v1):**
- the record grammar, the per-field class and era table, the canonical encodings of every field;
- the attempt model: which Core calls produce which records, and in what order;
- the golden file layout and the extraction cadence;
- the comparison semantics that O9a implements (modes, coverage, first-mismatch dump, decision lookup);
- the Core patch's design, flags, overhead budget and the hidden `getcanonicalmnlist` RPC (EV2 §6.3 item 3, DEC-177);
- the `dash-statedigest` crate's model (types, writer, strict parser);
- the recording rig and provenance.

**Not in scope (v1):**
- assumeutxo snapshot chainstates. The patch records only the chainstate that is `ActiveChainstate()` when the attempt runs (§8.4);
- `RollforwardBlock` (crash replay roll-forward). It does not run `ConnectBlock` (it calls `ProcessSpecialTxsInBlock` directly, `validation.cpp:5197`), so it produces no `C` records and its effects are dropped; a recording that crashed shows a gap that the comparator reports (§10.4);
- block rejections before `ConnectBlock`: `AcceptBlock`'s `CheckBlock`/`ContextualCheckBlock*`, and `TestBlockValidity`'s pre-checks. L3 (A1 §4.7) covers them;
- effects emitted outside an attempt: `qfcommit` intake, RPC-triggered member computation, `CreateNewBlock` work outside `TestBlockValidity`. v1 drops them (§6);
- the per-block MuHash inside the patch (E-O8-3);
- the full mined-commitment index as state. v1 records each block's index writes (§5.2 `qc`); the active quorum sets are already committed by CbTx `merkleRootQuorums`, and the sparse goldens cover the rest (§9.3);
- mainnet recordings (manager ruling, 2026-10-12) and the testnet node (deferred until disk frees, §13.3).

## 2. Files and the recording layout

A **recording** is one directory per node and run set:

| File | Writer | Records | Notes |
|---|---|---|---|
| `digest.jsonl` | patched dashd, `-statedigestlog=<file>` | `H`, `C`, `R`, `D` | append-only; one `H` per process start |
| `decisions.jsonl` | patched dashd, `-recordstatedecisions=<file>` | `H`, `A` | append-only; one `H` per process start |
| `golden.jsonl` | `dash-conformance digest extract` | `H`, `C` (merged), `S` | rewritten per extraction; byte-reproducible (§9.5) |
| `blocks.dat` | the rig (`getblock <hash> 0` per active-chain block) | Core `-loadblock` framing: network magic, LE32 size, block bytes | the replay input; height order |
| `chainargs.json` | the rig | `{"network", "starts": [{"version", "cmdline": {key: [values]}, "config": {key: [values]}}]}`: every process start of the node in order, with its args from debug.log's "Command-line arg:" and "Config file arg:" lines (section-qualified keys as `section.key`), without `-mocktime` | needed to restart or reindex an FT chain; tools use the last start |
| `PROVENANCE.md`, `provenance.json` | the rig | Core commit, dashd sha256, build flags, chain args, network, height range, start/stop times, rig version, test name | required for every fixture (IMPLEMENTER.md) |

rdashd writes its own `digest.jsonl` (and, once CH7b exists, nothing else: decisions are Core-only input). Files are compressed with zstd (`.jsonl.zst`) when stored; the grammar is defined on the uncompressed bytes.

## 3. Canonical line encoding

Every file is a sequence of lines. A **line** is one JSON object followed by one `\n` (0x0a). There is no BOM, no `\r`, no blank line, and no trailing data after the last `\n`. The grammar is a strict subset of JSON; a parser rejects anything outside it.

| Element | Canonical form |
|---|---|
| whitespace | none outside strings |
| object | `{` key `:` value (`,` key `:` value)* `}`; keys in the order this spec lists for that record (§4–§9); `S`-record payloads (§9.3) use sorted keys instead |
| key | a string of `[a-z0-9_]`, as listed in this spec |
| array | `[` value (`,` value)* `]`, or `[]` |
| integer | `-?(0\|[1-9][0-9]*)`, in the signed 64-bit range. `-0` is invalid |
| number (`S` payloads only) | Core's RPC text verbatim: `-?(0\|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?` |
| string | `"` … `"`. Bytes 0x20–0x7e except `"` and `\` appear verbatim; `"` is `\"`, `\` is `\\`; every other byte (including each byte of a multi-byte UTF-8 sequence) is `\u00` + two lowercase hex digits. No other escape exists |
| `hash` | a string of 64 lowercase hex digits: a `uint256` in Core's display order (`uint256::GetHex()`: the 32 bytes reversed). A hash this spec *defines* as `sha256d(bytes)` is that digest read as a `uint256` (Core: `(HashWriter{} << obj).GetHash()` or `::SerializeHash(obj)`), printed the same way |
| `serialize(x)` | notation only: Core's serialization of `x` (`stream << x`). No type used here depends on the stream's type or version; `CanonicalMNListHash` itself hashes a disk-type stream at `CLIENT_VERSION` |
| `hex` | a string of lowercase hex digits, two per byte, in serialization order (Core `HexStr`) |
| boolean, null | `true`, `false`, `null` |

**Absent versus null.** Core's patch writes every key its record kind defines. `null` means "not defined at this block" under the rule in the field table (§5). Another writer (rdashd) may **omit** a key to say "not implemented yet"; it never writes a value it does not compute. A parser keeps the distinction: absent ≠ `null`.

**Canonicality check.** A line is canonical iff re-encoding its parsed form gives the same bytes. The parser enforces it, so a non-canonical writer fails at parse time, not as a confusing mismatch later.

## 4. Record kinds

Every record starts with `"k"`, the kind. The digest log (`digest.jsonl`) holds `H`, `C`, `R`, `D`; the decisions file holds `H`, `A`; the golden file holds `H`, `C`, `S`.

### 4.1 `H`: file header

Written when the writer opens the file (each process start appends one):

```
{"k":"H","v":1,"file":"digest","src":"dashd","ver":"v24.0.0rc3-…","chain":"regtest","run":"4711-1760227200123456"}
```

| Key | Value |
|---|---|
| `v` | `1`, the spec version. Every record after an `H` follows that version |
| `file` | `"digest"`, `"decisions"`, `"golden"` or `"memory"` (an oracle chain-session report written to the shim's in-memory sink, O2b-2 D-rows; `dash-statedigest` `FileKind::Memory`) |
| `src` | `"dashd"`, `"rdashd"` or `"extract"` |
| `ver` | the writer's version string (Core `FormatFullVersion()`) |
| `chain` | Core's chain name: `"main"`, `"test"`, `"regtest"`, or `"devnet"` (the devnet name goes in provenance) |
| `run` | `"<pid>-<unix microseconds at open>"`. It joins a digest log with its decisions file (§7.3); schedule class |

### 4.2 `C`: a successful connect

One `C` per `ConnectBlock` call that returned `true` with `fJustCheck == false`:

```
{"k":"C","h":<height>,"b":<hash>,"org":<origin>,"s":{…},"o":{"fx":[…]},"x":{"seq":<n>}}
```

- `h`, `b`: the block's height and hash. `org`: the origin (§8.2). These are the record's identity, compared in every mode.
- `s`: the semantic fields, in §5.1–§5.3's order.
- `o`: the observation fields: `fx`, the effects this attempt emitted, in emission order (§6).
- `x`: the schedule fields: `seq`, the attempt's sequence number in this run (1, 2, …; shared by `C`, `R` and `D`, and by the decisions file's `A`).

### 4.3 `R`: an attempt that did not produce a `C`

One `R` per `ConnectBlock` call that returned `false`, or that returned `true` with `fJustCheck == true` (a `TestBlockValidity` dry run), or that threw:

```
{"k":"R","h":…,"b":…,"org":…,"s":{"res":…,"cls":…,"why":…},"o":{"fx":[…]},"x":{"seq":…,"dbg":…}}
```

| Key | Class | Value |
|---|---|---|
| `res` | S | `"ok"`: a `fJustCheck` attempt that passed; `"invalid"`: `state.IsInvalid()`; `"error"`: `state.IsError()` (including every `AbortNode` path), or an exception escaped `ConnectBlock` |
| `cls` | S | for `"invalid"`, the `BlockValidationResult` name without `BLOCK_`, lower case: `result_unset`, `consensus`, `recent_consensus_change`, `cached_invalid`, `invalid_header`, `mutated`, `missing_prev`, `invalid_prev`, `time_future`, `checkpoint`, `chainlock`; otherwise `null` |
| `why` | S | for `"invalid"`, `state.GetRejectReason()` (the BIP22 string, e.g. `"bad-cb-amount"`); for `"error"`, the reject reason (`"unset"` when Core set none) or `"exception"`; for `"ok"`, `null` |
| `fx` | O | effects that survived the attempt (§6), e.g. a ChainLock IS-lock removal before a later `bad-cb-payee` |
| `seq` | X | as in `C` |
| `dbg` | X | `state.GetDebugMessage()`, or the exception's `what()`; `null` if empty or the exception is not a `std::exception`. Never compared: debug texts are not part of parity |

A proposal (`org:"proposal"`) is identified by its block hash. `TestBlockValidity` builds a dummy index whose hash is `block.GetHash()` (`validation.cpp:4945-4947`), so the hash is the template's, nonce included.

### 4.4 `D`: a disconnect

One `D` per `DisconnectBlock` call:

```
{"k":"D","h":…,"b":…,"org":…,"s":{"res":…},"o":{"fx":[…]},"x":{"seq":…}}
```

`res` is `"ok"`, `"unclean"` or `"failed"` (`DisconnectResult`); an exception escaping `DisconnectBlock` also gives `"failed"` (and is rethrown). `fx` holds the effects of the undo: mineable-commitment re-inserts, the rotation snapshots `UndoBlock` re-writes (`blockprocessor.cpp:523`), and the list-changed notification (§6).

### 4.5 `A`: decisions (decisions file)

One `A` per attempt (`C` or `R` in the digest log; none for `D`):

```
{"k":"A","h":…,"b":…,"org":…,"t":<adjusted time>,"q":[[<query>,<args…>,<answer>],…],"x":{"seq":…}}
```

`t` is `GetAdjustedTime()` in Unix seconds when the attempt opened. `q` lists every chain-helper query of §7.1 the attempt made, in call order. `A` records are replay **inputs**, not comparison targets; §7 defines their use.

### 4.6 `S`: sparse golden (golden file)

```
{"k":"S","h":…,"b":…,"rpc":<name>,"args":[…],"res":<payload>}
```

§9.3 lists the RPCs. `res` is the RPC's `result`, re-encoded with object keys sorted bytewise and numbers kept as Core's text (§3), except that `protx list`'s `metaInfo` members whose names end in `Elapsed` (`lastOutboundAttemptElapsed`, `lastOutboundSuccessElapsed`) are dropped: they are computed from the wall clock (§9.5); and `protx listdiff`'s `updatedMNs` array is put in `proTxHash` order, because Core fills it from a salted `std::unordered_map` (`CDeterministicMNListDiff::updatedMNs`), so its order changes with every process.

## 5. Field table

Notation: **S** semantic, **O** observation, **X** schedule. "Era" is the condition under which the field is non-null; outside it the value is `null`. "Source": `patch` fields are written by Core's patch and by rdashd; `golden` fields are written by the extractor into merged `C` records (§9.2) and by rdashd, never by the patch. The genesis block is connected by Core's early return (`validation.cpp:2534-2539`), so for genesis every field produced inside `ConnectBlock`'s body (`undo`, `mnl`, `qc`) is `null`.

### 5.1 Header fields (patch, from `pindex` and the block)

| Key | Class | Encoding | Era | Core source |
|---|---|---|---|---|
| `prev` | S | hash | non-genesis | `pindex->pprev->GetBlockHash()` |
| `ver` | S | integer | always | `pindex->nVersion` |
| `mrkl` | S | hash | always | `pindex->hashMerkleRoot` |
| `time` | S | integer | always | `pindex->nTime` |
| `bits` | S | integer (u32) | always | `pindex->nBits` |
| `nonce` | S | integer (u32) | always | `pindex->nNonce` |
| `work` | S | hash (chainwork as `uint256`) | always | `pindex->nChainWork.GetHex()` |
| `ntx` | S | integer | always | `block.vtx.size()` |

### 5.2 Derived state (patch)

| Key | Class | Encoding | Era | Core source and definition |
|---|---|---|---|---|
| `undo` | S | hash | non-genesis | `sha256d(serialize(CBlockUndo))`: the undo data exactly as `WriteUndoDataForBlock` receives it (`validation.cpp:2888`), *without* the file checksum. Byte-level parity of spent coins |
| `mnl` | S | hash | DIP0003 active at `pindex` | `evo::CanonicalMNListHash(mnlist_updates.new_list)` (`evo/snapshot.cpp:150-155`; the list `ProcessSpecialTxsInBlock` produced at `validation.cpp:2663`). Covers every DMN field, `internalId` and `nTotalRegisteredCount` |
| `qc` | S | array of `[type, quorumHash, sha256d(serialize(CFinalCommitment))]` (`type` an integer), sorted by `type`, then by `quorumHash`'s hex string; `[]` if none | DIP0003 active at `pindex` | each `DB_MINED_COMMITMENT` row this block wrote (`llmq/blockprocessor.cpp:432-436`, `fJustCheck == false` path) |
| `cp` | S | `[locked, currentLimit, latelyUnlocked, sha256d(serialize(indexes))]`, amounts in duffs | V20 active at `pindex` | `CCreditPoolManager::GetCreditPool(pindex)` after the block (`evo/creditpool.cpp`; the pool its child sees). §11.4 notes the snapshot-row timing this call causes |
| `ehf` | S | array of `[bit, height]`, ascending `bit` | V20 active at `pindex` | the signals map `CMNHFManager::ProcessBlock` returns for the block in `ProcessSpecialTxsInBlock` (the map `GetForBlock(pindex)` returns afterwards, `evo/mnhftx.cpp:285`): the MNHF signals the block leaves behind |
| `bls` | S | boolean | always | `!DeploymentActiveAfter(pindex, …, DEPLOYMENT_V19)`: the value of `bls::bls_legacy_scheme` that `ConnectBlock`'s scheme guard and `ProcessSpecialTxsInBlock` leave, i.e. the scheme its child decodes with (`true` = legacy). Computed rather than read, because the process-wide flag is not established before the genesis block |
| `bur` | S | integer bitmask | always | bit *i* set iff the *i*-th `Consensus::BuriedDeployment` (`DEPLOYMENT_HEIGHTINCB` = bit 0 … `DEPLOYMENT_WITHDRAWALS` = bit 14) is `DeploymentActiveAfter(pindex, …)` |
| `dep` | S | array, one `[name, state, since]` per `Consensus::DeploymentPos` in enum order: `name` from `VersionBitsDeploymentInfo` (`"testdummy"`, `"v24"`, `"evo_shares"`), `state` one of `"defined"`, `"started"`, `"locked_in"`, `"active"`, `"failed"`, `since` an integer | always | `m_versionbitscache.State(pindex, …, pos)` and `StateSinceHeight(pindex, …, pos)`: the deployment **answers** for the child (A1 F16), never cache contents |

`qc`'s rows say only which commitments the block indexed; whether they are the right ones is consensus-checked by the block's own CbTx `merkleRootQuorums` from DIP0008. `qc` catches an rdashd that indexes a null commitment, skips one, or stores different bytes.

### 5.3 UTXO and block statistics (golden)

All amounts are duffs. The extractor converts Core's 8-decimal amount text exactly (no floating point).

| Key | Class | Encoding | Era | RPC (frozen copy, `-coinstatsindex=1`) |
|---|---|---|---|---|
| `muhash` | S | string, the RPC text | always | `gettxoutsetinfo muhash <h>` → `muhash` |
| `txouts` | S | integer | always | → `txouts` |
| `amount` | S | integer | always | → `total_amount` |
| `unspendable` | S | integer | always | → `total_unspendable_amount` |
| `bi` | S | `[prevout_spent, coinbase, new_outputs_ex_coinbase, unspendable, genesis_block, scripts, unclaimed_rewards]` | always | → `block_info` and `block_info.unspendables` |
| `stats` | S | `[subsidy, totalfee, txs, ins, outs, utxo_increase, utxo_size_inc]` | non-genesis | `getblockstats <h>` (the named stats) |

**Dropped from A1's per-block table, with the reason:**
- `getblockheader`: every field is in the raw block or in §5.1;
- `getblock <hash> 1` → `cbTx`, `getcreditpoolinfo`, `getchainlockbyheight`: the CbTx is in the raw block and its roots are consensus-checked by the connect itself; the credit pool is `cp`; `getchainlockbyheight <h>` returns the CL *for* `h` found in a later coinbase (R4 §2.3.2), not state of block `h`;
- `getdeploymentinfo`: `dep` and `bur` per block; the RPC stays as a sparse cross-check (§9.3).

### 5.4 Observation and schedule fields

| Key | Record kinds | Class | Definition |
|---|---|---|---|
| `fx` | `C`, `R`, `D` | O | §6 |
| `seq` | `C`, `R`, `D`, `A` | X | attempt sequence number in this run |
| `dbg` | `R` | X | §4.3 |

## 6. Effects (`fx`)

Effects are Core's non-transactional actions (A1 §2.10.1). The patch records each at its Core site, **only while an attempt is open on the same thread** (§8.1), into that attempt's `fx` in emission order. An effect outside any attempt is dropped in v1.

| Entry | Core site | Emitted during |
|---|---|---|
| `["snap",type,cycleBaseHash,sha256d(serialize(CQuorumSnapshot))]` | `CQuorumSnapshotManager::StoreSnapshotForBlock` (`llmq/snapshot.cpp:324-335`), after the EvoDB write; a write that reports failure fails the recording (§11.4). It is reached from `PreComputeQuorumMembers` (`llmq/blockprocessor.cpp:223`, `:523`) and from `CFinalCommitment::Verify` | connects (any mode), undo |
| `["is_rm",islockHash,txid]` | `CChainstateHelper::RemoveConflictingISLockByTx` (`evo/chainhelper.cpp:94-103`), called from the IS loop's override branch (`validation.cpp:2856`), when `RemoveConflictingLock` reports that it removed the lock: the lock was stored and `RemoveChainedInstantSendLocks` removed it (with its chained children) and archived it. Nothing when there was no conflicting lock, and nothing when the conflicting lock was no longer stored: Core then writes the hash's archive entry and removes no root lock (chained children still stored are removed, unrecorded, as children always are). Whether the lock is still stored when `RemoveConflictingLock` runs can depend on thread timing (another removal between the lookup and the removal), as whether a conflicting lock is found at all already can. `txid` is the block's transaction that overrides the lock (the helper's argument), not the transaction the lock names (that is `is_conflict`'s `conflictTxid`) | connects outside IBD, `fJustCheck` included |
| `["qc_unmine",type,quorumHash]` | the mineable-pool erase in `ProcessCommitment` (`llmq/blockprocessor.cpp:448-453`) | real connects |
| `["qc_remine",type,quorumHash]` | `AddMineableCommitment` in `UndoBlock` (`llmq/blockprocessor.cpp:556`) | disconnects |
| `["ban_reset",proTxHash,height,ok]` | `CMasternodeMetaMan::ResetPlatformBan` from `CDeterministicMNManager::ProcessBlock` (`evo/deterministicmns.cpp:745`); `ok` is its return value | real connects with a ProUpServTx |
| `["mn_changed",undo]` | `GetMainSignals().NotifyMasternodeListChanged` in `ConnectBlock` (`validation.cpp:2918-2921`) and `DisconnectBlock` (`:2368-2371`) | active-chainstate connects and disconnects whose list diff has changes; VerifyDB levels 3 and 4 included |

**Why observation, not semantic.** Whether an effect fires depends on the operation history, not only on the chain. Example: Core caches quorum members, so a node that already computed a cycle base's members for a template does not re-write the `llmq_S` row when it connects the block, while a node replaying the chain does. The *contents* of every row are deterministic; the semantic checks of rotation state are the sparse `quorum rotationinfo` goldens (§9.3).

## 7. Decisions (`A` records)

### 7.1 Queries

Each entry of `q` is an array: the query name, its arguments, then the answer Core returned. The patch records the answer at the function that computed it, so the recorded value is what Core used, with no second query.

| Entry | Core site | Notes |
|---|---|---|
| `["cl_conflict",h,hash,bool]` | `CChainstateHelper::HasConflictingChainLock`, its answer as `ConnectBlock` receives it, recorded at that call site (`validation.cpp:2516`) | the `bad-chainlock` check |
| `["cl_best",null]` or `["cl_best",h,hash,sig]` | `chainlocks.GetBestChainLock()` in `CheckCbTxBestChainlock` (`evo/specialtxman.cpp:151`) | `sig` is `hex` of the 96-byte signature as Core serializes it (basic scheme: CbTx CLs exist only from V20); `null` when the best CL is null |
| `["ibd",bool]` | `IsInitialBlockDownload()` at the IS loop (`validation.cpp:2801`) | the IS conflict check runs only when this is `false` |
| `["cl_has",h,hash,bool]` | `CChainstateHelper::HasChainLock`, its answer as `ConnectBlock` receives it, recorded at that call site (`validation.cpp:2803`) | |
| `["is_conflict",txid,null]` or `["is_conflict",txid,[islockHash,conflictTxid]]` | `CChainstateHelper::ConflictingISLockIfAny`, its answer as `ConnectBlock` receives it, recorded at that call site (`validation.cpp:2806`) | one entry per loop iteration, so an override shows the conflict, then the next query |
| `["sb_required",h,bool]` | `CChainstateHelper::IsSuperblockValidationRequired` (`evo/chainhelper.cpp:41-52`), recorded at its `ConnectBlock` call site (`validation.cpp:2841`) | depends on the best CL height and on `mn_sync` |
| `["gov_valid",bool]` | `governance::SuperblockManager::IsValid` (inline, `governance/superblock.h:135`), recorded at its two callers `masternode/payments.cpp:355`, `:404` | "governance data present" |
| `["sb_triggered",h,bool]` | `SuperblockManager::IsSuperblockTriggered` (`governance/superblock.cpp:578`) | |
| `["sb_payments",h,null]` or `["sb_payments",h,[[amount,scriptHex],…]]` | `SuperblockManager::IsValidSuperblock` (`governance/superblock.cpp:622-629`): the payment list of the best superblock it validated the coinbase against | `null` when no best superblock was found |

`check_superblock` (`validation.cpp:2841-2843`) is not a separate entry: it is a function of `sb_required` and the V24 rules, both of which the replay has.

### 7.2 What replay does with them

CH7b's `ReplayChainHelper` answers rdashd's chain-helper queries from `A` records, so a replay sees exactly the CL, IS and governance state Core saw. The lookup key is the attempt: `(b, org, n)`, where `n` counts earlier attempts with the same `(b, org)` in the recording. Two policies, chosen by the caller:
- **keyed** (default): within the attempt, a query `(name, args)` returns the recorded answer. If the attempt recorded the same `(name, args)` twice with different answers, the second and later calls return them in order. A query the attempt never recorded is a **decision miss**: the replay stops and reports it; it never guesses.
- **strict**: rdashd's query sequence must equal the recorded sequence entry by entry; the first difference is reported.

Besides the attempt key, the lookup offers `final_connect(b)`: for a block tried more than once (a reorg back to it, a proposal before the connect), the decisions of the attempt that produced the final-chain `C`, the digest's last `org:"tip"` `C` for `b` (the one the golden merges, §9.2).

`t` sets the replay clock (FT chains run under mocktime, so header time checks need it).

### 7.3 Joining with the digest log

A digest record and a decisions record belong to the same attempt iff their `seq` values are equal and they follow `H` records with the same `run`. Both flags are normally set together; either may be set alone.

## 8. The attempt model

### 8.1 Scope

An **attempt** is one call of `Chainstate::ConnectBlock` or `Chainstate::DisconnectBlock`. The patch renames each body to `…Impl` and wraps it. When either flag is set, the wrapper opens a thread-local attempt before calling the body and closes it after; the close writes exactly one record (`C`, `R` or `D`) and, for connects, one `A`. Taps (effects, decisions, derived values) write into the open attempt of their own thread. Calls are serialized by `cs_main`, so attempts never nest.

### 8.2 Origins (`org`)

| `org` | Calls |
|---|---|
| `"tip"` | `ConnectTip` (`validation.cpp:3431`) and `DisconnectTip` (`:3281`): normal connection, reorgs, `invalidateblock`, `reconsiderblock`, IBD and `-reindex(-chainstate)` |
| `"proposal"` | `TestBlockValidity` (`:4968`): every `fJustCheck == true` call: GBT proposals and every `CreateNewBlock` template (`generate*` included) |
| `"verifydb"` | `CVerifyDB::VerifyDB`'s level-3 disconnects (`:5113`) and level-4 reconnects (`:5160`) |
| `"replay"` | `ReplayBlocks`'s rollback disconnects (`:5275`) |

The patch sets `"verifydb"` and `"replay"` with a scope object in those two functions; `fJustCheck` implies `"proposal"`; everything else is `"tip"`. Startup runs VerifyDB (`-checkblocks`, default 6 blocks at level 3), so every restart appends `D` records with `org:"verifydb"`. Linear mode ignores them (§10.2).

### 8.3 Ordering and durability

Records are written in attempt order, each followed by `fflush`. A write error aborts the node (`AbortNode`), because a recording with gaps is worse than none. Files are opened in append mode; the `H` record marks each run.

### 8.4 Chainstates

An attempt is recorded only when `this == &m_chainman.ActiveChainstate()`. A node that loads an assumeutxo snapshot therefore records only the snapshot chainstate's connects, and a recording of such a node is out of scope for v1.

## 9. Golden extraction

### 9.1 Source

Goldens come from **O8's own patched nodes**, never from the live oracles (E-O8-1). The extractor talks JSON-RPC to a **frozen copy** of a recording node: the node is stopped, its datadir is copied with `cp --reflink=always`, and the copy is restarted with the recorded chain args plus `-connect=0 -listen=0 -rpcdoccheck=0 -coinstatsindex=1` and without the digest flags. The extractor waits for `getindexinfo` to report `coinstatsindex` synced before it starts.

### 9.2 Merged `C` records

For each block of the frozen copy's active chain, in height order, the extractor writes one `C` record:
1. it takes the **last** `C` with `org:"tip"` for that hash from the node's `digest.jsonl` (a block reconnected after a reorg has several; they must agree on `s`, and the extractor fails if they don't);
2. it checks every key of that `s` against the frozen copy (§13.5) and fails on the first disagreement;
3. it computes §5.3's golden fields from the RPCs and appends them to `s` in table order;
4. `o` and `x` are copied from that record.

The result is the reference stream for linear comparison: one record per active-chain block, every semantic key present. A node that started from a copied datadir (the FT framework's cached chain, masternodes cloned from another node) never connected the blocks it inherited, so its digest starts later: the extractor requires the digest's tip `C` records to cover a contiguous suffix of the active chain, and the golden covers that suffix only.

### 9.3 Sparse records

| `rpc` | Call | Cadence (testnet/mainnet) | Cadence (regtest `--dense`) | Era |
|---|---|---|---|---|
| `protx_list` | `protx list registered true <h>` | every 576, plus boundaries | every block | DIP0003 |
| `protx_listdiff` | `protx listdiff <h-1> <h>` | every 576, plus ±3 around boundaries | every block | DIP0003 |
| `mn_payments` | `masternode payments <hash>` | every 576 | every block | DIP0003 |
| `quorum_listextended` | `quorum listextended <h>` | every 24 | every block | DIP0003 |
| `quorum_info` | `quorum info <type> <quorumHash>` | per `qc` row | per `qc` row | DIP0003 |
| `rotationinfo` | `quorum rotationinfo <hash>` | per cycle base of each rotated type (a type is rotated when `quorum info` reports `previousConsecutiveDKGFailures`; the base is `height - quorumIndex`) | same | DIP0024; a refusal (`RPC_INVALID_REQUEST`, e.g. for an early cycle base without stored snapshots) skips the record and is listed in the extractor's summary |
| `deploymentinfo` | `getdeploymentinfo <hash>` | every 576, plus window boundaries | every block | always |
| `creditpool` | `getcreditpoolinfo <h>` | every 576 | every block | V20 |
| `assetunlockstatuses` | `getassetunlockstatuses [indexes of this block's unlocks] <h>` (indexes as decimal strings, as Core reads them) | per block with unlocks | same | V20; at most 100 indexes per call (dashd's limit), one record per call |
| `canonical_mnlist` | `getcanonicalmnlist <hash>` | the DEC-177 heights (§13.4) | every 24 | DIP0003 |

"Boundaries" are the A1 §4.6 segment heights for the network (mainnet; for testnet and regtest the extractor's `--boundary` list, empty by default). "Window boundaries" are the heights whose `bur` or `dep` differs from the parent's `C` record, plus the boundaries. Non-pure RPCs (`quorum *`, `rotationinfo`) are allowed here because the target is a frozen copy, never a live oracle.

### 9.4 Amount conversion

Core prints amounts as `-?[0-9]+\.[0-9]{8}` (`ValueFromAmount`). The extractor converts that text to duffs with integer arithmetic and rejects any other shape. `S` payloads keep the text.

### 9.5 Reproducibility

Two extractions of the same frozen copy produce byte-identical `golden.jsonl` files (O7's surviving acceptance item). Nothing time- or schedule-dependent enters the golden file: the `H` record's `run` is replaced by the frozen copy's `getbestblockhash`, and `ver` is the node's version.

## 10. Comparison (implemented by O9a)

### 10.1 Classes

| Class | Linear mode | Stream mode |
|---|---|---|
| S (`s`) | compared | compared |
| O (`o`) | ignored | compared |
| X (`x`) | ignored | ignored |

`h`, `b` and `org` are compared in both modes. `k` is compared in stream mode.

### 10.2 Modes

- **linear.** Each input is projected onto its final active chain: for each block hash of the chain in height order, the last `C` with `org:"tip"`. A golden file is already in that form. Records with other origins, and `R`/`D` records, are not in the projection. The two projections must cover the same height range (or the requested `--from/--to`), block by block, with equal `b` and equal `s` per key.
- **stream.** Both inputs are filtered to the requested origins (default: all), and the resulting record sequences must match record for record: same `k`, `h`, `b`, `org`, equal `s` and `o`. It is for runs of the same operation sequence, such as Core-vs-Core nodes fed by one test, or Core's recording against rdashd replaying `blocks.dat` plus the same invalid blocks and proposals.

### 10.3 Coverage

Per key, the comparator counts records where the key is: equal; different; absent on the left only; absent on the right only. An absence is **uncovered**, never a pass. The report lists every key with its four counts. A flag `--require <keys>` turns absences of those keys into failures, so a task can pin what it claims to cover. The exit status:
- **1** on any difference, or an absence of a `--require` key;
- otherwise **3 ("incomplete")** when any key is uncovered: with no `--require`, any uncovered key at all, and with one, any key it does not name;
- **0** only when every key of every compared record is present on both sides and equal.

*Erratum (O9a plan ruling, 2026-10-12):* the first version exited 0 when keys were uncovered and no `--require` was given, so an uncovered key could read as a pass.

### 10.4 First mismatch

The first differing record ends the comparison (unless `--keep-going`). The report names: the height and hash, the record kind and origin, the key or keys, both values, and the preceding record on each side. A height present on one side only is reported the same way ("gap").

The **dump** then collects both sides' detail at that height:
- from the golden file's sparse records, the nearest `protx_list`, `protx_listdiff`, `quorum_listextended`, `rotationinfo` and `creditpool` at or below the height;
- from a frozen copy given with `--rpc`, the same RPCs at exactly that height, plus the UTXO delta from `getblock <hash> 2`;
- from the Rust side, its own records around the height.

**Dumps over historical mainnet or testnet blocks are written to the private corpus by default** (PLAN §7.5; `$DASH_EMBARGO_DIR/statedigest/`), and the manager releases them. Regtest dumps from FT chains may go to the task's scratch.

### 10.5 Decision lookup

O9a's `RecordedDecisions` parses `A` records into the lookup of §7.2. It also checks at load time that every `A` has a matching digest record (§7.3) and reports orphans.

## 11. The Core patch (`node/state-digest-v24.0`)

### 11.1 Flags

`-statedigestlog=<file>` and `-recordstatedecisions=<file>`: `ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY`, `OptionsCategory::DEBUG_TEST`, so they appear only under `-help-debug`. Relative paths resolve against the datadir. Each opens its file in append mode at init and writes `H`. An unset, negated (`-nostatedigestlog`) or empty flag leaves its file closed; with only `-recordstatedecisions`, the derived fields `cp`, `bls`, `bur` and `dep` are not computed (the tapped ones, such as `mnl` and `undo`, still are; that cost is reported, not gated).

### 11.2 Structure

- `src/statedigest.{h,cpp}`: the writer (two `std::ofstream`), `statedigest::Attempt` (header data, the tapped values as canonical JSON, `fx`, `q`), the thread-local current-attempt pointer, one canonical-JSON encoder template, the taps (`Fx`, `Query`, `Decide` and a few hashing taps), the origin scope, the attempt scope's `Guard` (every record built after the body runs inside it, §11.4), and the derived fields `bls`, `bur` and `dep`. The canonical MN-list hasher is passed into `Init` by `init.cpp`, so that the module does not depend on `evo/`.
- `validation.cpp`: `ConnectBlock` and `DisconnectBlock` become wrappers around `…Impl`. The connect wrapper opens the attempt and calls the body; an escaping exception gives `R` and is rethrown. On `C` it computes `cp` (which needs `evo/`) and hands the rest to the module. Taps inside the body: the new list after `ProcessSpecialTxsInBlock` (real connects), the undo before `WriteUndoDataForBlock`, the `ibd`, `cl_conflict`, `cl_has`, `is_conflict` and `sb_required` answers, and the two `mn_changed` notifications. Origin scopes in `VerifyDB` and `ReplayBlocks`.
- `instantsend/db.{h,cpp}`, `instantsend/instantsend.{h,cpp}`: `RemoveChainedInstantSendLocks` reports whether it removed the root lock, and `RemoveConflictingLock` returns that, for `is_rm` (§6).
- One-line taps at the other sites of §6 and §7.1 (`evo/chainhelper.cpp` for `is_rm`, `llmq/snapshot.cpp`, `llmq/blockprocessor.cpp`, `evo/deterministicmns.cpp`, `evo/specialtxman.cpp`, `masternode/payments.cpp`, `governance/superblock.cpp`).
- `rpc/evo.cpp`: `getcanonicalmnlist <blockhash>` → `{"hex": SerializeCanonicalMNList bytes, "hash": CanonicalMNListHash}` over `GetListForBlock`, registered hidden (EV2 §6.3 item 3).
- `init.cpp`: the two args and the writer's construction and teardown.

### 11.3 Overhead

- **Flags off.** `Init` returns before it allocates anything when neither flag is set, so the writer pointer stays null. Each wrapper (`ConnectBlock` and `DisconnectBlock`) tests it once and calls the body directly, without opening a scope; each tap loads the thread-local attempt pointer and returns when it is null. No allocation, lock, serialization or I/O. **Gate:** user-space instructions over two workloads of an FT chain copy (§13.2), a regtest `-reindex-chainstate` (connects) and a startup `VerifyDB` at level 4 over the whole chain (a disconnect and a reconnect per block), stock rc.3 against the patched binary with the flags off: **Δ ≤ 0.1 %**. The count is callgrind's exact `Ir` total with two functions excluded (`--toggle-collect='SeedStrengthen*' --toggle-collect='ECC_InitSanityCheck*' --collect-atstart=yes`, in that order) and with `-par=1 -parbls=1`. `SeedStrengthen` (`random.cpp`) hashes SHA-512 for a fixed wall-clock time and alone varies a whole-process `perf stat -e instructions:u` count up to threefold on a loaded box; `ECC_InitSanityCheck` signs with a random key and grinds a random number of nonces for a low R; with script or BLS worker threads, how the signature checks split between threads changes the count. What is left (timer-driven threads) repeats to about 0.015 % over 3 runs, so the gate can compare the largest patched count with the smallest stock count, which also bounds the medians' difference. (A sampled `perf record` count that drops the `sha512` symbols was tried first; kernel throttling and per-thread period truncation leave about 30 % noise on regtest-sized chains.)
- **Flags on** (reported, not gated). `CanonicalMNListHash` re-serializes the whole list per block (about 1 ms per block at ~4k MNs); `undo` re-serializes the undo data; `cp` computes the credit pool the next block would otherwise compute first (§11.4); `ehf` reuses `ProcessBlock`'s result; `dep` queries the versionbits cache. Expected +10–40 % CPU in IBD.

### 11.4 Behaviour with the flags on

The patch must not change consensus or any observable behaviour. Two deliberate, harmless differences:
- `GetCreditPool(pindex)` at a height that is a multiple of 576 writes the credit-pool disk snapshot row during this block's EvoDB transaction rather than the next block's. The row is a derived cache whose content `WriteDerived` checks, so its value is unaffected.
- The versionbits and MNHF caches may hold entries earlier. Both are pure caches (A1 §2.10.1 leaves them unmodelled for that reason). The earlier queries can add debug-log lines (for example `CMNHFManager::GetSignalsStage`'s expiry line).
- A record that cannot be written, an exception of any type while a record is built after the body (`cp`, `C`, `A`, `R` or `D`, caught by `AttemptScope::Guard`), or a quorum snapshot whose EvoDB write reports failure calls `AbortNode` and writes nothing more to either file, although the blocks already queued in the current activation step still connect: a recording with a gap is worse than none, and the missing `seq` marks the end. So with the flags on, a full disk under the record files stops the node. (`CDBWrapper::Write` throws on a LevelDB error rather than return false; the snapshot's failure branch is for a `false` it cannot currently return, and its throw propagates as before.)

### 11.5 Tests in the Core branch

- `src/test/statedigest_tests.cpp`: flags off (no file, no scope, an `InvalidateBlock` included), a recorder exception of either kind thrown inside `Guard` (the recording fails, the node requests shutdown, nothing more is written), record encoding (escapes, nulls, ordering), `C` versus `R` on `TestChain100Setup` with a block made invalid (`bad-cb-amount`), a `TestBlockValidity` dry run (`R res:"ok"`). `is_rm` on `RemoveConflictingISLockByTx` itself: a stored conflicting lock is removed and recorded, and a conflicting lock that is no longer stored (removed by `RemoveConfirmedInstantSendLocks`) is not recorded (a ChainLocked block that overrides a lock needs quorums, which `TestChain100Setup` lacks; the `feature_llmq_is_cl_conflicts` recordings cover that path, §13.1). The snapshot tap has no unit test: its failure branch cannot be reached, since `CDBWrapper::Write` throws instead of returning false (§11.4).
- `test/functional/feature_statedigest.py`: (a) flags off create no file; (b) two nodes on one chain have equal linear projections; (c) `invalidateblock`/`reconsiderblock` give the expected `D`/`C` sequence; (d) a GBT proposal gives `R` with `res:"ok"` and `org:"proposal"`; (e) a `-reindex-chainstate` of a copy reproduces the semantic fields; (f) `getcanonicalmnlist`'s hash equals `mnl`, and `cp`, `dep` and the CbTx credit-pool balance equal `getcreditpoolinfo`, `getdeploymentinfo` and `getblock`.
- Regression: `test_dash` in full, and the FT list of §13.1 with the flags off, against stock rc.3 on the same list: the same pass/fail outcome per test.

## 12. The record crate (`dash-statedigest`)

A small pure crate (node-pure group: no I/O, threads or clocks), so that rdashd's emitter (`dashd-chainstate`, behind a feature) and dash-conformance share one grammar.

- **Model.** `Value` (`Null`, `Bool`, `Int(i64)`, `Num(String)` for `S` payloads, `Str(String)`, `Arr`, `Obj` with ordered keys); `Record { kind, h, b, org, s, o, x, … }` with typed views per kind (`Header`, `Connect`, `Reject`, `Disconnect`, `Attempt`, `Sparse`); `Effect` and `Query` enums for §6 and §7.1 with lossless conversion to and from `Value`.
- **Field table.** `FIELDS: &[FieldSpec { kind, key, class, source }]` in spec order, so a comparator iterates keys and classes from one table. The era rules stay prose (§5): they are the emitter's business, not the parser's.
- **Writer.** `Record::encode(&self, out: &mut Vec<u8>)` produces the canonical line.
- **Parser.** `parse_line(&[u8]) -> Result<Record, ParseError>`: strict §3 grammar, known keys only, spec order, canonicality by re-encoding, with the byte offset in errors. `Lines` iterates a buffer.
- **Added by O9a, also here** (pure): the comparator core, `RecordedDecisions` (§7.2, §10.5) and a header-field builder.
- **Not here**: the CLI, file IO, zstd, the dump (§10.4) and the replay driver stay in `dash-conformance`.

The crate needs a node-pure entry in `.github/ci-groups.yml` and, once SB1 merges, a `layers.toml` row (manager).

## 13. Recordings

### 13.1 Regtest functional-test chains (now)

The patched dashd runs Core's own FTs through `contrib/node/statedigest/dashd-wrapper`, which adds both flags with the files in each node's datadir. Tests run with `--nocleanup --tmpdir=/work/scratch/o8/ft/<test> --timeout-factor=4`. Target list, with the decision kinds and records each is chosen for:

| Test | Exercises |
|---|---|
| `feature_llmq_is_cl_conflicts` | `is_conflict` with an answer, `cl_has`, `is_rm`, `R conflict-tx-lock` |
| `feature_llmq_chainlocks` | `cl_conflict`, `R bad-chainlock`, reorgs (`D`) |
| `feature_governance`, `feature_governance_cl` | `sb_required`, `gov_valid`, `sb_triggered`, `sb_payments` |
| `feature_llmq_rotation` | `snap` effects, rotated `qc` rows, `rotationinfo` goldens |
| `feature_mnehf` | `ehf`, `dep` transitions |
| `feature_asset_locks` | `cp`, `assetunlockstatuses` |
| `feature_llmq_simplepose` | PoSe in `mnl`, `ban_reset` |
| `feature_masternode_shares`, `feature_evo_shares_activation` | v24 payloads, `evo_shares` in `dep` |
| `feature_block`, `p2p_invalid_block` | many `R` records (`invalid`, every class) |
| `mining_basic` | `R res:"ok"` proposals and `invalid` proposals |
| `rpc_invalidateblock` | `D`/`C` sequences |
| `feature_dip3_deterministicmns`, `feature_dip4_coinbasemerkleroots` | `mnl`, `qc` from DIP3/DIP8 |
| `feature_llmq_evo`, `feature_new_quorum_type_activation` | evonode lists, quorum-type activation |

Observed in the first recording set (v24.0.0-rc.3-8-g0a36fab32cb5, all 18 tests, counted by the `statedigest-stats` example of `dash-statedigest`): every effect kind, every query kind, and both answers of `cl_best`, `cl_has`, `ibd`, `is_conflict`, `sb_required` and `sb_triggered`; `R` with `res:"ok"` and `"invalid"` in the classes `consensus`, `chainlock` (`conflict-tx-lock`) and `result_unset` (`bad-cb-payee`, `bad-cb-amount`); `D` from `tip` and `verifydb`. Not reached by these FTs: `cl_conflict` true (so no `R bad-chainlock`), `gov_valid` false, `org:"replay"`, `D res:"failed"`, and the other `R` classes. No Core-produced record has them yet (the crate's strict-parser tests do parse a `D res:"failed"` line). A block that conflicts with a ChainLock is normally stopped before `ConnectBlock` (`AcceptBlockHeader` marks it `BLOCK_CONFLICT_CHAINLOCK`, `FindMostWorkChain` skips it); `ConnectBlock`'s own check is a backstop for a ChainLock that arrives between the header and `EnforceBestChainLock`. `feature_block` gives fewer `R` records than invalid blocks: many fail `CheckBlock` or `AcceptBlock` before any connect attempt, and blocks on side branches with less work are never connected.

Per node, after the test: `chainargs.json`, a frozen reflinked copy, `blocks.dat`, and `golden.jsonl` via `digest extract --dense`. Per test, `record-ft.py` writes `manifest.json`: every node the FT created, and which of them were recorded and which never started (a node is unstarted only if it has no `regtest/debug.log`; one that started without writing a digest stops the recording; `--backfill-manifest` derives it, for the first recording set, from `frozen/` and provenance's `unstarted_nodes`). Git gets the slim parts of each recording (the `.zst` digest, decisions, golden and `blocks.dat` files, provenance, `extract.json`, the DEC-177 verdicts and the manifest) under `test-vectors/statedigest/regtest-v24/<test>/node<i>/`; `blocks.dat.zst` is the replay input O9a-2's acceptance test needs (each raw file is under 210 KB). Frozen copies stay in scratch unless the manager wants them under `/work/conformance/statedigest/`.

`store-vectors.sh` copies them only after every check passes, and otherwise leaves the destination as it was:
- the tests are exactly those of `ft-list.txt`, and each test's `rec/node*` directories are exactly its manifest's recorded nodes, so a missing node directory is noticed;
- each node was extracted reproducibly, with every DEC-177 verdict `match`, by a clean rig at record and extract time;
- the digest, decisions, golden and `blocks.dat` files are the bytes `extract.json` hashed, so a digest cut down after its extraction no longer matches;
- the extraction's summary in `extract.json` is for the golden's last height (the frozen copy's tip), and every height whose BIP9 values the extraction left unchecked was checked by `check-bip9` at that tip, by groups whose runs are exactly the other-params runs, which `store-vectors.sh` derives again from `chainargs.json` and the digest's `H` records (§13.5);
- `statedigest-check` (`dash-statedigest::recording`) passes on the three files: strict parse, `H` first, the record kinds of each file, every `A` joined to its attempt by `seq` within its run (§7.3), and the golden's `C` records running through consecutive, linked heights, each the digest's last tip connect for its block plus the golden-only keys (that the run ends at the tip is the summary check above: the checker never sees the chain);
- the digest and decisions record counts equal provenance's.

The new tree is then built next to the destination and exchanged with it in one step (`mv -T --exchange`), and the old tree is deleted afterwards.

### 13.2 Fixed workload for the overhead gate

The overhead gate (§11.3) uses one recorded FT chain, the largest DIP3 chain of §13.1 (`feature_masternode_shares` node0 in the first recording set, tip 2027), copied, and run under callgrind by stock rc.3 and by the patched binary with flags off, with the same args, 3 runs each, alternating, in two modes: `-reindex-chainstate` (every block connected once) and `-checklevel=4 -checkblocks=<tip>` (startup's `VerifyDB` disconnects every block and connects it again, the reorg-shaped workload; the run must log both `Verifying last <tip> blocks at level 4` and `No coin database inconsistencies in last <tip> blocks`, and must not skip level 3). In each mode the largest patched count is at most 0.1 % above the smallest stock count (§11.3). `contrib/node/statedigest/overhead.py` runs it and reports the workload, both builds' info and hashes, each binary's spread and both deltas (medians, worst pair). It refuses unless each run's own `debug.log` ends at the workload's tip (a run with the wrong chain args invalidates a block, stops early and still exits 0) and unless each dashd has `SeedStrengthen` as a function of its own (an inlined copy would escape the toggle). The workload's `dash.conf` loses its network keys (`bind`, `port`, …) and the run uses `-listen=0 -connect=0`: stock rc.3 can deadlock in shutdown when init fails while the import thread runs (observed with a port clash), so the rig also bounds each run and kills its process group.

### 13.3 Testnet node (designed, deferred)

Deferred by the manager until disk frees after the current merge trains (P-1/P-2 of the plan). When it runs: a patched testnet node in `/work/scratch/o8/testnet`, `-assumevalid=0 -coinstatsindex=1 -listen=0`, synced from the public network under `systemd-run --user --unit=o8-testnet` (Nice 10, CPUWeight 20, IOWeight 20, own ports checked with `ss -ltn`), recording history and then live decisions for at least 3 days. `snapshot.sh` stops it, reflinks the datadir and restarts it; frozen copies serve golden extraction and LQ1 §8 Q3's non-pure RPCs. Disk cap 20 GB. Nothing under `/work/chains` or `/work/bench` is read or touched.

### 13.4 `getcanonicalmnlist` cross-check (DEC-177)

At each chosen height: `getcanonicalmnlist`'s `hex` decoded by `oracle_mnlist_canonical` (canonical input) gives the same hash as the RPC's `hash`, and dash-dml decodes and re-encodes the bytes unchanged. Regtest: every 24th block of each FT chain with a DIP3 list, at least 20 heights (evidence now). Testnet: every 576 from DIP3 plus the boundary heights, at least 20 (with the testnet node). Mainnet: when a mainnet node exists (out of scope).

The two legs run separately. `extract.py` runs `dash-oracle-sys`'s `canonical_mnlist_check` on each node's samples and stores the verdicts as `dec177.jsonl`. `contrib/node/statedigest/dml-leg.sh` runs dash-dml's `canonical_list_check` example (`decode_canonical_list`, then `encode_canonical_list_unvalidated` and `canonical_mn_list_hash`) over the stored goldens and requires `match` for every sample `extract.json` counted. The first regtest set: 875 samples from 71 of the 81 nodes (10 have no DIP3 list), all `match` on both legs; 749 lists are non-empty, at 171 (test, height) pairs in 13 tests.

### 13.5 Golden checks

Two extractions agreeing shows only that they agree: a Core hook that writes a wrong value writes it every time (Sol r1: a build without `TapNewList`, and one swapping `nBits` and `nNonce`, each gave byte-identical goldens). So before a merged `C` becomes golden, `digest extract` compares every key the patch writes into `s` with what the frozen copy says through another path (`conformance/src/digest/extract/checks.rs`), and fails on the first disagreement. The eras of these checks come from the copy's consensus parameters (`getdeploymentinfo`'s buried heights of `dip0003` and `v20`), never from the digest.

| Key | Checked against |
|---|---|
| (era) | `undo` is null only at genesis; `mnl` and `qc` are null exactly outside the DIP0003 era and at genesis; `cp` and `ehf` are null exactly outside the V20 era. A null in era fails, and so does a value outside it |
| `prev`, `ver`, `mrkl`, `time`, `bits`, `nonce` | the block's own 80-byte header, from `getblock <hash> 0` (genesis: the zero previous hash is `null`) |
| `ntx` | the transaction count after that header |
| `work` | `getblockheader`'s `chainwork` |
| `undo` | the copy's `rev*.dat` files (`--blocks-dir`): a record whose bytes hash to `undo` and whose stored checksum is `sha256d(parent hash ‖ bytes)` for this block's parent |
| `mnl` | `getcanonicalmnlist <hash>` at every DIP0003-era block. Core treats the list's registration counter (`nTotalRegisteredCount`) and `internalId` as sync-history dependent (`CDeterministicMNList::IsEqual`), so the stored list may differ from the list the block was validated with in the counter. The check passes on equal hashes. Otherwise the extractor re-hashes the RPC's `hex` with only the counter changed, for k in 1..=16 added to it, and passes if exactly one k reproduces `mnl`. `extract.json` reports how many heights passed this way. Any other difference fails |
| `qc` | the block's non-null commitments in `getblock <hash> 2` (null: no signers, no valid members, and zero `quorumPublicKey`, `quorumVvecHash` and signatures, as `CFinalCommitment::IsNull`; `llmqType`, `quorumHash`, `sha256d` of the payload's commitment bytes), in row order; and each row's `quorum info` names this block as `minedBlock` |
| `cp` | `getcreditpoolinfo <h>` (`balance`, `currentlimit`, `window.unlocked`), and the hash of every asset-unlock index mined since genesis (from `getblock <hash> 2`, blocks before the digest's suffix included), serialized as Core's `CRangesSet` |
| `ehf` | each MNHF signal of the block (`mnhfTx.signal.versionBit`) is a row `[bit, h]`; every row is a signal mined at its height (in a block of the active chain, the digest's suffix or before it), and every other row is one of the parent's (rows only expire, §5.2); every `ehf_height` that `getdeploymentinfo <hash>` reports is a row `[bit, ehf_height]` for the deployment's bit (its height only, when the reply has no bit). Which rows expire is not checked: no RPC lists the signals stage |
| `bls` | `getdeploymentinfo <hash>`: `v19` is not active |
| `bur` | `getdeploymentinfo <hash>`: the buried deployments it reports active, as bits in §5.2 order |
| `dep` | the names of §5.2 in their order (`testdummy`, `v24`, `evo_shares`), and every BIP9 deployment `getdeploymentinfo <hash>` lists is one of them (the reply has its own order); an entry the reply leaves out is a disabled deployment (start time `NEVER_ACTIVE`, which `getdeploymentinfo` skips), so it must be `failed` since 0, as `GetStateFor` and `GetStateSinceHeightFor` make it; each listed entry's state is its `status_next`; each listed entry's `since` is `StateSinceHeight(pindex)` as the block's own reply gives it: its `height` when `status_next` is active, else its `since` when `status_next` equals `status` (the child keeps the state) and the child's height when they differ (states change only at a period's first block), the reply's `height` agreeing; and it is what the child block's reply reports as `since`, on the copy with the block's parameters, also when the child came from a run with other parameters. The tip, which has no child, is checked by its own reply |

`o` (observation) and `x` (schedule) are copied without a check (§5.4): no other source has them, and comparison never treats them as semantic. The golden-only keys of §5.3 come from the extractor's own RPCs. With `--dense`, every era-driven `S` row of §9.3 must also cover its era: `protx_list`, `mn_payments` and `quorum_listextended` at every DIP0003 block, `protx_listdiff` at those from height 2, `quorum_info` per `qc` row, `deploymentinfo` at every block, `creditpool` at every V20 block, and `canonical_mnlist` at every 24th DIP0003 block.

**Restarts with other BIP9 parameters.** `feature_mnehf` and `feature_evo_shares_activation` restart their nodes with other `-vbparams`. The copy runs with the last start's, so the BIP9 values of records from earlier starts (`dep`, its `since`, the `ehf_height` check) cannot be checked against it. `extract.py` maps the i-th digest `H` to the i-th start of `chainargs.json`, refuses the extraction if any chainparams argument other than `-vbparams` differs, and passes the other runs as `--other-params-run`. Their records keep every other check there, and the extractor's `--summary` lists the heights of the merged `C` records whose BIP9 values it left unchecked. `extract.py` then checks those values on one more offline copy per parameter set, started with the first such start's command line (`-checkblocks=1 -checklevel=0`, since the copy only answers RPCs): `digest check-bip9 --run <run>…` applies the `bls`, `bur`, `dep` and `ehf_height` checks to the blocks with a tip `C` from those runs, and checks the `since` values of each such block whose child is not one of them against the child's reply on the same copy (the extraction does the same for its own blocks whose child came from another run). The copies must report the extraction's tip, and the heights they checked must include every unchecked one (they may include more: a block connected by runs of both parameter sets has one `s`, §9.2); `extract.json` records the summary and each group's runs, arguments, tip and heights (`"bip9"`), and `store-vectors.sh` checks them again, deriving the other-params runs anew from `chainargs.json` and the digest. Only a run whose config-file arguments equal the last start's qualifies, since only the command line is replayed.

## 14. Acceptance mapping

| Item | Evidence |
|---|---|
| spec with field classes (M0 exit 6, first sentence) | this file, reviewed |
| record crate | round-trip property tests; one golden line per record kind; strict-parser rejection tests |
| flag off is free | Core unit tests and the FT list match stock rc.3; `instructions:u` Δ ≤ 0.1 % (§13.2) |
| flag on is consistent | `feature_statedigest` (a)–(f) |
| `R` records | `statedigest_tests` (invalid block, dry run); `mining_basic`, `feature_block` recordings |
| goldens reproducible | two extractions byte-identical (§9.5) |
| goldens checked | every patch key of `s` against the copy (§13.5); `store-vectors.sh`'s checks (§13.1) |
| DEC-177 | regtest now (§13.4); testnet with the testnet node |
| recordings | §13.1 recordings with provenance |

## 15. Plan edits (for the manager's docs commit)

### 15.1 PLAN.md

**M0 exit 6 (PLAN:178)**, replace with:
> 6. The StateDigest v1 spec, with its field classes, is written (`designs/O8-statedigest-v1.md`). Golden extraction runs on **O8's patched v24 nodes**, never on the live oracles (their RPC use stays pure-read): regtest FT chains now, testnet when O8-T's node runs; mainnet waits for pasta (A1 §4.6 erratum E-O8-1).

**M0 content line (PLAN:165)**, replace the O7 item with:
> O7 (DROPPED, DEC-82; the StateDigest v1 spec, the record crate, the golden extractor and the testnet replica moved to O8, the comparator and its tests to O9a);

**O7 row (:656)**, append to the task text:
> · Pieces reassigned (O8 plan, 2026-10-12): spec, record crate, extractor, testnet replica → O8 / O8-T; comparator and comparator tests → O9a.

**O8 row (:657)**, replace:

| ID | Task | MS | Crates/repo | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| O8 | **StateDigest v1** (from O7): **(A)** the spec `designs/O8-statedigest-v1.md` (grammar, field classes, golden layout, comparison semantics) + the `dash-statedigest` record crate; **(B)** the `node/state-digest-v24.0` Core patch: hidden `-statedigestlog` (`C`/`R`/`D` records with effects, failed and `fJustCheck` attempts), `-recordstatedecisions` (`A` records), hidden `getcanonicalmnlist` (EV2 §6.3 item 3, DEC-177); **(C)** the golden extractor (`dash-conformance digest extract`, run on O8's own frozen copies) and the regtest FT recordings. The testnet node is split out as O8-T. v1's v23.1 branch is dropped (M-8) | M0 (A) → M2 (B, C) | Core branch `node/state-digest-v24.0`; rdc `dash-statedigest`, `dash-conformance` (`digest extract`), `contrib/node/statedigest/`, `test-vectors/statedigest/` | O1 (build recipe and depends cache only) | M | STATE | **D (spec)** | **impl O-m; rev Sol** (spec: Opus high, then Sol with the series) | NFR-CEQ-06 | flags off: Core unit tests and the FT list match stock rc.3, `instructions:u` Δ ≤ 0.1 % on a regtest `-reindex-chainstate`; flags on: `feature_statedigest` (two nodes' linear projections equal; `D`/`C` on invalidate/reconsider; `R` for a proposal and an invalid block; a copy's `-reindex-chainstate` reproduces the semantic fields; derived fields equal their RPCs); goldens extracted twice are byte-identical; DEC-177 on ≥ 20 regtest heights (`getcanonicalmnlist` = `oracle_mnlist_canonical` = dash-dml bytes); regtest recordings of the spec's FT list with provenance · BG: none (instructions overhead reported) |
| O8-T | **StateDigest testnet node** (deferred until disk frees, manager 2026-10-12): a patched v24 testnet node synced from the public network into scratch (`-assumevalid=0 -coinstatsindex=1`, no inbound), recording history and then live decisions (A1 m-12's intent; the live oracles stay stock); its frozen snapshots serve golden extraction and LQ1 §8 Q3's testnet replica; runbook | M2 | `contrib/node/statedigest/`, `test-vectors/statedigest/` | O8 | S | STATE | — | **impl O-m; rev Sol** | NFR-CEQ-06 | testnet history digests 0→tip with goldens; ≥ 3 days of live decisions with provenance; DEC-177 at ≥ 20 testnet heights; disk ≤ 20 GB · BG: never during bench windows |

**O9a row (:658)**, replace deps and acceptance:

| ID | Task | MS | Crates/repo | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| O9a | Comparator (from O7: field classes, `linear` and `stream` modes, per-key coverage, O8 spec §10) + first-mismatch dump + `RecordedDecisions` (O8 spec §7.2, for CH7b) + replay driver (`dash-conformance replay`) with `DigestTap<C: BlockConnector>`, the emitter seam CH3a/CH7a fill (rev1 split, F17). Two merges: **O9a-1** (comparator, dump, decisions, CLI) after O8-A; **O9a-2** (driver on V) after CH1-node merges (DEC-199 pattern) | M2–M3 | `dash-conformance` (`digest compare`, `replay`), `dash-statedigest` | **O8 (phase A; recordings i:O8-C)**, i:CH1-node (SB0 rev6.1) | M | STATE | — | **impl O-m; rev Sol** | NFR-CEQ-06 | comparator unit tests per class and mode; Core-vs-Core: two FT nodes compare clean, and a planted one-key mutation in each class is reported at the right block with the key named; replays a recorded FT regtest chain through V (`NullConnector` + `DigestTap`) and its header keys equal O8's goldens; dumps go to the private corpus by default (PLAN §7.5) · BG: — |

**CH3a (:570)**: add `i:O8` (goldens) to Deps, and append to Acceptance: "until a mainnet node exists (pasta), L2 runs on O8's regtest FT chains and, with O8-T, testnet pre-DIP3".

**CH7b (:577)**: append to Task: "`ReplayChainHelper` consumes O9a's `RecordedDecisions` (O8 spec §7)".

**compat/COVERAGE.md:301 and LQ1 §8 Q3**: "testnet replica → O8-T frozen snapshots"; "`getcanonicalmnlist` → O8 (B)".

### 15.2 A1 §4.6 errata

- **E-O8-1 (golden source).** Goldens come from O8's patched nodes (frozen copies), not from the BS2 oracles. The oracles' RPC use stays pure-read; the A1 table's `gettxoutsetinfo`, `getblockstats`, `masternode payments`, `protx list`, `quorum *`, `getcreditpoolinfo`, `getdeploymentinfo`, `getchainlockbyheight` and `getassetunlockstatuses` calls are not pure-read, and `quorum *` can write `llmq_S` rows.
- **E-O8-2 (decision source).** Decisions are recorded on O8's own patched node during live operation (O8-T), not on "the synced, live v24 oracles", which are stock binaries that must not be restarted with a patch. m-12's intent (record live, not during `-reindex-chainstate`) stands.
- **E-O8-3 (MuHash).** The per-block MuHash and `block_info` come from the golden pass (`gettxoutsetinfo muhash <h>` with `-coinstatsindex=1`), not from the patch: `CoinStatsIndex` updates on the scheduler thread, so at `ConnectBlock` time it has not seen the block. The patch keeps byte-level UTXO parity through `undo` (spent coins) and the raw block (created outputs).
- **E-O8-4 (grammar).** The `R` record carries `res` (`ok` for a passing `fJustCheck` attempt, `invalid`, `error`), the validation class and the reject reason; every record carries its origin (`tip`, `proposal`, `verifydb`, `replay`); and the decisions record carries the attempt's adjusted time, which replaying FT chains under mocktime needs. All records, `D` and `R` included, are canonical JSON lines (O8 spec §3–§4), not the space-separated `D <hash>` / `R <hash> <class> <reason>` sketch.

**Refinements that need no erratum** (A1 left them open): effects outside an attempt are dropped in v1 (§6); `R` covers `ConnectBlock` attempts only, pre-connect rejections being L3's (§1); assumeutxo chainstates are out of v1's scope (§8.4); CbTx, credit-pool and `getchainlockbyheight` per-block goldens are dropped as redundant (§5.3); v1's `mined-commitment index hash` is the per-block write set `qc` (§5.2).

### 15.3 Accepted deviation: the patch size

The plan targeted about 250 lines of Core product code. It came to **+564/−15** (`statedigest.{h,cpp}` 411 lines, the call sites about 153). Accepted by the manager on 2026-10-12 (sign-off 1 of the rulings after the Sol r1 hand-off); no change to the PLAN row is needed.

## 16. Decisions taken in this spec

| # | Decision | Alternative rejected | Why |
|---|---|---|---|
| 1 | JSON Lines with a canonical subset for every record | space-separated text for `D`/`R` (A1's sketch) | one parser; reject reasons and debug texts may contain spaces; `jq` works |
| 2 | classes as sub-objects `s`/`o`/`x` | a class column in a side table only | the comparator cannot apply the wrong class to a key |
| 3 | absent ≠ `null`; absence is uncovered | treat absence as equal | coverage is reported, never silently passed (O9a acceptance) |
| 4 | hashes in Core display order | internal byte order | matches every RPC, so dumps line up with `getblock` output |
| 5 | effects and decisions recorded at Core's own call sites, scoped to the open attempt | re-querying at the end of the attempt | records what Core actually used; no race with governance or CL updates on other threads |
| 6 | `qc` as the block's write set | a full index hash per block | O(1) per block; the active sets are already consensus-checked by `merkleRootQuorums` |
| 7 | `dep` + `bur` per block, `deploymentinfo` sparse | `getdeploymentinfo` per block | answers per block at no RPC cost |
| 8 | merged golden `C` records | separate patch and RPC golden files | one reference stream for linear mode |
| 9 | decisions keyed lookup by default, strict optional | strict only | rdashd may legitimately cache or reorder pure queries |
