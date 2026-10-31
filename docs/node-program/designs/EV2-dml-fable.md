# EV2: the deterministic masternode list (`dash-evo::dml`), Fable design

| | |
|---|---|
| Status | D2 design, Fable 5.1 leg (PLAN §4.4 rows EV2a and EV2b; reconciled by the manager with the Opus leg) |
| Date | 2026-10-08 |
| Author | EV2 designer (Fable 5.1) |
| Task | EV2a: `MnList` data model (imbl, unique properties, payee-queue structure), diff/snapshot codecs with the canonical `EvoSnapshot` MN-list encoding, `EvoStore` plus an in-memory store. EV2b: `build_new_list_from_block` in Core's order, PoSe, payee selection (queue plus a reference twin), confirmations, collateral spends including ordinary transactions |
| Binding inputs | A1 §2.5, §2.9, §2.10.1–2, §2.25, §3.5, §3.6, §3.9–3.11, §4.4, §4.6, Appendix A; PLAN §4.4 (EV0, EV1, EV2a/b, EV3, EV4, LQ1a/b), §8.2; PRD FR-EVO-01…05, FR-STO-08, NFR-PERF-01…04, NFR-CEQ-12; O17 EVO-11, EVO-13, EVO-15, EVO-25 (and EVO-08, EVO-10 as gated context); R4 §1.8, appendix D §3, quirks 13, 17, 23, 24, 25, 26; CS0 §8.4 (`ConsensusCtx`, `BlockId`, the views); DEC-11, DEC-16, DEC-24 D3, DEC-30 N6, DEC-37 |
| Core verified at | `v23.1.8` and `v24.0.0-rc.3` in `~/workspace/dash` (`git show <tag>:src/…`); every citation below names the pin. Where only one pin is cited the other has the same lines in the same file unless the text says otherwise |
| rust-dashcore verified at | `node/integration` 276685fc (no `dash-evo` crate yet; CS0's `dash-consensus` design r2 approved, DEC-32); `dev` 67b307c6 for the existing `dashcore::sml` code |

> **EMBARGO-1 / EMBARGO-2.** Nothing in this document relates to either. The design was written without looking for them.

---

## 0. Summary

1. **One immutable, persistent list.** `MnList` is a value type: `imbl` maps give O(1) clone and O(log n) mutation with structural sharing, exactly the role `immer::map` plays in Core (`v24 evo/deterministicmns.h:142-144`; `v23 :141-143`). Every mutation goes through three primitives, `add_mn`, `update_mn`, `remove_mn`, which keep **six derived indexes** consistent: the unique-property map (Core's, byte-keyed), the payee queue, the last-payee slot, the unconfirmed set, the penalised set and the streak set, plus O(1) `Counts`. The indexes are never serialized; they are rebuilt from the entries on decode, so a list has one canonical content whatever path produced it.
2. **`build_new_list_from_block` is a line-by-line port** of Core's `RebuildListFromBlock` (`v24 evo/specialtxman.cpp:309-794`; `v23 :185-520`), same order, same reject strings, same throw-to-`failed-procspectxsinblock` mapping. The only differences are inside the list primitives, where the indexes turn Core's O(n) scans (payee, confirmation, PoSe decay, `CalcMaxPoSePenalty` per punishment, streak reset) into O(k log n) work. Each of those scans survives as a **reference twin** under `cfg(any(test, feature = "xcheck"))` and the fast path is cross-checked against it (A1 §3.10).
3. **Payee selection** keeps Core's total order `(effective_height, proTxHash memcmp)` over non-banned MNs as an `imbl::OrdSet` (A1 §2.5), and models the Evo "pay four in a row" rule with a derived `last_payee` slot instead of Core's scan. Unconfirmed MNs are payable; an all-banned list has no payee (quirk 13).
4. **Codecs.** The on-disk snapshot is Core's canonical `EvoSnapshot` MN-list encoding (`v24 evo/snapshot.h:238-298`), so `CanonicalMNListHash` (`v24 evo/snapshot.cpp:150-155`) is computable from our bytes and equal to the patched v24 oracle's. The on-disk diff is Core's `dmn_D4` list-diff encoding verbatim (sorted internal ids, the field-bitmask state diff with the `nVersion`-forcing rule), inside our versioned envelope. Both are the same decoders the EV4 importer needs (FR-EVO-12). Core's HAMT-ordered `dmn_S3` snapshot gets a decode-only, order-insensitive reader for the importer.
5. **`EvoStore`** is A1's trait with ancestry-resolved reads. `MemEvoStore` (in-memory, used by tests, the oracle harness and LQ1a from day one, PLAN M-9c) implements Core's `GetListForBlockInternal` walk: nearest cached list or snapshot along `prev`, diffs applied forward, keyed by block hash so reorgs need nothing special.
6. **Per-profile behaviour** is small: EVO-15 moves one ProUpServTx check out of this component under `V24_0` (EV1 performs it), and the V24-only rules (version unification, cross-scheme operator-key uniqueness, shared masternodes, the ProDisTx-only collateral spend) are deployment-gated, so they are implemented once from the v24 source and are unreachable on mainnet.
7. **Evidence.** L0: a transcribed `evo_deterministicmns_tests` and `evo_sharedmn_tests` set plus our own unit tests; L1: `diff_dml_apply` ≥ 24 CPU-h against O2a's `oracle_dml_apply`, and the model fuzzer `mnlist_payee_queue` against the twin; L2: `CanonicalMNListHash` per block equal over 50k blocks from 1,028,160 and at every segment boundary; PF0: `crit:dml_build_mainnet`, `crit:mnlist_clone_diff`, `crit:mnlist_codec` with a ≥ 2× fast-path-over-twin bench.

---

## 1. Scope and non-goals

### 1.1 In scope (PLAN EV2a)

- The `dash-evo::dml` module: `MnList`, `Dmn`, `MnState`, `MnStateDiff`, `MnListDiff`, `Counts`, `PayeeKey`, the unique-property map, the derived indexes, the three list primitives, the read API consumers need (§2.3).
- The codecs: canonical snapshot bytes and `canonical_mn_list_hash`; the `dmn_D4` list-diff bytes (and the `dmn_D3` legacy reader for EV4); the `dmn_S3` HAMT-order snapshot reader for EV4; our versioned envelope.
- `EvoStore` (the full trait of A1 §2.5 with `at: BlockId` reads, §2.10.2) and `MemEvoStore`, including the list-reconstruction walk and Core-shaped caching (`mnListsCache`, mini-snapshots) as a tunable, unobservable policy.
- FR-EVO-01 (data model, unique-property hashing rules, state diffs), FR-STO-08 (historical list access at any height Core can answer), and the storage-facing half of FR-EVO-05 (`SetStateVersion` as a state transition over `MnState`).

### 1.2 In scope (PLAN EV2b)

- `build_new_list_from_block` and everything it calls: confirmation, PoSe decay and punishment, the eight transaction branches (types 1, 2, 3, 4, 11, 12, 6 and the second pass for collateral spends including ordinary transactions and ProDisTx), payee bookkeeping and the streak reset.
- `payee()` (`GetMNPayee`), `projected_payees()` (`GetProjectedMNPayees`, RPC-only, same comparator), the reference twins and the xcheck harness.
- The store-side apply step Core performs in `CDeterministicMNManager::ProcessBlock` (`v24 evo/deterministicmns.cpp:689-782`): diff construction, the snapshot cadence, the DIP3 enforcement-hash check (`bad-dip3-enf-block`), as a pure function returning the `EvoDelta` slice CH7a commits.
- FR-EVO-02, -03, -04 and the transition half of FR-EVO-05.

### 1.3 Non-goals

- Contextual special-tx checks (`CheckProRegTx` and friends), NetInfo validation and `MakeSignString`: **EV1**. EV2b trusts that every transaction it sees passed `CheckSpecialTxInner` against `pprev`, as Core's `ProcessSpecialTxsInBlock` guarantees (`v24 evo/specialtxman.cpp:871`), and re-checks only what Core re-checks in-block.
- Payload types and their decode: **BS6a** (`ProRegTx` v1–v3, `ProUpServTx`, `ProUpRegTx`, `ProUpRevTx`, `ProUpShareTx`, `ProUpSharedRegTx`, `FinalCommitmentTxPayload`, `MnNetInfo`/`ExtNetInfo`, `DomainPort`, payouts and shares). EV2 consumes them through `Tx::payload(bls)` (CS0 §5).
- BLS key types and encodings: **BS11a-t** (`BlsPkBytesVersioned`, `BlsLazy`). EV2 stores the key in the encoding its state version dictates and never decodes it except where Core's `Get()` is observable (§3.6).
- Payments (`get_block_tx_outs`, payee validation, `SplitAmountByShares`, `GetOwnerPayouts` as a payment rule): **EV3**. EV2 exposes the payee `Dmn`; EV3 turns it into outputs.
- SML entries, `merkleRootMNList`, mnlistdiff building, the evodb importer wiring: **EV4**. EV2a provides `Dmn::sml_hash()` caching slots and the sorted iteration EV4 needs, and the decoders EV4's importer calls.
- Quorum member selection, snapshots, modifiers: **LQ1a/b**. EV2b takes a `QuorumMemberSource` (A1 Appendix A) for the QcTx punishment branch.
- Commitment validation (`CheckLLMQCommitment`, `quorumIndex` rules): **LQ2**. By the time EV2b sees a QcTx, `m_qblockman.ProcessBlock` has validated it (`v24 evo/specialtxman.cpp:895-900`).
- The effect channel's contents: EV2b emits no effects itself. `ResetPlatformBan` (`v24 evo/deterministicmns.cpp:735-750`) is emitted by **CH7a**'s orchestration from the same transaction list (A1 §2.10.1 table), because it belongs to `CDeterministicMNManager::ProcessBlock`, which runs only when `fJustCheck == false`.
- The RocksDB column families, batches and the flush protocol: **CH7a** and **ST1**. EV2a gives them bytes and the trait.
- RPC JSON (`protx list/info/diff`, `masternode list/winners`): **RP2a**; EVO-28/29 are theirs. EV2a exposes the fields.
- `mncache.dat` metadata (FR-EVO-10): **AM1**.
- assumeutxo `EvoSnapshot` reconstruction (historical diffs, quorum snapshot data): out of scope (A1 NG-6). Only the MN-list part of the codec is implemented, because that is what `CanonicalMNListHash` covers.

---

## 2. Data model and public API

### 2.1 Principles

- **Pure and sans-io.** `dash-evo` depends on `dash-consensus` (CS0) and base-sdk types only. No I/O, clocks, globals, logging side effects. `debugLogs` is a `&mut dyn DmlTrace` sink that defaults to no-op; Core's log lines are reproduced as trace events because `feature_dip3_deterministicmns.py` and operators read them, but they never affect results.
- **Determinism lint (DEC-30 N6).** No `std::collections::HashMap`/`HashSet`. The unique-property map is an `imbl::HashMap` behind a newtype that exposes no iteration (§2.2.4), with a fixed-key hasher; that newtype carries the single `#[allow]` with its justification.
- **Immutable entries.** `Dmn` and `MnState` are never mutated in place; `update_mn` installs a new `Arc<Dmn>`. Unchanged MNs keep their `Arc` across blocks, which is what makes diffs O(k) and SML-hash caching free (`Dmn::sml_hash: OnceLock`).
- **Core's integer types.** Heights in `MnState` are `i32` as in Core (`nRegisteredHeight{-1}`, `nPoSeRevivedHeight{-1}`, `nPoSeBanHeight{-1}`) so sentinel semantics and the canonical bytes are identical. Public accessors return `Option<u32>` where a sentinel exists.

### 2.2 Types

```rust
// dash-evo/src/dml/mod.rs
pub mod list;      // MnList and the primitives
pub mod state;     // Dmn, MnState, MnStateDiff
pub mod unique;    // UniqueKey, UniqueMap
pub mod payee;     // PayeeKey, payee(), projected_payees(), the twin
pub mod build;     // build_new_list_from_block, apply_block (EV2b)
pub mod codec;     // canonical snapshot, dmn_D4 diff, dmn_S3/dmn_D3 readers
pub mod store;     // EvoStore, MemEvoStore
#[cfg(any(test, feature = "xcheck"))] pub mod reference;   // the twins
```

#### 2.2.1 Entry and state

```rust
/// Core CDeterministicMN (v24 evo/deterministicmns.h:43-93; v23 :43-93). Immutable.
pub struct Dmn {
    pub pro_tx_hash: ProTxHash,          // uint256; Ord = memcmp of the 32 LE bytes (Core base_blob::operator<)
    pub internal_id: u64,                // VARINT on disk; assigned from MnList::total_registered at registration
    pub collateral: OutPoint,
    pub operator_reward: u16,            // immutable after registration
    pub mn_type: MnType,                 // Regular | Evo (u16 on disk; any value parses, IsValidMnType = < 2)
    pub state: Arc<MnState>,
    sml_hash: OnceLock<Hash256>,         // EV4 fills it (quirk 17: excludes nVersion); cleared by construction only
}

/// Core CDeterministicMNState (v24 evo/dmnstate.h:33-175; v23 :33-157).
#[derive(Clone, PartialEq, Eq)]
pub struct MnState {
    pub version: u16,                    // ProTxVersion; stored as i32 nVersion on disk
    pub registered_height: i32,          // -1 default
    pub last_paid_height: i32,           // 0 default
    pub consecutive_payments: i32,
    pub pose_penalty: i32,
    pub pose_revived_height: i32,        // -1
    pose_ban_height: i32,                // -1; private: IsBanned ⇔ != -1 (dmnstate.h:146-149)
    pub revocation_reason: u16,
    pub confirmed_hash: BlockHash,       // null until confirmed
    pub confirmed_hash_with_protx: Hash256,   // single SHA256(proTxHash ‖ confirmedHash), dmnstate.h:162-169
    pub key_id_owner: PubKeyHash,        // null for shared MNs (V24)
    pub pub_key_operator: BlsPkBytesVersioned, // 48 bytes in the encoding of `version` (legacy iff version == 1); null = 48 zero bytes
    pub key_id_voting: PubKeyHash,
    pub net_info: NetInfo,               // Mn(MnNetInfo) for version < 3, Ext(ExtNetInfo) for version ≥ 3
    pub script_payout: ScriptBuf,        // version < 3
    pub payouts: Vec<PayoutShare>,       // version ≥ 3 (DIP-0026)
    pub shares: Vec<CollateralShare>,    // non-empty ⇔ shared MN (V24)
    pub early_period_blocks: u32,
    pub early_penalty: i64,
    pub script_operator_payout: ScriptBuf,
    pub platform_node_id: PlatformNodeId,   // uint160
    pub platform_p2p_port: u16,          // version < 3 only
    pub platform_http_port: u16,
}
impl MnState {
    pub fn from_pro_reg(tx: &ProRegTx) -> Self;         // dmnstate.h:74-89
    pub fn is_banned(&self) -> bool;                     // pose_ban_height != -1
    pub fn banned_height(&self) -> Option<u32>;
    pub fn is_shared(&self) -> bool;                     // !shares.is_empty()
    pub fn reset_operator_fields(&mut self);             // dmnstate.h:127-135: version = 1, key null, net_info = MnNetInfo::empty(), op payout empty, reason 0, platform_node_id 0; ports NOT reset
    pub fn ban_if_not_banned(&mut self, h: i32);         // :136-141
    pub fn revive(&mut self, h: i32);                    // :150-155: penalty 0, ban -1, revived = h
    pub fn update_confirmed_hash(&mut self, pro_tx: &ProTxHash, confirmed: &BlockHash); // :162-169
}
```

`MnType`, `PayoutShare`, `CollateralShare`, `NetInfo`, `DomainPort` are BS6a's types re-exported; `voting_weight` and `collat_amount` come from the `MnType` table (`v24 evo/dmn_types.h:30-43`: Regular 1 / 1000 DASH, Evo 4 / 4000 DASH).

#### 2.2.2 State diff and list diff

```rust
/// Core CDeterministicMNStateDiff (v24 evo/dmnstate.h:177-324; v23 :157-287). The bit layout is the
/// "nVersion-first" one both pins use on disk under dmn_D4 (v24 dmnstate.h:180-204; v23 :160-178).
pub struct MnStateDiff { pub fields: u32, pub state: MnState /* only masked members are meaningful */ }
pub mod field { pub const VERSION: u32 = 0x1; pub const REGISTERED_HEIGHT: u32 = 0x2; /* … */ pub const EARLY_PENALTY: u32 = 0x40_0000; }
impl MnStateDiff {
    /// v24 dmnstate.h:250-281: member-wise `!=`; netInfo by value through the shared_ptr; pubKeyOperator by the
    /// hash of its *encoding* (v24 :259-268; v23 :221-242 compares the point). VERSION is forced when
    /// PUB_KEY_OPERATOR or NET_INFO is set (:276-280). See §3.7 for why the two pins agree pre-V24.
    pub fn between(a: &MnState, b: &MnState) -> Self;
    pub fn apply_to(&self, target: &mut MnState);        // :316-323
}

/// Core CDeterministicMNListDiff (v24 evo/deterministicmns.h:627-712).
pub struct MnListDiff {
    pub added: Vec<Arc<Dmn>>,                                   // sorted by internal_id (BuildDiff, v24 deterministicmns.cpp:362-366)
    pub updated: BTreeMap<u64, MnStateDiff>,                    // by internal_id; serialized in ascending id order
    pub removed: BTreeSet<u64>,
}
impl MnListDiff { pub fn has_changes(&self) -> bool; }
```

#### 2.2.3 The list

```rust
/// Core CDeterministicMNList (v24 evo/deterministicmns.h:130-625). Clone is O(1).
#[derive(Clone)]
pub struct MnList {
    block_hash: BlockHash,                 // null while a block is being built (RebuildListFromBlock sets it to uint256())
    height: i32,                           // -1 for the pre-DIP3 default list (GetHeightForSnapshotCodec, :341)
    total_registered: u32,                 // nTotalRegisteredCount
    // --- content ---
    by_pro_tx: imbl::OrdMap<ProTxHash, Arc<Dmn>>,          // memcmp order; the iteration order every consumer sees
    by_internal_id: imbl::OrdMap<u64, ProTxHash>,
    unique: UniqueMap,                                     // §2.2.4: Core's mnUniquePropertyMap, byte-keyed
    // --- derived indexes (never serialized; rebuilt by the codecs; §2.4) ---
    payee_queue: imbl::OrdSet<PayeeKey>,                   // non-banned MNs, Core's CompareByLastPaid order
    last_payee: Option<ProTxHash>,                         // the MN (banned or not) with last_paid_height == height
    unconfirmed: imbl::OrdSet<(i32 /*registered*/, ProTxHash)>,   // confirmed_hash null
    penalised: imbl::OrdSet<ProTxHash>,                    // non-banned with pose_penalty > 0
    streaks: imbl::OrdSet<ProTxHash>,                      // Evo with consecutive_payments != 0
    counts: Counts,                                        // Core GetCounts(), maintained incrementally
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts { pub total_evo: u32, pub total_mn: u32, pub total_weighted: u32, pub valid_evo: u32, pub valid_mn: u32, pub valid_weighted: u32 }
impl Counts { pub fn total(&self) -> u32; pub fn enabled(&self) -> u32; }
```

The derived indexes cost one `OrdSet` path copy per mutation each (≈ 5 × O(log n) node copies, n ≈ 3,500 → 3–4 levels of 64-wide nodes). The alternative, Core's scans, costs O(n) per block for five separate loops. §5 quantifies both.

#### 2.2.4 The unique-property map

```rust
/// Key = Core's `GetUniquePropertyHash` *pre-image*: the SER_GETHASH serialization of the value at
/// PROTOCOL_VERSION, with ADDRV2_FORMAT added only for a CService that is not AddrV1-compatible
/// (v24 evo/deterministicmns.h:526-547; v23 :488-510). Two values collide in Core iff their
/// serializations are equal (SHA256d of equal bytes), so keying by the bytes preserves every collision
/// class of quirk 23 (an owner CKeyID and an Evo platformNodeID are both 20 raw bytes; a legacy-encoded and a
/// basic-encoded operator key are different 48-byte strings) without a SHA256d per lookup.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct UniqueKey(SmallVec<[u8; 48]>);
impl UniqueKey {
    pub fn outpoint(o: &OutPoint) -> Self;                 // 36 bytes
    pub fn service(s: &Service) -> Self;                   // 18 bytes (v1-compatible) or BIP155 form
    pub fn domain_port(d: &DomainPort) -> Self;            // v24 evo/netinfo.h:176-180
    pub fn key_id(k: &PubKeyHash) -> Self;                 // 20 bytes (owner, share owners)
    pub fn operator_key(k: &BlsPkBytesVersioned) -> Self;  // the 48 bytes in the state's encoding
    pub fn platform_node_id(p: &PlatformNodeId) -> Self;   // 20 bytes: collides with key_id by design (quirk 23)
}
/// imbl::HashMap behind a newtype: get/insert/remove only, no iteration API, fixed-key FxHasher.
/// This is the one HashMap in dash-evo; the lint exemption lives here with this comment.
#[derive(Clone)]
pub struct UniqueMap(imbl::HashMap<UniqueKey, (ProTxHash, u32 /*refcount*/), FixedHasher>);
```

Null values are never inserted (`AddUniqueProperty` returns false for `nullValue`, `v24 :549-566`): a null `collateral` cannot occur, an empty `MnNetInfo` has no entries, a null operator key is skipped (`AddMN`, `v24 deterministicmns.cpp:497`), a zero `platform_node_id` is skipped (`:505`).

`refcount` matters: a shared MN registers each share owner key under the same MN, and `UpdateUniqueProperty` of an unchanged value is a no-op (`v24 :590-614`), so counts above 1 arise only when one MN legitimately holds the same bytes twice (Core allows `AddUniqueProperty` when the existing holder is the same `proTxHash`). We keep the count so `delete` semantics (`:568-588`: decrement, erase at 1) stay identical.

#### 2.2.5 Payee key

```rust
/// Core CompareByLastPaid (v24 evo/deterministicmns.cpp:132-156; v23 :157-181).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PayeeKey { pub effective_height: i32, pub pro_tx_hash: ProTxHash }
impl PayeeKey {
    pub fn of(d: &Dmn) -> Self {
        let s = &d.state;
        let mut h = s.last_paid_height;
        if s.pose_revived_height != -1 && s.pose_revived_height > h { h = s.pose_revived_height; }
        else if h == 0 { h = s.registered_height; }
        PayeeKey { effective_height: h, pro_tx_hash: d.pro_tx_hash }
    }
}
```

Note the `else if`: a revived height that is set but not above `last_paid_height` leaves `last_paid_height` in force, and the `registered_height` fallback applies only when `last_paid_height == 0` *and* the revived branch did not fire.

### 2.3 `MnList` API

```rust
impl MnList {
    // construction
    pub const fn empty() -> Self;                                           // Core's default list: height -1, null hash, total 0
    pub fn new(block_hash: BlockHash, height: u32, total_registered: u32) -> Self;
    pub fn with_block_hash(self, h: BlockHash) -> Self;                     // SetBlockHash
    pub fn with_height(self, h: u32) -> Self;                               // SetHeight (asserts ≥ 0)

    // metadata
    pub fn block_hash(&self) -> &BlockHash;  pub fn height(&self) -> Option<u32>;  pub fn height_i32(&self) -> i32;
    pub fn total_registered(&self) -> u32;   pub fn counts(&self) -> Counts;        pub fn len(&self) -> usize;

    // lookups (Core GetMN / GetValidMN / GetMNByCollateral / GetValidMNByCollateral / GetMNByService / GetMNByInternalId / GetMNByOperatorKey)
    pub fn get(&self, p: &ProTxHash) -> Option<&Arc<Dmn>>;
    pub fn get_valid(&self, p: &ProTxHash) -> Option<&Arc<Dmn>>;
    pub fn has(&self, p: &ProTxHash) -> bool;  pub fn is_valid(&self, p: &ProTxHash) -> bool;  pub fn is_banned(&self, p: &ProTxHash) -> bool;
    pub fn by_collateral(&self, o: &OutPoint) -> Option<&Arc<Dmn>>;
    pub fn valid_by_collateral(&self, o: &OutPoint) -> Option<&Arc<Dmn>>;
    pub fn by_service(&self, s: &Service) -> Option<&Arc<Dmn>>;
    pub fn by_internal_id(&self, id: u64) -> Option<&Arc<Dmn>>;
    pub fn by_operator_key(&self, k: &BlsPkBytesVersioned) -> Option<&Arc<Dmn>>;   // the given encoding only (GetMNByOperatorKey, v24 :119-127)
    pub fn has_unique(&self, k: &UniqueKey) -> bool;
    pub fn unique_holder(&self, k: &UniqueKey) -> Option<&Arc<Dmn>>;
    /// v24 deterministicmns.h:471-482 (V24 only, EVO-13): both encodings probed; `self_` excluded; invalid key → false.
    pub fn has_operator_key_under_any_scheme(&self, pk: &BlsPublicKey, self_: &ProTxHash) -> bool;

    // iteration: always memcmp order of proTxHash (Core's own ForEachMN order is HAMT order; §3.9 shows no result depends on it)
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Dmn>>;                 // all, banned included
    pub fn iter_valid(&self) -> impl Iterator<Item = &Arc<Dmn>>;           // non-banned
    pub fn iter_sorted_for_sml(&self) -> impl Iterator<Item = &Arc<Dmn>>;  // == iter(); named for EV4

    // mutation primitives (Core AddMN / UpdateMN / RemoveMN, v24 deterministicmns.cpp:439-679)
    pub fn add_mn(&mut self, d: Arc<Dmn>, bump_total: bool) -> Result<(), ListError>;
    pub fn update_mn(&mut self, p: &ProTxHash, new_state: Arc<MnState>) -> Result<(), ListError>;
    pub fn update_mn_with_diff(&mut self, p: &ProTxHash, diff: &MnStateDiff) -> Result<(), ListError>;
    pub fn remove_mn(&mut self, p: &ProTxHash) -> Result<(), ListError>;

    // PoSe (v24 :267-334)
    pub fn calc_max_pose_penalty(&self) -> i32;             // max(100, counts.total())
    pub fn calc_penalty(&self, percent: i32) -> i32;        // max * percent / 100 (integer)
    pub fn pose_punish(&mut self, p: &ProTxHash, penalty: i32, trace: &mut dyn DmlTrace) -> Result<(), ListError>;
    pub fn decrease_scores(&mut self);

    // payee (§3.4)
    pub fn payee(&self, flags: PayeeFlags) -> Option<&Arc<Dmn>>;
    pub fn projected_payees(&self, flags: PayeeFlags, n: usize) -> Vec<Arc<Dmn>>;

    // diffs (v24 :336-437)
    pub fn build_diff(&self, to: &MnList) -> MnListDiff;
    pub fn apply_diff(&mut self, block_hash: BlockHash, height: u32, diff: &MnListDiff) -> Result<(), ListError>;
    pub fn apply_diff_for_snapshot(&mut self, block_hash: BlockHash, height: u32, total_registered: u32, diff: &MnListDiff) -> Result<(), ListError>;

    // equality
    pub fn is_equal(&self, rhs: &MnList) -> bool;           // Core IsEqual (:487-526): ignores internal ids and total_registered
    pub fn eq_strict(&self, rhs: &MnList) -> bool;          // Core operator== (:616-624)

    // codecs: §2.5
    pub fn canonical_bytes(&self) -> CoreBytes;             // SerializeCanonicalMNList
    pub fn canonical_hash(&self) -> Hash256;                // CanonicalMNListHash
    pub fn from_canonical_bytes(b: &[u8]) -> Result<Self, CodecError>;
}

#[derive(Clone, Copy)]
pub struct PayeeFlags { pub v19_after_prev: bool, pub mn_rr_after_prev: bool }

#[derive(Debug, thiserror::Error)]
pub enum ListError {        // every variant maps to Core's thrown std::runtime_error (text kept for the trace)
    DuplicateProTxHash(ProTxHash), DuplicateInternalId(u64),
    DuplicateUniqueProperty { kind: UniqueKind, holder: ProTxHash, mn: ProTxHash },
    InvalidAddress(ProTxHash), MissingMn(ProTxHash), MissingInternalId(u64),
    NegativeHeight, SnapshotIdentityReuse,
}
```

`add_mn` is the only place internal ids are assigned when `bump_total` is true: `total_registered = max(id + 1, total_registered)` (`v24 :513-517`). `build_new_list_from_block` creates the `Dmn` with `internal_id = list.total_registered()` (`v24 specialtxman.cpp:365`).

**Atomicity.** Core snapshots `mnUniquePropertyMap` before each primitive and restores it on failure (`v24 deterministicmns.cpp:452`, `:525`, `:621`). With persistent maps the Rust primitives are naturally transactional: they compute the new maps into locals and commit all of them at the end, so a failing primitive leaves `self` untouched.

### 2.4 Index maintenance rules

The six indexes are a function of `(by_pro_tx, height)` and nothing else. A single private `fn reindex_entry(&mut self, old: Option<&Dmn>, new: Option<&Dmn>)` is called by each primitive with the before/after entry and applies these rules, so every mutation path (block build, `apply_diff`, codec decode) maintains them identically:

| Index | Membership rule | Key |
|---|---|---|
| `payee_queue` | `!state.is_banned()` | `PayeeKey::of(d)` |
| `last_payee` | `state.last_paid_height == self.height` (banned or not) | the single such MN (at most one: only payee bookkeeping writes `last_paid_height`, once per block; the decoder rejects a second) |
| `unconfirmed` | `state.confirmed_hash.is_null()` | `(registered_height, pro_tx_hash)` |
| `penalised` | `!is_banned() && pose_penalty > 0` | `pro_tx_hash` |
| `streaks` | `mn_type == Evo && consecutive_payments != 0` | `pro_tx_hash` |
| `counts` | per `GetCounts` (`v24 deterministicmns.h:269-294`) | `±1`/`±weight` on add, remove and on a banned-flag change |

`with_height` re-derives `last_payee` (it is the only index that depends on `height`). The codecs call `reindex_entry(None, Some(d))` per decoded entry, then `with_height`. A `debug_assert_indexes()` recomputes all six from scratch and compares; the xcheck harness runs it after every block (§6.4).

### 2.5 Codecs

| Codec | Direction | Bytes | Used by |
|---|---|---|---|
| `canonical` | encode + decode | Core `SerializeCanonicalMNList` / `UnserializeCanonicalMNList` (`v24 evo/snapshot.h:238-298`) | `mn_snapshot` CF (A1 §2.9), `CanonicalMNListHash`, StateDigest, the O8/O2a vectors, EV4 import of `EvoSnapshot` files |
| `list_diff_d4` | encode + decode | Core `CDeterministicMNListDiff::Serialize/Unserialize` under `SER_DISK, CLIENT_VERSION` (`v24 evo/deterministicmns.h:639-712`) | `mn_diff` CF; EV4 import of `dmn_D4` rows |
| `list_diff_d3` | decode only | the same with `CDeterministicMNStateDiffLegacy` (`v24 evo/dmnstate.h:327-483`) | EV4 import of pre-migration evodbs (FR-EVO-12) |
| `list_s3` | decode only | Core `CDeterministicMNList::Unserialize` (`v24 deterministicmns.h:235-255`): base fields then a compact-size vector of `CDeterministicMN` in HAMT order; `AddMN(bump=false)` each | EV4 import of `dmn_S3` rows |
| `dmn` / `state` / `state_diff` | encode + decode | `CDeterministicMN` (`:69-83`, with `nType` because the disk stream is `CLIENT_VERSION`), `CDeterministicMNState` (`dmnstate.h:93-125`), `CDeterministicMNStateDiff` (`:288-314`) | all of the above |

**Canonical MN-list layout** (`v24 evo/snapshot.h:238-248`; stream `SER_DISK, CLIENT_VERSION`; hash = SHA256d over these bytes, `snapshot.cpp:150-155`):

```
blockHash[32] ‖ i32 height ‖ u32 totalRegistered ‖ CompactSize(n) ‖ n × DMN, sorted by proTxHash (base_blob operator<, i.e. memcmp)
DMN   = proTxHash[32] ‖ VARINT(internalId) ‖ COutPoint(hash[32] ‖ u32 n) ‖ u16 operatorReward ‖ STATE ‖ u16 nType
STATE = i32 nVersion ‖ i32 registered ‖ i32 lastPaid ‖ i32 consecutive ‖ i32 poSePenalty ‖ i32 poSeRevived ‖ i32 poSeBan ‖ u16 revocationReason
      ‖ confirmedHash[32] ‖ confirmedHashWithProRegTxHash[32] ‖ keyIDOwner[20]
      ‖ pubKeyOperator[48] in legacy encoding iff nVersion == 1 (CBLSLazyPublicKeyVersionWrapper)
      ‖ keyIDVoting[20] ‖ NetInfoSerWrapper(netInfo, extended = nVersion ≥ 3)
      ‖ (nVersion ≥ 3 ? payouts ‖ shares ‖ u32 earlyPeriodBlocks ‖ i64 earlyPenalty : scriptPayout)
      ‖ scriptOperatorPayout ‖ platformNodeID[20] ‖ (nVersion < 3 ? u16 platformP2PPort ‖ u16 platformHTTPPort : ∅)
```

`NetInfoSerWrapper` writes `MnNetInfo` as one legacy `CService` (16 bytes + BE port; empty = 18 zero bytes, since v23's normalisation) or `ExtNetInfo` with ADDRv2 entries (`v24 evo/netinfo.h:466-506`, `:230-250`). BS6a owns these encoders; EV2a calls `core_serialize` on them.

**Decoder rules reproduced from `UnserializeCanonicalMNList`** (`v24 snapshot.h:250-298`), each with Core's error text: negative height; `n > 100_000`; per entry: a bounded compact-size budget of 10,000 nested items; the re-encoding must equal the consumed bytes (`CheckCanonicalEncoding`, `:193-201`, after materialising the lazy key, `:158-162`); `internalId` present; `payouts.len() ≤ 8`; non-empty `netInfo` must validate (BS6a's `validate()` == `Success`); strictly increasing `proTxHash`; at most 8 consecutive entries sharing the same leading 64 bits; `add_mn(bump=false)` errors become codec errors; and `max(internalId) < totalRegistered` when `n > 0`. `canonical_bytes()` applies `ValidateCanonicalMNInvariants` (`snapshot.cpp:104-125`) before encoding so we never write what Core would refuse to read.

**Our envelope** (DEC-11; A1 §2.9 "Formats"): every `mn_snapshot` and `mn_diff` value is `u8 schema_version ‖ u8 codec_id ‖ payload`, where `codec_id ∈ {CANONICAL_V3, DIFF_D4}`. No checksum: RocksDB has block checksums. The envelope lets a later `codec_id` (for example a compressed diff) coexist without a migration.

**`dmn_D4` diff layout** (`v24 deterministicmns.h:639-660`): `vector<CDeterministicMN>` added (compact size + entries, in ascending `internalId` as `BuildDiff` sorted them) ‖ `CompactSize(updated)` then for each updated in ascending `internalId`: `VARINT(internalId) ‖ STATE_DIFF` ‖ `CompactSize(removed)` then `VARINT(internalId)` each ascending. `STATE_DIFF = VARINT(fields) ‖ masked members in the `members` tuple order` (`dmnstate.h:216-240`), where `pubKeyOperator` uses the wrapper with `state.nVersion == 1` and `netInfo` with `state.nVersion ≥ 3`; decoding a diff with `PUB_KEY_OPERATOR` or `NET_INFO` set but not `VERSION` is an error (`:292-295`). Note the tuple order is **not** the bit order: `payouts`, `shares`, `nEarlyPeriodBlocks`, `nEarlyPenalty` serialize between `scriptPayout` and `scriptOperatorPayout`.

### 2.6 `EvoStore` and `MemEvoStore`

```rust
/// A1 §2.5 + §2.10.2 (every historical read takes `at`). dashd-chainstate implements it over chainstate/;
/// MemEvoStore is the test, oracle-harness and LQ1a implementation.
pub trait EvoStore: Sync {
    fn best_block(&self) -> Option<BlockHash>;
    /// Core GetListForBlock (v24 deterministicmns.cpp:826-939): the list *after* connecting `at`;
    /// MnList::empty() for any block where DIP3 is not active (:830-832).
    fn mn_list(&self, at: BlockId, chain: &dyn ChainAncestry, deployments: &dyn DeploymentView) -> Result<Arc<MnList>, StoreError>;
    fn mn_list_snapshot(&self, at: &BlockHash) -> Option<Arc<MnList>>;    // only where one was written
    fn mn_list_diff(&self, at: &BlockHash) -> Option<Arc<MnListDiff>>;    // per block from DIP3
    fn credit_pool(&self, at: &BlockHash) -> Option<CreditPool>;           // EV3
    fn mnhf_signals(&self, at: &BlockHash) -> Option<Signals>;             // EV3
    fn mined_commitment(&self, t: LlmqType, q: &QuorumHash) -> Option<(FinalCommitment, BlockHash)>;              // LQ2
    fn mined_commitments_until(&self, at: BlockId, t: LlmqType, n: usize, index: Option<i16>) -> Vec<(FinalCommitment, BlockHash)>; // LQ2
    fn quorum_snapshot(&self, t: LlmqType, cycle_base: &BlockHash) -> Option<QuorumSnapshot>;                   // LQ1b
}

/// The DML slice of A1's EvoDelta; CH7a assembles the rest.
pub struct DmlDelta { pub block_hash: BlockHash, pub height: u32, pub diff: MnListDiff, pub snapshot: Option<Arc<MnList>>, pub list: Arc<MnList> }

pub struct MemEvoStore { /* BTreeMap<BlockHash, Arc<MnList>> lists (snapshots + cache), BTreeMap<BlockHash, Arc<MnListDiff>> diffs, …, cache policy */ }
impl MemEvoStore {
    pub fn apply(&mut self, delta: &DmlDelta);                 // what CH7a's batch does: put diff, maybe snapshot, cache the list
    pub fn undo(&mut self, at: &BlockHash);                    // Core UndoBlock (:784-816): drops the cached list; the diff is NOT erased
    pub fn set_tip(&mut self, at: Option<BlockId>);            // UpdatedBlockTip → cache retention policy
}
```

**The reconstruction walk** (`v24 deterministicmns.cpp:826-939`, same in v23 `:778-870`) is a free function so both stores share it:

```rust
pub fn resolve_list(reader: &dyn DmlReader, at: BlockId, chain: &dyn ChainAncestry, deployments: &dyn DeploymentView) -> Result<Arc<MnList>, StoreError>
```

1. If DIP3 is not active at `at`: `MnList::empty()`.
2. Walk `at, prev(at), …`: stop at the first block with a cached list or a stored snapshot; otherwise push its diff and continue. A block with neither a snapshot nor a diff is the **initial snapshot**: `MnList::new(hash, height, 0)` (`:881-889`). This is how a datadir whose evodb predates DIP3 activation bootstraps; in our store it happens exactly once, at `DIP3Height − 1`'s successor, because CH7a writes a diff for every DIP3-active block.
3. Apply the pushed diffs forward (`apply_diff(hash, height, diff)`), caching a **mini-snapshot** every 32 applied heights, as Core does (`:899-917`; EVO-25 says cache policy is memory-only).
4. Retention (`CleanupCache`, `:975-1010`): keep the tip's list, every list at a DKG-interval boundary still within `dkgInterval × (keepOldConnections + 1)` of the tip, drop lists older than `LIST_DIFFS_CACHE_SIZE`. The policy is a `CachePolicy` value the runtime can tune; nothing observable depends on it.

**Why `undo` keeps the diff.** Core's `UndoBlock` reads the diff to compute the inverse `MNListUpdates` for listeners and erases only the caches (`:784-816`); the diff row stays, keyed by hash (A1 §2.9 table "kept"). `mn_list(at)` for a block on an abandoned branch therefore still works, which `protx diff` and `mnlistdiff` serving across a reorg rely on.

### 2.7 Neighbours and exchanged types

| Direction | Task | Types exchanged |
|---|---|---|
| upstream | **EV0** | the skeleton of `MnList`/`Dmn`/`MnState`/`MnListDiff`/`EvoStore`/`EvoDelta`/`Effect`; EV2a fills them in. If EV0 has not merged, EV2a lands them (PLAN §8.2 "EV0 waits for CS0 and BS6a types") |
| upstream | **CS0** | `Tx`, `TxRef`, `Block`, `BlockId`, `ChainAncestry`, `DeploymentView`, `Deployment`, `BlsCtx`, `ValidationError`/`BlockValidationResult`, `CoreBytes`, `core_serialize` |
| upstream | **BS6a / BS4a / BS11a-t** | payload structs, `NetInfo`, `Service`, `DomainPort`, `PayoutShare`, `CollateralShare`, `MnType`, `BlsPkBytesVersioned`, `BlsPublicKey`; `OutPoint`, `ScriptBuf` |
| upstream | **EV1** | nothing at the type level; the contract that each special tx passed `check_special_tx_inner` against `pprev` |
| upstream | **LQ1a/b** | `QuorumMemberSource::members(t, base) -> QuorumMembersResult` for the QcTx branch (A1 Appendix A) |
| upstream | **CS4 / BS5** | `DeploymentView`, `ChainParams { masternode_minimum_confirmations, dip3_enforcement: (height, hash), llmqs[].dkg_interval }` |
| downstream | **CH7a** | `build_new_list_from_block(…) -> Result<MnList, DmlError>`, `make_delta(prev, new, params) -> Result<DmlDelta, DmlError>` (the `ProcessBlock` half), `EvoStore`; CH7a maps `DmlError` to `ValidationError` and emits `Effect::ResetPlatformBan` for each ProUpServTx when `!just_check` |
| downstream | **EV3** | `payee()` → `Arc<Dmn>` (`v24 masternode/payments.cpp:152-157`), `Dmn::operator_reward`, `MnState::{script_operator_payout, payouts, script_payout, shares}`; `counts()` for "no masternode registered" |
| downstream | **EV4** | `iter_sorted_for_sml()`, `Dmn::sml_hash` slot, `build_diff`/set differences by `pro_tx_hash`, every codec reader, `MnList::new` + `add_mn(bump=false)` for the importer |
| downstream | **LQ1a/b** | `iter_valid()` with `confirmed_hash_with_protx`, `collateral` (tie-break), `mn_type` (Evo-only quorums), `is_banned` for `MnsUsedAtH` partitioning (`v24 llmq/utils.cpp:118-187, 397-406`); `EvoStore::mn_list(at)` at `base − 8` |
| downstream | **GV1a / GV1b** | `counts().valid_weighted`, `iter_valid()` with `key_id_voting` and `mn_type` |
| downstream | **RP2a** | `projected_payees()`, lookups, `MnListDiff` field masks for `protx listdiff`, `internal_id` |
| downstream | **MP1a** | lookups by collateral, keys, services, operator key under either encoding (EVO-26 profile) |
| downstream | **AM1 / LK*/ SG*** | `by_operator_key`, `by_service`, `iter_valid()` for MN connections and signing membership |
| downstream | **dash-spv, desktop** | nothing direct: they consume EV4's `mnlistdiff`/`qrinfo` over P2P and RP2a's RPC |

---

## 3. Core-equivalence

### 3.1 Function map

| Core (v24.0.0-rc.3) | v23.1.8 | Rust | Notes |
|---|---|---|---|
| `CSpecialTxProcessor::RebuildListFromBlock` `evo/specialtxman.cpp:309-794` | `:185-520` | `dml::build::build_new_list_from_block` | §3.3 |
| `HandleQuorumCommitment` `:283-298` | `:159-174` | `build::handle_quorum_commitment` | quirk 24 |
| `SetStateVersion` `:51-106`, `IsSchemeMigration` `:46-49`, `ApplyProUpServTx` `:110-137` | absent (inline at `:350-367`) | `state::set_state_version`, `is_scheme_migration`, `apply_pro_up_serv` | V24 gated except the pre-V24 subset (§3.5) |
| `CDeterministicMNList::GetMNPayee` `evo/deterministicmns.cpp:158-194` | `:183-219` | `MnList::payee` | §3.4; twin `reference::payee_scan` |
| `GetProjectedMNPayees` `:196-249` | `:221-274` | `MnList::projected_payees` | RPC only |
| `CalcMaxPoSePenalty`/`CalcPenalty`/`PoSePunish`/`DecreaseScores`/`PoSeDecrease` `:267-334` | `:292-359` | `MnList::{calc_max_pose_penalty, calc_penalty, pose_punish, decrease_scores}` | twin `reference::{max_penalty_scan, decrease_scan}` |
| `BuildDiff` `:336-369`, `ApplyDiff` `:371-393`, `ApplyDiffForSnapshot` `:395-437` | `:361-419` (no `ApplyDiffForSnapshot`) | `MnList::{build_diff, apply_diff, apply_diff_for_snapshot}` | `build_diff` uses `OrdMap::diff` with the twin `reference::build_diff_scan` |
| `AddMN`/`UpdateMN`/`RemoveMN` `:439-679` | `:420-647` | `MnList::{add_mn, update_mn, remove_mn}` | §3.6 |
| `CDeterministicMNManager::ProcessBlock` `:689-782` | `:649-734` | `build::make_delta` (pure) + CH7a (persist, effect) | §3.8 |
| `UndoBlock` `:784-816` | `:736-769` | `MemEvoStore::undo` / CH7a | |
| `GetListForBlockInternal` `:826-939`, `CleanupCache` `:975-1010` | `:778-870`, `:906-947` | `store::resolve_list`, `CachePolicy` | |
| `CDeterministicMNState::{ResetOperatorFields, BanIfNotBanned, Revive, UpdateConfirmedHash}` `evo/dmnstate.h:127-169` | `:112-148` | `MnState::*` | |
| `CDeterministicMNStateDiff` `evo/dmnstate.h:177-324` | `:157-287` | `MnStateDiff` | §3.7 |
| `SerializeCanonicalMNList`/`Unserialize…`/`CanonicalMNListHash` `evo/snapshot.h:238-298`, `snapshot.cpp:150-155` | absent | `codec::canonical` | v23 lacks the codec; the hash is a v24-oracle-only golden (A1 §2.25 item 1) |
| `GetUniquePropertyHash`, `Add/Delete/UpdateUniqueProperty` `evo/deterministicmns.h:526-614` | `:488-567` | `UniqueKey`, `UniqueMap` | byte-keyed pre-image, §2.2.4 |
| `HasOperatorKeyUnderAnyScheme` `evo/deterministicmns.h:471-482` | absent | `MnList::has_operator_key_under_any_scheme` | EVO-13, gated |

### 3.2 `build_new_list_from_block` signature

```rust
pub struct BuildInput<'a> {
    pub prev: &'a MnList,                      // list at pprev (MnList::empty() below DIP3)
    pub block: &'a Block,
    pub prev_id: BlockId,                      // pindexPrev
    pub ctx: &'a ConsensusCtx<'a>,             // height = prev.height + 1; chain, deployments, params
    pub coins: CollateralView<'a>,             // Pprev(&dyn CoinsView) | Unchecked (the shim's "no best block" mode)
    pub members: &'a dyn QuorumMemberSource,   // LQ1; only consulted for non-null QcTx
    pub bls: BlsCtx,                           // forwarded to Tx::payload; never used for state encoding
    pub trace: &'a mut dyn DmlTrace,
}
pub fn build_new_list_from_block(input: BuildInput<'_>) -> Result<MnList, DmlError>;

#[derive(Debug)]
pub enum DmlError {
    /// Core state.Invalid(BLOCK_CONSENSUS, reason) from inside RebuildListFromBlock
    Invalid { reason: &'static str },
    /// Core: a std::runtime_error thrown by AddMN/UpdateMN/RemoveMN/PoSePunish, caught by ProcessSpecialTxsInBlock
    /// → "failed-procspectxsinblock" (v24 evo/specialtxman.cpp:993-995; v23 :701-703). CH7a maps it.
    ListThrow(ListError),
}
```

The function **never reads the release profile**: everything it does differently between the pins is deployment-gated or lives in EV1 (EVO-15). CS0's `UngatedRules` is reachable through `ctx.rules` but unused here, and a unit test asserts no field of it is read (so a later profile row cannot creep in unreviewed).

### 3.3 Order of operations (quirk 25)

Numbered as A1 §2.5 and R4 D §3.3; Core lines are v24 `evo/specialtxman.cpp` unless marked v23 (`evo/specialtxman.cpp`).

0. **Preconditions.** `prev.is_empty_default() || prev.block_hash() == hash(prev_id)` (`:317`; v23 `:192`) is a `debug_assert` and a `DmlError::Invalid("dml-prev-list-mismatch")` in release builds (unreachable from CH7a; it protects the shim harness). `new = prev.clone().with_block_hash(null).with_height(h)` where `h = prev_height + 1` (`:319-323`; v23 `:194-198`).
1. **Payee** from `prev`: `payee = prev.payee(PayeeFlags { v19_after_prev, mn_rr_after_prev })` (`:325`; v23 `:200`). Computed **before** any mutation; the flags are `DeploymentActiveAfter(pindexPrev, …)`.
2. **Confirmation** (`:330-343`; v23 `:205-218`). For every MN of `prev` with a null `confirmed_hash` and `prev_height − registered_height ≥ params.masternode_minimum_confirmations` (mainnet 15, every other network 1: `v24 chainparams.cpp:177, 386, 569, 810`): `update_confirmed_hash(pro_tx_hash, hash(prev_id))`. Fast path: pop `new.unconfirmed` while `registered ≤ prev_height − min_conf`; the set is keyed by registered height so the candidates are a prefix. Iteration over `prev` and mutation of `new` are equivalent here because nothing above this line mutates `new` (Core's own comment, `:327-329`). The confirmed hash is `hash(pprev)`, so an MN registered at R is confirmed in `list(R + min_conf + 1)` with `confirmedHash = hash(R + min_conf)`.
3. **`decrease_scores`** (`:345`; v23 `:220`): every non-banned MN with `pose_penalty > 0` gets `−1` (`v24 deterministicmns.cpp:309-334`). Fast path iterates `penalised` (a snapshot copy, since each update mutates the set).
4. **Flags**: `mn_rr = DeploymentActiveAfter(prev, MN_RR)` (`:347`), `v24 = ctx.deployments.active_after(prev, V24)` (passed in by `ProcessSpecialTxsInBlock` as `rules.v24`, `validation.cpp:2659` → `GetSpecialTxRules(pindex->pprev)`; v23 computes `is_v24_deployed` inline, `:224`).
5. **Transactions in block order, coinbase skipped, non-special-version transactions skipped** (`:350-356`): one loop, branches below. Any `tx.payload()` failure is `Invalid("bad-protx-payload")` / `"bad-qc-payload"` (unreachable after EV1, kept).
   - **ProRegTx** (`:358-453`; v23 `:232-315`): `dmn = Dmn { internal_id: new.total_registered(), mn_type, pro_tx_hash: txid, collateral: if payload.collateral.hash.is_null() { (txid, n) } else { payload.collateral } }` (`:365-373`). **Collateral re-check** (`:378-387`): under `CollateralView::Pprev` and for an external collateral, the coin must exist, be unspent and have `value == collat_amount(mn_type)`, else `Invalid("bad-protx-collateral")`; `Unchecked` skips it, matching Core's `view.GetBestBlock().IsNull()` branch for synthetic views. **Replacement** (`:389-401`): if `new.by_collateral(dmn.collateral)` exists, `remove_mn` it (the new MN goes to the back of the payment queue because it gets a fresh `registered_height` and `last_paid_height == 0`). **Netinfo uniqueness against `new`** (`:403-415`): for each entry, an addr-port or domain-port already held → `Invalid("bad-protx-dup-netinfo-entry")`; an entry that is neither → `Invalid("bad-protx-netinfo-entry")`. **Key uniqueness** (`:416-427`): shared → each share owner key and the operator key; else owner key or operator key held → `Invalid("bad-protx-dup-key")`. State: `MnState::from_pro_reg` with `registered_height = h`; empty netinfo → `ban_if_not_banned(h)` (`:431-437`). V24 only (`:443-446`): `has_operator_key_under_any_scheme(key, null)` → `Invalid("bad-protx-dup-key")`. Then `add_mn(bump=true)` (`:448`); a `ListError` here (duplicate `platform_node_id`, quirk 23's cross-namespace collision with an owner key registered earlier in the block, or a duplicate collateral that `by_collateral` could not see because the holder is the same MN) is `DmlError::ListThrow`.
   - **ProUpServTx** (`:454-521`; v23 `:313-379`). Order as it is **reachable** under both profiles (§3.10): netinfo entries held by *another* MN in `new` → `"bad-protx-dup-netinfo-entry"` / `"bad-protx-netinfo-entry"` (`:463-477`; v23 `:322-336`); MN missing → `"bad-protx-hash"` (`:479-482`; v23 `:338-341`); `payload.mn_type != dmn.mn_type` → `"bad-protx-type-mismatch"` (`:483-485`; v23 `:342-344`); `!is_valid_mn_type` → `"bad-protx-type"` (v23 `:345-347`, after mismatch; v24 `:459-461`, before the netinfo loop). Then `apply_pro_up_serv(state, payload, v24)` (`:487-493`, §3.5), **revive** if banned and the operator key is non-null and the voting key non-null and (shared or owner key non-null) (`:494-504`; v23 `:369-377` without the shared clause); V24 only, if `is_scheme_migration(current, target)` and the *old* key is held under either scheme by another MN → `"bad-protx-dup-key"` (`:511-515`); `update_mn` (`:517`).
   - **ProUpRegTx** (`:522-586`; v23 `:384-413`): missing → `"bad-protx-hash"`; shared → `"bad-protx-shared-mn"` (`:532-535`, gated); `operator_changed = state.key != payload.key` as **points** (Core `CBLSLazyPublicKey::operator!=` ignores the encoding, `:538`); `target = if v24 { max(old, payload.version) } else if changed { payload.version } else { old }` (`:539-540`); V24 only (`:549-556`): if `changed || is_scheme_migration(old, target)` and the new key is held under either scheme by another MN → `"bad-protx-dup-key"`; if changed: `reset_operator_fields(); ban_if_not_banned(h); key = payload.key` (`:558-563`); `key_id_voting = payload.voting` (`:564`); `set_state_version(target)` (`:568-570`, §3.5; pre-V24 this is exactly v23's `nVersion = old > 2 ? old : tx.nVersion; netInfo = MakeNetInfo(nVersion)` at `v23 :401-402`); payouts: `target ≥ 3` → `payouts = payload.version ≥ 3 ? payload.payouts : legacy_payout_as_list(payload.script_payout); script_payout.clear()` else `script_payout = payload.script_payout; payouts.clear()` (`:571-579`; v23 `:406` only sets `scriptPayout`); `update_mn` (`:581`).
   - **ProUpRevTx** (`:587-618`; v23 `:414-434`): missing → `"bad-protx-hash"`; `target = if v24 { max(old, payload.version) } else { 1 }` (`:602-605`); `reset_operator_fields()` (`:606`); `if target ≥ 2 { set_state_version(target)? }` (`:607-609`, gated: pre-V24 `target == 1`); `ban_if_not_banned(h)`; `revocation_reason = payload.reason` (`:610-611`); `update_mn`.
   - **ProUpShareTx** (`:619-648`) and **ProUpSharedRegTx** (`:649-701`): V24 only; ported line by line from v24 (`"bad-proupshare-index"`, `"bad-proupshare-payee-reuse"`, `"bad-proupsharedreg-not-shared"`, `"bad-protx-dup-key"`, `"bad-proupsharedreg-payee-reuse"`, the `SetLegacy(false)` after a key change at `:680`). Under the `V23_1` profile these types never reach EV2b (EV1 rejects them, EVO-09), and on mainnet never at all.
   - **QcTx** (`:702-728`; v23 `:435-461`): null commitment → nothing. Else unknown `llmq_type` → `"bad-qc-commitment-type"`; `quorum_height = qc.n_height − qc.n_height % dkg_interval + quorum_index` using the **payload's** `nHeight` (`:712-714`); `chain.ancestor(prev_id, quorum_height)` missing or its hash `!= quorum_hash` → `"bad-qc-quorum-hash"` (`:715-719`; `ancestor` returns `None` above `prev`'s height as Core's `GetAncestor` returns null); `members = input.members.members(llmq_type, base)` (`:723-725`); `handle_quorum_commitment` (`:726`): for `i` in `0..members.len()`, if `new.has(members[i])` and `!valid_members[i]`, `new.pose_punish(members[i], new.calc_penalty(66))` (quirk 24: the penalty is added to already-banned MNs, and `calc_max_pose_penalty` reads `counts.total()` of the list **as rebuilt so far**, registrations earlier in the block included). `pose_punish` on a missing MN throws in Core; unreachable after the `has` check.
6. **Collateral spends, second pass over all non-coinbase transactions and all inputs** (`:732-757`; v23 `:464-480`): `new.by_collateral(prevout)` with `dmn.collateral == prevout` (the unique map is keyed by the outpoint bytes, so the extra comparison only guards a key collision); V24: a shared MN's collateral spent by anything but a ProDisTx → `"bad-shared-collateral-spend"` (`:739-743`); `remove_mn`. This pass runs after pass 5, so an MN registered and spent in the same block is added then removed, and a ProUpServTx for an MN whose collateral is spent in the same block is applied then removed (R4 D §3.3 step 4). Ordinary transactions count: any input spending a collateral outpoint removes the MN.
7. **Payee bookkeeping** (`:759-775`; v23 `:484-499`): if `payee` is `Some` and still in `new`: `last_paid_height = h`; if `mn_type == Evo && !mn_rr`: `consecutive_payments += 1`; `update_mn`. The bump happens even if the payee was banned or revoked earlier in this block.
8. **Streak reset** (`:778-790`; v23 `:502-514`): every Evo MN with `consecutive_payments != 0` that is not `(payee && !mn_rr)` → `consecutive_payments = 0`. Fast path iterates `streaks` (a snapshot copy). Core iterates a copy `newList2` of the list for the same reason.
9. Return `new` with `block_hash` still null; CH7a sets it after `ProcessSpecialTxsInBlock` learns the hash (`:907-911`).

### 3.4 Payee selection (quirk 13)

```rust
pub fn payee(&self, f: PayeeFlags) -> Option<&Arc<Dmn>> {
    if self.len() == 0 { return None; }                                    // v24 deterministicmns.cpp:160-162
    if f.v19_after_prev && !f.mn_rr_after_prev {                           // :166-181
        if let Some(p) = self.last_payee.as_ref().and_then(|h| self.get(h)) {
            if !p.state.is_banned() && p.mn_type == MnType::Evo && p.state.consecutive_payments < EVO_VOTING_WEIGHT /* 4 */ { return Some(p); }
        }
    }
    self.payee_queue.get_min().and_then(|k| self.get(&k.pro_tx_hash))      // :184-192: min by (effective_height, proTxHash)
}
```

- **Unconfirmed MNs are payable**: the queue admits every non-banned MN, confirmed or not; only `is_banned` excludes (`ForEachMNShared(onlyValid=true)`).
- **All banned** → the queue is empty → `None`. Core returns `nullptr`; `GetMasternodeTxOuts` then returns false, and `IsBlockPayeeValid` accepts anything (quirk 13's third clause; EV3).
- **The Evo rule** is Core's "last payee with `nLastPaidHeight == nHeight`" scan: at most one MN can have `last_paid_height == height` (only step 7 writes it, once per block), so `last_payee` is that MN or `None`. The validity, type and streak checks are evaluated at query time exactly as Core's lambda does. Core returns the *last* match in HAMT order; with at most one candidate the order is irrelevant. The pre-V19 gate is kept for line-by-line parity even though no Evo MN can exist before V19 (`:164-165`).
- **Tie-break by `proTxHash`** is `uint256::operator<`, a byte-wise compare of the 32 bytes in memory order (little-endian hash bytes); `ProTxHash`'s `Ord` is defined to match (CS0 §8.4's `BlockId`/hash types; A1 §2.5 "memcmp of LE bytes").
- `projected_payees(flags, n)` (`:196-249`) is a direct port with the same comparator; it runs on the RPC path only and keeps Core's `std::sort` over a vector (equal elements are the same `Arc`, so instability cannot show).

### 3.5 `set_state_version` and `apply_pro_up_serv` (FR-EVO-05; v24 `evo/specialtxman.cpp:51-137`)

```rust
pub fn set_state_version(s: &mut MnState, version: u16, mn_type: MnType) -> Result<(), &'static str /* reject reason */> {
    let needs_ext = version >= 3;
    if needs_ext && s.payouts.is_empty() && !s.script_payout.is_empty() { s.payouts = legacy_payout_as_list(&s.script_payout); s.script_payout.clear(); }   // :57-60
    if !s.pub_key_operator.is_null() { s.pub_key_operator = s.pub_key_operator.reencoded(legacy = version == 1); }                                   // :70-73, Set(pubkey, legacy)
    if s.version == version && s.net_info.can_store_platform() == needs_ext { return Ok(()); }                                                       // :75-77
    let mut converted = NetInfo::make(version);                                                                                                      // :79
    if needs_ext {
        converted.add_entries(CoreP2p, s.net_info.entries(CoreP2p)).map_err(reason)?;                                                                // :81-84
        if s.net_info.can_store_platform() { converted.add_entries(PlatformP2p, …)?; converted.add_entries(PlatformHttps, …)?; }                      // :85-91
        else if mn_type == Evo && !s.net_info.is_empty() { return Err("bad-protx-netinfo-version"); }                                               // :92-96
        s.platform_p2p_port = 0; s.platform_http_port = 0;                                                                                            // :97-98
    } else { converted.add_entries(CoreP2p, s.net_info.entries(CoreP2p)).map_err(reason)?; }                                                         // :99-102
    s.version = version; s.net_info = converted; Ok(())                                                                                              // :104-105
}
```

`reencoded()` is BS11a-t's: decode the 48 bytes under the current encoding, re-serialize under the target. Core's `Get()` on an undecodable lazy key yields the null key; a stored key is always decodable because `IsTriviallyValid` required `pubKeyOperator.IsValid()` (R4 D §2.3), so `reencoded` cannot fail on a validated chain. The implementation still returns `Err(ListThrow)` rather than panicking if BS11a-t reports an undecodable key, which the L1 fuzzer can produce.

`apply_pro_up_serv` (`:110-137`): `current = s.version; target = if v24 { max(current, payload.version) } else { current }; if v24 { s.version = payload.version }` (the pre-set that `set_state_version`'s comment explains); `s.net_info = payload.net_info; s.script_operator_payout = payload.script_operator_payout`; Evo: `platform_node_id = payload.platform_node_id`, and `payload.version < 3 ? (ports = payload ports) : (ports = 0)`; `if v24 { set_state_version(s, target, payload.mn_type)? }`. Pre-V24 this equals v23's inline code (`v23 :350-367`), state version untouched.

### 3.6 The list primitives and unique-property rules (`v24 evo/deterministicmns.cpp:439-679`)

`add_mn(d, bump)`: duplicate `pro_tx_hash` or `internal_id` → `ListError`; register in order: collateral; each netinfo entry (addr-port or domain-port; an entry that is neither → `InvalidAddress`); share owner keys (shared) or the owner key; the operator key if non-null, **in the state's encoding**; `platform_node_id` if Evo and non-zero. Any `AddUniqueProperty` false → `DuplicateUniqueProperty` and no change. Then insert into both maps, `reindex_entry(None, Some(d))`, `counts += d`, and if `bump`: `total_registered = max(id + 1, total_registered)`.

`update_mn(p, new_state)`: `old = get(p)?`; netinfo: if the `NetInfo` value changed, delete every old entry then add every new one (the lambda inside `UpdateMN`, `:518-595`; the error texts "internal error", "invalid address", "duplicate (…)" become `ListError` variants); `UpdateUniqueProperty` for the owner key, the operator key and (Evo) `platform_node_id` (same function, `:518-595`). For the operator key, equality is by **encoded bytes** in v24 (`UpdateUniqueProperty`, `deterministicmns.h:590-602`) and by point in v23 (`:552-555`); §3.7 shows the two agree pre-V24, and `UniqueKey::operator_key` compares bytes, which is v24's rule. Then install the new `Arc<Dmn>` (same `pro_tx_hash`, `internal_id`, `collateral`, `operator_reward`, `mn_type`; `sml_hash` fresh), `reindex_entry(Some(old), Some(new))`, adjust `counts` if the banned flag changed.

`remove_mn(p)`: delete every property in the same order as `add_mn` (`:612-679`), erase from both maps, `reindex_entry(Some(old), None)`, `counts −= old`. `total_registered` is not decremented.

Core's checks that a property being deleted is held by *this* MN (`DeleteUniqueProperty`, `:568-588`, returns false if the holder differs) are kept; they can only fire on a corrupted list and surface as `ListError` → `failed-procspectxsinblock`, as in Core.

### 3.7 State diffs (`MnStateDiff::between`)

v24 compares `pubKeyOperator` by the hash of its encoding (`dmnstate.h:259-268`), v23 by point (`v23 dmnstate.h:221-242`). They differ only when a state's key keeps its point but changes encoding, which only `set_state_version`'s re-encode under V24 rules can cause (`IsSchemeMigration`). Pre-V24 the encoding is a function of the version, and the version changes only together with the key (ProUpReg with a changed key, ProUpRev to null). So on mainnet, and under the `V23_1` profile, the two rules give identical diffs; the Rust implementation uses v24's byte comparison, which is also what the `dmn_D4` decoder needs to round-trip a migration diff. The `VERSION`-forcing rule (`:276-280`) and the serialization guard (`:292-295`) are identical in both pins.

### 3.8 `make_delta` (Core `CDeterministicMNManager::ProcessBlock`, `v24 evo/deterministicmns.cpp:689-782`; v23 `:649-734`)

```rust
pub fn make_delta(prev: &MnList, new: MnList /* block hash set */, at: BlockId, ctx: &ConsensusCtx, is_initial_successor: bool) -> Result<DmlDelta, DmlError>
```

1. If DIP3 is not active **at** the block: no delta (`:696-698`). CH7a never calls it then.
2. `diff = prev.build_diff(&new)` (`:710-711`).
3. `snapshot = Some(new)` iff `height % 576 == 0 || is_initial_successor` (`:722`), where `is_initial_successor` means `prev` was the initial (empty) snapshot of the store walk; for our store this is the first DIP3-active block. A1 §2.9 writes `mn_snapshot` every 576 blocks; the initial snapshot adds one row at `DIP3Height`.
4. DIP3 enforcement (`:769-778`): if `height == params.dip3_enforcement_height` and the configured hash is non-null and `!= hash(at)` → `Invalid("bad-dip3-enf-block")`. Mainnet 1,047,200 with its checkpoint hash (`v24 chainparams.cpp:187`).
5. The `ResetPlatformBan` loop (`:735-750`) is **not** here: it is non-transactional (`mncache.dat`) and runs only when `!fJustCheck`; CH7a emits `Effect::ResetPlatformBan { pro_tx_hash }` for every ProUpServTx of the block, in block order, as A1 §2.10.1's table specifies.
6. `WriteDerived` mismatches (`:716-726`) are local-corruption aborts in Core, never block verdicts; the Rust store treats a differing existing row the same way (`StoreError::Inconsistent`, CH7a aborts the node).
7. The catch-all `"failed-dmn-block"` (`:753-756`) wraps `BuildDiff` and the caches; nothing in the Rust path can fail there, and the string is kept in `DmlError` documentation as unreachable.

### 3.9 Why HAMT iteration order never leaks

Core iterates `immer::map` in hash-array-mapped-trie order (`ImmerHasher` = the low 64 bits of the hash, `deterministicmns.h:136-139`); we iterate in memcmp order. The result of every consensus computation is order-independent:

| Core loop | Why order-free |
|---|---|
| confirmation (`specialtxman.cpp:330-343`) | independent updates of distinct MNs |
| `DecreaseScores` (`deterministicmns.cpp:309-326`) | same |
| `GetMNPayee` (`:158-194`) | a strict total order picks a unique minimum; the Evo scan has at most one candidate |
| `GetProjectedMNPayees` | sorted with the same total order before use |
| `HandleQuorumCommitment` | iterates the member vector, not the map |
| streak reset (`specialtxman.cpp:778-790`) | independent updates |
| `GetCounts`, `CalcMaxPoSePenalty` | sums |
| `BuildDiff` | the diff is a set; added entries are sorted by `internalId`, updated/removed serialize sorted |
| `to_sml` | sorted by `proRegTxHash` before hashing (`simplifiedmns.cpp:73-79`) |
| `CDeterministicMNList::Serialize` (`dmn_S3`) | **does** leak: HAMT order on disk. Import-only (DEC-11, A1 §2.9); our reader is order-insensitive |

### 3.10 Per-profile behaviour (A1 §2.25, O17)

| Row | Class | What EV2 does |
|---|---|---|
| EVO-15 `proupserv_type_in_special_tx` | profile | Under `V24_0` EV1 rejects an out-of-range or mismatched `nType` before EV2b (`"bad-protx-type"` / `"bad-protx-type-mismatch"`, TX_CONSENSUS). Under `V23_1` EV1 does not check it, and EV2b's branch order decides the block string: netinfo-dup → hash → **mismatch** (`v23 :342-344`) → type-valid (`:345-347`, unreachable because `dmn.mn_type` is always valid). EV2b implements that order once; under `V24_0` the type-valid check is dead code (it would fire only for a tx EV1 already rejected). Test: a ProUpServTx with unknown `proTxHash` and `nType = 7` gives `"bad-protx-hash"` under both profiles at the block level, and under `V23_1` the same tx with a known hash gives `"bad-protx-type-mismatch"` |
| EVO-13 cross-scheme operator-key uniqueness | gated (V24) | implemented from v24 (`has_operator_key_under_any_scheme`, the three in-block re-probes); never active on mainnet |
| EVO-11 version unification | gated (V24) | `set_state_version`, the `max(state, tx)` targets, ProUpRev keeping its version, EvoNode `platform ports` clearing; pre-V24 paths proven identical to v23 in §3.3 and §3.5 |
| EVO-08 / EVO-10 shared MNs and payout lists | gated (V24 / `evo_shares`) | types 11 and 12, `shares`, `payouts`, the ProDisTx-only collateral rule; `evo_shares` never reaches EV2b (EV1 gates registrations) |
| EVO-25 MN-list cache bounds | none | `CachePolicy` mirrors v24's in-walk cleanup; unobservable |
| EVO-28 / EVO-29 RPC fields | profile | RP2a's; EV2a exposes all fields |
| CONS-09 BLS-flag leak | profile | not EV2's: state encodings are per version, never per the global flag; `BlsCtx` is only forwarded to payload decode |
| `dmn_D3` → `dmn_D4` migration | neither | both pins already read and write `dmn_D4` with the nVersion-first bitmask (`v23 evo/deterministicmns.cpp:34`, `evo/dmnstate.h:160-178`); the D3 reader exists for older Core datadirs (EV4) |

### 3.11 Reject strings owned by EV2b

`bad-protx-payload`, `bad-protx-collateral`, `bad-protx-dup-netinfo-entry`, `bad-protx-netinfo-entry`, `bad-protx-dup-key`, `bad-protx-type`, `bad-protx-hash`, `bad-protx-type-mismatch`, `bad-protx-netinfo-version` (from `set_state_version`), `bad-protx-shared-mn`, `bad-proupshare-index`, `bad-proupshare-payee-reuse`, `bad-proupsharedreg-not-shared`, `bad-proupsharedreg-payee-reuse`, `bad-qc-payload`, `bad-qc-commitment-type`, `bad-qc-quorum-hash`, `bad-shared-collateral-spend`, `bad-dip3-enf-block`; and the mapping of `ListError` to `failed-procspectxsinblock` (CH7a). All `BLOCK_CONSENSUS`. A census test asserts the set equals the strings in both pins' `RebuildListFromBlock` and `CDeterministicMNManager::ProcessBlock`.

---

## 4. Persistence, crash consistency and recovery

EV2a stores nothing itself; it defines what is stored and how it is read back.

- **What is written per block** (CH7a's B1 sub-batch, A1 §3.5.4): `mn_diff[hash] = envelope(DIFF_D4, diff bytes)` for every DIP3-active block, including blocks with an empty diff (Core writes an empty `CDeterministicMNListDiff`, and the walk needs the row to distinguish "no changes" from "initial snapshot"); `mn_snapshot[hash] = envelope(CANONICAL_V3, canonical bytes)` when `DmlDelta::snapshot` is `Some`. Both are pure functions of the block and its ancestry, so re-writing after a crash is idempotent, and a differing existing row is corruption (Core `WriteDerived`).
- **Atomicity** comes from A1 §3.5: the evo rows travel in the Durable B1 batch together with `meta.evo_tip`, so after any crash the evo column families describe exactly `evo_tip`, and recovery is coins-only. EV2a adds one invariant check at startup (CH7a's `recover`): `mn_list(evo_tip)` must resolve without an `initial snapshot` fallback above `DIP3Height`; otherwise "Found EvoDB inconsistency, you must reindex" (Core's text, `validation.cpp` evo check).
- **Reorgs**: rows are keyed by block hash and never erased, so a disconnected block's list remains resolvable; a reconnect of the same block re-derives identical bytes.
- **Reading at any height Core can answer (FR-STO-08)**: `resolve_list` walks at most 575 diffs from the nearest snapshot (plus the 32-block mini-snapshots in memory), as Core does. Costs are in §5.
- **Decoder robustness**: every codec is total over arbitrary bytes (bounded allocations: the canonical limits above; the diff decoder bounds compact sizes by the remaining input length) because the importer (EV4) and the `EvoSnapshot` path feed untrusted files, and the corpus fuzzers (§6.5) run them.
- **Schema evolution**: a new `codec_id` is added rather than mutating an existing one; the decoder refuses unknown ids with the schema version in the message (FR-STO-02).

---

## 5. Performance

### 5.1 dashd metrics affected

Metrics 1, 2, 3, 5, 8 and 12 of A1 §3.9 (IBD, IBD without assumevalid, reindex, per-block connect latency, GBT, CPU per block). PF1 has not yet published the DML share of `ConnectBlock` (PF1 "started", PLAN §8.1), so every number below is a hypothesis to be confirmed by PF1 **before** the payee queue is treated as a lever rather than a parity feature (PLAN EV2b depends on PF1 for exactly this budget).

### 5.2 Per-block cost model (n MNs, k changed entries per block, mainnet n ≈ 3,500 with ≈ 1,100 evonodes)

| Step | Core (v24) | Rust fast path | Rust twin |
|---|---|---|---|
| list copy | O(1) + HAMT path copies per mutation | O(1) + 3 map/5 set path copies per mutation | same as fast |
| payee | O(n) scan, plus an O(n) Evo scan between V19 and MN_RR (`:166-181`) | O(log n) `get_min` + O(1) `last_payee` | O(n) |
| confirmation | O(n) scan of `prev` | O(c log n), c = newly confirmable | O(n) |
| decay | O(n) scan + O(p) updates | O(p log n) | O(n) |
| per QC punishment | O(members) × `CalcMaxPoSePenalty` **O(n)** each (`GetCounts` walks the map) | O(members log n) with O(1) counts | O(members × n) |
| collateral spends | one SHA256d + HAMT lookup per input | one hash-map lookup per input (no SHA256d) | same |
| payee bump, streak reset | O(1) + O(n) scan of `newList2` | O(1) + O(s log n), s ≤ 2 | O(n) |
| `BuildDiff` | **O(n)** per block (`:336-369`: iterates `to`, looks up each in `from`; per-MN state diff over 23 members when the `Arc` differs) | `OrdMap::diff` O(k log n): shared subtrees are skipped by pointer equality | O(n) |
| `to_sml` after `ProcessBlock` | O(n) entry construction when the cache is invalidated, which every block does (`AddMN`/`RemoveMN`/`UpdateMN` with a changed SML entry) | EV4: O(k) new hashes, O(n) compressions for the root | — |

Core's per-block DML work is therefore ≈ 6 × O(n) scans ≈ 20k pointer-chasing steps plus the SML rebuild; at 1.5 M post-DIP3 blocks that is ~3 × 10¹⁰ steps, a few minutes of a ~2 h `-reindex-chainstate`. The fast path removes the scans and leaves O(k log n) with k typically ≤ 20. The expected effect on metric 3 is small but real (1–3 %); on metric 5 (connect latency at the tip) and on GBT it is a larger share because both pay the payee scan on every call (`masternode/payments.cpp:152-157` and `TestBlockValidity`). PF1 will say whether it clears A1 §3.10's bar for keeping a twin.

### 5.3 Clone and diff cost at scale

- One `MnList` clone is 3 `Arc` bumps for the maps, 5 for the sets and a `Counts` copy. Mutations copy one path per structure: for `OrdMap` (B-tree, 64-way) that is ≈ 3 nodes of ≤ 64 entries at n = 3,500; for the HAMT ≈ 3 nodes of 32 slots. Per block with k ≈ 10 changes: ≈ 10 × (3 + 5) × ~3 node copies ≈ 240 small allocations, ≈ 60 KB. Core does the same for three structures with `std::shared_ptr` state copies on top.
- Memory of the retained window: 576 lists share all unchanged nodes; the unique bytes are the per-block path copies, ≈ 576 × 60 KB ≈ 35 MB worst case, versus Core's cache of 64 lists plus mini-snapshots. The `CachePolicy` keeps Core's bounds so RSS stays ≤ dashd's (NFR-RES-01).
- `build_diff` via `imbl::OrdMap::diff` yields `k` change records without touching shared subtrees; the twin (`reference::build_diff_scan`, Core's algorithm) runs under xcheck and in the fuzzer.
- The unique map avoids the SHA256d per probe that Core pays for every `HasUniqueProperty`, `GetMNByCollateral` and every input of every transaction in the second pass. On a 2,000-tx block that is ≈ 5,000 SHA256d saved.

### 5.4 The payee-queue budget rule

Per A1 §3.10 and PLAN EV2b's dependency on PF1: the queue is implemented (it is also what makes `projected_payees` and GBT cheap), the twin is implemented, and xcheck runs in CI replay. If PF1 shows the DML share of connect time is below 1 %, the queue stays as a parity-neutral structure and the "2× over the twin" bench remains the only gate for it; the twin is never removed either way (NFR-CEQ-12).

### 5.5 PF0 counter workloads and benches (BG rows)

| Workload (`contrib/node/perfgate/workloads.toml`) | Fixture | Counter | Gate |
|---|---|---|---|
| `crit:mnlist_clone_diff` | the canonical list at mainnet 2,100,000 (from the O8 oracle); 100 random `update_mn` per iteration, then `clone` + `build_diff` | instructions, allocations | never slower than the previous head |
| `crit:mnlist_codec` | the same list: `canonical_bytes`, `from_canonical_bytes`, `canonical_hash`; a 576-diff window encode/decode | instructions | never slower |
| `crit:dml_build_mainnet` | 2,000 mainnet blocks from 1,900,000 (V19 era, QCs present) and 2,000 from 2,150,000 (post-MN_RR), with the list at the window start; `build_new_list_from_block` per block | instructions, allocations | never slower; **fast path ≥ 2× the twin** (the twin bench runs the same window with `reference::build_new_list_from_block_scan`) |
| `crit:dml_payee` | 10⁴ `payee()` calls on the 2,100,000 list, both paths | instructions | fast path ≥ 10× the twin (sanity) |
| `crit:dml_resolve_576` | `resolve_list` 575 diffs from a snapshot | instructions | never slower |
| `reindex-chainstate` windows (PF0's existing four) | — | the whole-node counters | regression floor |

Core-side comparison benches (DEC-16 local patch, built with B1's script, as CS0's N4): `bench/evo_dml.cpp` timing `RebuildListFromBlock` and `GetMNPayee` on the same windows, so the ratio to dashd's instruction count is recorded at each milestone (A1 §3.11 "Against dashd").

---

## 6. Verification

### 6.1 L0: Core vectors and unit tests

- Transcribe `src/test/evo_deterministicmns_tests.cpp` (both pins) and `src/test/evo_sharedmn_tests.cpp` (v24) through O4's vector pipeline where they are data-driven, and as Rust unit tests where they are procedural: register/update/revoke/revive sequences, PoSe ban and decay, the collateral-replacement path, the same-block register-and-spend, `IsEqual` semantics, diff round trips, the legacy-diff migration vectors (`MigrateLegacyDiffs` tests).
- Unit tests per index rule of §2.4, including the `last_payee` edge cases: payee banned in the same block; payee removed in the same block; payee revived; MN_RR activation block (streaks all reset, including the payee's); the `else if` of `PayeeKey::of`.
- Quirk tests: 13 (unconfirmed payable; all-banned → `None`; memcmp tie), 23 (owner key equal to an Evo `platform_node_id` registered earlier in the block → `ListThrow` → `failed-procspectxsinblock`; a legacy-encoded and a basic-encoded copy of one key coexist pre-V24 and conflict under V24), 24 (penalty on an already-banned MN; `max(100, total)` with registrations earlier in the block), 25 (the order; a ProUpServTx applied to an MN whose collateral is spent later in the block), 26 (an external collateral created in the same block fails `bad-protx-collateral` in the re-check), 17 (the `sml_hash` slot survives `clone` and is cleared by `update_mn`).
- Codec tests: canonical bytes of the empty list; the Core-generated vectors of §6.2; every decoder rule of §2.5 with its error text; `dmn_D3` → `dmn_D4` bit mapping (`dmnstate.h:452-482`).

### 6.2 Canonical-hash acceptance (EV2a's PLAN acceptance row)

"Snapshot bytes → `CanonicalMNListHash` equal to the patched v24 oracle on imported lists." Two sources, in preference order:

1. **O8's `node/state-digest-v24.0` branch** gains a hidden RPC `dumpmnlistcanonical <blockhash>` (≈ 30 lines: `SerializeCanonicalMNList` into a `CDataStream`, hex out), next to `-statedigestlog`'s `CanonicalMNListHash`. Vectors at the 13 segment boundaries of A1 §4.6 plus every 576th block of the 50k window are stored as `test-vectors/evo/mnlist_canonical/<height>.hex` with the hash. The test decodes, re-encodes byte-identically, hashes, and compares. This exercises the full decoder and encoder and is independent of EV4.
2. **EV4's importer** later re-runs the same comparison on lists read from a Core `evodb/` (`dmn_S3` + `dmn_D4` replay) at M4.

### 6.3 L1: differential targets

| Target | Oracle entry (A1 §4.4) | Inputs | Compared |
|---|---|---|---|
| `diff_dml_apply` (≥ 24 CPU-h per release) | `oracle_dml_apply(prev_list, block, prev_header, coins, deployment_flags)` (O2a) | a generated `prev` list (0–5,000 MNs, mixed versions/types/ban states/streaks, seeded from the canonical vectors) and a block of 0–50 special and ordinary txs over it; flags for V19/MN_RR/V24 | verdict and string; the new list's canonical bytes (sorted, so comparable) and `payee`; on `Err`, equality of the string |
| `diff_dml_apply_window` | the same over real mainnet blocks (BS2 corpus) with the oracle's list at `h−1` | 50k blocks from 1,028,160, sampled | canonical bytes equal per block |
| `diff_mnlist_codec` | `oracle_mnlist_codec(op, bytes)` (**new, O2a**: `op ∈ {canonical_decode_encode, canonical_hash, diff_d4_decode_encode, s3_decode_canonical}`) | random and mutated bytes from the vector corpus | decode outcome (ok/error), re-encoding, hash |
| `diff_state_diff` | `oracle_state_diff(a, b)` (**new, O2a**: `CDeterministicMNStateDiff(a, b)` serialized) | random state pairs incl. re-encoded keys and netinfo kind changes | bytes |
| `mnlist_payee_queue` (model, no oracle) | — | random op sequences on a list: add/update/remove/ban/revive/pay | fast `payee()`, `decrease_scores`, confirmation, streak reset vs the twin; `debug_assert_indexes` |

**New oracle entry points needed** (flagged for O2a): `oracle_mnlist_codec`, `oracle_state_diff`. `oracle_dml_apply` exists in A1's v2 list; its synthetic `CCoinsView` must expose `GetBestBlock()` non-null so the collateral re-check runs, and the harness passes `CollateralView::Pprev` on our side (an `Unchecked` mode mirrors the shim's null-best-block form for the twin tests).

### 6.4 L2: replay windows and digests

- Per consensus PR (A1 §4.6 segment snapshots): the DIP3, DIP3-enforcement, V19, V20, MN_RR and WITHDRAWALS windows, 20k blocks each, comparing `CanonicalMNListHash` per block from the oracle's `-statedigestlog`, plus the sparse `protx list`/`listdiff`/`masternode payments` goldens.
- The acceptance window: **50k blocks from 1,028,160** with the per-block digest equal (EV2b's PLAN row), run nightly once CH7a wires `build_new_list_from_block` into `connect_block`; until then the `diff_dml_apply_window` target (§6.3) runs the same comparison without the chainstate.
- `-consensusxcheck` replay runs the twins and `debug_assert_indexes` after every block.
- O2b `diff_chain` (later): failed connects and proposals touching the DML (a block failing after a ProRegTx; `TestBlockValidity` at a snapshot height) to confirm nothing persists from a failed build.

### 6.5 Fuzz and model targets

`mnlist_payee_queue` (above), `fuzz_canonical_decode`, `fuzz_diff_d4_decode`, `fuzz_s3_decode`, `fuzz_state_diff_roundtrip` (no oracle; bounded allocation, no panic, round-trip stability), and `fuzz_apply_diff` (random diffs onto random lists: never panics, errors are `ListError`).

### 6.6 Functional tests to port (L4, with CH7a)

`feature_dip3_deterministicmns.py`, `feature_dip3_v19.py`, `feature_llmq_evo.py` (evonode payments and streaks), `feature_dip4_coinbasemerkleroots.py` (through EV4), `rpc_masternode.py`/`rpc_protx*.py` (through RP2a), `feature_masternode_shares.py` and `feature_masternode_payout_shares.py` (v24, V24 rules). Their DML-specific assertions (payee order, PoSe bans, revival) are the ones this component owns.

---

## 7. Implementation plan

Phases are sized for one review each; each lists its acceptance tests. EV2a is phases A–D, EV2b is E–H.

| Phase | Content | Accepts when |
|---|---|---|
| **A. Types and primitives** | `Dmn`, `MnState`, `MnStateDiff` (+ `between`/`apply_to`), `UniqueKey`/`UniqueMap`, `MnList` with `by_pro_tx`/`by_internal_id`/`unique`/`counts` only, `add_mn`/`update_mn`/`remove_mn`, `ListError`, the transcribed `evo_deterministicmns_tests` cases for add/update/remove and uniqueness | unit tests green; quirk 23 tests; clippy determinism lint clean with the one documented `UniqueMap` exemption |
| **B. Codecs** | `dmn`/`state`/`state_diff` codecs on `core_serialize`; `canonical` encode/decode/hash; `list_diff_d4`; `list_diff_d3` and `list_s3` readers; the envelope | §6.2 vectors byte-identical and hash-equal at the 13 boundaries; corpus fuzzers 1 h clean; `diff_mnlist_codec` and `diff_state_diff` clean on 10⁶ cases once O2a lands (until then: Core vectors only) |
| **C. Derived indexes and payee** | the five sets, `last_payee`, `reindex_entry`, `with_height`, `PayeeKey`, `payee`, `projected_payees`, `calc_*`/`pose_punish`/`decrease_scores`; `reference::{payee_scan, max_penalty_scan, decrease_scan}` and `debug_assert_indexes` | `mnlist_payee_queue` 10⁷ ops clean; quirk 13 and 24 tests; `crit:dml_payee` ≥ 10× twin |
| **D. Store** | `EvoStore` trait as in A1 (the LQ2/EV3/LQ1b methods as trait items with `MemEvoStore` stubs), `MemEvoStore`, `resolve_list`, `CachePolicy`, `apply_diff`/`apply_diff_for_snapshot`, `build_diff` via `OrdMap::diff` with `reference::build_diff_scan` | the walk reproduces Core's `GetListForBlockInternal` on a synthetic chain with forks; `crit:dml_resolve_576` recorded; API review by the LQ1a owner (PLAN M-9c: LQ1a starts here) |
| **E. Build, non-V24 branches** | `build_new_list_from_block` with ProReg/ProUpServ/ProUpReg/ProUpRev/QcTx/collateral pass/payee/streaks, `apply_pro_up_serv` and `set_state_version` pre-V24 subset, `DmlTrace`, `make_delta`, `reference::build_new_list_from_block_scan` | quirk 25/26 tests; transcribed FT-derived sequences; `diff_dml_apply` 4 CPU-h clean with V24 flags off; `crit:dml_build_mainnet` fast ≥ 2× twin |
| **F. V24 branches** | cross-scheme probes, version unification, types 11/12, ProDisTx collateral rule, `set_state_version` full | `evo_sharedmn_tests` transcription; `diff_dml_apply` 8 CPU-h with V24 on |
| **G. L1/L2 soak** | the ≥ 24 CPU-h `diff_dml_apply` run (both releases), `diff_dml_apply_window` over 50k blocks from 1,028,160, reject-string census | zero divergences; results filed under `reviews/` |
| **H. Integration hooks** | `DmlDelta` consumption by CH7a, `Effect::ResetPlatformBan` emission site documented, the PF0 workloads merged into `workloads.toml`, Core-side `bench/evo_dml.cpp` patch | PF0 baselines recorded; CH7a's design references the final signatures |

Dependencies: A needs BS6a's payload and netinfo types (or EV0's interim stubs) and CS0's `core_serialize`; B needs the O8 hidden RPC for vectors (§8 Q1); E needs EV1 only for the end-to-end tests, not for compilation; F needs BS11a-t's `reencoded`; the QcTx branch needs LQ1a's `QuorumMemberSource` or the harness stub.

---

## 8. Open questions for the manager

1. **Canonical vectors before EV4.** The EV2a acceptance says "on imported lists", but the importer is EV4 (M4). Recommendation: add the hidden `dumpmnlistcanonical` RPC to O8's `node/state-digest-v24.0` branch now (DEC-16 scope, ≈ 30 lines) and accept EV2a on those vectors; EV4 re-runs the check on real evodb imports.
2. **Diff on-disk format.** A1 §2.9 names a `MnListDiffRecord` of our own. Recommendation: use Core's `dmn_D4` bytes inside our envelope (identical decoder to the importer's, canonical by construction, one less codec to fuzz). DEC-11 allows it ("own format allowed"; this is a choice, not a compatibility promise).
3. **The unique-property map structure.** Byte-keyed `imbl::HashMap` behind a non-iterable newtype (recommended; O(1), exact collision classes, one lint exemption) versus `imbl::OrdMap<UniqueKey, …>` (no exemption, O(log n) 48-byte compares). If the determinism rule is meant to be absolute, the `OrdMap` variant is a one-line change and PF0 decides.
4. **Where `bad-dip3-enf-block` lives.** Core checks it in `CDeterministicMNManager::ProcessBlock`; I put it in `make_delta` (EV2b). CH7a could own it instead since it is a chain fact, not a list fact. Recommendation: keep it in `make_delta` so the oracle's `diff_chain` compares it at the same step.
5. **Twin retention after PF1.** If PF1 finds the DML share of connect time negligible, do we still keep `cfg(xcheck)` twins for all five scans? Recommendation: yes (NFR-CEQ-12 and A1 §3.10 require a twin for every algorithmic replacement, and they are small), but the twin bench gate can relax from 2× to "not slower" for the sub-millisecond steps.
6. **`EvoStore` trait ownership.** EV0 is not merged; EV2a will land the trait with `MemEvoStore` stubs for the EV3/LQ1b/LQ2 methods. Confirm that EV0's implementer adopts these signatures rather than the reverse, so LQ1a can start on EV2a's model per PLAN M-9c.
7. **New O2a entry points** `oracle_mnlist_codec` and `oracle_state_diff` (§6.3) and the `GetBestBlock()` requirement on `oracle_dml_apply`'s synthetic view. Recommendation: add them to O2a's coverage matrix now; phase B runs on Core vectors until they exist.

---

## Appendix A: Core line map used in this design

| Topic | v24.0.0-rc.3 | v23.1.8 |
|---|---|---|
| `CDeterministicMN` and serialization | `evo/deterministicmns.h:43-93` | `:43-93` |
| immer maps, `ImmerHasher`, `Counts`, fields | `evo/deterministicmns.h:136-175` | `:135-174` |
| list `Serialize`/`Unserialize` (`dmn_S3`) | `:235-255` | `:234-255` |
| `GetCounts`, `ForEachMN`, `ForEachMNShared` | `:269-324` | `:268-323` |
| `HasOperatorKeyUnderAnyScheme` | `:471-482` | — |
| `IsEqual` | `:487-525` | `:449-487` |
| `GetUniquePropertyHash`, `Add/Delete/UpdateUniqueProperty` | `:526-614` | `:488-567` |
| `CDeterministicMNListDiff` codec | `:627-712` | `:580-660` |
| `DISK_SNAPSHOT_PERIOD`, cache sizes, `MAX_CACHED_LISTS` | `:760-769` | `:681-684` |
| `GetMNPayee`, `CompareByLastPaid` | `evo/deterministicmns.cpp:132-194` | `:157-219` |
| `GetProjectedMNPayees` | `:196-249` | `:221-274` |
| PoSe | `:267-334` | `:292-359` |
| `BuildDiff`, `ApplyDiff`, `ApplyDiffForSnapshot` | `:336-437` | `:361-419` |
| `AddMN`, `UpdateMN`, `RemoveMN` | `:439-679` | `:420-647` |
| `CDeterministicMNManager::ProcessBlock` | `:689-782` | `:649-734` |
| `UndoBlock` | `:784-816` | `:736-769` |
| `GetListForBlockInternal`, `CleanupCache` | `:826-939`, `:975-1010` | `:778-870`, `:906-947` |
| evodb keys `dmn_S3`/`dmn_D4`/`dmn_D3` | `evo/deterministicmns.cpp:31-33` | `:33-36` |
| `CDeterministicMNState` fields, codec, helpers | `evo/dmnstate.h:33-175` | `:33-157` |
| `CDeterministicMNStateDiff` | `evo/dmnstate.h:177-324` | `:157-287` |
| legacy diff | `evo/dmnstate.h:327-483` | `:288-446` |
| `SetStateVersion`, `ApplyProUpServTx` | `evo/specialtxman.cpp:46-137` | inline `:350-367`, `:396-404` |
| `HandleQuorumCommitment` | `:283-298` | `:159-174` |
| `BuildNewListFromBlock`, `RebuildListFromBlock` | `:300-794` | `:176-520` |
| `ProcessSpecialTxsInBlock` DML calls and catch | `:895-919`, `:993-995` | `:610-630`, `:701-703` |
| `SpecialTxRules.v24` source | `evo/specialtxman.h:49-53`; `validation.cpp:2659` | `is_v24_deployed`, `evo/specialtxman.cpp:224` |
| canonical MN-list codec and hash | `evo/snapshot.h:158-301`; `evo/snapshot.cpp:93-155` | — |
| `WriteDerived` | `evo/evodb.h:150-205` | `Write`, `:78-83` |
| SML entry (for `sml_hash`) | `evo/simplifiedmns.h:27-108`; `evo/deterministicmns.cpp:43-50` | `:27-95`; `:43-50` |
| member selection consumers | `llmq/utils.cpp:118-187`, `:397-406`, `:634-700` | same functions |
| payments consumer | `masternode/payments.cpp:152-157` | `:55-60` |
| `nMasternodeMinimumConfirmations` | `chainparams.cpp:177, 386, 569, 810` | same values |
| MN type table | `evo/dmn_types.h:30-43` | same |

## Appendix B: how the design maps to the reconciliation points named in the brief

- **Clone/diff cost at mainnet scale:** §2.2.3 (structures), §5.2–5.3 (model and numbers), `crit:mnlist_clone_diff`.
- **Payee-queue ordering across versions:** §2.2.5 (`PayeeKey`), §3.4 (the V19–MN_RR Evo rule via `last_payee`, the MN_RR switch in steps 7–8), §6.1 edge cases.
- **Hard-fork-gated behaviours:** V19 (Evo type, streaks; state key encodings are per version, never per the global scheme), V20 (nothing in the DML), MN_RR (steps 1, 7, 8), V24 (§3.3 branches, §3.5, §3.10), DIP3 enforcement hash (§3.8).
- **How DML state feeds LQ1 and the SPV/desktop consumers:** §2.7 (exact types), §2.6 (`mn_list(at)` with ancestry), §3.9 (iteration order is safe for them).
