# EV2: the deterministic masternode list (`dash_evo::dml`), Opus design

| | |
|---|---|
| Status | **Draft r0**, the Opus half of a D2 pair (PLAN §0 "D2"). The manager reconciles it with the independent Fable design. No code until the reconciled design is approved |
| Date | 2026-10-08 |
| Author | EV2 design sub-agent (Opus 5.5) |
| Task | PLAN §4.4 rows **EV2a** (`MnList` data model with `imbl`, unique properties, payee-queue structure; diff and snapshot codecs with a canonical EvoSnapshot; `EvoStore` plus an in-memory store) and **EV2b** (`build_new_list_from_block` in Core's order, PoSe, payee selection with the queue and a reference twin, confirmations, collateral spends including ordinary transactions). PRD FR-EVO-01…05, FR-STO-08. Quirks 13, 23, 24, 25, the DML half of 26, and the `AddMN` contract that quirk 28 (LQ1b) relies on |
| Binding inputs | A1 §2.5, §2.9 (evo column families), §2.10 with §2.10.1 (effects) and §2.10.2 (views), §3.5, §3.6, §3.10, §3.11, §4.4, §4.6, Appendix A/B/E; PLAN §0, §3, §4.4 (EV0–EV4, LQ1a/b), §8.2; R4 §1.7, §1.8, §1.15, R4-D §3; O17 `O17-pin-delta/evo.md` (EVO-08, -11, -13, -15, -25, -28, -29), `O17-release-profile.md` §4.1; DEC-05, -11, -16, -24 (D3), -30 (N3), -37, -41; CS0 design §4 and §8 |
| Code verified at | Dash Core **v23.1.8** (728f505583) and **v24.0.0-rc.3** (0364ed626e), read with `git show <tag>:<path>`; the vendored immer is identical at both tags (`git diff --stat v23.1.8 v24.0.0-rc.3 -- src/immer` is empty); oracle shim `node/oracle-shim-v24.0` 23769258c2; base-sdk 33fcc59; `imbl` 7.0.2 source |
| Evidence | `/work/scratch/ev2-opus/`: `scripts/champ_order.py` with `data/tn-protx.json`, `data/tn-diff-full.json`, `data/tn-diff-2000.json` (Core's iteration order reproduced exactly against the testnet oracle at height 1,133,744); `bench-imbl/` (a persistent-list model at mainnet scale, measured in instructions retired) |

Core anchors are written `v23:<line>` / `v24:<line>` (file named once per row). Where one number is given, both tags agree.

---

## 0. Summary

1. **What EV2 builds.** `dash_evo::dml`, the pure deterministic masternode list:
   - EV2a: the `MnList` value type, Core's unique-property map, the secondary indexes, the diff type, the canonical codecs, `CanonicalMNListHash`, Core's iteration order, the DML part of `EvoStore`, an in-memory store and list reconstruction from snapshots and diffs;
   - EV2b: `build_new_list_from_block` (Core `RebuildListFromBlock`), PoSe, payee selection with a reference twin, confirmations, collateral spends, and the DML part of `CDeterministicMNManager::{ProcessBlock, UndoBlock}`.
2. **Data model.**
   - The list is a set of `imbl` 7 persistent maps, so a clone is O(1) and consecutive lists share structure:
     - MNs keyed by proTxHash in Core's `uint256` order (memcmp of the internal bytes);
     - the internal-ID map;
     - Core's unique-property map keyed by `SerializeHash(value)` with reference counts.
   - Five secondary indexes remove Core's O(n) scans per block: the payee queue, the decaying-PoSe set, the unconfirmed set, the evonode-streak set and a typed collateral mirror. Every mutation goes through one choke point that keeps them consistent with the maps; the reference twin and an invariant checker verify this.
   - An MN is split into a small `Copy` **hot** part (payment heights and PoSe) and a shared `Arc` **cold** part (keys, addresses, scripts). The per-block churn (PoSe decay, payee bookkeeping) therefore copies about 20 bytes and does no hashing.
3. **Core's iteration order is not memcmp order (A1 erratum).**
   - `immer::map` with `ImmerHasher = ReadLE64(proTxHash)` iterates in CHAMP trie order. That order is observable:
     - in the bytes of every `mnlistdiff`/`qrinfo` (`BuildSimplifiedDiff`), so EV4's byte-identity target depends on it;
     - in `protx list` and `masternodelist`;
     - in `GetMNByOperatorKey` (first match).
   - EV2a reproduces it exactly. The order is a pure function of the key set because immer's CHAMP is canonical. It was verified on the live testnet oracle (§2.9). Proposed **quirk 41**.
4. **Codecs.**
   - Our `mn_snapshot` value is v24's `SerializeCanonicalMNList` byte for byte, and `CanonicalMNListHash = SHA256d` of it.
   - Our `mn_diff` value is v24's `SerializeCanonicalMNListDiff`.
   - Both encodings are release-independent for every pre-V24 state, so one codec serves both profiles and the v24 oracle's digests are valid for mainnet (A1 §2.25).
5. **Pins.**
   - `RebuildListFromBlock` is identical in v23.1.8 and v24.0.0-rc.3 for every pre-V24 path except **EVO-15**: v24 checks a ProUpServTx's `nType` first (`bad-protx-type`), v23 checks it last, after `bad-protx-type-mismatch`. EV2b reads `UngatedRules.proupserv_type_in_special_tx`; O17 does not yet list EV2b as a consumer.
   - The V24-gated paths (EVO-08, -11, -13) follow v24's rules.
   - v23.1.8's own V24 rule set is not emulated (Q1).
6. **Order and errors.**
   - Core's transition order is reproduced step by step (§3.2).
   - Core's explicit rejects keep their strings.
   - The exceptions Core's `AddMN`/`UpdateMN`/`RemoveMN` throw inside a block become `failed-procspectxsinblock`, as Core's catch makes them. Which string a block gets can therefore depend on transaction order, e.g. a platformNodeID colliding with an owner keyID (proposed **quirk 42**).
7. **Effects.**
   - `process_block` emits `ResetPlatformBan { pro_tx_hash, height }` for every ProUpServTx, as Core does inside `ProcessBlock`. It does so before the DIP3-enforcement-hash check, so the effect survives `bad-dip3-enf-block`. A1's effect lacks `height`, which Core's `SetPlatformBan` needs (erratum).
   - Member-selection effects from LQ1's source pass through untouched.
8. **Persistence** (A1 §3.5).
   - One diff per DIP3 block and a snapshot every 576 blocks (plus the first DIP3 block) go into the Durable B1 batch. They are never erased; disconnects keep them, as Core does.
   - Recovery needs no DML replay.
   - Historical lists are rebuilt from the nearest snapshot plus diffs. Caches are not observable.
9. **Performance.**
   - A model of mainnet-scale blocks (3,500 MNs, ~370 decaying penalties, 100 inputs per block) measures **1.44 M instructions per block on the indexed path against 7.6 M for a Core-shaped twin (5.3×)**, and **4.5×** at 20,000 MNs.
   - The floor is Core's eager PoSe decay: one update per penalised MN per block. A lazy representation would remove it but is deferred (Q5).
   - The PLAN target, `build_new_list_from_block` ≥ 2× the twin on mainnet blocks, has margin. The never-slower check against dashd uses a DEC-16 Core bench over the same blocks.
10. **Verification.**
    - L0: Core's `evo_deterministicmns_tests` and `evo_snapshot_tests`.
    - Model fuzzing against the twin (`mnlist_payee_queue`, `mnlist_model`).
    - L1: `diff_dml_apply` (≥ 24 CPU-h, both pins) plus three new codec and order targets. Four new or extended oracle entry points are needed, including a test seam for quorum members (§6.3).
    - L2: the per-block `CanonicalMNListHash` digest over mainnet 1,028,160 – 1,078,159. Starting at DIP3 needs no imported state.
    - L3 operators handed to O10.
11. **Phases.** EV2a-1 lands first (the read API LQ1a needs, the in-memory store and builders), then codecs and the store. EV2b goes pre-V24, then V24, then the performance gate (§7).
12. **Open questions** (§8): v23's own V24 rules on regtest; ownership of `process_block`; the DEC-16 dump RPC and oracle seams; two new quirks; lazy decay; the `imbl` pin; the acceptance harness; a tip snapshot for startup.

---

## 1. Scope and non-goals

**EV2a (M2–M3, `dash-evo::dml`):**
- types: `Dmn`, `MnHot`, `MnCold`, `MnType`, `UniqueKey`, `PayeeKey`;
- `MnList` with Core's `AddMN`/`UpdateMN`/`RemoveMN` semantics, including rollback, error classes and the reference-count rules of the unique map (FR-EVO-01; quirk 23);
- the secondary indexes and counts, with an invariant checker;
- the read API that EV1, EV3, EV4, LQ1a/b, GV1, AM1, RP2a and P6b consume (§2.12);
- Core's iteration order (`core_order`, proposed quirk 41);
- `MnListDiff` and `MnStateDiff` (Core's field bitmask), `diff_to` (`BuildDiff`) and `apply_diff` (`ApplyDiff`);
- codecs: the canonical list (`SerializeCanonicalMNList`), `CanonicalMNListHash`, the canonical diff, `CDeterministicMN` and the state at `SER_DISK`, and Core's on-disk `dmn_S3`/`dmn_D4`/`dmn_D3` readers for EV4's importer (FR-EVO-12 uses them);
- `DmlStore` (the DML part of EV0's `EvoStore`), `DmlDelta`, `MemDmlStore`, `ListCache` and `list_for_block` (Core `GetListForBlockInternal`; FR-STO-08);
- test kit: list and MN builders, and a synthetic block generator (feature `test-utils`).

**EV2b (M3, `dash-evo::dml`):**
- `build_new_list_from_block` in Core's exact order, with every reject string;
- PoSe (`pose_punish`, `decrease_scores`, `calc_penalty`);
- confirmations, payee bookkeeping and evonode streaks (FR-EVO-02/03);
- `payee` (`GetMNPayee`), `projected_payees` (`GetProjectedMNPayees`) and the reference twin (FR-EVO-04; quirk 13);
- collateral spends by any transaction (quirk 25);
- the V24 paths: types 11 and 12, `SetStateVersion`, cross-scheme operator-key probes and the shared-collateral spend rule (FR-EVO-05; EVO-08, -11, -13);
- `process_block` / `undo_block`, the DML part of `CDeterministicMNManager::{ProcessBlock, UndoBlock}` (Q2);
- `-consensusxcheck` wiring points; criterion and iai benches.

**Not in scope (owner):**
- `CheckProRegTx`/`CheckProUpServTx`/… contextual checks: **EV1**. EV2b runs after them inside `ProcessSpecialTxsInBlock`, and only Core's in-list re-checks are EV2b's.
- Payload and NetInfo types and decoding: **BS6a/BS6c**.
- BLS lazy keys and re-encoding: **BS11a-t**.
- `GetBlockTxOuts`, payee and value validation: **EV3**. EV2b hands EV3 the payee it already computed.
- SML entries, `merkleRootMNList`, `mnlistdiff`: **EV4**. EV2a provides `core_order` and an SML-hash slot per cold state.
- Member selection (`GetAllQuorumMembers`) and rotation snapshots: **LQ1a/b**. EV2b calls it through `QuorumMemberSource`, and EV2a gives LQ1b `MnList::scratch` + `try_add` with Core's exact `AddMN` failure semantics.
- Orchestration in `process_special_txs_in_block`, `EvoDelta` in the B1 batch, the `MnListChanged` event, `EvoReadView` and the concurrent list cache: **CH7a**.
- Platform-ban state (`mncache`): **SP1/AM1** apply the effect.
- RPC JSON (`protx list/info/diff/listdiff`, `masternodelist`, `masternode count/winners/payments`, `evodb verify/repair`): **RP2a/RP2b**.
- v24's `ApplyDiffForSnapshot` and the EvoSnapshot v3 container (assumeutxo): out of scope (NG-6).

**PRD mapping.** FR-EVO-01 (EV2a), FR-EVO-02…04 (EV2b), FR-EVO-05 (EV2b phase 3), FR-STO-08 (EV2a store), FR-EVO-12 (readers for EV4). Stories: US-MN-10 (protx RPCs, through RP2a), US-LCS-01/04/05 (serving, through EV4/P6b), US-NODE-01 (sync).

---

## 2. Data model and public API

### 2.1 Module layout and dependencies

```
dash-evo/src/dml/
  mod.rs        re-exports; the ARCHITECTURE.md table of Core files mirrored
  types.rs      MnType, RawMnType, Dmn, MnHot, MnCold, PayeeKey, Counts
  unique.rs     UniqueKey and the Core serializations it hashes
  list.rs       MnList: storage, try_add / update / remove (AddMN / UpdateMN / RemoveMN), reads
  index.rs      the five secondary indexes and counts; check_invariants()
  order.rs      core_order(): immer's CHAMP iteration order
  payee.rs      pay_height, payee(), projected_payees()
  pose.rs       calc_max_pose_penalty, calc_penalty, pose_punish, decrease_scores
  diff.rs       MnListDiff, MnStateDiff, diff_to (BuildDiff), apply_diff (ApplyDiff)
  codec.rs      canonical list / diff / Dmn / state codecs, canonical_mn_list_hash; core_disk::{s3, d4, d3}
  store.rs      DmlStore, DmlDelta, MemDmlStore, ListCache, list_for_block
  build.rs      build_new_list_from_block (EV2b)
  process.rs    process_block, undo_block (EV2b)
  reference.rs  the twin: RefMnList and build_new_list_from_block_ref   [cfg(any(test, feature = "xcheck"))]
  testkit.rs    builders and a synthetic block generator                 [feature = "test-utils"]
```

Dependencies:
- `dash-consensus` (CS0):
  - `CoreWrite`, `HashWriter`;
  - `Block`, `Tx`, `BlockId`, `ChainAncestry`;
  - `BlockValidationError`, `UngatedRules`;
  - `CoinsView`/`Coin` (CS5a).
- `dash-primitives` (BS6a/BS6c): payload types, `NetInfo`, `OutPoint`, `Script`, `KeyId`, `PlatformNodeId`, `BlockHash`, `ProTxHash`.
- `dash-pkc` (BS11a-t): `OperatorKey`, i.e. `BlsLazy<PublicKey>` with its encoding scheme.
- `imbl = "=7.0.2"`, pinned exactly (Q6), so a `cargo update` cannot change iteration or diff behaviour unreviewed.
- No `HashMap` anywhere: every container is an `imbl::OrdMap`/`OrdSet` or a `Vec`.

### 2.2 Keys

- **`ProTxHash` order.** Core compares `uint256` with `base_blob::Compare`, a `memcmp` of the internal (little-endian) bytes. The tie-break of `CompareByLastPaid` (`deterministicmns.cpp` v23:168-181 / v24:143-156) and the canonical encoding's sort (`snapshot.h` v24:244) both use it. `ProTxHash: Ord` must be lexicographic over the **internal** bytes; a unit test pins it against Core-formatted hex, because display hex is byte-reversed.
- **`UniqueKey`** is Core's `GetUniquePropertyHash` (`deterministicmns.h` v23:488-510 / v24:526-548): `SerializeHash(value, SER_GETHASH, PROTOCOL_VERSION)`, i.e. SHA256d of the value's serialization. CService adds `ADDRV2_FORMAT` when the address is not ADDRv1-compatible. The protocol version (70240 / 70242) changes none of these encodings; only the ADDRv2 bit matters.

  | Property | Serialized bytes (hashed with SHA256d) | Added by `AddMN` when |
  |---|---|---|
  | collateral | `txid[32] ‖ u32 n` | always |
  | each NetInfo entry | CService: ADDRv1 `ip[16] ‖ port BE16` if v1-compatible, else ADDRv2 `net u8 ‖ CompactSize len ‖ addr ‖ port BE16`; DomainPort: its serialization | per entry; an entry that is neither throws "invalid address" |
  | owner | `keyIDOwner[20]`, or every share's `keyIDOwner[20]` for a shared MN | always (a null key makes `AddUniqueProperty` fail, hence a throw) |
  | operator | the 48 key bytes **in the key's own encoding** (legacy iff the state's `nVersion == 1`) | if the key is not null |
  | platformNodeID | `[20]` | Evo and non-null |

  The table makes the cross-type collisions explicit (quirk 23):
  - `CKeyID` and `uint160` both serialize to 20 raw bytes, so an Evo platformNodeID collides with an equal owner keyID;
  - the same operator key under the legacy and the basic encoding occupies two slots (pre-V24 uniqueness is per encoding);
  - a 36-byte ADDRv2 TORv3/I2P service has the same length as a collateral outpoint.

  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
  pub struct UniqueKey(Hash256);
  impl UniqueKey {
      pub fn collateral(op: &OutPoint) -> Self;
      pub fn service(s: &Service) -> Self;              // ADDRv1 unless !is_addr_v1_compatible()
      pub fn domain(d: &DomainPort) -> Self;
      pub fn key_id(k: &KeyId) -> Self;                 // == platform_node_id(x) for equal 20 bytes (quirk 23)
      pub fn platform_node_id(p: &PlatformNodeId) -> Self;
      pub fn operator_key(k: &OperatorKey) -> Self;     // the stored encoding's bytes
  }
  ```

### 2.3 `Dmn`: hot and cold

Core's `CDeterministicMN` (`deterministicmns.h:43-94`) holds a `shared_ptr<const CDeterministicMNState>`. Every update copies the whole state (`dmnstate.h` v23:33-155 / v24:33-175): about 400 bytes plus heap scripts, a shared NetInfo and a lazy BLS wrapper with a cached hash. Per block, almost all updates are PoSe decay and payee bookkeeping, which touch a handful of `int`s. The Rust split follows that:

```rust
pub struct Dmn {
    pub pro_tx_hash: ProTxHash,
    pub internal_id: u64,                 // VARINT on disk; consensus-deterministic from genesis (snapshot.h v24:229-236)
    pub collateral: OutPoint,
    pub operator_reward: u16,             // immutable after registration
    pub mn_type: MnType,                  // Regular = 0, Evo = 1 (u16 on the wire)
    pub hot: MnHot,
    pub cold: Arc<MnCold>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MnHot {                         // Core `int` fields
    pub last_paid_height: i32,            // nLastPaidHeight      (0 = never)
    pub consecutive_payments: i32,        // nConsecutivePayments
    pub pose_penalty: i32,                // nPoSePenalty
    pub pose_revived_height: i32,         // nPoSeRevivedHeight   (-1 = never)
    pose_ban_height: i32,                 // nPoSeBanHeight       (-1 = valid); private as in Core
}
impl MnHot { pub fn is_banned(&self) -> bool; pub fn banned_height(&self) -> i32;
             pub fn ban_if_not_banned(&mut self, h: i32); pub fn revive(&mut self, h: i32); }

pub struct MnCold {
    pub version: i32,                     // nVersion (Core `int`; 4 bytes on disk)
    pub registered_height: i32,
    pub revocation_reason: u16,
    pub confirmed_hash: Hash256,
    pub confirmed_hash_with_pro_tx: Hash256,   // single SHA256(proTxHash ‖ confirmedHash), dmnstate.h v24:162-169
    pub key_id_owner: KeyId,
    pub pub_key_operator: OperatorKey,         // BS11a-t; Core CBLSLazyPublicKey semantics (§3.6)
    pub key_id_voting: KeyId,
    pub net_info: Arc<NetInfo>,                // MnNetInfo (v < 3) | ExtNetInfo (v >= 3)
    pub script_payout: Script,                 // v < 3
    pub payouts: Vec<PayoutShare>,             // v >= 3
    pub shares: Vec<CollateralShare>,          // v >= 3; non-empty ⇔ shared MN
    pub early_period_blocks: u32,
    pub early_penalty: i64,
    pub script_operator_payout: Script,
    pub platform_node_id: PlatformNodeId,
    pub platform_p2p_port: u16,                // v < 3
    pub platform_http_port: u16,               // v < 3
    derived: ColdDerived,                      // OnceLock caches; never serialized
}
struct ColdDerived {
    unique_keys: OnceLock<UniqueKeys>,          // the keys this state holds in the unique map (§2.5)
    sml_hash: [OnceLock<Hash256>; 2],           // EV4's SML entry hash, indexed by is_valid
}
```

- **Which fields are hot.** Exactly the five fields the per-block steps change without touching keys or addresses: payee bookkeeping (`last_paid`, `consecutive`), `DecreaseScores` (`penalty`), and `PoSePunish`/`BanIfNotBanned`/`Revive` (`penalty`, `ban`, `revived`). A ban by ProUpRegTx/ProUpRevTx also changes cold fields, and those transactions build a new cold anyway.
- **The SML hash cache is keyed by validity.** The SML entry depends on cold fields, `nType`, the proTxHash and `isValid = !banned` (`to_sml_entry`, `deterministicmns.cpp` v24:43-50). A ban or revive picks the other slot and never rehashes the cold part. EV4 fills the slots; EV2a only owns them. This is A1 §3.6 lever 2 without per-block invalidation from PoSe decay.
- **Field equality** is per field in Core's member order (§2.7). `Dmn: PartialEq` compares fields, with an `Arc::ptr_eq` fast path on `cold`.
- **A payload's `nType`** is `RawMnType(u16)`, compared numerically with the MN's type (EVO-15 needs out-of-range values). `MnType` exists only for stored MNs.

### 2.4 `MnList`

```rust
#[derive(Clone)]                                       // O(1): one Arc increment per map (8) and for a cached order
pub struct MnList {
    block_hash: BlockHash,                             // null while building (Core SetBlockHash(uint256()))
    height: i32,                                       // -1 for the pre-DIP3 default list (A1 erratum: i32, not u32)
    total_registered: u32,                             // nTotalRegisteredCount (Core uint32_t)
    // Core's three maps
    mns: OrdMap<ProTxHash, Arc<Dmn>>,
    by_internal_id: OrdMap<u64, ProTxHash>,
    unique: OrdMap<UniqueKey, UniqueSlot>,             // UniqueSlot { owner: ProTxHash, refs: u32 }
    // secondary indexes (index.rs); derived from `mns`, never serialized
    payee_queue: OrdSet<PayeeKey>,                     // valid MNs, ordered as CompareByLastPaid
    decaying: OrdSet<ProTxHash>,                       // valid ∧ pose_penalty > 0          (DecreaseScores)
    unconfirmed: OrdSet<(i32, ProTxHash)>,             // confirmed_hash null, by registered (confirmations)
    evo_streak: OrdSet<ProTxHash>,                     // Evo ∧ consecutive_payments != 0   (streak reset)
    by_collateral: OrdMap<OutPoint, ProTxHash>,        // typed mirror of the COutPoint slots (spend pass)
    counts: Counts,                                    // GetCounts() in O(1)
    core_order: OnceLock<Arc<[ProTxHash]>>,            // derived; reset by every mutation
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PayeeKey { pub height: i32, pub pro_tx: ProTxHash }   // (CompareByLastPaid_GetHeight, memcmp)
pub struct Counts { pub total_mn: u32, pub total_evo: u32, pub valid_mn: u32, pub valid_evo: u32 }
impl Counts { pub fn total(&self) -> u32; pub fn enabled(&self) -> u32;
              pub fn total_weighted(&self) -> u32; pub fn valid_weighted(&self) -> u32 }  // Evo weight 4 (dmn_types.h)
```

**Invariants** (asserted by `check_invariants()` in debug, fuzz and xcheck builds):
- **I-U1.** Every MN's unique keys (§2.2 table) map to it with the right reference count, and every slot is owned by an MN in `mns`. Hence `get_by_collateral(op)` never dangles (Core would dereference null).
- **I-U2.** `by_collateral[m.collateral] = m` for every MN. Together with I-U1: *there is an MN whose collateral equals `op`* ⇔ `unique[H(op)]` is owned by an MN whose collateral equals `op`. This is what makes the typed spend check equal to Core's (§3.5).
- **I-X.** Each index equals its definition recomputed from `mns`. `counts` equals a recount.
- **I-P.** At most one MN has `last_paid_height == height` (the evonode-continuation lookup, §3.4, depends on it). Proof: `nLastPaidHeight` is only written by payee bookkeeping (one MN per block, value = the block height) or copied from older states, and registrations start at 0.

### 2.5 Mutations: Core's `AddMN` / `UpdateMN` / `RemoveMN`, exactly

These are the semantics LQ1b's quirk 28 swallows, so the exact failure conditions are part of the contract, not an implementation detail.

```rust
impl MnList {
    /// CDeterministicMNList() — the pre-DIP3 default (hash 0, height -1, count 0); also LQ1b's MnsUsedAtH scratch list.
    pub fn default_pre_dip3() -> MnList;
    pub fn scratch() -> MnList { Self::default_pre_dip3() }

    /// AddMN (deterministicmns.cpp v23:420-485 / v24:439-516). All-or-nothing: on Err the list is unchanged.
    pub fn try_add(&mut self, dmn: Arc<Dmn>, bump_total: bool) -> Result<(), MnListError>;
    /// UpdateMN(oldDmn, newState) (v23:487-571 / v24:518-602).
    pub(crate) fn update(&mut self, pro_tx: &ProTxHash, hot: MnHot, cold: Arc<MnCold>) -> Result<(), MnListError>;
    /// UpdateMN for hot-only changes: no unique-map work is possible, so it cannot fail except NotFound.
    pub(crate) fn update_hot(&mut self, pro_tx: &ProTxHash, hot: MnHot) -> Result<(), MnListError>;
    /// RemoveMN (v23:581-647 / v24:612-679).
    pub(crate) fn remove(&mut self, pro_tx: &ProTxHash) -> Result<(), MnListError>;
}
```

- **`try_add` order and failures**, in Core's order:
  1. the proTxHash is already present → `DuplicateProTx`;
  2. the internalId is already present → `DuplicateInternalId`;
  3. collateral (a dup → `DuplicateCollateral`);
  4. each NetInfo entry in `GetEntries()` order (dup → `DuplicateAddress`; neither AddrPort nor DomainPort → `InvalidAddress`);
  5. owner: every share key if shared, else `keyIDOwner`, and a null keyID fails;
  6. operator, if not null;
  7. platformNodeID, if Evo and not null.

  Then the MN goes into `mns` and `by_internal_id`. If `bump_total`, `total_registered = max(id + 1, total)`, truncated to `u32` as Core assigns the `uint64` result to a `uint32_t`.

  "Duplicate" means the slot exists **and is owned by another proTxHash**; the same owner just increments `refs` (`AddUniqueProperty`, `deterministicmns.h` v23:512-530 / v24:550-568).
- **`update`** follows `UpdateMN`'s order:
  1. the NetInfo entries: if the NetInfo objects differ (deep compare, Core `shared_ptr_not_equal`), delete every old entry, then add every new one;
  2. the owner key via `UpdateUniqueProperty`;
  3. the operator key;
  4. platformNodeID, only for Evo.

  `UpdateUniqueProperty` returns early when old equals new. **v24** compares the serialized hash for BLS keys (`deterministicmns.h` v24:590-603); **v23** compares with `operator==`, which is point equality (v23:552-566). The two differ only when a key changes encoding but not point, which needs V24's `SetStateVersion`; pre-V24 states always hold the encoding implied by `nVersion` (O17 `evo.md` §3, re-verified here). EV2a implements v24's rule.

  Owner keys of shared MNs are immutable after registration and are never updated.
- **`remove`** deletes in the same order: collateral, entries, owner(s), operator, platformNodeID. A slot whose `refs` is 1 is erased; otherwise `refs` is decremented.
- **Rollback.** Every method snapshots `unique` (an O(1) clone) and restores it on `Err`, as Core's `mnUniquePropertyMapSaved`. The other maps are written only after every check passed.
- **One choke point.** `try_add`, `update`, `update_hot` and `remove` all end in `IndexDelta::apply(old: Option<&Dmn>, new: Option<&Dmn>)`. It updates the five indexes and `counts` from the old and new values and nowhere else, then clears `core_order`. `PayeeKey` uses `pay_height` (§3.4).
- **Errors.** `MnListError` is an enum with one variant per Core `runtime_error` text in the four functions. `core_message()` renders Core's text (`"%s: Can't add a masternode %s with a duplicate keyIDOwner=%s"`, …) for the `FAILURE!` log line (`specialtxman.cpp` v23:702 / v24:994). Inside `build_new_list_from_block` every `MnListError` becomes `DmlBuildError::Exception` → `failed-procspectxsinblock` (§3.8).
- **Unique keys are cached** in `MnCold::derived.unique_keys`: one SHA256d per property per cold state, ever. Core recomputes the hashes on each `Add`/`Update`/`Remove`; v24's `UpdateUniqueProperty` hashes both BLS keys even when nothing changes.

### 2.6 Read API

```rust
impl MnList {
    pub fn block_hash(&self) -> &BlockHash;   pub fn height(&self) -> i32;   pub fn total_registered(&self) -> u32;
    pub fn counts(&self) -> Counts;                                              // GetCounts
    pub fn get(&self, h: &ProTxHash) -> Option<&Arc<Dmn>>;                       // GetMN
    pub fn get_valid(&self, h: &ProTxHash) -> Option<&Arc<Dmn>>;                 // GetValidMN
    pub fn is_valid(&self, h: &ProTxHash) -> bool;  pub fn is_pose_banned(&self, h: &ProTxHash) -> bool;
    pub fn get_by_internal_id(&self, id: u64) -> Option<&Arc<Dmn>>;
    pub fn get_by_collateral(&self, op: &OutPoint) -> Option<&Arc<Dmn>>;         // GetMNByCollateral: owner of H(op), Core semantics
    pub fn get_valid_by_collateral(&self, op: &OutPoint) -> Option<&Arc<Dmn>>;
    pub fn collateral_owner(&self, op: &OutPoint) -> Option<&Arc<Dmn>>;          // typed: MN whose collateral == op (I-U2)
    pub fn get_by_service(&self, s: &Service) -> Option<&Arc<Dmn>>;
    pub fn get_by_operator_key(&self, k: &BlsPublicKey) -> Option<&Arc<Dmn>>;    // GetMNByOperatorKey: first match in core_order()
    pub fn has_unique(&self, k: &UniqueKey) -> bool;                             // HasUniqueProperty
    pub fn unique_owner(&self, k: &UniqueKey) -> Option<&Arc<Dmn>>;              // GetUniquePropertyMN
    pub fn has_operator_key_under_any_scheme(&self, k: &BlsPublicKey, except: &ProTxHash) -> bool; // v24 deterministicmns.h:471-482
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Dmn>>;                       // memcmp order: canonical encoding, SML root
    pub fn iter_valid(&self) -> impl Iterator<Item = &Arc<Dmn>>;
    pub fn core_order(&self) -> Arc<[ProTxHash]>;                                // immer CHAMP order (§2.9)
    pub fn payee(&self, r: PayeeRules) -> Option<&Arc<Dmn>>;                     // GetMNPayee (§3.4)
    pub fn projected_payees(&self, r: PayeeRules, n: usize) -> Vec<Arc<Dmn>>;    // GetProjectedMNPayees
    pub fn calc_max_pose_penalty(&self) -> i32;  pub fn calc_penalty(&self, percent: i32) -> i32;
    pub fn diff_to(&self, to: &MnList) -> MnListDiff;                            // BuildDiff (§2.7)
}
pub struct PayeeRules { pub v19: bool, pub mn_rr: bool }   // DeploymentActiveAfter(pindexPrev, V19 / MN_RR)
```

`get_by_collateral` keeps Core's unique-map semantics because the ProRegTx replacement step and EV1's checks rely on them (§3.6). `collateral_owner` is the typed fast path, used only where Core also checks `dmn->collateralOutpoint == prevout`.

### 2.7 Diffs

```rust
pub struct MnListDiff {
    pub added: Vec<Arc<Dmn>>,              // sorted by (internal_id, pro_tx_hash): canonical (snapshot.h v24:307-311)
    pub updated: Vec<(u64, MnStateDiff)>,  // sorted by internal_id
    pub removed: Vec<u64>,                 // ascending (Core std::set)
}
pub struct MnStateDiff { pub fields: u32, values: MnStateFields }   // Core Field_* bits, v24 layout (dmnstate.h v24:180-204)
impl MnStateDiff {
    /// CDeterministicMNStateDiff(a, b) (dmnstate.h v23:221 / v24:250-281): per member, in Core's member
    /// order; NetInfo by deep comparison; the operator key by its serialized hash (v24); nVersion is forced
    /// in whenever the operator key or NetInfo changed.
    pub fn between(a: &Dmn, b: &Dmn) -> MnStateDiff;
    pub fn apply(&self, hot: &mut MnHot, cold: &MnCold) -> (MnHot, Option<Arc<MnCold>>);  // ApplyToState; cold rebuilt only if a cold bit is set
}
impl MnList {
    /// ApplyDiff (deterministicmns.cpp v23:396-418 / v24:371-393): removed, then added (bumping the total), then updated.
    pub fn apply_diff(&mut self, block: BlockHash, height: i32, d: &MnListDiff) -> Result<(), MnListError>;
}
```

- **`diff_to`** walks `imbl::OrdMap::diff`. In `imbl` 7 this skips shared subtrees (`advance_skipping_shared`, `ord/map.rs:1929-1963`), so the cost is O(changes · log n).
  - Add and Remove items become `added`/`removed` (by internal ID).
  - An Update item becomes `MnStateDiff::between` and is kept only if `fields != 0`. That is Core's `BuildDiff` (v23:361-394 / v24:336-369), which compares pointers first and then field by field.
  - Core instead iterates **every** MN in `to`, with a HAMT lookup in `from` for each.
- **The v24 bit layout is used for every profile.** v23.1.8 already has the nVersion-first format (`Field_nVersion = 0x0001`, `dmnstate.h` v23:160); v24 adds bits `0x80000…0x400000` for V24-only fields, which never appear before V24. The legacy layout (`dmn_D3`) is read only by the importer.
- **Order of `updated`.** v23 serializes `updatedMNs` in `std::unordered_map` order (`deterministicmns.h` v23:591-605), and v24 sorts them (v24:639-659). Our diffs are always sorted. The order is visible only in Core's evodb bytes (import-only) and in `protx diff` JSON, which O14 compares as a set (handed to RP2a/O14).
- **Application order vs build order.** `apply_diff` uses Core's order (removed, added, then updated in ascending ID). Core iterates an `unordered_map`, so its update order is unspecified.
  - For a per-block diff no order can conflict: EV1 checks every unique property against the **pprev** list. So a valid block can never move a property from one surviving MN to another; only a removal (replacement, or a collateral spend) can free one.
  - Multi-block diffs (v24's `ApplyDiffForSnapshot`) are never built here.

### 2.8 Codecs and `CanonicalMNListHash`

The snapshot value **is** Core v24's `SerializeCanonicalMNList` (`snapshot.h` v24:237-247). The diff value **is** `SerializeCanonicalMNListDiff` (v24:304-328). `CanonicalMNListHash` is the SHA256d of the list encoding (`snapshot.cpp` v24:150-155):

```
list    = blockHash[32] ‖ i32 height ‖ u32 totalRegistered ‖ CompactSize(n) ‖ n × Dmn      (Dmn sorted by proTxHash, memcmp)
Dmn     = proTxHash[32] ‖ VARINT(internalId) ‖ txid[32] ‖ u32 n ‖ u16 operatorReward ‖ State ‖ u16 nType
State   = i32 nVersion ‖ i32 registered ‖ i32 lastPaid ‖ i32 consecutive ‖ i32 penalty ‖ i32 revived ‖ i32 banHeight
          ‖ u16 revocationReason ‖ confirmedHash[32] ‖ confirmedHashWithProRegTx[32] ‖ keyIDOwner[20]
          ‖ pubKeyOperator[48]            (encoded legacy iff nVersion == 1; a null key is 48 zero bytes)
          ‖ keyIDVoting[20] ‖ NetInfo     (MnNetInfo = CService ADDRv1 18 bytes, CService{} when empty; ExtNetInfo iff nVersion >= 3)
          ‖ (nVersion >= 3 ? payouts ‖ shares ‖ u32 earlyPeriodBlocks ‖ i64 earlyPenalty : CScript scriptPayout)
          ‖ CScript scriptOperatorPayout ‖ platformNodeID[20] ‖ (nVersion < 3 ? u16 platformP2PPort ‖ u16 platformHTTPPort)
diff    = CompactSize(a) ‖ a × Dmn (sorted by (internalId, proTxHash)) ‖ CompactSize(u) ‖ u × (VARINT id ‖ StateDiff)
          ‖ CompactSize(r) ‖ r × VARINT id
StateDiff = VARINT(fields) ‖ for each member in Core order whose bit is set: its encoding
hash    = SHA256d(list)
```

Sources: `dmnstate.h` v23:83-~110 / v24:93-125; `deterministicmns.h:69-84`; `snapshot.h` v24:237-328.
- `nType` is always written: the disk stream's version equals `CLIENT_VERSION`.
- VARINT is Core's `VarIntMode::DEFAULT`.

**One encoding for both profiles.** v23.1.8 has no `snapshot.*`. But the `CDeterministicMN` and state encodings it would feed are byte-identical for every pre-V24 state: the diff above adds only V24-gated branches. So a v23-profile node's lists hash exactly as the v24 oracle's do over mainnet history (A1 §2.25 point 1). The pre-DIP3 default list hashes too (height −1, `snapshot.h` v24:339-340), which is why `height` is `i32`.

```rust
pub fn encode_canonical_list(list: &MnList, w: &mut impl CoreWrite);
pub fn decode_canonical_list(bytes: &[u8], lim: &SnapshotLimits) -> Result<MnList, CodecError>;
pub fn canonical_mn_list_hash(list: &MnList) -> Hash256;                // HashWriter, no buffering
pub fn encode_canonical_diff(d: &MnListDiff, w: &mut impl CoreWrite);
pub fn decode_canonical_diff(bytes: &[u8], budget: &mut usize) -> Result<MnListDiff, CodecError>;
pub mod core_disk {                                                     // read-only, for EV4's evodb importer
    pub fn decode_list_s3(bytes: &[u8]) -> Result<MnList, CodecError>;  // CDeterministicMNList::Serialize, any (HAMT) order
    pub fn decode_diff_d4(bytes: &[u8]) -> Result<MnListDiff, CodecError>;  // updated in any order
    pub fn decode_diff_d3_legacy(bytes: &[u8]) -> Result<MnListDiff, CodecError>; // CDeterministicMNStateDiffLegacy bits
}
```

- **The decoder applies Core's canonical reader rules** (`UnserializeCanonicalMNList`, v24:249-298), because EV4's importer and CH6b's fast import read untrusted bytes with it:
  - strictly ascending proTxHash;
  - a prefix run of at most 8 proTxHashes sharing their first 64 bits (quirk 41's collision class);
  - `internalId < totalRegistered`;
  - re-encoding equals the input (`CheckCanonicalEncoding`);
  - bounded counts (`EVO_SNAPSHOT_MAX_MNS` 100,000 and the nested CompactSize budget);
  - non-empty NetInfo validates;
  - every MN goes through `try_add`.
- **Our own stored records** are `[u8 record_version = 1] ‖ VARINT height ‖ body`. The envelope is ours (DEC-11).
- **The `CoreBytes` rule for DML records.** CS0 seals `CoreBytes` to transactions, headers and blocks (CS0 §4.5), so it cannot carry DML records. Those records have the same guarantee another way: only `dml::codec` can write them (its encoder types are crate-private), so no stored DML byte comes from base-sdk or a hand-built buffer. No RPC returns DML bytes. The P2P `mnlistdiff` bytes are EV4's, built from SML entries.

### 2.9 Core's iteration order (proposed quirk 41)

Core's MN map is `immer::map<uint256, CDeterministicMNCPtr, ImmerHasher>` (`deterministicmns.h` v23:135-143 / v24:136-144) with `ImmerHasher(h) = ReadLE64(h.begin())` and immer's default `B = 5` (`immer/config.hpp:126`). `ForEachMN` therefore visits MNs in CHAMP pre-order:
- each node yields its inline values first, in slot order, then descends into its children in slot order (`champ_iterator.hpp`: `step_down`, `step_right`);
- the slot at depth L is `(h >> 5L) & 31` (depth 12 has 4 bits);
- 64-bit hash collisions go to a collision node at depth 13, in insertion order.

**The order is canonical.** immer's erase collapses a one-entry subtree back into its parent (`champ.hpp` v24:1198-1270, `sub_result::singleton`). So the trie shape, and with it the order, is a function of the key set alone, whatever the insertion and deletion history.

**Where it is observable** (both pins; the code is identical):

| Site | v23.1.8 | v24.0.0-rc.3 | Consumer |
|---|---|---|---|
| `BuildSimplifiedDiff`: `mnList` in `to` order, `deletedMNs` in `from` order (mnlistdiff, qrinfo bytes) | `smldiff.cpp:122` | `:121` | EV4, P6b, dash-spv and the desktop as consumers |
| `protx list` | `rpc/evo.cpp:1476,1506` | `:1709,1740` | RP2a, O14 |
| `masternodelist` | `rpc/masternode.cpp:612` | `:666` | RP2a |
| `GetMNByOperatorKey` (first match; two MNs can hold the same point under different encodings pre-V24) | `deterministicmns.cpp:119-127` | `:93-102` | AM1 |
| `GetMNPayee`'s evonode scan (last match) | `:196-204` | `:171-179` | none: I-P makes the match unique |

**Algorithm (`order.rs`).**
1. Map each key to `p(h)`, the 64-bit value whose bits are the chunks `c0 c1 … c11` (5 bits each) then `c12` (4 bits), most significant first. Sorting by `(p(h), proTxHash)` lays out the trie leaves left to right.
2. Recurse over contiguous runs. At depth L, emit the keys that are alone in their slot (in slot order), then recurse into each multi-key run (in slot order).
3. At depth 13 (identical 64-bit hashes), Core uses insertion order, which can differ between Core nodes with different histories. We emit by proTxHash and declare it.

O(n log n) time, O(n) space; it is computed once per list and cached.

**Evidence** (`/work/scratch/ev2-opus/`): the Python reference `scripts/champ_order.py` reproduces, exactly:
- the testnet oracle's `protx list registered true` (522 MNs);
- `protx diff 1 <tip>` (524 entries);
- `protx diff <tip−2000> <tip>` (13 entries). That one equals the **full** `to` list's order filtered to the changed entries, as `BuildSimplifiedDiff` predicts.

### 2.10 The store: `DmlStore`, deltas, reconstruction

```rust
/// The DML part of EV0's EvoStore (A1 §2.5). Keys are block hashes; nothing is ever erased.
pub trait DmlStore: Sync {
    fn mn_snapshot(&self, block: &BlockHash) -> Result<Option<Arc<MnList>>, StoreError>;
    fn mn_diff(&self, block: &BlockHash) -> Result<Option<Arc<MnListDiffRecord>>, StoreError>;
}
pub struct MnListDiffRecord { pub height: i32, pub diff: MnListDiff }
/// Goes into EvoDelta (EV0) and so into the Durable B1 batch (A1 §3.5.4).
pub struct DmlDelta { pub block: BlockHash, pub height: i32, pub diff: Arc<MnListDiff>, pub snapshot: Option<Arc<MnList>> }

pub struct MemDmlStore { snapshots: BTreeMap<BlockHash, Arc<MnList>>, diffs: BTreeMap<BlockHash, Arc<MnListDiffRecord>> }
impl MemDmlStore { pub fn apply(&mut self, d: &DmlDelta); }          // impl DmlStore

pub trait ListCache: Sync {
    fn get(&self, block: &BlockHash) -> Option<Arc<MnList>>;
    fn offer(&self, list: &Arc<MnList>, hint: CacheHint);             // Tip | QuorumBase | Mini | Built
}
pub struct LruListCache { /* single-owner, for tests and LQ1a; CH7a supplies the concurrent one */ }

/// Core GetListForBlockInternal (deterministicmns.cpp v23:778-870 / v24:826-939): the list after `at`.
pub fn list_for_block(at: BlockId, chain: &dyn ChainAncestry, dip3_height: i32,
                      store: &dyn DmlStore, cache: &dyn ListCache) -> Result<Arc<MnList>, DmlStoreError>;
```

- **Semantics.**
  - Below DIP3 the answer is the default list.
  - Otherwise, walk back through `prev` until a cached list or a stored snapshot is found, or until the first block below DIP3 (answer: the default list), collecting diffs. Then apply them forward, offering a mini-snapshot every 32 heights (not during reindex) and the result.
  - Core fabricates an "initial snapshot" `CDeterministicMNList(hash(D−1), D−1, 0)` where we start from the default list. `apply_diff(D)` sets hash and height, so both yield the same list.
  - Core's other fabrication case (an evodb older than its chain, as in functional-test caches) cannot arise: rdashd rebuilds imported Core datadirs (A1 §2.9).
  - A missing diff for a block at or above DIP3 is corruption: `DmlStoreError::Inconsistent`, which CH7a turns into a fail-stop abort (I5). Core would throw inside `ConnectBlock` and mark the block `failed-procspectxsinblock`; we never mark a block invalid for local corruption (declared, §3.11).
- **Cache policy is not observable** (O17 EVO-25 `none`; A1 §2.10.1). Ours is Core's v24 shape: pin the tip, keep quorum bases inside `keepOldConnections`, keep mini-snapshots every 32 heights, LRU-cap at 64 lists. CH7a owns the concurrent implementation and its memory budget (§5.3).
- **Snapshot cadence:** `height % 576 == 0` (`DISK_SNAPSHOT_PERIOD`, `deterministicmns.h` v23:681 / v24:760) **and** the first DIP3 block.
  - Core writes the latter only when an in-memory marker happens to be set (`m_initial_snapshot_index`, v24:722). We always write it. It is a storage choice (DEC-11), and it bounds every reconstruction to 575 diffs.

### 2.11 EV2b entry points

```rust
/// Core inputs that BuildNewListFromBlock reads besides the block and the previous list.
pub struct DmlParams {                                  // from ChainParams (BS5)
    pub min_confirmations: i32,                          // nMasternodeMinimumConfirmations: mainnet 15 (chainparams.cpp:177, both tags); testnet, devnet, regtest 1
    pub dip3_height: i32, pub dip3_enforcement_height: i32, pub dip3_enforcement_hash: Option<BlockHash>,
    pub llmqs: &'static [LlmqParams],                    // Params().GetLLMQ: every configured type (dkgInterval)
}
pub struct DmlRules<'a> {
    pub params: &'a DmlParams,
    pub v19: bool, pub mn_rr: bool,                      // DeploymentActiveAfter(pindexPrev, ·)
    pub v24: bool,                                       // SpecialTxRules.v24 = GetSpecialTxRules(pindexPrev) (validation.cpp v24:2659)
    pub ungated: &'a UngatedRules,                       // proupserv_type_in_special_tx (EVO-15)
}
pub struct PrevBlock<'a> { pub id: BlockId, pub hash: BlockHash, pub height: i32, pub chain: &'a dyn ChainAncestry }

/// `view.GetBestBlock().IsNull()` in Core decides whether external collateral is re-checked.
pub enum CollateralView<'a> { Coins(&'a dyn CoinsView) /* connect, TestBlockValidity, miner */, Unchecked /* Core's null-best-block path: evodb repair */ }

/// Core llmq::utils::GetAllQuorumMembers(type, base) (utils.cpp v23:569 / v24:634-730): members in Core's order,
/// empty when the type is not enabled at base->pprev. LQ1a/b implement it; any snapshot it stores is an effect.
pub trait QuorumMemberSource {
    fn members(&self, t: LlmqType, base: BlockId, effects: &mut dyn EffectSink) -> Result<Arc<[Arc<Dmn>]>, MemberError>;
}

pub struct BuiltList { pub list: MnList /* block_hash null */, pub payee: Option<Arc<Dmn>> }
pub enum DmlBuildError {
    Invalid(BlockValidationError),                       // Core's explicit state.Invalid(BLOCK_CONSENSUS, …)
    Exception(DmlException),                             // a Core throw: the orchestrator maps it to failed-procspectxsinblock
}
pub enum DmlException {
    List(MnListError),                                   // AddMN / UpdateMN / RemoveMN / PoSePunish throws
    Members(MemberError),                                // a throw from GetAllQuorumMembers
    OutOfDomain(&'static str),                           // Core UB, unreachable after EV1/LQ2 (§3.3); never a panic
}

/// Core CSpecialTxProcessor::RebuildListFromBlock (specialtxman.cpp v23:185-520 / v24:309-795).
pub fn build_new_list_from_block(
    block: &Block, payloads: &BlockPayloads, prev: PrevBlock<'_>, prev_list: &MnList,
    coins: CollateralView<'_>, members: &dyn QuorumMemberSource, rules: &DmlRules<'_>,
    effects: &mut dyn EffectSink,
) -> Result<BuiltList, DmlBuildError>;

/// The DML part of CDeterministicMNManager::ProcessBlock (deterministicmns.cpp v23:649-734 / v24:689-782).
/// Called only when DIP3 is active at the block and fJustCheck is false (specialtxman.cpp v23:628 / v24:913).
pub fn process_block(block: &Block, at: (BlockHash, i32), prev_list: &MnList, new_list: MnList,
                     params: &DmlParams, effects: &mut dyn EffectSink) -> Result<ProcessedList, BlockValidationError>;
pub struct ProcessedList { pub list: Arc<MnList>, pub diff: Arc<MnListDiff>, pub delta: DmlDelta,
                           pub change: Option<MnListChange> /* Some iff the diff has changes */ }
pub struct MnListChange { pub old: Arc<MnList>, pub new: Arc<MnList>, pub diff: Arc<MnListDiff> }

/// The DML part of CDeterministicMNManager::UndoBlock (v23:736-769 / v24:784-817): the inverse change, for the
/// MnListChanged event. Nothing is erased; the active list simply becomes `prev_list`.
pub fn undo_block(cur: &Arc<MnList>, prev: &Arc<MnList>, stored: &MnListDiff) -> Option<MnListChange>;
```

- **`BlockPayloads`** is the block's special-transaction payloads, decoded once by CH7a and shared by EV1 and EV2b. Core decodes each payload twice (`GetTxPayload` in `CheckSpecialTxInner` and again in `RebuildListFromBlock`). Decoding is a deterministic function of the bytes (and of `UngatedRules.protx_payload_decode`, EVO-14), so sharing is equivalent. A per-tx decode failure is kept and becomes `bad-protx-payload`/`bad-qc-payload` at the step where Core would decode.
- **Reuse by the miner.** `node/miner.cpp` (v23:291 / v24:287) calls `BuildNewListFromBlock` for the CbTx root, and `TestBlockValidity` calls it again. MN1's exact-block memo (A1 §2.13) keys `BuiltList` by the whole template and the prev tip, never by a transaction subset (GPT F08).

### 2.12 Neighbours: who exchanges what

| Task | Direction | Types exchanged | Notes |
|---|---|---|---|
| EV0 | upstream | `EvoStore`, `EvoDelta`, `Effect`/`EffectSink`, `QuorumMemberSource` | EV0 lands this document's §2 signatures as stubs (types first) |
| CS0 / CS5a | upstream | `Block`, `Tx`, `BlockId`, `ChainAncestry`, `CoreWrite`/`HashWriter`, `BlockValidationError`, `UngatedRules`, `CoinsView` | `UngatedRules.proupserv_type_in_special_tx` is added by EV1 or EV2b, whichever lands first (O17 field list) |
| BS6a/c, BS11a-t | upstream | payload structs, `NetInfo`, `Service`, `DomainPort`; `OperatorKey` with Core's `CBLSLazyWrapper` equality, the stored encoding and `reencode(legacy)` | §3.6 lists what EV2 needs from `OperatorKey` |
| EV1 | sibling (runs first in a block) | `&MnList` at pprev: `get`, `has_unique`, `unique_owner`, `get_by_collateral`, `has_operator_key_under_any_scheme`; `BlockPayloads` | EV1's checks see pprev only; EV2b re-checks against the list as rebuilt so far |
| LQ1a | downstream (starts on EV2a-1, PLAN lever 2) | `list_for_block(work block)`, `iter_valid`, `counts`, `Dmn::{confirmed_hash, confirmed_hash_with_pro_tx, collateral, mn_type}`; implements `QuorumMemberSource` | scoring sorts, so order is irrelevant; tie-break on collateral |
| LQ1b | downstream | `MnList::scratch`, `try_add` (exact `AddMN` failures, quirk 28), `has_mn`, `is_pose_banned`, `counts().total()`, `list_for_block` at the work blocks of the previous cycles | `utils.cpp` v23:339-420 / v24:343-466 |
| EV3 | downstream | `BuiltList.payee`; `Dmn::{operator_reward, cold.script_payout/payouts/shares/script_operator_payout}`; `MnList::payee` for the GBT and RPC payee | Core computes the payee twice (`payments.cpp` v23:55-60 / v24:152-157); we compute it once |
| EV4 | downstream | `iter()` (memcmp, SML root), `core_order()` (mnlistdiff), `MnCold` SML-hash slots, `MnListDiff`; `core_disk` readers for the evodb importer | byte identity depends on `core_order` |
| CH7a | downstream | `build_new_list_from_block`, `process_block`, `undo_block`, `DmlDelta`, `MnListChange`, `list_for_block`; it maps `DmlBuildError::Exception` | owns ordering inside `process_special_txs_in_block` and the views |
| GV1a/b | downstream | `counts().valid_weighted()` at the tip, `get_by_collateral`, `get_valid_by_collateral`, voting keys | tallies weight evonodes 4 |
| AM1, DK1, QM1 | downstream | `get_by_operator_key` (CHAMP-first), `get_valid`, `list_for_block` at quorum bases; `MnListChanged` | |
| RP2a/b, O14 | downstream | `core_order`, `diff_to`, `projected_payees`, `counts`; `MnStateDiff` for `protx diff`/`listdiff` (EVO-28, -29 are RPC rows) | `evodb verify/repair` recomputes diffs with `CollateralView::Unchecked` |
| P6b | downstream | through EV4 | the mnlistdiff/qrinfo bytes that dash-spv and the desktop (through dash-spv/platform-wallet) consume |
| SP1/AM1 | downstream | `Effect::ResetPlatformBan { pro_tx_hash, height }` | Core `CMasternodeMetaMan::ResetPlatformBan` → `SetPlatformBan(false, height)` (`masternode/meta.h:73-84`, both pins) |

dash-spv and the desktop are **not** crate dependants (A1 §2.1 rule 8, NFR-CMP-03). They see the DML only through P2P serving (EV4/P6b, byte-identical including quirk 41) and RPC (RP2a). `core_order` and the codecs are self-contained and could later be lent to dash-spv by the convergence track (CV5), which is out of scope here.

---

## 3. Core equivalence

### 3.1 Functions replaced

| Core | v23.1.8 | v24.0.0-rc.3 | Rust |
|---|---|---|---|
| `CDeterministicMN` + serialize | `deterministicmns.h:43-94` | `:43-94` | `Dmn`, `codec` |
| `CDeterministicMNState` + serialize, `ResetOperatorFields`, `BanIfNotBanned`, `Revive`, `UpdateConfirmedHash` | `dmnstate.h:33-155` | `:33-175` | `MnHot` + `MnCold`, `build.rs` helpers |
| `CDeterministicMNStateDiff` (fields, ctor, serialize, apply) | `dmnstate.h:156-286` | `:177-324` | `MnStateDiff` |
| `CDeterministicMNStateDiffLegacy` | `:288-…` | `:327-483` | `codec::core_disk::decode_diff_d3_legacy` |
| `GetUniquePropertyHash`, `Add/Delete/UpdateUniqueProperty` | `deterministicmns.h:488-566` | `:526-614` | `UniqueKey`, `list.rs` |
| `HasOperatorKeyUnderAnyScheme` | — | `:471-482` | `has_operator_key_under_any_scheme` |
| `GetMN…`, `GetMNByOperatorKey`, `GetMNByCollateral` | `deterministicmns.cpp:83-155` | `:57-130` | §2.6 |
| `CompareByLastPaid(_GetHeight)`, `GetMNPayee` | `:157-219` | `:132-194` | `payee.rs` |
| `GetProjectedMNPayees` | `:221-274` | `:196-249` | `projected_payees` |
| `CalcMaxPoSePenalty`, `CalcPenalty`, `PoSePunish`, `DecreaseScores`, `PoSeDecrease` | `:292-359` | `:267-334` | `pose.rs` |
| `BuildDiff`, `ApplyDiff` | `:361-418` | `:336-393` | `diff.rs` |
| `AddMN`, `UpdateMN`, `RemoveMN` | `:420-647` | `:439-679` | `list.rs` |
| `CDeterministicMNManager::ProcessBlock`, `UndoBlock` | `:649-769` | `:689-817` | `process.rs` |
| `GetListForBlockInternal`, `CleanupCache` | `:778-947` | `:826-1014` | `store.rs` (+ CH7a's cache) |
| `RebuildListFromBlock`, `HandleQuorumCommitment` | `specialtxman.cpp:159-520` | `:283-795` | `build.rs` |
| `SetStateVersion`, `ApplyProUpServTx`, `AddNetInfoEntries`, `IsSchemeMigration` | — | `:33-137` | `build.rs` (V24) |
| `SerializeCanonicalMNList`, `UnserializeCanonicalMNList`, `CanonicalMNListHash`, canonical diff | — | `snapshot.h:237-394`, `snapshot.cpp:150-155` | `codec.rs` |

### 3.2 `build_new_list_from_block`: Core's order, step by step

Here `h` = `prev.height + 1`. Every list operation below happens on `new`, a clone of `prev_list` with its hash nulled and its height set to `h`.

| # | Core (v23 / v24 lines in `specialtxman.cpp`) | Rust | Notes |
|---|---|---|---|
| 0 | `payee = prevList.GetMNPayee(pindexPrev)` (200 / 325) | `prev_list.payee(PayeeRules{v19, mn_rr})` | before any mutation (FR-EVO-02) |
| 1 | confirmations over **prevList**, banned included: null `confirmedHash` and `pindexPrev.height − registered ≥ minConf` → `UpdateConfirmedHash(proTx, hash(pindexPrev))` (205-218 / 330-343) | `prev_list.unconfirmed.range(..=(prev.height − minConf, MAX))` → new cold with both hashes | `int` arithmetic; mainnet 15 → hash of R+15, visible in list(R+16); others 1 |
| 2 | `newList.DecreaseScores()` (220 / 345) | snapshot `decaying`; `update_hot(penalty − 1)`; an MN leaves `decaying` at 0 | only valid MNs; order-free |
| 3 | for `i ≥ 1` (coinbase skipped by index), `IsSpecialTxVersion()` only (`nVersion ≥ 3`), by `nType`: | | single loop, block order |
| 3a | ProRegTx (235-312 / 358-453) | §3.2.1 | |
| 3b | ProUpServTx (313-383 / 454-521) | §3.2.2 | EVO-15 order per profile |
| 3c | ProUpRegTx (384-413 / 522-586) | §3.2.3 | |
| 3d | ProUpRevTx (414-434 / 587-618) | §3.2.4 | |
| 3e | ProUpShareTx, ProUpSharedRegTx (— / 619-701) | §3.2.5 | V24 types; unreachable unless V24 |
| 3f | QcTx (435-461 / 702-728) → `HandleQuorumCommitment` (159-174 / 283-298) | §3.3 | interleaved in block order (quirk 25) |
| 3g | any other type | nothing | |
| 4 | second pass, `i ≥ 1`, **every** transaction (special or ordinary), every input: MN with that collateral → V24 shared check, then `RemoveMN` (464-482 / 731-757) | §3.5 | an MN registered and spent in one block is added, then removed |
| 5 | if the payee is still in `newList` (`GetMN`, banned allowed): `nLastPaidHeight = h`; Evo ∧ ¬MN_RR → `++nConsecutivePayments` (486-500 / 759-775) | `update_hot` | no V19 check (Core's comment) |
| 6 | over a copy of `newList`, all MNs: Evo ∧ `consecutive ≠ 0` ∧ ¬(it is the payee ∧ ¬MN_RR) → `consecutive = 0` (503-515 / 777-790) | iterate a snapshot of `evo_streak` | after MN_RR the payee is reset too |

**3.2.1 ProRegTx.**
1. Payload, else `bad-protx-payload`.
2. `dmn.internal_id = new.total_registered` (Core passes the `uint32_t` to the `uint64_t` constructor). `pro_tx_hash = txid` (the rules' txid, CS0). `collateral = (txid, n)` if the payload's hash is null, else the payload's outpoint.
3. With `CollateralView::Coins` and an external collateral: the coin must exist, be unspent and be exactly `GetMnType(payload.nType).collat_amount` (1000/4000 DASH), else `bad-protx-collateral`. The view is the **pprev** UTXO set, because `ProcessSpecialTxsInBlock` runs before `UpdateCoins` (`validation.cpp` v24:2661-2669; quirk 26). With `Unchecked` this step is skipped, as Core does when `view.GetBestBlock().IsNull()`.
4. Replacement: `if let Some(old) = new.get_by_collateral(&dmn.collateral) { new.remove(old) }`.
   - This uses **unique-map semantics**: the owner of `H(collateral)`, with no equality check. Core does exactly this (`GetMNByCollateral`, then `RemoveMN`).
   - The replaced MN loses its payment position.
5. Each NetInfo entry: `new.has_unique(entry)` → `bad-protx-dup-netinfo-entry`; an entry that is neither AddrPort nor DomainPort → `bad-protx-netinfo-entry`.
6. Shared (V24 only): any share key, or the operator key, in the map → `bad-protx-dup-key`. Otherwise owner **or** operator in the map → `bad-protx-dup-key`. Note that **platformNodeID is not checked here** (§3.6).
7. State from the payload (`CDeterministicMNState(const CProRegTx&)`, `dmnstate.h` v24:74-89): `registered = h`; an empty NetInfo → `ban_if_not_banned(h)` (quirk: since v23 an invalid CService decodes as empty, so the MN registers banned).
8. V24 only: `has_operator_key_under_any_scheme(key, ProTxHash::null)` → `bad-protx-dup-key`.
9. `new.try_add(dmn, bump_total = true)`; any `Err` → `Exception`.

**3.2.2 ProUpServTx.**
- Payload, else `bad-protx-payload`.
- **V24_0 profile** (`proupserv_type_in_special_tx = true`):
  1. `!IsValidMnType(nType)` → `bad-protx-type`;
  2. each entry is in the map with another owner → `bad-protx-dup-netinfo-entry` (non-address entry → `bad-protx-netinfo-entry`);
  3. MN absent → `bad-protx-hash`;
  4. `nType ≠ mn.mn_type` → `bad-protx-type-mismatch`.
- **V23_1 profile:** entries, then `bad-protx-hash`, then `bad-protx-type-mismatch`, then `bad-protx-type`.
- Apply:
  - **pre-V24** (`ApplyProUpServTx(·, is_v24 = false)`; v23 lines 346-367 do the same): `net_info`, `script_operator_payout`; for Evo, `platform_node_id`, plus the two ports when `payload.version < 3`, else both zeroed.
  - **V24** additionally sets the version (`max(state, payload)`) and runs `SetStateVersion` (§3.2.6).
- Revive if banned and operator ≠ null ∧ voting ≠ null ∧ (shared ∨ owner ≠ null).
- V24 only: if this is a scheme migration (`old == 1 ∧ target > 1`), probe the operator key under any scheme against the other MNs → `bad-protx-dup-key`.
- `new.update(...)`; `Err` → `Exception` (e.g. a platformNodeID taken earlier in the block).

**3.2.3 ProUpRegTx.**
- Payload; MN absent → `bad-protx-hash`; V24: shared MN → `bad-protx-shared-mn`.
- `operator_changed = state.op != payload.op`, by `CBLSLazyWrapper` equality, which is point equality for valid keys (§3.6).
- Target version:
  - pre-V24: `changed ? payload.version : old` (v23 writes `state > BasicBLS ? state : payload` when changed, which is equal because pre-V24 states are ≤ 2);
  - V24: `max(old, payload)`.
- V24: `(changed ∨ migrating) ∧ has_operator_key_under_any_scheme(payload.op, self)` → `bad-protx-dup-key`.
- If changed: `ResetOperatorFields`, then `ban_if_not_banned(h)`, then `op = payload.op`. `ResetOperatorFields` sets version 1, a null operator key, an empty MnNetInfo, an empty operator payout, reason 0 and a zero platformNodeID, but keeps the ports (`dmnstate.h` v23:112 / v24:127-135).
- `voting = payload.voting`.
- `SetStateVersion(target)`, which re-encodes the key to the target's scheme and converts the NetInfo kind; a failure gives `bad-protx-netinfo-version`.
- Payout: target ≥ 3 → `payouts` (from the payload, or `LegacyPayoutAsList`) and an empty `script_payout`; else `script_payout` and empty `payouts`.

Pre-V24 this is byte-equal to v23's branch: the payload's lazy key is already in the target's encoding, and an empty NetInfo converts to an empty NetInfo.

**3.2.4 ProUpRevTx.**
- Payload; MN absent → `bad-protx-hash`.
- `ResetOperatorFields`; under V24, restore `max(old, payload)` through `SetStateVersion` when it is ≥ 2.
- `ban_if_not_banned(h)`; `revocation_reason = payload.reason`.
- Pre-V24 the version drops to 1 (A1 §2.5; identical in v23).

**3.2.5 ProUpShareTx / ProUpSharedRegTx** (V24; v24 lines 619-701). Implemented as written:
- `bad-proupshare-index`, `bad-proupshare-payee-reuse`;
- `bad-proupsharedreg-not-shared`; the operator-change reset, `SetStateVersion(old)`, `SetLegacy(false)`; `bad-protx-dup-key`; `bad-proupsharedreg-payee-reuse`.

ProDisTx (type 10) has **no** branch here: dissolution happens in the spend pass (v24:744-747 comment).

**3.2.6 `SetStateVersion`** (v24:51-106; FR-EVO-05):
1. If the target is ≥ 3 and `payouts` is empty while `script_payout` is not: migrate `script_payout` into `payouts`.
2. Re-encode a non-null operator key to `legacy = (target == 1)`. This is `Set`, not `SetLegacy`: bytes change, so `UniqueKey::operator_key` changes and `update` re-keys the slot.
3. Return if the version and the NetInfo kind already match.
4. Otherwise build a NetInfo of the target kind, copying CORE_P2P entries, and the Platform entries if the old NetInfo can store them. An Evo whose legacy NetInfo is non-empty can only be raised by a ProUpServTx → `bad-protx-netinfo-version`.
5. When raising to ≥ 3, zero the two ports.

### 3.3 PoSe

- `calc_max_pose_penalty() = max(100, counts.total())`. This is unweighted, includes banned MNs, and is **evaluated on the list as rebuilt so far** (registrations earlier in the block count; quirk 24).
- `calc_penalty(p) = max * p / 100` (integer).
- `pose_punish(m, x)`:
  - `penalty = min(max, penalty + x)`, applied **even if already banned** (quirk 24);
  - if not banned and `penalty ≥ max`, ban at `new.height`;
  - `update_hot`.
- The QcTx branch:
  - payload, else `bad-qc-payload`; null commitments are ignored;
  - `GetLLMQ(type)` over the configured types, else `bad-qc-commitment-type`;
  - `quorumHeight = int(nHeight) − int(nHeight) % dkgInterval + quorumIndex`, in `i32` with C++'s truncating `%`;
  - `prev.chain.ancestor(prev.id, quorumHeight)` must exist and have the commitment's `quorumHash`, else `bad-qc-quorum-hash`. A negative height yields `None`, as Core's `GetAncestor` does;
  - `members = members_source.members(type, base, effects)`;
  - for each `i`: if `new.get(member)` exists and `!validMembers[i]`, then `pose_punish(member, calc_penalty(66))`, recomputed per call.
- **Reachable domain.** In a block, EV1/LQ2's `CheckLLMQCommitment` has already enforced `nHeight == h` and `validMembers.len() == size ≥ members.len()`. Outside that domain Core's arithmetic can overflow `int`, which is undefined behaviour, and its `validMembers[i]` can read out of bounds. EV2b uses `wrapping_add`, and returns `Exception` for an out-of-range index rather than panicking. The L1 generator stays inside the reachable domain (CS0/O1 precedent: "narrow the oracle shim's guards to Core's assert and UB conditions").

### 3.4 Payee selection and projections (FR-EVO-04; quirk 13)

```rust
fn pay_height(h: &MnHot, c: &MnCold) -> i32 {                // CompareByLastPaid_GetHeight (v23:157-166 / v24:132-141)
    let mut height = h.last_paid_height;
    if h.pose_revived_height != -1 && h.pose_revived_height > height { height = h.pose_revived_height; }
    else if height == 0 { height = c.registered_height; }
    height
}
```

- **The queue.** `payee_queue` holds `PayeeKey { pay_height, pro_tx }` for every **valid** MN, unconfirmed ones included (quirk 13). `CompareByLastPaid` is a strict total order, so the minimum is unique whatever the iteration order, and Core's linear minimum equals `payee_queue.first()`.
- **`payee(r)`:**
  - An empty list gives `None`. A non-empty list with every MN banned gives `None` too; EV3 then accepts any coinbase, and the credit pool is still credited (quirk 13).
  - If `r.v19 ∧ ¬r.mn_rr`: the valid MN with `last_paid_height == list.height` that is Evo with `consecutive < 4` (`dmn_types::Evo.voting_weight`) is paid again.
    - Such an MN's `pay_height` equals `list.height`: a revival height can never exceed the list's height, so the revival branch of `pay_height` cannot fire. It therefore lies in the queue's top run of keys with `height == list.height`. That run is scanned downwards and is tiny in practice: the paid MN plus registrations and revivals at that height.
    - By I-P there is at most one match, so Core's "last match in CHAMP order" equals ours.
  - Otherwise `payee_queue.first()`.
- **The rules across versions.**
  - `CompareByLastPaid` and `GetMNPayee` are byte-identical in v23.1.8 and v24.0.0-rc.3 and unchanged since DIP3, apart from the evonode branch added at V19.
  - Before V19 no Evo can exist, since EV1 rejects Evo registrations before V19. Core's `isv19Active` gate is an optimisation with the same result.
  - From MN_RR (mainnet 2,128,896): no continuation, no streak bump, and step 6 also resets the payee.
- **`projected_payees(r, n)`** (RPC `masternode winners/payments` and GBT):
  - pre-MN_RR, each valid MN appears `voting_weight` times (Evo 4), with an in-progress streak's remaining copies first and its already-paid copies at its sorted position; after MN_RR, once each;
  - the result is truncated to `min(n, weighted_count)`.

  Emitting in queue order is equivalent to Core's `std::sort` from `begin() + remaining`: equal keys are the same MN, so no unstable-sort ambiguity exists.
- **The reference twin** (`reference.rs`) is Core's code over Core's data layout: three `imbl` maps; the same linear scans for payee, confirmations, decay and streaks; unique hashes recomputed on every `Add`/`Update`/`Remove` as Core does; `GetMNByCollateral(H(prevout))` plus the equality check per input. It shares only the payload decoder and the canonical encoder with the fast path, and is the `-consensusxcheck` partner and the benchmark baseline (§5).

### 3.5 Collateral spends, including ordinary transactions (quirk 25)

- For every input of `vtx[1..]`: `new.collateral_owner(prevout)` (typed index).
- **Equivalence with Core.** Core takes `GetMNByCollateral(prevout)` (the owner of `H(prevout)`) and checks `dmn->collateralOutpoint == prevout`. By I-U1 and I-U2 the two return the same MN or both nothing.
  - A cross-type hash collision can make Core's lookup return a different MN (§3.6). The equality check then rejects it, and the typed index never finds it.
  - xcheck builds assert the equivalence on every input.
- **Cost.** This removes a SHA256d of 36 bytes per input. On agentbox (no SHA-NI) that is about 3 k instructions per input, about 0.3 M for a 100-input block.
- **V24.** A shared MN's collateral spent by anything other than a ProDisTx → `bad-shared-collateral-spend`.
- `remove`; an `Err` here is impossible by I-U1 but maps to `Exception`.

### 3.6 Unique properties and BLS key semantics (quirk 23; proposed quirk 42)

- **`OperatorKey` contract** (to BS11a-t). EV2 needs Core's `CBLSLazyWrapper` behaviour (`bls.h` v23:393-577 / v24:392-577; `operator==` at 514 in both, textually identical):
  - equality: both raw with the same scheme → bytes; both decoded → points; else decode both, where invalid decodes compare equal to each other;
  - the stored bytes and scheme (for `UniqueKey` and serialization);
  - `reencode(legacy)` for V24;
  - `is_null`.

  Keys in states are always valid and canonical, because EV1's `IsTriviallyValid` requires `Get().IsValid()` and that includes `CheckMalleable`. The invalid-equals-invalid corner is therefore unreachable from a list, but it is kept for fidelity.
- **Cross-type collisions** are reproduced, not "fixed":
  - a ProRegTx whose `keyIDOwner` equals an existing Evo's platformNodeID gets `bad-protx-dup-key`;
  - in the opposite order (owner first, then an Evo ProRegTx with that platformNodeID **in the same block**), step 3a's explicit checks miss it and `try_add` fails → `failed-procspectxsinblock`. In a later block EV1 rejects it with `bad-protx-dup-platformnodeid` (from the pprev list).
- **Proposed quirk 42** ("in-block list exceptions and order-dependent strings"):
  - Core's explicit in-block re-checks cover only some properties: ProRegTx re-checks NetInfo, owner and operator but not platformNodeID; ProUpServTx re-checks NetInfo only; ProUpRegTx re-checks nothing pre-V24.
  - The remaining conflicts surface as `AddMN`/`UpdateMN` throws, i.e. `failed-procspectxsinblock` (`BLOCK_CONSENSUS`), and which string a block gets depends on transaction order.
  - Cases, each a named test with Core's verdict taken from the oracle:
    - two ProRegTxs in one block with the same platformNodeID;
    - owner keyID vs platformNodeID, in both orders;
    - two ProUpRegTxs moving to the same operator key (pre-V24; V24 gets `bad-protx-dup-key` from the probe);
    - two ProUpServTxs claiming the same platformNodeID.

### 3.7 Quirks owned or relied on

| Quirk | Statement | EV2 test |
|---|---|---|
| 13 | Payee tie-break by memcmp; unconfirmed MNs payable; all-banned list → `None` (EV3 accepts anything) | `quirk_13_*`: queue vs twin on tie-heavy lists (equal `pay_height`, proTxHashes differing only in the first or last internal byte); unconfirmed payee; all banned |
| 23 | One unique map keyed by `SerializeHash`; keyID/platformNodeID collision; per-encoding operator slots before V24 | `quirk_23_*`: both collision orders (§3.6); same point under two encodings held by two MNs pre-V24 (allowed) and rejected under V24 (EVO-13); ADDRv1 vs ADDRv2 hashing of the same address family |
| 24 | PoSe added to banned MNs; `max(100, total)` on the partially rebuilt list | `quirk_24_*`: a QcTx after a ProRegTx that lifts the total above 100; punishing a banned MN raises its penalty up to the max |
| 25 | Update order; ProRegTx with the same collateral replaces | `quirk_25_*`: register-and-spend in one block; replacement keeps neither payment height nor internalId; QcTx between two ProTxs |
| 26 (DML half) | External collateral from the pprev UTXO set | ProRegTx whose collateral was created earlier in the same block → `bad-protx-collateral` (only with `Coins`; EV1 rejects it first in a real block) |
| 28 (contract) | LQ1b swallows `AddMN` throws on scratch lists | `try_add` failure-order tests: proTx, internalId, collateral, entries, owner, operator, platformNodeID |
| **41** (proposed) | Core's iteration order is CHAMP over `ReadLE64(proTxHash)` (§2.9) | `quirk_41_*`: vs a naive trie, vs the oracle (`protx list`, `oracle_mnlist_core_order`), 60-bit shared prefixes, 64-bit collisions (declared) |
| **42** (proposed) | In-block list exceptions → `failed-procspectxsinblock`; order-dependent strings (§3.6) | the four cases above, against both pins |

### 3.8 Reject strings and classes

Every explicit reject is `state.Invalid(BlockValidationResult::BLOCK_CONSENSUS, reason)` with no debug message. They are returned as `DmlBuildError::Invalid(BlockValidationError::new(Consensus, reason))`.

| Reason | Where | Pins |
|---|---|---|
| `bad-protx-payload` | 3a–3e payload decode | both |
| `bad-protx-collateral` | 3a, `Coins` only | both |
| `bad-protx-dup-netinfo-entry`, `bad-protx-netinfo-entry` | 3a, 3b | both |
| `bad-protx-dup-key` | 3a; V24 probes in 3a/3b/3c/3e | both (V24 probes v24 only) |
| `bad-protx-type` | 3b (first under V24_0, last under V23_1) | EVO-15 |
| `bad-protx-hash` | 3b–3e | both |
| `bad-protx-type-mismatch` | 3b | both |
| `bad-protx-netinfo-version` | `SetStateVersion` | v24 (V24) |
| `bad-protx-shared-mn`, `bad-proupshare-index`, `bad-proupshare-payee-reuse`, `bad-proupsharedreg-not-shared`, `bad-proupsharedreg-payee-reuse` | 3c, 3e | v24 (V24) |
| `bad-qc-payload`, `bad-qc-commitment-type`, `bad-qc-quorum-hash` | 3f | both |
| `bad-shared-collateral-spend` | 4 | v24 (V24) |
| `failed-procspectxsinblock` | any `MnListError` inside the build; mapped by CH7a at Core's catch (`specialtxman.cpp` v23:701-704 / v24:993-996) | both |
| `failed-dmn-block` | `process_block`: an internal error while building the diff (Core's `try` at `deterministicmns.cpp` v23:665-703 / v24:705-757) | both |
| `bad-dip3-enf-block` | `process_block`: `h == DIP0003EnforcementHeight` and a non-null pinned hash differs (quirk 34's rule site; BS5 owns the params) | both |

Inside `process_special_txs_in_block`, Core's outer catch also turns throws from the member source into `failed-procspectxsinblock`. `MemberError` therefore maps the same way.

**Logging.** `ProcessSpecialTxsInBlock` calls the build with `debugLogs = true`, so Core logs every registration, update, revocation, removal, PoSe punishment and ban with fixed texts (`"%s -- MN %s added at height %d: %s\n"`, …; `specialtxman.cpp` v24:396-399, 450-452, 501, 518-520, …). `PoSePunish` logs as well (`deterministicmns.cpp` v24:300-304). EV2b emits them as `tracing` events with Core's text and category (A1 Appendix E), so ND1's log-compatibility work (NFR-OPS-02) and O11's asserted-log extraction find them.

### 3.9 Per-profile and per-hardfork behaviour

| Behaviour | Keyed by | v23.1 profile | v24.0 profile | Evidence |
|---|---|---|---|---|
| ProUpServTx `nType` check position | `UngatedRules.proupserv_type_in_special_tx` (O17 **EVO-15**, `profile`) | after mismatch | first | v23 `specialtxman.cpp:339-344`; v24 `:459-461, 483-485`. O17 lists consumers EV1, MP1a, P4, MN1, **not EV2b** (erratum) |
| ProTx version handling, `SetStateVersion`, version raise/keep, cross-scheme probes, shared MNs, payout lists, ExtAddr | `DmlRules.v24` (EVO-08, -11, -13; `gated`) | v24's rules if V24 were active (Q1) | v24's rules | v24 `specialtxman.cpp:51-137, 416-446, 488-515, 532-616, 619-701, 739-743` |
| The BLS encoding of the stored key | `MnCold.version` (not the global flag) | same | same | `dmnstate.h` v23:97 / v24:107; CONS-09's leak does not reach the DML |
| `UpdateUniqueProperty` for BLS (point vs hash) | — (pre-V24 unreachable) | same results | same results | §2.5 |
| Diff serialization order | — (storage) | ours sorted | ours sorted | O17 `evo.md` §3 |
| List cache bounds | — (EVO-25 `none`) | ours | ours | `deterministicmns.cpp` v23:906-956 / v24:836-838 |

| Hardfork (activation semantics) | DML effect |
|---|---|
| DIP3 (`active_at(block)`: `h ≥ DIP0003Height`; mainnet 1,028,160, testnet 7,000, devnet 2, regtest 432 by default) | The DML exists; below it the list is the default list (height −1); the first DIP3 block's previous list is the default list |
| DIP3 enforcement (h == 1,047,200 mainnet, 7,300 testnet) | `bad-dip3-enf-block` hash pin (in `process_block`) |
| V19 (`active_after(prev)`) | Evo MNs can exist (EV1); the evonode continuation in `payee` when ¬MN_RR. The BLS scheme flip is CH7a's |
| V20 | none in the DML (LQ1: work-block lists, the CL modifier; EV3: payments) |
| MN_RR (`active_after(prev)`) | no continuation, no streak bump; step 6 resets the payee's streak; `projected_payees` stops weighting |
| V24 (`GetSpecialTxRules(pprev).v24`) | §3.2.2–3.2.6 V24 branches; the shared-collateral spend rule |
| `evo_shares` | none in the DML (EV1 gates registrations and payouts) |
| Evonode weight 4 / collateral 4000 | `Counts::*_weighted`, `projected_payees` pre-MN_RR; `bad-protx-collateral` amount by the payload's `nType` |

### 3.10 Effects

- `process_block` emits `Effect::ResetPlatformBan { pro_tx_hash, height: h }` for each transaction `i ≥ 1` with `IsSpecialTxVersion ∧ nType == ProUpServ` whose payload decodes, in block order:
  - after the diff is built and before the DIP3-enforcement check, so the effect survives `bad-dip3-enf-block`. v23 resets before writing the diff, v24 after `WriteDerived`; both reset before the enforcement check (`deterministicmns.cpp` v23:676-725 / v24:734-778);
  - never in just-check mode, because Core does not call `ProcessBlock` then.

  `CMasternodeMetaMan::ResetPlatformBan` ignores an MN without meta info and `SetPlatformBan(false, h)` refuses a stale height. SP1/AM1 apply those rules. **A1 erratum:** the effect needs `height`.
- **Member effects.** When LQ1b's source computes rotated members inside the QcTx branch, it stores the snapshot, as `GetAllQuorumMembers` → `BuildNewQuorumQuarterMembers(storeSnapshot = true)` does on a cache miss (`utils.cpp` v24:458-463). EV2b passes the same `EffectSink`, so the emission is in Core's place. Whether Core hits its static cache is history-dependent, so it is an "observation"-class digest field (A1 §4.6).
- `MnListChanged` is not an effect. It is an event CH7a raises after a successful connect, only if the diff has changes (`validation.cpp` v24:2918-2920; v23:2709-2713 uses an `optional` that `ProcessBlock` fills only when the diff has changes).

### 3.11 Declared deviations (all internal or unreachable)

1. **Local store corruption aborts the node** (I5). Core marks the block `failed-procspectxsinblock` and keeps running.
2. **Out-of-domain QcTx arithmetic** (§3.3) gets defined behaviour where Core has UB. It is unreachable after EV1/LQ2.
3. **Order within a 64-bit `ReadLE64` collision class** follows proTxHash; in Core it follows insertion order, which differs between Core nodes. Reaching it needs a birthday search (~2³²) over the attacker's own ProRegTx txids. The effect is limited to JSON and mnlistdiff entry order. O14/EV4 compare such classes as sets, and v24's snapshot reader caps runs at 8.
4. **Snapshot cadence and cache policy** are storage choices (§2.10).

---

## 4. Persistence, crash consistency and recovery

- **Column families** (A1 §2.9): `mn_diff` (block hash → `[v1] ‖ VARINT height ‖ canonical diff`) and `mn_snapshot` (block hash → `[v1] ‖ VARINT height ‖ canonical list`). Both are written only through `DmlDelta` in the **B1 Durable batch** of A1 §3.5.4, with the block index, every other evo delta and `meta.evo_tip`.
- **Never erased.**
  - Disconnects keep diffs and snapshots (Core keeps them: `UndoBlock` only reads; A1 §2.9 table). Reorgs therefore reconnect previously seen blocks without recomputing their storage.
  - A rewrite of an existing key is a pure function of `(list(pprev), block, rules)` and so byte-identical. xcheck builds compare the bytes, as v24's `WriteDerived` does (`evodb.h` v24:160-200), and abort on a mismatch.
- **Recovery** (A1 §3.5.5): evo state is atomic at `meta.evo_tip`, and coin replay never touches it.
  - On startup the tip list is `list_for_block(evo_tip)`: at most 575 diffs over the nearest snapshot, or the optional tip snapshot (Q8).
  - Blocks above the recovered tip are reconnected through ordinary validation (I3), and their diffs are rewritten identically.
- **Effects** (`ResetPlatformBan`) persist through the meta store's own flush, as Core's `mncache.dat` does. That is "no more durable than Core" (A1 §2.10.1).
- **Invariant on load.** `list_for_block` runs `check_invariants()` on every list it decodes from a snapshot in debug and xcheck builds. Release builds rely on the decoder's canonical checks.
- **Tests:**
  - ST3a's `FaultyEngine` model with CH7a: crash at every sub-batch boundary across a cycle-base height, a 576-multiple and a reorg; the reloaded tip list must have the same `CanonicalMNListHash` as an uncrashed run;
  - ST3b device-level runs come with CH7a.

---

## 5. Performance

### 5.1 Metrics affected and budgets (never slower)

| PRD / B1 metric | How the DML enters | Budget |
|---|---|---|
| 1–3 IBD / reindex (post-DIP3 windows), 12 CPU per block | `build_new_list_from_block` + `process_block` per block; the diff encoding into B1 | PF0 counters for `reindex-chainstate` over the post-DIP3, post-V20 and tip−50k windows ≤ the previous head; DML instructions per block ≤ dashd's (Core bench patch, §5.5) |
| 5 per-block connect latency | the same work, on the connect critical path | budget ≤ dashd's `BuildNewListFromBlock` + `ProcessBlock` (from `-debug=bench` "m_dmnman.ProcessBlock" plus the build share); target 0.5× |
| 8 GBT | the payee once, the build reused through MN1's memo | ≤ dashd |
| 9 startup | the tip list from a snapshot + ≤ 575 diffs | ≤ dashd's first `GetListAtChainTip` |
| 10 memory | the list cache | ≤ dashd's `mnListsCache` + `mnListDiffsCache` |
| 11 disk | `mn_diff` + `mn_snapshot` vs evodb `dmn_D4` + `dmn_S3` | ≤ dashd per component (same encoding plus RocksDB compression) |
| 13 RPC | `protx list/info/diff`, `masternodelist`, `masternode count/winners` | ≤ dashd (`core_order` cached per list, O(1) counts) |

### 5.2 Where Core spends, and what we remove

Per block on mainnet (about 3.5 k MNs, of which some hundred carry a decaying PoSe penalty; as an estimate that PF1 replaces with a measurement, about 0.17 DKG punishments per block, each decaying for about 2,300 blocks, gives about 400), Core's DML work is:
1. `GetMNPayee`: a scan of every valid MN.
2. Confirmations: a scan of every MN.
3. `DecreaseScores`: a scan, then for each penalised MN a state copy and an `UpdateMN`. v24's `UpdateMN` hashes both BLS keys (`GetUniquePropertyHash` ×2) and builds two SML entries whenever the SML cache is set (`deterministicmns.cpp` v24:589-592; v23 the same).
4. Each input: `GetMNByCollateral` = SHA256d(36 B) + a HAMT lookup.
5. The streak reset: a scan of every MN.
6. `ProcessBlock`: `BuildDiff` iterates every MN with a lookup, and builds a field-by-field diff for every changed one.
7. The payee again in `GetBlockTxOuts`, and the whole build again in GBT's `TestBlockValidity`.

The fast path:
- (1)(2)(5) become O(log n) index operations;
- (3) touches only `decaying` and copies a 20-byte `MnHot`, with no hashing and no SML work, because SML hashes are cached per cold state and validity;
- (4) uses the typed collateral index;
- (6) skips shared subtrees;
- (7) is computed once and shared.

### 5.3 Data structures and measured costs

The structures are `imbl` 7 `OrdMap`/`OrdSet`: a B-tree with 16-entry nodes and `Arc` sharing (`config.rs`: `ORD_CHUNK_SIZE = 16`).

**Measured** with `/work/scratch/ev2-opus/bench-imbl` (a model with the real map shapes, not the real code), in instructions retired (`perf stat -e instructions:u`), because wall time on agentbox (load about 60) is meaningless:

| Model | Indexed path | Core-shaped twin | Ratio |
|---|---|---|---|
| 3,500 MNs, ~370 decaying, 100 inputs/block (4,000 blocks) | **1.44 M instr/block** (clone, payee, decay, bookkeeping, probes, plus the structural diff) | **7.6 M** (scans; 2 SHA256d per update; SHA256d per input) | **5.3×** |
| 20,000 MNs, ~2,000 decaying (1,000 blocks) | **7.0 M** | **31.8 M** | **4.5×** |

- **Clone:** one `Arc` increment per persistent map (eight), plus the cached order.
- **Update:**
  - per decaying MN: a new `Arc<Dmn>` (about 130 B) plus a path copy of about three 640 B nodes;
  - the structural diff costs about 0.1 µs per changed entry.
- **Memory:**
  - A list's unshared part after a typical block is about 0.25 MB: the touched `mns` leaves, the `decaying` leaves, and the new `Dmn`s. Cold parts and the unique map (the largest map, about 1.2 MB at 3.5 k MNs × 5 properties) are almost always shared, because they change only on registrations and key or address changes.
  - Lists 32 heights apart share almost no `mns` leaves, at about 0.4 MB each. The 64-list cache is therefore about 25–50 MB, which is Core's order of magnitude (immer HAMT nodes plus `shared_ptr` states).
- **Disk:**
  - The diff encoding equals Core's `dmn_D4` content. Per penalised MN per block that is `VARINT id ‖ VARINT fields ‖ i32`, about 7 B.
  - RocksDB compression on the evo CFs keeps us at or below dashd. The decay entries dominate the CF (about 2.5 KB per block), which is what Q5's lazy decay would remove.
- **Reconstruction** (`list_for_block` far from a cache):
  - decoding one snapshot is about 3.5 k MNs × 5 SHA256d (unique keys) ≈ 30–45 M instructions;
  - each diff entry is about 0.5–1 k instructions, about 0.1–0.2 G instructions for 575 diffs, against Core's heavier `UpdateMN`.
  - Mini-snapshots every 32 heights bound repeated nearby requests (P6b serving).

### 5.4 Route to 2×, and the declared limit

- **Target (PLAN EV2b BG):** `build_new_list_from_block` ≥ 2× the twin on mainnet blocks, in PF0 instructions. Expected 3–5× (§5.3).
- **Against dashd:** the twin reproduces Core's algorithmic work, and Core additionally pays for `shared_ptr` state copies, SML-entry construction in `UpdateMN`, its second payee computation and its second payload decode. We expect ≥ 3× fewer instructions than dashd's DML per block; the Core bench patch (§5.5) settles it.
- **Declared limit: eager PoSe decay.** Every penalised valid MN gets a new state every block, as in Core, so O(p) remains.
  - A lazy representation (store `(penalty, height)`, materialize per list height) would remove it, and about 95% of the stored diff bytes with it.
  - Cost: the effective penalty becomes a function of `(Dmn, list height)`. Encoding, `diff_to` (decay-only changes of MNs whose `Dmn` is shared) and `pose_punish` must all materialize it, and the twin comparison must cover them.
  - Deferred to the PF track, gated on PF1 (Q5).
- **PF1 dependency (PLAN).** The indexes are cheap to keep correct, because one choke point is checked by the twin and the invariant checker, and they remove work that grows with the MN count. EV2b's phase B4 (the benchmark gate) waits for PF1's budget; the correctness phases do not.

### 5.5 Benches and counters

- **Criterion and iai benches in `dash-evo`** (wired into B1's microbench comparison; Appendix E):
  - `dml_clone` and `dml_diff_consecutive`: the EV2a BG, "crit list clone/diff";
  - `dml_build_w1`: mainnet 1,028,160 +2,000 blocks, starting from the default list;
  - `dml_build_w2`: 2,000 blocks at tip − 50k, from an imported canonical snapshot;
  - each `dml_build_*` runs fast vs twin;
  - `canonical_encode/decode`, `canonical_hash`, `core_order`, `list_for_block_575`.
- **PF0 counters:** instructions, allocations and allocated bytes per block for `dml_build_w2`. A regression beyond max(noise, 0.5%) blocks a merge (A1 §3.11).
- **Core side (DEC-16 test patch, built with B1's script):** `bench/evo_dml.cpp` runs `RebuildListFromBlock` + `BuildDiff` + diff serialization over the same W1/W2 blocks with the same starting list (from the dump RPC, Q3). It reports instructions per block for both pins. This gives the "never slower than dashd" number at component level, as CS0/CS1 did.

---

## 6. Verification

### 6.1 L0: Core's own tests

- **`src/test/evo_snapshot_tests.cpp` (v24)**, the canonical MN-list part:
  - `populated_v3_golden_value` (the MN-list section of the golden);
  - `canonical_mn_reader_rejects_order_and_counter`;
  - `nested_mn_map_wire_order_is_canonical`;
  - `hash_prefix_collision_runs_are_bounded`;
  - `unset_mn_id_is_rejected`;
  - `inactive_mn_roundtrip`;
  - `malformed_lazy_operator_keys_are_rejected`;
  - `diff_chain_roundtrip_and_canonical_determinism`;
  - `bounded_readers_reject_claimed_sizes_first` (the MN parts).

  Expected values come from the oracle at test time where the input is generated (CS0's rule), or from Core's committed constants.
- **`src/test/evo_deterministicmns_tests.cpp`.** The DML assertions are ported as scenario tests on the `testkit` chain: synthetic blocks, keys, ProTx builders and `MemDmlStore`.
  - Both pins: `dip3_activation_*`, `v19_activation_*`, `dip3_protx_*` (registration, update, revoke, payee rotation), `field_bit_migration_validation` and `migration_logic_validation` (the legacy diff reader).
  - v24: `deterministic_mn_list_diff_serialization_is_canonical`, `FuncProUpServInvalidNType` (EVO-15, run under both profiles with the v23 expectation from the v23 oracle), `FuncProUpRegTxVersionHandlingBeforeV24`, and the V24 cases (`migration_rejected_when_key_squatted`, `proupserv_migrates_legacy`, `evonode_raised_to_extaddr_by_service_update_only`, `FuncSameMnSameBlock*`, `FuncPreV24CrossSchemePairCannotBecomeResident`, `FuncSameBlockCrossSchemeKeyPairRejected`, `FuncProRegTxRejectsCrossSchemeKeyReuse`, `FuncProUpRegTxRejectsCrossSchemeKeyReuse`).
  - The mempool cases go to MP1a and `FuncVerifyDB` to CH5.
- **`block_reward_reallocation_tests`:** the payee rotation over many blocks (shared with EV3).

### 6.2 Unit, property and model tests (every PR)

- **`mnlist_model`** (proptest, also a fuzz target): random sequences of `try_add`, `update`, `update_hot`, `remove`, `pose_punish`, `decrease_scores` and `apply_diff` on `MnList` and on the twin's `RefMnList`. After every step: equal canonical bytes, equal payee and projected payees, `check_invariants()`, and `collateral_owner == get_by_collateral` filtered by equality.
- **`mnlist_payee_queue`** (the PLAN's name): payee and projected payees under every `PayeeRules`, on lists with heavy ties, streaks, revivals and all-banned states.
- **Codec round trip:**
  - `decode(encode(x)) == x`, and `encode(decode(b)) == b` or a rejection, on arbitrary bytes, with no panic and bounded allocation;
  - `apply_diff(a, a.diff_to(b)) == b` for random pairs.
- **`core_order`** against a naive recursive trie on adversarial key sets (shared 5k-bit prefixes for k = 1…12; 64-bit collisions).
- **Quirk tests** as in §3.7.

### 6.3 L1: differential targets against the oracle

| Target | Core entry point | Inputs | Compares | Budget |
|---|---|---|---|---|
| `diff_dml_apply` | `oracle_dml_apply` (O2a; extended, below) | a prev list (canonical bytes: real lists at sampled heights + synthetic); a block of 0–40 transactions from a structured generator (every ProTx type and version incl. malformed payloads, duplicates, every quirk 23/42 collision, replacements, QcTx with random members and `validMembers`, ordinary transactions spending collaterals, registered-and-spent); pprev ancestry hashes; coins or `Unchecked`; rules flags | verdict class + reason (or the exception class), the canonical bytes of the new list, payee | ≥ 24 CPU-h on **both** shims with V24 off; ≥ 24 CPU-h on v24 with V24 on |
| `diff_mnlist_codec` | `oracle_mnlist_canonical(bytes)` → Core's `UnserializeCanonicalMNList` verdict, re-encoding, `CanonicalMNListHash` | mutated canonical lists | accept/reject, bytes, hash | ≥ 24 CPU-h (v24 shim) |
| `diff_mnlist_diff` | `oracle_mnlist_diff(a, b)` → `BuildDiff` + canonical diff bytes; `oracle_mnlist_apply_diff(a, d)` | list pairs from `mnlist_model` traces | diff bytes; applied list bytes or throw | ≥ 24 CPU-h (v24 shim) |
| `diff_mnlist_order` | `oracle_mnlist_core_order(bytes)` → `ForEachMN` order; `oracle_mnlist_payee(bytes, v19, mn_rr)`, `oracle_mnlist_projected(bytes, n)` | lists with adversarial prefixes | order, payee, projection | ≥ 24 CPU-h (v24 shim; the code is identical in v23) |

**New oracle work (flagged; Q3).**
1. `oracle_dml_apply` exists only in A1's ABI sketch; the shim branch has no DML entry point today. It needs:
   - explicit pprev ancestry hashes (for `GetAncestor(quorumHeight)`);
   - a `has_best_block` flag (`Coins` vs `Unchecked`);
   - `v24` and `proupserv` rule flags;
   - **a test seam that replaces `llmq::utils::GetAllQuorumMembers` with caller-supplied member lists** per `(type, quorumHash)`. This is a DEC-16 patch in the shim build. Without it, QcTx PoSe is fuzzable only through O2b `diff_chain`.

   It must return the full `oracle_result` and the canonical list bytes.
2. The four list entry points above.
3. A hidden RPC on the O8 patched v24 node: `getcanonicalmnlist <blockhash>` → `{ hex, hash }`, i.e. `SerializeCanonicalMNList` + `CanonicalMNListHash`. EV2a's acceptance ("on imported lists") and the W2 bench start depend on it.

The codec and order targets run on the v24 shim only: v23.1.8 has no `snapshot.*`, and the encodings and iteration code they test are identical at both tags (§2.8, §2.9).

### 6.4 L2: replay windows

- **EV2b merge gate.** Mainnet **1,028,160 → 1,078,159**: `CanonicalMNListHash` per block equal to the O8 `-statedigestlog` of the patched v24 oracle (A1 §4.6).
  - The window starts at DIP3, so the previous list is the default list and no imported state is needed.
  - The harness feeds BS2 blocks through `build_new_list_from_block` + `process_block` with `CollateralView::Unchecked` (Core's own null-best-block path, used by `evodb repair`) and a **recorded** `QuorumMemberSource` built from the oracle's `quorum info <type> <hash>` member lists (in order). It therefore needs neither CH3a's UTXO set nor LQ1a (Q7).
  - The window covers the mass DIP3 registrations, ProUpServ/ProUpReg/ProUpRev, replacements and collateral spends, and the DIP3 enforcement block (`bad-dip3-enf-block` passes). It covers QcTx PoSe only if the first non-null commitments fall inside it. That is not yet checked, because the mainnet oracle is still below DIP3 (height 1,008,550 on 2026-10-08); it is read off the oracle's blocks once the oracle passes 1,078,159. If they fall outside, the PoSe path's L2 evidence is the post-V19 window run with CH7a.
  - On a mismatch, the harness pulls `protx listdiff h−1 h` around it (digest-first, A1 §4.6 X-2).
- **EV2a gate.** `getcanonicalmnlist` at every 576th block from DIP3 and at R4's boundary heights, on mainnet and testnet. Our decoder reads the bytes, our encoder must reproduce them, and our hash must equal Core's. Plus field equality against `protx list registered true <h>` JSON.
- **With CH7a (M4):** full mainnet post-DIP3 and full testnet with real UTXO views and real members; `-consensusxcheck` (fast vs twin per block) on at milestone tags; the segment snapshots at 1,028,159, 1,047,199, 1,899,071, 1,987,775, 2,128,895 and tip − 50k (A1 §4.6).

### 6.5 L3: operators for O10

The operators below run through GBT proposal mode against both references:
- duplicate platformNodeID in-block;
- keyID↔platformNodeID in both orders;
- a duplicate service in-block;
- a duplicate operator key in-block, pre-V24 and with V24;
- a collateral created in the same block;
- a collateral spent by an ordinary tx in the same block as a ProUpServTx for that MN;
- a replacement ProRegTx;
- a QcTx with a wrong quorum hash, beyond the tip, or with a negative computed height;
- a ProUpServTx with out-of-range `nType` (EVO-15 per profile);
- a ProUpRevTx then a ProUpServTx in one block.

### 6.6 Functional tests (O11/O12)

- `feature_dip3_deterministicmns.py`, `feature_dip3_v19.py`, `feature_llmq_simplepose.py` (PoSe), `feature_llmq_evo.py` (four in a row, MN_RR), `feature_masternode_params.py`, `rpc_masternode.py` (winners/payments), `feature_dip4_coinbasemerkleroots.py` (with EV4), `rpc_netinfo.py`, `feature_protx_version.py`.
- Mixed mode A at M5, B and all-Rust at M7 (A1 Appendix D).
- Under `-releaseprofile=23.1`, the V24 parts of v23.1.8's `rpc_netinfo.py` and `feature_asset_locks.py` are the Q1 exceptions.

### 6.7 `-consensusxcheck`

- After each build: the twin's list and payee must be byte-equal (canonical encoding); `check_invariants()` holds; on every collateral probe, typed index == unique-map lookup + equality.
- After each `list_for_block` from the store: the reconstructed list equals the cached one when both exist.
- Any difference aborts (A1 §3.10).

---

## 7. Implementation plan

Each phase is one reviewable PR (CON class: two reviewers + `code-review` high), with its own tests.

| Phase | Content | Depends on | Acceptance |
|---|---|---|---|
| **EV2a-1** | `types`, `unique`, `list` (try_add/update/update_hot/remove with rollback), `index`, `counts`, read API (§2.6 minus `core_order`), `MemDmlStore` + `LruListCache` + `list_for_block` over `MnListDiff` structs (no bytes yet), `testkit` builders | EV0, CS0, BS6a, BS11a-t types | unit tests for every `try_add` failure order; `mnlist_model` against a minimal twin; quirk 23 tests; **LQ1a can start here** (PLAN lever 2) |
| **EV2a-2** | `core_order` (quirk 41), `get_by_operator_key`, `diff_to`/`apply_diff`, `MnStateDiff` | EV2a-1 | `core_order` equals the testnet and mainnet oracles' `protx list` order (RPC) and a naive trie; diff round trip |
| **EV2a-3** | `codec`: canonical list/diff/Dmn/state, `canonical_mn_list_hash`, `core_disk` readers, record envelope | EV2a-2; O8 dump RPC for the gate | L0 `evo_snapshot_tests` subset; codec fuzz; **imported lists: our bytes = Core's bytes, our hash = Core's hash** at every 576th block on both networks (EV2a's PLAN acceptance); `diff_mnlist_codec`/`_diff`/`_order` ≥ 24 CPU-h once O2a's entry points exist |
| **EV2b-1** | `payee`, `projected_payees`, `pose`, confirmations, bookkeeping, streaks; the full reference twin | EV2a-2 | `mnlist_payee_queue` fuzz; quirks 13 and 24; the L0 payee scenarios |
| **EV2b-2** | `build_new_list_from_block` pre-V24 (3a–3d, 3f, step 4) with EVO-15; `process_block`/`undo_block`; error mapping; effects | EV2b-1, EV1 (for scenario tests), `QuorumMemberSource` (EV0) | `diff_dml_apply` (both pins, V24 off) ≥ 24 CPU-h; **L2: mainnet 1,028,160 + 50k digest equal**; quirks 25, 26 (DML half), 42; L0 `evo_deterministicmns_tests` (both pins) |
| **EV2b-3** | V24: 3e, `SetStateVersion`, the probes, the shared spend rule | EV2b-2, BS6c (v3 payloads) | v24 L0 V24 cases; `diff_dml_apply` (v24, V24 on) ≥ 24 CPU-h; a regtest V24 window digest (and testnet once V24 activates there) |
| **EV2b-4** | benchmark gate: benches, PF0 workloads, the Core bench patch | EV2b-2, PF1 budget | `dml_build_w1`/`w2` ≥ 2× the twin (instructions); ≤ dashd's DML instructions per block |

Sizes: EV2a ≈ L (≈ 3 weeks), EV2b ≈ L (≈ 3 weeks), matching the PLAN. EV2b-3 can run in parallel with EV2b-4.

---

## 8. Open questions for the manager

1. **v23.1.8's own V24 rules on regtest.**
   - v23.1.8's `rpc_netinfo.py` and `feature_asset_locks.py` activate v23's V24 (bit 12) on regtest. v23 has different DML rules for that deployment (EVO-11: version handling; EVO-14: payload layout).
   - Under `-releaseprofile=23.1`, rdashd would apply v24's V24 rules.
   - *Recommendation:* declare it a regtest-only deviation (mainnet has V24 NEVER_ACTIVE under both pins). O11 puts those tests' V24 parts on the 23.1 expected-failures list. Do not emulate a dead rule set.
2. **Who owns `process_block`/`undo_block`** (the DML part of `CDeterministicMNManager::ProcessBlock/UndoBlock`: diff, snapshot cadence, `ResetPlatformBan`, `bad-dip3-enf-block`)? *Recommendation:* EV2b. They are pure DML functions; CH7a only orders them.
3. **DEC-16 and oracle scope.**
   - (a) O8 adds `getcanonicalmnlist` to the patched v24 node.
   - (b) An O2a follow-up adds the extended `oracle_dml_apply` (with the member seam) and the four list entry points (§6.3).

   *Recommendation:* yes to both, as a small "O2a-EV" follow-up like O1b was for CS1. EV2a-3's gate needs (a), and EV2b-2's L1 gate needs (b).
4. **Register quirks 41 (CHAMP order) and 42 (in-block list exceptions)** with owners EV2a and EV2b, and EV4/RP2a/AM1 as consumers of 41. *Recommendation:* yes; O5 adds the `#[ignore]` stubs.
5. **Lazy PoSe decay.** *Recommendation:* defer. Make it a PF item only if PF1 shows the DML above 5% of post-DIP3 connect instructions after EV2b's indexes, or the mn CFs exceed dashd's evodb share. It removes about 95% of the diff bytes and the O(p) per-block floor, at the price of a representation that every reader must materialize.
6. **`imbl = "=7.0.2"`, exact pin.** It contains `unsafe` internally (via `imbl-sized-chunks`). *Recommendation:* accept, as A1 chose `imbl`. Record it in the `cargo deny`/vet review notes, and treat a version bump as a CON change that re-runs the model fuzz and the L1 targets.
7. **EV2b's L2 acceptance harness** (§6.4) uses Core's null-best-block path and recorded quorum members, so it does not wait for CH3a or LQ1a. Collateral checks are covered by L1, unit tests and CH7a's full replay. *Recommendation:* approve, and add `i:O8` and `i:LQ1a` (trait only) to EV2b's PLAN row.
8. **A tip snapshot written at each flush,** a derived record used only when `meta.evo_tip` matches. It speeds startup (metric 9) from ≤ 575 diff applications to one decode. *Recommendation:* yes, owned by CH7a, as a non-consensus record outside `mn_snapshot`.

## 9. Proposed errata

- **A1 §2.5.**
  - "O's sorted maps, so that iteration is in Core's order": Core's order is immer CHAMP order (§2.9). memcmp order is what the canonical encoding and the SML root sort into. Add quirk 41's consumers.
  - `MnList.height: u32` → `i32`.
  - The `Dmn`/`MnState` sketch → the hot/cold split (§2.3), with the SML-hash cache per cold state and validity.
- **A1 §2.10.1:** `Effect::ResetPlatformBan { pro_tx_hash }` → `{ pro_tx_hash, height }`.
- **A1 Appendix A:** `QuorumMemberSource::members` takes `&mut dyn EffectSink`. `EvoStore`'s DML methods return `Result` (store errors are fail-stop).
- **A1 §3.6 lever list:** add the per-update unique-key hashing, the per-update SML-entry construction and the per-input collateral hashing that Core does and we avoid. "Payee O(log n)" is a minor part of the gain (§5.2).
- **A1 §2.9:** `mn_snapshot` is written every 576 blocks **and at the first DIP3 block**.
- **O17:** EVO-15's consumers add **EV2b** (`specialtxman.cpp` v23:339-344 is the block-path half of the row).
- **PLAN EV2a/EV2b rows:** EV2a acceptance needs `i:O8` (the dump RPC); EV2b adds `i:LQ1a` (trait) and `i:O8`; EV2b's L1 needs the O2a follow-up (Q3).

## Appendix: evidence

- **CHAMP order.** `python3 -I /work/scratch/ev2-opus/scripts/champ_order.py data/tn-protx.json` → `n 522 match True`; `data/tn-diff-full.json` → `n 524 match True`; the 13-entry `protx diff (tip−2000, tip)` subset matches the full list's CHAMP order filtered to those entries (`True`). Testnet oracle height 1,133,744; MNs: 476 regular, 46 evo, 448 banned.
- **Microbenchmark.** `/work/scratch/ev2-opus/bench-imbl`, `perf stat -e instructions:u`, modes 1 (indexed path) and 2 (twin) minus mode 3 (setup):
  - 3,500/400/4,000: 5.91 G − 0.13 G vs 30.60 G − 0.13 G;
  - 20,000/2,000/1,000: 7.74 G − 0.78 G vs 32.61 G − 0.78 G.

  It is a model of map shapes and per-block operations, not the implementation; PF1/PF0 replace it with real blocks.
