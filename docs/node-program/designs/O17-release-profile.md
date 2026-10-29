# O17: `ReleaseProfile` spec

> **Superseded by DEC-82 (v24 only, RP-V24).** `ReleaseProfile`, `UngatedRules` and `-releaseprofile` were removed; A1 §2.25 now states the v24-only rule. This spec is kept as history.

| | |
|---|---|
| Task | PLAN O17 (M0), second deliverable: the `ReleaseProfile` shape and how tasks consume it |
| Date | 2026-10-08 |
| Inputs | `O17-pin-delta.md` (the classified rows), DEC-05, DEC-24 D3, DEC-34, DEC-37, `reviews/O17-r2-gpt.md`, `CS0-node-primitives.md` §8.2/§8.4 (CS0's draft `Release`/`UngatedRules`), A1 §2.25, §2.14, §2.18 |
| Replaces | the `ReleaseProfile` struct in A1 §2.25 and Appendix A |
| Status | Updated for DEC-37 (42 rows moved to `profile`; 170 `profile` rows, 22 `v24-all`). The policies are decided (DEC-34, DEC-37) |

## 1. What the profile is for

DEC-05 pins parity per network: mainnet emulates **v23.1.8**, testnet, devnets and regtest emulate **v24.0.x** (today v24.0.0-rc.3, the head of `v24.0.x`). The releases differ in three kinds of way, and each kind has its own home:

| Kind of difference | Home | Example |
|---|---|---|
| Deployment-gated rule (same code; activation decides) | `ChainParams` deployments + `DeploymentView` | V24 shared collateral, `evo_shares` payouts |
| Per-network parameter that differs per pin | `ChainParams` built for `(NetworkKind, Release)`; BS5 holds both pins' tables | V24 bit 12 → 13, `evo_shares` (bit 14), testnet V24 start time |
| Behaviour that differs and is not gated | **`ReleaseProfile`** | object download, SPORK_3/9, DKG bad-connection criteria, AU v2 stable txid |

The profile holds only the third kind. A behaviour enters it only through a row of `O17-pin-delta.md` keyed `profile`. Rows keyed `v24-all` are implemented once (the v24 behaviour) and are declared deviations (§8).

## 2. Rules every consumer follows

1. **Data, not code.** Every field is a value (`u32`, `Duration`, `&'static str`, `bool`) or a small `enum` naming the variant. A group holds no trait objects or closures. Call sites `match` on the enum, so the compiler forces every variant to be handled. A consumer may build a trait object per variant (e.g. P5 builds one of two object-request trackers), but the choice comes from the field.
2. **No release comparisons at call sites.** Only the group definitions (`dash_consensus::release` and each runtime crate's `release` module, §3) match on `Release`. Everything else reads the field that names the behaviour and never writes `release == Release::V24_0`. A CI grep, `contrib/node/pin-delta/check-release-refs.sh` (first added by CS0), fails the build on `Release::` outside files named `release.rs` and their tests. Init-time selection (`default_for`, `parse`) lives in `dashd_node::release`, which is such a file. This keeps every difference discoverable from the groups, and adding a third release becomes a data change.
3. **Every field cites its row.** Each field's doc comment names its O17 row id(s) and the Core call site at both tags, e.g. `/// O17 P2P-10 (A1 §2.25 row 11). v23.1.8 net_processing.cpp:5602; v24 :5752.` The check in §7 keeps fields and rows in step.
4. **Statics are complete literals.** Each group's `V23_1` and `V24_0` values are full struct literals with no `..Default::default()` and no `Default` impl. Adding a field without giving both releases a value is a compile error.
5. **Immutable for the process.** The release is chosen once at init (§5); every group is a `&'static` value. Nothing switches profile at runtime; a re-pin is a new binary.
6. **Each consumer sees only its group.** Pure code sees `ConsensusCtx.rules: &'static UngatedRules` (CS0 §8.4) and nothing else. A runtime actor receives its own group (and `&'static UngatedRules` if it checks validity) at construction from `dashd-node`.
7. **Tests cover both variants.** A task that consumes a field ships a test for each variant (§7).

## 3. Where it lives (DEC-24 D3)

Two layers, keyed by one enum:

1. **`dash_consensus::release`** (CS0) owns `Release` and **`UngatedRules`**: every `profile` row that decides block, transaction or header validity, a txid, a consensus reject string, or a node-local validity gate. A row that is not about validity lives there only when it **shares its value** with a validity row and must flip with it: the miner and ChainLock-signer IS gates (SIG-13) share `is_gate` with ConnectBlock; the miner clock (MPIX-19) and the sync clock (P2P-16) share `time_source` with header acceptance. Conversely, SIG-15 (coinbase ChainLock adoption) feeds chain selection only through the ChainLock state its runtime crate owns, so it lives in `LockRules`. Two `UngatedRules` fields select the semantics of **state** rather than a value: `bls_scheme_scope` (CONS-09) and `quorum_root_cache` (DKG-08). The state itself lives in `dashd-chainstate` (CH7a, CH7c); pure code receives it as a parameter or emits an effect. `au_window_failure_class` (MPIX-16) is here because the consensus check produces the validation class that P4/P5 turn into a score. Pure code reads it through `ConsensusCtx.rules`. Runtime code that checks validity or builds blocks (chainstate, mempool, mining, the ChainLock signer) reads the same value, which it can because every node crate depends on `dash-consensus`.
2. **Runtime groups**, one per concern, each defined in a `release` module of the **lowest runtime crate that reads it**, so no crate needs an upward or sideways dependency to read a rule. Each is built by `Group::of(Release) -> &'static Group` (§4.2).
3. **Per-release tables generated from Core's source**: `dash_rpc_spec::surface(Release)` (AR2: methods, arguments, results, help, error strings, honoured `-deprecatedrpc` names, REST endpoints, dash-cli conversions) and `dash_args::options(Release)` (AR1: the option table). They are data, keyed by `Release`; they are not fields of the profile.
4. **`dashd_node::ReleaseProfile`** is only the aggregate: `ReleaseProfile::of(release)` collects `&'static` references to `UngatedRules`, every group and both tables. `dashd-node` picks the release at init (§5) and hands each actor **its own group**. The aggregate exists for init wiring, for `getimplementationinfo`, and for the row check.

```
 dash-consensus ── Release, UngatedRules ──────────────────────────────┐ (every node crate)
 dashd-chainstate ── ChainstateRules    dashd-indexes ── IndexRules    │
 dashd-mempool  ── PolicyRules          dashd-mining ── MiningRules    │
 dashd-p2p      ── Identity, P2pRules                                  │
 dashd-masternode ── SporkRules, SyncRules                             │
 dashd-quorums  ── LlmqRules        dashd-locks ── LockRules           │
 dashd-governance ── GovernanceRules   dashd-coinjoin ── CoinJoinRules │
 dashd-wallet   ── WalletRules                                         │
 dashd-rpc      ── RpcRules         rdash-cli ── CliRules              │
 dash-rpc-spec  ── surface(Release)  dash-args ── options(Release)     │
 dashd-node     ── InitRules, ReleaseProfile (aggregate) ◄─────────────┘
```

Consequences:
- No runtime crate depends on `dashd-node` or on a sibling just to read a rule.
- Pure crates other than `dash-consensus` (`dash-evo`, `dash-llmq`, `dash-governance`, `dash-coinjoin`) see consensus rows through `ConsensusCtx.rules`. A pure crate that needs a non-consensus knob defines its own plain parameter type, and its runtime caller fills it from the group. For example `dash-llmq`'s DKG session receives the bad-member set or a `BadConnectionCriteria` value from DK1, never `LlmqRules`.
- A1's `rpc: &'static RpcSurface` field is dropped from every group below `dashd-node` (it would make them depend on `dash-rpc-spec`); only the `dashd-node` aggregate holds a reference to the generated surface.
- **CS0 alignment.** CS0 §8.2's draft stands, with these changes:
  - `header_time_source: HeaderTimeSource` is renamed `time_source: TimeSource`, because the miner and the P2P sync decisions read it too (MPIX-19, P2P-16). The §8.4 `ConsensusCtx` comment changes with it;
  - `Release::V23_1_8` is renamed `V23_1`: variants name release lines, not tags (§9). `Release` also derives `Hash` and gains `ALL`, `name()` and `parse()`;
  - the §8.2 comment "Rows 3 (MNHF valid bits; CS4/EV3)" is dropped (A1 row 3 is refuted, CONS-03);
  - `default_for` takes CS0's `dash_params::Network` (A1 calls it `NetworkKind`);
  - `Effect::BlsSchemeLeak` stays: DEC-37 keys CONS-09 `profile`, so the v23.1.8 flag leak is modelled (GPT r2 §8). An earlier draft of this spec dropped it;
  - `UngatedRules` gets the fields of §4.1. CS0 lands the type with `au_stable_txid` and `time_source`; the owning tasks add the rest, with the names fixed here: `bls_scheme_scope`, `asset_unlock_index_membership`, `protx_payload_decode`, `protx_check_order`, `quorum_root_cache`, `old_budget_check`, `au_window_failure_class` and `reject_strings.special_tx_before_dip3`.

## 4. The types

### 4.1 `dash_consensus::release`

```rust
/// A Dash Core release line that a network can emulate (DEC-05). Each variant
/// denotes the tag pinned for that line (§9): V23_1 is v23.1.8 and V24_0 is
/// v24.0.0-rc.3 today.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Release {
    V23_1,
    V24_0,
}

impl Release {
    pub const ALL: [Release; 2] = [Release::V23_1, Release::V24_0];

    /// DEC-05: mainnet pins v23.1.x; testnet, devnets and regtest pin v24.0.x.
    pub const fn default_for(network: dash_params::Network) -> Release {
        match network {
            Network::Mainnet => Release::V23_1,
            Network::Testnet | Network::Devnet | Network::Regtest => Release::V24_0,
        }
    }

    /// `-releaseprofile` spelling: "23.1" or "24.0"; the pinned tag ("23.1.8",
    /// "24.0.0") and a leading "v" are accepted too.
    pub fn parse(s: &str) -> Option<Release>;
    pub const fn name(self) -> &'static str; // "23.1", "24.0"
}

/// The clock that "now" means for header acceptance, block templates and sync
/// decisions. O17 CONS-04, MPIX-19, P2P-16 (A1 row 4).
pub enum TimeSource {
    /// GetAdjustedTime(): mockable time plus the median peer offset (≥ 5 samples,
    /// capped by -maxtimeadjustment, 4200 s).
    AdjustedTime,
    /// NodeClock::now(): mockable local time, no peer offset.
    NodeClock,
}

/// InstantSend conflict gates. O17 SIG-12, SIG-13 (A1 row 6). Both releases
/// additionally require IsInstantSendEnabled() (SPORK_2, not reindexing or importing)
/// through GetConflictingLock.
pub struct IsGate {
    pub connect_block: IsBlockGate,
    pub miner: IsSafetyGate,
    pub cl_signer: IsSafetyGate,
}
pub enum IsBlockGate {
    /// v23.1.8: only if mnsync is blockchain-synced && SPORK_3 active; txs with !vin.empty().
    /// Mainnet hard-codes every spork to active (SPORK_21 to 1) in both releases, so there
    /// this reduces to "blockchain-synced"; the SPORK_3 value matters only off mainnet.
    SyncedAndSpork3,
    /// v24: only outside IBD of the connecting chainstate; txs with HasLockInputs (incl. asset unlocks).
    NotInIbd,
}
pub enum IsSafetyGate {
    /// v23.1.8: RejectConflictingBlocks() && IsInstantSendEnabled().
    SyncedAndSpork3,
    /// v24: IsInstantSendEnabled().
    IsEnabled,
}

/// Whether superblock payments depend on SPORK_9. O17 MNO-02 (A1 row 7). With
/// Spork9 the spork state reaches pure code through the superblock view
/// (ChainHelper::superblock), never through this struct. Mainnet hard-codes
/// SPORK_9 active, so the variants differ only off mainnet (the v23.1 functional
/// tests toggle it).
pub enum SuperblockGate { Spork9, Always }

/// Reject reasons that differ for the same invalid input. O17 EVO-02, EVO-17.
pub struct RejectStrings {
    pub asset_lock_v2: &'static str,        // "bad-assetlocktx-version" | "bad-assetlocktx-version-2"
    pub asset_unlock_v2: &'static str,      // "bad-assetunlocktx-version" | "bad-assetunlocktx-version-2"
    pub special_tx_before_dip3: &'static str, // "bad-tx-type" | "bad-tx-type-dip3-inactive"
}

/// Scope of the BLS legacy-scheme flag. O17 CONS-09 (A1 row 26).
pub enum BlsSchemeScope {
    /// v23.1.8: one process-global flag. ProcessSpecialTxsInBlock sets basic when V19 activates after
    /// the block; UndoSpecialTxsInBlock sets legacy below V19. Nothing restores it after a failed
    /// ConnectBlock, a failed TestBlockValidity, or VerifyDB: a level-3 run whose disconnects cross
    /// below V19 leaves it legacy (a successful level-4 run reconnects through V19 and flips it back; a
    /// failed one can leave it switched). CH7a and CH5 emit Effect::BlsSchemeLeak at each of those
    /// points; the BlsCtx follows it.
    Global,
    /// v24: ScopedBLSLegacyScheme. Set from the block's own V19 status, reverted unless committed
    /// (ConnectTip, DisconnectTip), always reverted in VerifyDB.
    Scoped,
}

/// The asset-unlock index set (credit pool) around a stored range whose end wraps to 0 (2^64). O17 CONS-10.
pub enum RangeMembership {
    /// v23.1.8 `CRangesSet`: Contains misses a value inside [b, 0) other than b, so Add of such a value
    /// succeeds and inserts [v, v+1) (with v23's neighbour merges), leaving overlapping ranges that the
    /// credit pool persists. The store keeps that set as built; importers accept overlapping sets that
    /// v24's strict decoder (#7592) would reject. The credit pool only adds; undo restores the previous
    /// block's pool.
    LegacyWrap,
    /// v24: every value in [b, 2^64) is present, so the duplicate is rejected and the set stays canonical.
    Wrapped,
}

/// Version-3 ProTx payload layout and DomainPort decoding. O17 EVO-14.
pub enum ProTxDecode {
    /// v23.1.8: v3 ProRegTx/ProUpRegTx carry one scriptPayout; an undecodable DomainPort marks the
    /// netinfo entry invalid and decoding continues.
    SinglePayout,
    /// v24: v3 ProRegTx carries payouts, shares, join signatures and the early-period/penalty fields,
    /// ProUpRegTx payouts; DomainPort strings ≤ 253 bytes; a domain decode failure fails the payload.
    PayoutList,
}

/// Which check fails first for a multiply-invalid ProRegTx/ProUpRegTx. O17 EVO-16.
pub enum ProTxCheckOrder {
    /// v23.1.8: netinfo version and MN lookup before the payee checks (e.g. bad-protx-hash, TX_CONSENSUS).
    LookupFirst,
    /// v24: payee checks in IsTriviallyValid first (e.g. bad-protx-payee-reuse, TX_BAD_SPECIAL).
    PayeeFirst,
}

/// Lifetime of the CbTx merkleRootQuorums caches. O17 DKG-08. The caches live in dashd-chainstate (CH7c).
pub enum QuorumRootCache {
    /// v23.1.8: process-wide, keyed by the set of active quorum base blocks (outer) and by base-block
    /// hash (inner LRU); never invalidated when mined commitments change.
    ProcessWide,
    /// v24: owned by the block processor and dropped on every mined-commitment write or erase.
    InvalidatedOnCommitmentChange,
}

/// Block-value check at old-budget heights. O17 MNO-12.
pub enum OldBudgetCheck {
    /// v23.1.8: coinbase ≤ block reward is enforced whenever mnsync is synced, even for a ChainLocked block.
    WhenSynced,
    /// v24: NoCheck when not synced or when the best ChainLock covers the height.
    WhenSyncedAndNotChainLocked,
}

/// Validation class of an asset unlock outside the receiver's quorum or height window. O17 MPIX-16.
pub enum AuWindowFailure {
    /// v23.1.8: TX_CONSENSUS (the relaying peer gets +100).
    Consensus,
    /// v24: TX_BAD_SPECIAL (unscored).
    BadSpecial,
}

/// Consensus-relevant differences between the pinned releases that no deployment gates.
#[non_exhaustive]
pub struct UngatedRules {
    pub release: Release,
    pub au_stable_txid: bool,                    // SIG-01
    pub time_source: TimeSource,                 // CONS-04, MPIX-19, P2P-16
    pub special_tx_max_type: u16,                // CONS-06
    pub mandatory_script_flags: ScriptFlags,     // CONS-07, MPIX-01
    pub qc_dup_prepass: bool,                    // DKG-07 (A1 row 24)
    pub cbtx_v4_known: bool,                     // EVO-06
    pub proupserv_type_in_special_tx: bool,      // EVO-15
    pub reject_strings: &'static RejectStrings,  // EVO-02, EVO-17
    pub is_gate: IsGate,                         // SIG-12, SIG-13
    pub superblocks_gate: SuperblockGate,        // MNO-02
    pub bls_scheme_scope: BlsSchemeScope,        // CONS-09
    pub asset_unlock_index_membership: RangeMembership, // CONS-10
    pub protx_payload_decode: ProTxDecode,       // EVO-14
    pub protx_check_order: ProTxCheckOrder,      // EVO-16
    pub quorum_root_cache: QuorumRootCache,      // DKG-08
    pub old_budget_check: OldBudgetCheck,        // MNO-12
    pub au_window_failure_class: AuWindowFailure, // MPIX-16
}

impl UngatedRules {
    pub const V23_1: UngatedRules = /* full literal, table below */;
    pub const V24_0: UngatedRules = /* full literal, table below */;
    pub fn of(release: Release) -> &'static UngatedRules;
}
```

| Field | `V23_1` | `V24_0` | Behaviour selected | Rows |
|---|---|---|---|---|
| `au_stable_txid` | false | true | txid of `nVersion ≥ 3 ∧ nType = 9 ∧ payload.len() = 145 ∧ payload[0] ≥ 2` zeroes the last 132 bytes (judged on raw bytes, before payload validation) | SIG-01 |
| `time_source` | `AdjustedTime` | `NodeClock` | `time-too-new` (+7200 s); miner `nTime` and GBT `curtime`; `CanDirectFetch`, header-sync start, stall and timeout | CONS-04, MPIX-19, P2P-16 |
| `special_tx_max_type` | 9 | 12 | `ContextualCheckTransaction` type bound (`bad-txns-type` at AcceptBlock vs a later CheckSpecialTx reject) | CONS-06 |
| `mandatory_script_flags` | P2SH | P2SH ∣ DERSIG ∣ NULLDUMMY ∣ CLTV ∣ CSV | mempool `mandatory-` vs `non-mandatory-script-verify-flag` (+100 vs 0) and the inline-check block reject string | CONS-07, MPIX-01 |
| `qc_dup_prepass` | false | true | duplicate rotated-commitment pre-pass; under `false` the first copy's mineable-map erasure is emitted as an effect that survives the failed connect | DKG-07 |
| `cbtx_v4_known` | false | true | CbTx `nVersion ≥ 4`: parsed with `merkleRootAssetUnlocks` and checked in CheckMerkleRoot (MUTATED) vs parsed as v3 layout (`bad-cbtx-payload`/`bad-cbtx-version`) | EVO-06 |
| `proupserv_type_in_special_tx` | false | true | ProUpServTx `nType` checked in CheckSpecialTx (mempool TX_CONSENSUS +100; block `bad-protx-type`) vs only in BuildNewListFromBlock (`bad-protx-type-mismatch`) | EVO-15 |
| `reject_strings` | `&RejectStrings::V23_1` | `&RejectStrings::V24_0` | as in the struct comment; `special_tx_before_dip3` is reachable on mainnet while the active tip is below DIP3 (bootstrap, reindex) through `testmempoolaccept`/`sendrawtransaction` | EVO-02, EVO-17 |
| `is_gate` | `{SyncedAndSpork3, SyncedAndSpork3, SyncedAndSpork3}` | `{NotInIbd, IsEnabled, IsEnabled}` | IS conflict check in ConnectBlock; `IsTxSafeForMining` in the miner; ChainLock signer's IS safety walk. On mainnet (SPORK_3 hard-coded active) the ConnectBlock gate differs only between leaving IBD and mnsync reporting blockchain-synced; the CL-signer half is unobservable there (`TrySignChainTip` already waits for sync) and exists for O11 | SIG-12, SIG-13 |
| `superblocks_gate` | `Spork9` | `Always` | IsBlockValueValid/IsBlockPayeeValid/FillBlockPayments superblock branch; GBT `superblocks_enabled` and the scope of its "is syncing" refusal (every chain vs mainnet only) | MNO-02 |
| `bls_scheme_scope` | `Global` | `Scoped` | the BLS scheme after a failed or dry-run connect at the V19 boundary and after `verifychain` (a level-3 run whose disconnects cross below V19, or a failed level-4 run); under `Global` the leak is `Effect::BlsSchemeLeak` (A1 §2.10.1) | CONS-09 |
| `asset_unlock_index_membership` | `LegacyWrap` | `Wrapped` | duplicate-index decisions in the asset-unlock tx check and credit-pool update (`bad-assetunlock-duplicated-index`, `failed-creditpool-unlock-duplicated-index`) and `getassetunlockstatuses`, once a stored range ends at 2^64−1; under `LegacyWrap` also the overlapping set v23's `Add` then persists, which the store and the importers (CH6a/b) must accept | CONS-10 |
| `protx_payload_decode` | `SinglePayout` | `PayoutList` | `bad-protx-payload` vs `bad-protx-version` for v3 payloads before V24, DomainPort failures, decode JSON (BS6a's decoder takes it as a parameter) | EVO-14 |
| `protx_check_order` | `LookupFirst` | `PayeeFirst` | reject string, class and relayer score of multiply-invalid ProRegTx/ProUpRegTx | EVO-16 |
| `quorum_root_cache` | `ProcessWide` | `InvalidatedOnCommitmentChange` | `bad-cbtx-quorummerkleroot` and template roots after a reorg that mines a different commitment for an unchanged quorum base; tested with a fresh and a warmed reference | DKG-08 |
| `old_budget_check` | `WhenSynced` | `WhenSyncedAndNotChainLocked` | block-value enforcement at old-budget heights ([328008, 614820) on mainnet), reached by a synced node's historical reconnects (`verifychain 4`) | MNO-12 |
| `au_window_failure_class` | `Consensus` | `BadSpecial` | class of `bad-assetunlock-too-old-quorum`/`-too-late`, hence +100 vs no score for the relayer | MPIX-16 |

`RPC-46`'s ungated part, the decode JSON of `decoderawtransaction`/`getrawtransaction` (`instanceHash` for AU-v2-shaped transactions, payload JSON for types 10–12), follows `au_stable_txid` and `special_tx_max_type`; the rest of RPC-46 is deployment-gated.

Not in `UngatedRules` (A1 had it): `mnhf_valid_bits` (A1 row 3 refuted, CONS-03: the bit→deployment map is `params`). A1's `Effect::BlsSchemeLeak` stays in A1 §2.10.1, selected by `bls_scheme_scope`.

### 4.2 Runtime groups

All groups follow the same pattern; only `P2pRules` is shown with its `of`:

```rust
// dashd_p2p::release
impl P2pRules {
    pub const V23_1: P2pRules = P2pRules { /* … */ };
    pub const V24_0: P2pRules = P2pRules { /* … */ };
    pub fn of(release: Release) -> &'static P2pRules {
        match release { Release::V23_1 => &Self::V23_1, Release::V24_0 => &Self::V24_0 }
    }
}
```

**`dashd_p2p::release::Identity`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `protocol_version: u32` | 70240 | 70242 | P2P-01, P2P-03: every send-side gate ≥ 70241 (`dsa` flags, rebalance DSAs, `MSG_ASSET_UNLOCK` announcements) follows from it |
| `min_mn_proto_version: u32` | 70240 | 70242 | P2P-02, DKG-01 |
| `ua_version: &'static str` | "23.1.8" | "24.0.0" | P2P-01 (`/Dash Core:<v>/`, no rc suffix; `-honestua` overrides, DEC-04) |
| `client_version: u32` | 230108 | 240000 | P2P-01 (`getnetworkinfo.version`; the wallet `version` record) |

**`dashd_p2p::release::P2pRules`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `object_download: ObjectDownload` | `Legacy` | `TxRequest` | P2P-05..-09, P2P-11. One enum, not several booleans: candidate selection, delays, in-flight cap, expiry, the global erase stamp, `AskPeersForTransaction`, orphan-parent requests and `GetRequestedObjectCount` all follow from which scheduler runs |
| `notfound_limit: usize` | 116 | 50,000 | P2P-10 |
| `unsolicited_scoring: bool` | false | true | P2P-12 (+10, grace 2 × interval, 256-entry per-peer record, type-bound) |
| `tx_inv_before_verack: bool` | true | false | P2P-15 |
| `msg_asset_unlock: bool` | false (type 34 unknown end to end) | true | P2P-04 |
| `mnauth_inbound_cap: Option<usize>` | None | Some(3) | P2P-18 |
| `request_gov_invs_when_disabled: bool` | true | false | P2P-21 |
| `qrinfo_request: QrInfoRequest` | `Unbounded` | `Capped { max_base_blocks: 4096 }` | DKG-12, RPC-39. v23: `baseBlockHashes` unbounded on the wire and in `quorum rotationinfo`; a malformed `getqrinfo` throws inside processing, unscored. v24: over 4,096 or any decode failure is +100 "malformed getqrinfo received" with no reply; the RPC errors "too many baseBlockHashes: N > 4096" (checked before deduplication); the tip's block data is checked. Sorting and deduplication are common (byte-identical replies) |
| `mnlist_data_unavailable_score: u32` | 1 | 0 | DKG-13: a server without the block data for `getmnlistdiff`/`getqrinfo` scores the requester (v23, after the failed read) or only logs (v24, after its `BLOCK_HAVE_DATA` checks) |
| `block_read_vs_prune: BlockRead` | `UnderMainLock` | `Unlocked` | P2P-17: v23 reads a requested block under the main lock, so pruning cannot win; v24 reads outside it and disconnects with "Block was pruned before it could be read". A genuine read fault disconnects under both; v23's abort is not reproduced |

**`dashd_mempool::release::PolicyRules`** (the mandatory flags come from `UngatedRules`; the orphanage lives in `dashd-mempool`, A1 §2.2, and `dashd-p2p` reads this group too)

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `min_standard_tx_size: u32` | 83 | 65 | MPIX-02 |
| `min_relay_fee_checked_first: bool` | false | true | MPIX-04 |
| `blocksonly_max_mempool_mb: Option<u32>` | None | Some(5) | MPIX-05 |
| `fee_estimates: FeeEstimatePersistence` | `{ max_file_age: None, flush_interval: None }` | `{ Some(60 h), Some(1 h) }` | MPIX-07 |
| `protx_cross_scheme_dup: bool` | false | true | MPIX-13 |
| `proupserv_operator_key_binding: bool` | false | true | MPIX-14 |
| `asset_unlock_mempool: AssetUnlockMempool` | `Lenient` | `OneClaimantPerIndex` | MPIX-15 |
| `protx_collateral_vs_pending: bool` | false (admit both) | true (`protx-dup`) | MPIX-12 |
| `orphan_processing: OrphanProcessing` | `ParentSupplierSync` | `OrphanSenderDeferred` | P2P-13 |
| `disconnect_pool_accounting: DisconnectPoolUsage` | `MultiIndexEstimate` | `ListAndMap` | CONS-25: the shared 20,000 kB disconnect-pool budget is measured with v23's boost multi_index formula or v24's `DynamicUsage(list + unordered_map)`, as a logical budget (not Rust's allocation). CH3b passes disconnected transactions through `MempoolSink` |

**`dashd_masternode::release::{SporkRules, SyncRules}`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `SporkRules.defs: &'static [SporkDef]` | 7 (SPORK_2, 3, 9, 17, 19, 21, 23) | 5 (SPORK_2, 17, 19, 21, 23) | MNO-01 |
| `SporkRules.signer_model: SignerModel` | `MultiKey { default_min_keys: 1 }` | `SingleKey` | MNO-03 (`getsporks` reply multiplicity follows it) |
| `SporkRules.getsporks_dedup: bool` | false | true | P2P-14 |
| `SyncRules.gov_vote_sync_peer_bound: VoteSyncBound` | `MaxObjRequestsPerNode` | `PeersPerHashMax` | MNO-11 |

**`dashd_quorums::release::LlmqRules`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `dkg_bad_connection: BadConnectionCriteria` | `{ require_mn_tx_relay: false, require_mn_compact_filters: false }` | `{ true, true }` | DKG-01 (version threshold from `Identity.min_mn_proto_version`) |
| `dkg_status_phase: DkgStatusPhase` | `OnPhaseEntry` | `AfterOwnMessages` | DKG-02 |
| `platform_recsig_per_msghash: bool` | false | true | SIG-16 (also the RPC side, RPC-38) |
| `sigshare_recovery_skip_unreachable: bool` | false | true | SIG-17 |
| `ehf_sign_window: EhfSignWindow` | `AnyValidActivation` | `DeploymentWindowOnly` | SIG-19 |
| `dkg_intake: DkgIntake` | `DecodeCopyAtIntake` | `FramingAtIntake` | DKG-03: v23 scores an empty payload +100, leaves 1–32-byte payloads unscored (exception inside processing), and fully decodes a copy at intake (trailing bytes ignored); v24 checks framing and bounds at intake (+100) and validates types in the worker. Shared checks (SPORK_17, unverified sender, unknown quorum, `MaxDKGMessageSize`) are common |
| `observer_dkg_push: ObserverDkg` | `ProcessAndRequest` | `Ignore` | DKG-05: a `-watchquorums` non-MN runs the intake, retains payloads and requests announced DKG invs after QWATCH (v23), or ignores the four pushes and never requests them (v24). v23's undrained queues need not be reproduced, only their external effects |
| `qgetdata_decode: QGetDataDecode` | `OptionalErrorByte` | `Exact` | DKG-09: a 68-byte QGETDATA is answered (v23, with the request's `nError` able to skip the +25) or scored +100 "oversized qgetdata" (v24) |
| `qgetdata_unregistered_type_score: u32` | 0 | 100 | DKG-10, after the common `QUORUM_TYPE_INVALID` reply; v23's +25 on a repeat within 300 s comes from `qgetdata_tracking` |
| `qgetdata_tracking: QGetDataTracking` | `TrackAll` | `Bounded { per_requester: 64, total: 4096 }` | DKG-11: v23 tracks every request (invalid type, unknown block) and answers `QUORUM_NOT_FOUND` for a known block off the active chain; v24 tracks only valid requests within the caps (+25 above them) and answers `QUORUM_BLOCK_NOT_FOUND`, unscored |
| `non_mn_recsig_relay: bool` | false | true | SIG-18: a non-masternode relays recovered sigs (including ones rebuilt from ISDLOCKs) to any peer that sent `QSENDRECSIGS` |

**`dashd_locks::release::LockRules`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `coinbase_cl_processing: bool` | false | true | SIG-15 |
| `seen_clsig_cache: SeenCache` | `EvictOldest { cap: 1024 }` | `Hysteresis { keep: 1024, trigger: 2048 }` | P2P-20 |
| `islock_batch_missing_quorum: MissingQuorum` | `DropBatch` | `MarkLockOnly` | SIG-14: when one lock in a verification batch (≤ 32) has no selectable quorum, v23 drops the whole batch unverified; v24 marks only that lock and processes and relays the rest |

**`dashd_governance::release::GovernanceRules`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `invalid_vote_cache: bool` | true (persisted in `governance.dat`) | false (written empty) | MNO-06 |
| `orphan_vote_requires_valid_sig: bool` | false (cache any orphan vote, no penalty, request the parent) | true (`IsValidForUnknownParent` or +20) | MNO-07; under `V23_1` an orphan vote never reaches the invalid-vote cache, exactly as in v23.1.8 |
| `record_rejected_votes: bool` | true | false | MNO-08: v23 leaves an empty record (a "none" vote with time 0 in `gobject getcurrentvotes`, persisted in `governance.dat`) for a vote rejected after the MN lookup |

**`dashd_coinjoin::release::CoinJoinRules`** (if CJ0's pure session state machine holds the admission logic, CJ1 passes these fields to it as parameters)

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `dstx_check_order: DstxCheckOrder` | `StructureFirst` | `KnownHashFirst` | INTG-01: a known DSTX hash resent in a structurally invalid wrapper scores +10 (v23) or nothing (v24) |
| `server_reject_duplicate_collateral: bool` | false | true | MNO-17: a dsa whose collateral spends a prevout already in the session counts as another participant (v23) or gets ERR_ALREADY_HAVE (v24) |
| `server_entry_admission: EntryAdmission` | `AnyValidCollateral` | `SessionBound` | MNO-18: entry acceptance, charging and the state that admits a `dsi` |
| `server_reject_empty_entry: bool` | false | true | MNO-19: a 0-in/0-out entry takes a participant slot (v23) or gets ERR_SIZE_MISMATCH and consumes the collateral (v24) |
| `server_dsa_check_order: DsaCheckOrder` | `AcceptabilityFirst` | `StateFirst` | MNO-20: e.g. a dsa with an invalid denomination to a session in SIGNING gets ERR_DENOM (v23) or ERR_MODE (v24) |

**`dashd_wallet::release::WalletRules`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `default_kind: WalletKind` | `Legacy` (pending DEC-02, report §6.1) | `Descriptor` | WAL-01; the `createwallet` `load_on_startup` guard follows it (v23: only when `descriptors=true` is given; v24: whenever `descriptors` is given) |
| `tool_create_default: WalletKind` | `Legacy` | `Descriptor` | WAL-02 (`rdash-wallet create`) |
| `hardened_marker: HardenedMarker` | `Apostrophe` | `H` | WAL-21 (generation and normalised output; readers accept both) |
| `empty_hd_chain_counts_as_one: bool` | false | true | WAL-22 |
| `extra_db_records: bool` | false | true | WAL-24 (writer only; readers accept both) |
| `rescan_option: bool` | true | false | WAL-26 (`-rescan`) |
| `dust_protection: bool` | false | true | WAL-27 (`-dustprotectionthreshold`) |
| `import_label_sanitize: bool` | false | true | WAL-14 |
| `rescan_requires_unlock: bool` | false | true | WAL-16 |
| `importwallet_pruned: bool` | false (refuse) | true (if blocks present) | WAL-18 |
| `platform_addresses: bool` | false | true | WAL-19 |
| `platform_recipients: PlatformRecipients` | `Invalid` | `V24Gated` | WAL-20 |
| `label_all_descriptor_scripts: bool` | false | true | WAL-39 |
| `descriptor_reject_hybrid: bool` | false | true | WAL-46 |
| `psbt_verify_finalized: bool` | false | true | CONS-21 (the node RPCs `finalizepsbt`/`analyzepsbt` use the same PSBT module) |
| `protx_legacy_key_reencode: bool` | false | true | EVO-30 |
| `protx_presign_checks: bool` | false | true | RPC-43 (DIP3-inactive hint, partial-signing refusal) |
| `resubmission: Resubmission` | `ReacceptAndResend` | `Resubmit` | WAL-31: which wallet transactions are resubmitted at startup, import and on the timer, and in which order (v23: hash order, so a child can precede its parent) |
| `psbt_sign_with_mixing_unlock: bool` | true | false | WAL-08: whether a wallet unlocked only for mixing signs in `FillPSBT` (`send`, `sendall` complete or return a PSBT) |
| `reconcile_freed_inputs: bool` | false | true | WAL-35: inputs freed by abandoning or conflicting a tx return to the available set at once (v24) or after a restart (v23) |
| `coinjoin_unconfirmable_filter: bool` | false | true | WAL-36: CoinJoin balances, rounds and denomination limits exclude conflicted and non-mempool depth-0 outputs |
| `coinjoin_rounds_memo_unknown: bool` | true | false | WAL-37: the rounds lookup memoises -1 for an outpoint whose tx is unknown, which `IsFullyMixed` and `listunspent.coinjoin_rounds` then see |
| `coinjoin_pending_locks: bool` | false | true | MNO-21: after `dsc` success the session's inputs stay locked until seen spent or 1 h (v24) or are unlocked at once (v23); `getcoinjoininfo.pending_inputs` is AR2's |
| `fast_rescan: bool` | false | true | WAL-28: filter-based rescans for descriptor wallets with `-blockfilterindex=basic` (v24 can miss an OP_RETURN `raw()` script that basic filters omit) and the rescan log line |

**`dashd_node::release::InitRules`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `invalid_log_option_fatal: bool` | false (warning) | true (InitError) | P2P-24 |
| `log_scan_category: bool` | false | true | WAL-29 |
| `verifydb_require_full: bool` | false | true | P2P-25 |
| `settings_file: SettingsFile` | `V23` | `V24` (`_warning_` member, new messages) | P2P-29 |
| `bind_failure_fatal: bool` | false | true | P2P-33 |
| `onion_bind: OnionBind` | `AlwaysAddDefault` | `DedicatedRequired` | P2P-34 |
| `mn_init: MnInit` | `{ duty_refusals: false }` | `{ duty_refusals: true }` (blocksonly, `peerblockfilters=0`, `maxuploadtarget`, stricter relay policy) | P2P-35; soft-sets and the shared refusals are identical |
| `governance_requires_txindex: bool` | false | true | P2P-36 |
| `ignored_conf_fatal: bool` | false | true | P2P-37 |
| `help_before_config: bool` | true | false | P2P-38 |
| `sigcache_sizing: SigcacheSizing` | `HalfPerCacheCapped` | `ClampedBytes` | CONS-23: `min(max(0, N/2), 16384)` MiB per cache (v23) or `clamp(N, 0, 2^63/2^20) × 2^20 / 2` bytes with an InitError above ≈ 128 GiB per cache (v24). CS6 receives the size |
| `abort_exit_failure: bool` | false | true | P2P-31: exit status after a fatal error post-init (`AbortNode`): 0 (v23) or 1 (v24). The low-disk shutdown exits 0 in both |
| `initial_tip_fanout: bool` | false | true | DKG-06: after import (non-MN) or `nodeman->Init` (MN), v24 delivers the initial tip to every subscriber, so DKG handlers, signers, quorum connections and data recovery start at once; v23 delivers it only to the notification interface, and they start on the next block |

**`dashd_rpc::release::RpcRules`** (behaviour the generated surface cannot express)

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `arg_type_check: ArgTypeCheck` | `Lazy` | `Declared` | RPC-11 |
| `composite_args: CompositeArgs` | `Permissive` | `Strict` (`-deprecatedrpc=permissive_bool` restores) | RPC-12 |
| `doccheck_depth: DocCheck` | `TopLevel` | `Deep` | RPC-15 |
| `setmocktime_max: Option<i64>` | None | Some(9_223_372_036) | RPC-16 |
| `maxfeerate_cap: Option<FeeRate>` | None | Some(1 DASH/kB) | RPC-18 |
| `rawmempool_order: RawMempoolOrder` | `DepthAndScore` | `Hashed` | RPC-22 |
| `getrawtransaction_verbosity: bool` | false | true | RPC-23 |
| `utxoupdatepsbt_txindex: bool` | false | true | RPC-25 |
| `address_decoding: AddressDecoding` | `V23` | `V24WithPlatform` | RPC-27 |
| `networkhashps_strict: bool` | false | true | RPC-28 |
| `deployment_json: DeploymentJson` | `NextBlock` | `AtBlock` | RPC-03 |
| `bip9_stats_window: Bip9StatsWindow` | `AfterPeriodEnd` | `ContainingPeriod` | CONS-12 |
| `rest_mempool_query: bool` | false | true | RPC-06 |
| `index_rpc_errors: IndexRpcErrors` | `Sync` | `Async` | RPC-35 |
| `dkgstatus_mineable_height: MineableHeight` | `Tip` | `Next` | RPC-36 |
| `listdiff_legacy_evo_addresses: bool` | false | true | RPC-41 |
| `raw_tx_arg_order: RawTxArgOrder` | `OutputsFirst` | `LocktimeInputsOutputs` | RPC-49 |
| `verifychain_strict: bool` | false | true | P2P-25 |
| `batch_whitelist: BatchWhitelist` | `RequestMethod` | `PerElement` | RPC-10: under `-rpcwhitelist` v23 authorises a batch by the batch request's (empty) method, so an allowed-only batch gets HTTP 403 unless `""` is whitelisted; v24 checks each element |

**`rdash_cli::release::CliRules`**: `netinfo_transport_column: NetinfoTransport` (`V23` / `V24`), RPC-14.

**`dashd_chainstate::release::ChainstateRules`**: `persist_cl_conflict_flags: bool` (`V23_1` false, `V24_0` true), CONS-11. Only `BLOCK_CONFLICT_CHAINLOCK` on descendants found by FindMostWorkChain differs after a restart (`getchaintips`); `BLOCK_FAILED_CHILD` is re-derived at load under both.

**`dashd_mining::release::MiningRules`**: `package_special_checks: PackageChecks` (`SelectedTxOnly` / `EveryMember`), MPIX-21. v23 checks the credit pool and EHF duplicates only for a package's selected tx, so a bad ancestor makes the template self-check fail and GBT errors; v24 validates every member and skips failing packages.

**`dashd_indexes::release::IndexRules`**

| Field | `V23_1` | `V24_0` | Rows |
|---|---|---|---|
| `dash_index_model: DashIndexModel` | `Legacy` | `BaseIndex` | MPIX-08, MPIX-10. `Legacy`: enabling address/spent/timestamp index on an existing datadir needs `-reindex`; any of them forces `-checklevel=4`; `-prune` is accepted with all three (IX2 pins undo with a prune lock until indexed, or indexes at connect). `BaseIndex`: catch-up in the background; `-prune` refused with address/spent index; timestampindex on already-pruned data fails at init. The on-disk layout is rdashd's own under both (DEC-11); importers read both Core layouts |
| `rewind_on_disconnect: bool` | false | true | MPIX-11: after `invalidateblock`, `getindexinfo.best_block_height`/`synced` stay at the old tip until the next connect (v23) or rewind at once (v24). Query results are the same |

**Generated per-release tables.**
- `dash_rpc_spec::surface(Release)`, AR2. Rows: RPC-01 (19 v24-only methods), RPC-02/-20/-40 and WAL-09/-13 (`-deprecatedrpc` names and defaults), RPC-05 (REST endpoints), RPC-13 (dash-cli conversions), RPC-19/-21/-24/-29/-33/-37/-44/-45 and WAL-03..-07/-10..-12/-17 (args and results), RPC-26/-32/-42/-43, MNO-09, P2P-32 and WAL-33 (error strings).
- `dash_args::options(Release)`, AR1. Examples: `-minsporkkeys` and `-rescan` exist only in v23.1.8; `-allowignoredconf`, `-acceptstalefeeestimates` and `-dustprotectionthreshold` only in v24.
- `sporks.dat` is not a profile field (MNO-04, Policy F): rdashd keeps its own spork store, and IM1b reads both Core formats.

### 4.3 The aggregate

```rust
// dashd_node::release
pub struct ReleaseProfile {
    pub release: Release,
    pub consensus: &'static UngatedRules,
    pub identity: &'static Identity,
    pub p2p: &'static P2pRules,
    pub policy: &'static PolicyRules,
    pub sporks: &'static SporkRules,
    pub sync: &'static SyncRules,
    pub llmq: &'static LlmqRules,
    pub locks: &'static LockRules,
    pub governance: &'static GovernanceRules,
    pub chainstate: &'static ChainstateRules,
    pub mining: &'static MiningRules,
    pub coinjoin: &'static CoinJoinRules,
    pub indexes: &'static IndexRules,
    #[cfg(feature = "wallet")]
    pub wallet: &'static WalletRules,
    pub init: &'static InitRules,
    pub rpc: &'static RpcRules,
    pub rpc_surface: &'static RpcSurface, // only the aggregate refers to the generated surface
    pub options: &'static ArgsTable,
}

impl ReleaseProfile {
    pub fn of(release: Release) -> ReleaseProfile; // every field from `Group::of(release)`
}
```

### 4.4 How tasks consume it

```rust
// dash-consensus, CS3: the caller picks the clock; the rule reads the bound.
let now = match ctx.rules.time_source {
    TimeSource::AdjustedTime => clock.adjusted_now(),
    TimeSource::NodeClock => clock.now(),
};

// dashd-p2p, P5: one tracker per variant, chosen once at construction.
let requests: Box<dyn ObjectRequests> = match p2p.object_download {
    ObjectDownload::Legacy => Box::new(LegacyObjectDownload::new(p2p)),
    ObjectDownload::TxRequest => Box::new(TxRequestTracker::new(p2p)),
};

// dashd-node, ND1: wiring hands each actor its own group.
let profile = ReleaseProfile::of(release);
let peerman = PeerManager::new(profile.identity, profile.p2p, profile.consensus, /* … */);
let mempool = TxMemPool::new(profile.policy, profile.consensus, /* … */);
```

Effects stay effects: when `qc_dup_prepass` is false, LQ2 emits the mineable-commitment erasure through the A1 §2.10.1 effect channel before returning `bad-qc-dup`, so it survives the failed connect exactly as in v23.1.8. Likewise, when `bls_scheme_scope` is `Global`, CH7a and CH5 emit `Effect::BlsSchemeLeak` wherever v23.1.8 leaves its flag switched: a failed `ConnectBlock` or `TestBlockValidity` at the V19 boundary, and `verifychain` (a level-3 disconnect pass that crosses below V19, or a level-4 run that fails before reconnecting through V19). The chainstate's `BlsCtx` follows the effect.

## 5. Selection

- **Default.** `Release::default_for(network)` (DEC-05): mainnet → `V23_1`; testnet, devnet, regtest → `V24_0`.
- **`-releaseprofile=<23.1|24.0>`** is a hidden debug option (`-help-debug`):
  - `23.1` is accepted only on **regtest** (O11 runs v23.1.8's functional tests with it; O12 runs mixed networks with v23.1.8 binaries). Devnets stay on `24.0`: several `v24-all` rows (e.g. MPIX-03's `-acceptnonstdtxn` default) are justified by "only v24-pinned networks differ", which would not hold for a v23 devnet;
  - on mainnet, testnet and devnets it is an InitError unless it equals the default, because a network's emulation must track what that network runs and changing it is a reviewed re-pin (§9), not an operator switch.
- The chosen release is logged at startup ("Release profile: 23.1 (v23.1.8, mainnet default)") and reported by `getimplementationinfo` (FR-RPC-09) as `releaseprofile`. It never appears in Core-shaped RPC output (DIFF-RPC equality).
- **rdash-cli** picks its conversion table and `CliRules` from `-releaseprofile` or the selected network's default. It cannot ask the server. It must pick per release because the tables differ in one inverted case: `masternode winners count` is a JSON string in v23.1.8 and a number in v24 (RPC-12).
- **`rdash-wallet`** uses `WalletRules::of` for the chain it is given.

## 6. Per-call-site table

Every `profile` field, the rows it implements, the main Core call sites at both tags, the tasks that **read** it (their crate can depend on the field's home, A1 §2.2/§2.3), and the other tasks involved, with their role:
- **P**: provides both variants or receives the chosen value as a parameter from a reader. Such a task is a pure crate or sits below the home crate;
- **T**: consumes a generated per-release table (AR1 `options(Release)`, AR2 `surface(Release)`);
- **V**: verifies it (O10–O16, PL1).

Full evidence is in each row (report §5 → packet file). Paths are under Dash Core's `src/`; evo-packet rows omit the `evo/` prefix. Generated from `decisions.json`, the merged rows and PLAN's task table by `contrib/node/pin-delta/spec_tables.py` (§10 too); a field with several rows lists the union of their call sites.

| Field | Rows | v23.1.8 call sites | v24.0.0-rc.3 call sites | Read by | Other roles |
|---|---|---|---|---|---|
| `UngatedRules.asset_unlock_index_membership` | CONS-10 | util/ranges_set.cpp:96; evo/assetlocktx.cpp:183-184; evo/creditpool.cpp:291-292 | util/ranges_set.cpp:96-110; evo/assetlocktx.cpp:214-215; evo/creditpool.cpp:320-321 | EV3, RP3b | O10 (V) |
| `UngatedRules.au_stable_txid` | SIG-01 | primitives/transaction.cpp:73-75, 96-98. | primitives/transaction.h:47, 50, 53, 383-395; primitives/transaction.cpp:82-87, 93, 117, 121-124 | CS0, CS3, RP1b, MP1a, P6a | BS6b (P), O10 (V) |
| `UngatedRules.au_window_failure_class` | MPIX-16 | evo/assetlocktx.cpp:131, 137 | evo/assetlocktx.cpp:126-135 | MP1a, P5, P4, EV1 | O13b (V), O10 (V) |
| `UngatedRules.bls_scheme_scope` | CONS-09 | validation.cpp:4548-4585; evo/specialtxman.cpp:695-699 | validation.cpp:147-178, 2490-2492, 2916, 3272-3274, 3320, 3405, 3477, 5044 | CH3a, CH7a, MN1, CH5 | BS11a-t (P), O10 (V) |
| `UngatedRules.cbtx_v4_known` | EVO-06 | cbtx.h:52-60 | validation.cpp:4347-4359; cbtx.h:61-66 | CS3, EV4 | O10 (V), BS6a (P) |
| `UngatedRules.is_gate.connect_block` | SIG-12 | validation.cpp:2607-2622; instantsend/instantsend.cpp:813-828; evo/chainhelper.cpp:72. | validation.cpp:2801-2815; instantsend/instantsend.cpp:510-512; instantsend/lock.cpp:59-62 | CH3a, CH7b, LK1b, SP0 | — |
| `UngatedRules.is_gate.{miner, cl_signer}` | SIG-13 | node/miner.cpp:382; chainlock/signing.cpp:83, 129. | node/miner.cpp:417; chainlock/signing.cpp:129 | MN1, LK1c, SP0 | — |
| `UngatedRules.mandatory_script_flags` | CONS-07, MPIX-01 | script/standard.h:57; policy/policy.h:85; validation.cpp:1913-1935, 2544; script/standard.h:57; validation.cpp:1912-1937; net_processing.cpp:2047-2049 | script/standard.h:47-51; policy/policy.h:80; validation.cpp:2197-2223, 2752; script/standard.h:47-51; validation.cpp:2198-2223; net_processing.cpp:2028-2030 | CS3, CS6, CH3a, MN1, MP1a, P5, RP3a, P4 | O10 (V), O13b (V) |
| `UngatedRules.old_budget_check` | MNO-12 | masternode/payments.cpp:156-175; validation.cpp:2642 | masternode/payments.cpp:276-294; validation.cpp:2841-2843; evo/chainhelper.cpp:41-52 | EV3, CH7a, CH5, GV1b | O10 (V) |
| `UngatedRules.protx_check_order` | EVO-16 | providertx.cpp:67-118,210-228; specialtxman.cpp:1150-1171 | providertx.cpp:265-333,459-473; specialtxman.cpp:1438-1482 | EV1, CS3 | O10 (V) |
| `UngatedRules.protx_payload_decode` | EVO-14 | providertx.h:96-104,216-225; netinfo.h:168-171,243-250 | providertx.h:176-213,343-361; netinfo.h:176-179,253-262 | EV1 | BS6a (P), BS4a (P), O10 (V) |
| `UngatedRules.proupserv_type_in_special_tx` | EVO-15 | specialtxman.cpp:1061-1138 (no type check), 339-344; providertx.cpp:165-185; rpc/evo.cpp:1008-1009 | providertx.cpp:420-422; specialtxman.cpp:1344-1350,477-485 | EV1, MP1a, P4, MN1 | — |
| `UngatedRules.qc_dup_prepass` | DKG-07 | llmq/blockprocessor.cpp:187-231, 322-324, 349-351, 362, 370-375, 446-451 (non-rotated dup), 502-520. | llmq/blockprocessor.cpp:234-290, 259-264, 381-392, 417-419, 432-453, 588-592 | LQ2, CH7b, MN1, RP3a | O10 (V) |
| `UngatedRules.quorum_root_cache` | DKG-08 | evo/cbtx.cpp:55-108 (61, 66), 129. | llmq/blockprocessor.cpp:446, 461-468, 470-509, 551; evo/cbtx.cpp (caller) | EV4, LQ2, CH7b, CH7c | O15 (V), O10 (V) |
| `UngatedRules.reject_strings` | EVO-02, EVO-17 | assetlocktx.cpp:72,180; primitives/transaction.cpp:96-99; specialtxman.cpp:1266-1270; specialtxman.cpp:111-113 | assetlocktx.cpp:60,211; primitives/transaction.cpp:93,117,123; primitives/transaction.h:392; specialtxman.cpp:227-229 | CS3, EV3, P6a, EV1 | O10 (V) |
| `UngatedRules.special_tx_max_type` | CONS-06 | validation.cpp:326-341 | validation.cpp:360-377; evo/specialtxman.cpp:241-246, 1573, 1630, 1799 | CS3, EV1, MP1a | O10 (V) |
| `UngatedRules.superblocks_gate` | MNO-02 | governance/governance.cpp:1505-1508; masternode/payments.cpp:243-251,311,343; rpc/mining.cpp:737-740,966 … | masternode/payments.cpp:355-362,421-438,450; rpc/mining.cpp:740-754,980; spork.cpp:173-181 | EV3, CH7a, MN1, RP3a, SP0 | O11 (V), O14 (V) |
| `UngatedRules.time_source` | CONS-04, MPIX-19, P2P-16 | validation.cpp:4143-4144, 4282, 4570; timedata.cpp:35-38; timedata.h:16; node/miner.cpp:53, 222; rpc/mining.cpp:931; net_processing.cpp:1407, 6225, 6246, 6627 | validation.cpp:4509-4510, 4649, 4953; node/miner.cpp:51, 215; rpc/mining.cpp:945; net_processing.cpp:1430, 6370, 6391, 6785 | CS3, CH1, MN1, ND1, MN0, RP3a, P2b, P3 | O10 (V) |
| `UngatedRules.{au_stable_txid, special_tx_max_type}` | RPC-46 | evo/core_write.cpp:242-244 (payoutAddress only) | evo/core_write.cpp:110,147,183-188,317-326; core_write.cpp:328; rpc/masternode.cpp:269-282,535-546 | RP2a, RP1b, WL5 | AR2 (T) |
| `ChainstateRules.persist_cl_conflict_flags` | CONS-11 | validation.cpp:3280-3286 | validation.cpp:3555-3561 | CH3b, RP1a | O14 (V) |
| `Identity.min_mn_proto_version` | DKG-01, P2P-02 | active/dkgsession.cpp:213-255 (217, 229, 232, 242, 246, 250), version.h:23; version.h:23, active/dkgsession.cpp:242 | active/dkgsession.cpp:226-287 (230, 250-256, 259, 269, 273-279, 281, 285), version.h:23, net.h:821-823; version.h:23, active/dkgsession.cpp:269 | DK1, P2a | LQ4a (P), O13c (V), O12b (V) |
| `Identity.protocol_version` | P2P-03 | version.h:68 | version.h:73, 77, coinjoin/coinjoin.h:163, net_processing.cpp:3613-3627 | P2a, P6a, CJ1 | WL6 (P), BS9b (P), CJ0 (P) |
| `Identity.{protocol_version, ua_version, client_version}` | P2P-01 | version.h:14, configure.ac:3-6, rpc/net.cpp:682 | version.h:14, configure.ac:3-6, rpc/net.cpp:695 | P2a, P2b, RP1a | O13a (V) |
| `P2pRules.block_read_vs_prune` | P2P-17 | net_processing.cpp:2783-2789 | net_processing.cpp:2812-2828 | P4 | O13b (V) |
| `P2pRules.mnauth_inbound_cap` | P2P-18 | evo/mnauth.cpp:158-159, rpc/node.cpp:314-320 | net.h:118, net.cpp:4492-4518, evo/mnauth.cpp:158-163, rpc/node.cpp:323 | P7, AM1, RP1a | — |
| `P2pRules.mnlist_data_unavailable_score` | DKG-13 | evo/smldiff.cpp:207-210; net_processing.cpp:5546-5549, 5586-5589. | llmq/snapshot.cpp:26-32, 55, 132, 146; evo/smldiff.cpp:190-193, 201-206, 249-251; evo/deterministicmns.h:742-743 … | P6b | LQ3 (P), O13b (V) |
| `P2pRules.msg_asset_unlock` | P2P-04 | protocol.h:526 (last type 33), protocol.cpp:302, net_processing.cpp:2907, 4479 | protocol.h:527, 555-558, protocol.cpp:305, net_processing.cpp:2353-2381, 2901-2910, 4524, 4880-4908 | P5, P6a | BS7 (P), O13b (V), BS10 (P), MP1a (P) |
| `P2pRules.notfound_limit` | P2P-10 | net_processing.cpp:5598-5620 (limit at :5602) | net_processing.cpp:5744-5765 (limit at :5752) | P4, P5 | O13b (V) |
| `P2pRules.object_download` | P2P-05, P2P-06, P2P-07, P2P-08, P2P-09, P2P-11 | net_processing.cpp:87-101, 1117-1118, 1631-1645, 1652-1678, 1680-1712, 6707-6760; net_processing.cpp:97, 6692-6705, 6709-6756; net_processing.cpp:1574-1588, 4837, 6722-6727; net_processing.cpp:2466-2492; net_processing.cpp:4917-4924 … | net_processing.cpp:85-115, 1595-1609, 1635-1664, 6838-6896, txrequest.h:22-95; net_processing.cpp:6849-6853, 6884-6889; net_processing.cpp:3825-3835, 4900-4908, 6912-6917; net_processing.cpp:2473-2512; net_processing.cpp:4986-4995 … | P5, P5a, P6a, LK1b, GV1b, SP1 | O13b (V) |
| `P2pRules.qrinfo_request` | DKG-12, RPC-39 | llmq/snapshot.h:100; llmq/snapshot.cpp:76-80, 214-226; net_processing.cpp:5575-5590; rpc/quorums.cpp:955-977; llmq/snapshot.cpp:56-81 | llmq/snapshot.h:100, 111; llmq/snapshot.cpp:84-88, 113-124, 259-265; net_processing.cpp:5660-5686; rpc/quorums.cpp:986-1000; llmq/snapshot.cpp:84-124 | P6b, RP3b | BS8 (P), LQ3 (P), O13b (V), O11 (V), O14 (V) |
| `P2pRules.request_gov_invs_when_disabled` | P2P-21 | net_processing.cpp:2419-2421, init.cpp:2252-2261 | net_processing.cpp:2426-2431, governance/net_governance.cpp (`NetGovernance::AlreadyHave`), init.cpp:2276-2280 | GV1b, P6a | — |
| `P2pRules.tx_inv_before_verack` | P2P-15 | net_processing.cpp:1220-1232 | net_processing.cpp:1240-1245, 4243-4255 | P5 | O13b (V) |
| `P2pRules.unsolicited_scoring` | P2P-12 | net_processing.cpp:5666-5675, chainlock/handler.cpp:105-131, llmq/blockprocessor.cpp:64-80 | net_processing.cpp:96-111, 1611-1631, 5794-5816, 6876-6881, 6925-6957, msg_result.h:21 … | P5, P5a, LK1a, DK1 | — |
| `PolicyRules.asset_unlock_mempool` | MPIX-15 | evo/specialtxman.cpp:152-157; evo/assetlocktx.cpp:183-184; txmempool.cpp:1090-1140 | validation.cpp:905, 921-957, 1148-1173, 1249, 1457-1468; txmempool.cpp:838-850, 1266; rpc/mempool.cpp:674 | MP1a, MP1b, RP3a | PL1 (V) |
| `PolicyRules.blocksonly_max_mempool_mb` | MPIX-05 | init.cpp:1001-1006; policy/policy.h:35 | init.cpp:1059-1065; kernel/mempool_options.h:27; validation.cpp:2937-2943 | ND1, MP1b | CH2 (P) |
| `PolicyRules.disconnect_pool_accounting` | CONS-25 | validation.cpp:95, 3074-3084; txmempool.h:1008 | kernel/disconnected_transactions.h:18, 80-110; validation.cpp:3307-3313 | MP1b | CH3b (P), O15 (V) |
| `PolicyRules.fee_estimates` | MPIX-07 | policy/fees.cpp:562-567, 919-926; init.cpp:1685 | policy/fees.h:26,31,34; policy/fees.cpp:562-580, 930-943; init.cpp:840, 1719-1731 | MP2 | IM1a (P) |
| `PolicyRules.min_relay_fee_checked_first` | MPIX-04 | validation.cpp:719-731, 917-926 | validation.cpp:775-787, 1052-1066 | MP1a, RP3a | — |
| `PolicyRules.min_standard_tx_size` | MPIX-02 | policy/policy.h:29; validation.cpp:797-798 | policy/policy.h:29; validation.cpp:875-876 | MP1a, P5, RP3a | — |
| `PolicyRules.orphan_processing` | P2P-13 | txorphanage.cpp:155-170, 178-196, net_processing.cpp:3420, 3430, 4882, 4892, 5745 | txorphanage.cpp:155-172, 180-197, 223-233, net_processing.cpp:3402, 3413, 4954, 5886, 5939 | P5, MP1b | O13b (V) |
| `PolicyRules.protx_collateral_vs_pending` | MPIX-12 | txmempool.cpp:1018-1048, 1371-1460 | txmempool.cpp:1031-1052, 1165-1173, 1629-1640, 1660-1665, 1680-1684, 1706-1710 | MP1a, MN1 | — |
| `PolicyRules.protx_cross_scheme_dup` | MPIX-13 | validation.cpp:954-959; txmempool.cpp:1371 | validation.cpp:1105-1125; txmempool.cpp:1502-1583 | MP1a, RP3a | — |
| `PolicyRules.proupserv_operator_key_binding` | MPIX-14 | txmempool.cpp:672-680, 993-1004, 1067-1074 | txmempool.cpp:644-650, 1085, 1196-1203 | MP1b, ZQ1, MP1a | — |
| `MiningRules.package_special_checks` | MPIX-21 | node/miner.cpp:374-391, 540-570, 625 | node/miner.cpp:379-424, 629-681; doc/release-notes-7570.md | MN1 | O14 (V) |
| `SporkRules.defs` | MNO-01 | spork.h:40-41,72-75; rpc/node.cpp:182,188,220; test/functional/test_framework/test_framework.py:1852 | spork.h:66; rpc/node.cpp:190,196,229; spork.cpp:86-125 (no ID check) | SP0, RP2b | O11 (V), O14 (V) |
| `SporkRules.getsporks_dedup` | P2P-14 | spork.cpp:127-135, 201-209 | net_processing.cpp:5713-5736 | SP0 | P5a (P), O13b (V) |
| `SporkRules.signer_model` | MNO-03 | spork.cpp:25-54,137-198,327-336,445-462; spork.h:166,226-227; init.cpp:770,773,1699-1715; chainparams.cpp:291-292 … | spork.cpp:22-30,86-125,228; spork.h:151,205; init.cpp:826,1746-1752; chainparams.cpp:302 | SP0, ND1 | AR1 (T), O11 (V) |
| `SyncRules.gov_vote_sync_peer_bound` | MNO-11 | node/sync_manager.cpp:66,74-79,86,120-126 | node/sync_manager.cpp:55,66,86 | SP1 | GV1b (P), O13b (V) |
| `LlmqRules.dkg_bad_connection` | DKG-01 | active/dkgsession.cpp:213-255 (217, 229, 232, 242, 246, 250), version.h:23. | active/dkgsession.cpp:226-287 (230, 250-256, 259, 269, 273-279, 281, 285), version.h:23, net.h:821-823 … | DK1 | LQ4a (P), P2a (P), O13c (V), O12b (V) |
| `LlmqRules.dkg_intake` | DKG-03 | llmq/dkgsessionmgr.cpp:41-114, 160-279 (199, 270, 276); active/dkgsessionhandler.cpp:395-422 … | llmq/net_dkg.cpp:43-211, 369-418, 492-603 (531, 593-603); llmq/dkgsession.cpp:294, 512 | DK1 | BS9a (P), O3 (V), O13c (V) |
| `LlmqRules.dkg_status_phase` | DKG-02 | active/dkgsessionhandler.cpp:149-157, 470-474, 483. | active/dkgsessionhandler.cpp:137-139, 240-243; llmq/net_dkg.cpp:852, 862, 935 | DK1 | O11 (V), O14 (V) |
| `LlmqRules.ehf_sign_window` | SIG-19 | llmq/ehf_signals.cpp:36-48; chainparams.cpp:109-130, 212-219. | llmq/ehf_signals.cpp:36-63 | SG1a | CS4 (P) |
| `LlmqRules.non_mn_recsig_relay` | SIG-18 | llmq/net_signing.h:19; active/context.cpp:122-125; init.cpp:2203, 2214; net_processing.cpp:2643-2664. | llmq/net_signing.h:39; llmq/net_signing.cpp:198-217, 393-396; net_processing.cpp:1691-1695, 2663-2684 … | SG1a | P5a (P), O13b (V) |
| `LlmqRules.observer_dkg_push` | DKG-05 | llmq/dkgsessionmgr.cpp:186-197, 283-298; llmq/observer/context.cpp:24, 28-31. | llmq/net_dkg.cpp:501-518, 668-688 | DK1, QM1b | O13b (V) |
| `LlmqRules.platform_recsig_per_msghash` | SIG-16 | llmq/signing.cpp:135, 535-552, 622; llmq/signing_shares.cpp:564, 620, 818, 901, 956, 1113; rpc/quorums.cpp:529 … | llmq/signing.cpp:33-36, 136-145, 152-165, 180-182, 594-596, 666-667, 678; llmq/signing_shares.cpp:412, 455, 629 … | SG1a, SG1b, RP3b | PL1 (V) |
| `LlmqRules.qgetdata_decode` | DKG-09 | llmq/quorums.h:108-121; llmq/quorumsman.cpp:424-425, 492-507. | llmq/net_quorum.cpp:85-94 | QM1b | BS8 (P), O13c (V) |
| `LlmqRules.qgetdata_tracking` | DKG-11 | llmq/quorumsman.cpp:349-360, 417-508 (452-473). | llmq/net_quorum.cpp:72-75, 121-170; llmq/quorumsman.h:50-58; llmq/quorumsman.cpp:451-473, 558-606 | QM1b | O13c (V) |
| `LlmqRules.qgetdata_unregistered_type_score` | DKG-10 | llmq/quorumsman.cpp:452-468. | llmq/net_quorum.cpp:121-129 | QM1b | O13c (V) |
| `LlmqRules.sigshare_recovery_skip_unreachable` | SIG-17 | llmq/signing_shares.cpp:1036-1049, 1283-1326. | llmq/signing_shares.cpp:750-774, 1009-1069 | SG1b | LQ5 (P) |
| `LockRules.coinbase_cl_processing` | SIG-15 | no `ProcessCoinbaseChainLock` in the tree; net_processing.cpp:2183 (BlockConnected without it) … | chainlock/handler.cpp:104-131; net_processing.cpp:2164 | LK1a | — |
| `LockRules.islock_batch_missing_quorum` | SIG-14 | instantsend/net_instantsend.cpp:119-127, 295-321. | instantsend/net_instantsend.cpp:149-157, 330-356 | LK1b | O13b (V) |
| `LockRules.seen_clsig_cache` | P2P-20 | chainlock/handler.h:63, chainlock/handler.cpp:48 | chainlock/handler.h:57-58, chainlock/handler.cpp:53 | LK1a | O13b (V) |
| `GovernanceRules.invalid_vote_cache` | MNO-06 | governance/governance.cpp:798-810,835-838; governance/governance.h:186,203,223 | governance/governance.cpp:802-851; governance/governance.h:206-209,229-231 | GV1b | P6a (P), IM1b (P) |
| `GovernanceRules.orphan_vote_requires_valid_sig` | MNO-07 | governance/governance.cpp:812-821; governance/net_governance.cpp:261-269 | governance/governance.cpp:820-836; governance/vote.cpp:244-250; governance/net_governance.cpp:260-268 | GV1b | P6a (P) |
| `GovernanceRules.record_rejected_votes` | MNO-08 | governance/object.cpp:83-84,99-100 | governance/object.cpp:414-419,472-473 | GV1b, GS1, RP2b | O14 (V) |
| `CoinJoinRules.dstx_check_order` | INTG-01 | net_processing.cpp:3627-3640 | net_processing.cpp:3637-3660 | CJ1 | P6a (P), O13b (V) |
| `CoinJoinRules.server_dsa_check_order` | MNO-20 | coinjoin/server.cpp:800-822 | coinjoin/server.cpp:1041-1061,1077-1085,1122-1127 | CJ1 | O13c (V) |
| `CoinJoinRules.server_entry_admission` | MNO-18 | coinjoin/server.cpp:215,616-693 | coinjoin/server.cpp:204,711-766,842-866 | CJ1 | O13c (V) |
| `CoinJoinRules.server_reject_duplicate_collateral` | MNO-17 | coinjoin/server.cpp:800-831 (no prevout check; push at :825) | coinjoin/server.cpp:1128-1139,941-948 | CJ1 | O13c (V) |
| `CoinJoinRules.server_reject_empty_entry` | MNO-19 | coinjoin/server.cpp:672-680; coinjoin/coinjoin.cpp:177-262 | coinjoin/server.cpp:792-803 | CJ1 | O13c (V) |
| `IndexRules.dash_index_model` | MPIX-08, MPIX-10 | txdb.cpp:24-27, 381-406; validation.cpp:2128-2150, 2684-2700, 5055-5068; node/chainstate.cpp:117-131; init.cpp:1200-1208 | index/addressindex.cpp:17-23; index/spentindex.cpp:20-23; index/timestampindex.cpp:13-17; txdb.cpp:492-645; init.cpp:1249-1254; index/base.cpp:122; doc/release-notes-7101.md | IX2, CH8, ND1 | CH6a (P), CH6b (P), IM1a (P), RL2 (V), O11 (V) |
| `IndexRules.rewind_on_disconnect` | MPIX-11 | index/base.cpp:244-270 (no BlockDisconnected override in index/base.h) | index/base.cpp:308-330 | IX1 | O14 (V) |
| `WalletRules.coinjoin_pending_locks` | MNO-21 | coinjoin/client.cpp:690-703 | coinjoin/client.cpp:634-666,681-883,2478; coinjoin/coinjoin.h:51; wallet/walletdb.cpp:45,245 | WL6, WL2a, RP2b, WL8 | O14 (V) |
| `WalletRules.coinjoin_rounds_memo_unknown` | WAL-37 | wallet/coinjoin.cpp:280-286 | wallet/coinjoin.cpp:283-290 | WL6, WL8 | O14 (V) |
| `WalletRules.coinjoin_unconfirmable_filter` | WAL-36 | wallet/coinjoin.cpp:160, 251, 490, 504, 543 | wallet/coinjoin.cpp:161, 252, 544-546, 573, 587, 625; wallet/wallet.cpp:757-763, 1478-1481 | WL6, WL2b, WL8 | O14 (V) |
| `WalletRules.default_kind` | WAL-01 | wallet/rpc/wallet.cpp:592, 634-640 | wallet/rpc/wallet.cpp:613, 665-669 | WL2b | AR2 (T), O14 (V) |
| `WalletRules.descriptor_reject_hybrid` | WAL-46 | script/descriptor.cpp:840-860, 1053-1100 | script/descriptor.cpp:917-919, 1131-1146, 1153-1196 | WL4 | AR2 (T) |
| `WalletRules.dust_protection` | WAL-27 | qt/optionsmodel.cpp:374-380 (GUI only) | wallet/init.cpp:75-80; wallet/wallet.h:119-121; wallet/wallet.cpp:1090-1092, 2875-2939, 3450-3457, 3479 | WL7 | AR1 (T) |
| `WalletRules.empty_hd_chain_counts_as_one` | WAL-22 | wallet/hdchain.cpp:206-210; wallet/rpc/wallet.cpp:868-876; wallet/wallet.cpp:2997-3014 | wallet/hdchain.cpp:187-214; wallet/rpc/wallet.cpp:916-930; wallet/wallet.cpp:3242-3256 | WL2b, WL3 | WL1 (P) |
| `WalletRules.extra_db_records` | WAL-24 | wallet/walletdb.cpp:763-768 | wallet/walletdb.cpp:45, 62, 235-263, 669-674, 834; wallet/wallet.cpp:3873-3893, 3988-4026 | WL3, IM2, WL6, WL2a | — |
| `WalletRules.fast_rescan` | WAL-28 | wallet/wallet.cpp:1844 | wallet/wallet.cpp:307-361, 1978-1983, 2009-2024 | WL2a, WL8 | IX1 (P), O11 (V) |
| `WalletRules.hardened_marker` | WAL-21 | wallet/scriptpubkeyman.cpp:2156-2162; util/bip32.cpp:59; wallet/rpc/addresses.cpp:543; wallet/rpc/backup.cpp:2063 … | wallet/scriptpubkeyman.cpp:2535-2541; util/bip32.cpp:54-67; script/descriptor.cpp:412-422, 1144, 1271-1277 … | WL2b, WL3, WL4, IM2, RP1a | O14 (V), AR2 (T) |
| `WalletRules.import_label_sanitize` | WAL-14 | wallet/rpc/backup.cpp:137, 244; wallet/rpc/util.cpp:133-139 | wallet/rpc/backup.cpp:155, 260; wallet/rpc/util.cpp:134-142 | WL2b | AR2 (T) |
| `WalletRules.importwallet_pruned` | WAL-18 | wallet/rpc/backup.cpp:515-519 | wallet/rpc/backup.cpp:90-104, 583 | WL2b, WL7 | CH8 (P) |
| `WalletRules.label_all_descriptor_scripts` | WAL-39 | wallet/wallet.cpp:4121-4124 | wallet/wallet.cpp:4558-4566 | WL2b | — |
| `WalletRules.platform_addresses` | WAL-19 | wallet/rpc/addresses.cpp:479-487 | wallet/rpc/addresses.cpp:434, 483-504 | WL2b | PL1 (V) |
| `WalletRules.platform_recipients` | WAL-20 | wallet/rpc/spend.cpp:24-50 | wallet/rpc/spend.cpp:26-84; wallet/spend.cpp:786-813, 1114-1118, 1134-1140; node/interfaces.cpp:1617-1621 | WL2b, WL5 | — |
| `WalletRules.protx_legacy_key_reencode` | EVO-30 | rpc/evo.cpp:1149-1161 | evo/providertx_service.cpp:900-911 | WL5 | AR2 (T) |
| `WalletRules.protx_presign_checks` | RPC-43 | rpc/evo.cpp:352-372,705,746-749,1311; rpc/evo_util.cpp:91-97 | evo/providertx_service.cpp:366-385,417-427,983-988; rpc/evo.cpp:536-541,880-885,916-921 | WL5 | AR2 (T), O11 (V) |
| `WalletRules.psbt_sign_with_mixing_unlock` | WAL-08 | wallet/wallet.cpp:2146-2160 | wallet/wallet.cpp:2311-2323 | WL2b, WL4, WL6, WL8 | O14 (V) |
| `WalletRules.psbt_verify_finalized` | CONS-21 | psbt.cpp:214-217, 241 | psbt.cpp:184-209, 242-245, 269 | WL4 | AR2 (T) |
| `WalletRules.reconcile_freed_inputs` | WAL-35 | wallet/wallet.cpp:945-1000 (no reconcile) | wallet/wallet.cpp:1073-1079, 1202-1227, 1438 | WL2a, WL2b, WL5, WL8 | O14 (V) |
| `WalletRules.rescan_option` | WAL-26 | wallet/init.cpp:68-69, 151-156; dummywallet.cpp:52-53; wallet/walletdb.cpp:875; wallet/wallet.cpp:2950, 3268, 3327 | wallet/walletdb.cpp:960, 1023-1024; wallet/wallet.cpp:3199-3202, 3526-3529; util/system.cpp:292, 904 | ND1, WL2a, WL7 | AR1 (T) |
| `WalletRules.rescan_requires_unlock` | WAL-16 | wallet/rpc/transactions.cpp:877; wallet/rpc/backup.cpp:1891; wallet/rpc/encrypt.cpp:126, 179, 216 | wallet/rpc/transactions.cpp:896, 902-907; wallet/rpc/backup.cpp:1879-1886; wallet/rpc/encrypt.cpp:148, 211, 261 | WL2b, WL2a | — |
| `WalletRules.resubmission` | WAL-31 | wallet/wallet.cpp:1953-1979, 2040-2073, 3393 | wallet/wallet.cpp:2160-2236, 3655; test/functional/wallet_resendwallettransactions.py:72 | WL2a, WL8 | P5 (P), O11 (V) |
| `WalletRules.tool_create_default` | WAL-02 | wallet/wallettool.cpp:148; bitcoin-wallet.cpp:41 | wallet/wallettool.cpp:141-165; bitcoin-wallet.cpp:40 | AR4 | — |
| `InitRules.abort_exit_failure` | P2P-31 | node/blockstorage.cpp:887-888, shutdown.cpp:31, bitcoind.cpp:279 | node/blockstorage.cpp:950, shutdown.cpp:34-35, node/context.h:99, node/interfaces.cpp:875, 894 | ND1 | RL1 (V), O11 (V) |
| `InitRules.bind_failure_fatal` | P2P-33 | net.cpp:3963-3983, 4003 | net.cpp:3957-3989, 4009 | ND1 | P2a (P), P8 (P) |
| `InitRules.governance_requires_txindex` | P2P-36 | absent (init.cpp:1413-1431 has only the MN-only txindex check) | init.cpp:1443-1448 | ND1 | GV1b (P) |
| `InitRules.help_before_config` | P2P-38 | bitcoind.cpp:132-145, 161, 231-234 | bitcoind.cpp:122, 143-159, 279-287 | ND1 | AR1 (T), AR3 (T) |
| `InitRules.ignored_conf_fatal` | P2P-37 | absent (util/system.cpp:1087 auto-create) | init.cpp:616, common/init.cpp:73-88, util/system.cpp:1003 | ND1 | AR1 (T) |
| `InitRules.initial_tip_fanout` | DKG-06 | init.cpp:2423; dsnotificationinterface.cpp:41-45. | init.cpp:2459-2473, 2547-2555; llmq/net_quorum.cpp:374-401; active/context.cpp:88-113 | ND1 | QM1b (P), DK1 (P), P7 (P), O13c (V) |
| `InitRules.invalid_log_option_fatal` | P2P-24 | init/common.cpp:99, 105, 122, 131, logging.cpp (no `scan`) | init/common.cpp:66, 72, 90, 99, init.cpp:1317-1320, logging.cpp (`{BCLog::SCAN, "scan"}`) | ND1 | AR1 (T), O11 (V) |
| `InitRules.log_scan_category` | WAL-29 | logging.cpp:160-200 (no scan) | logging.cpp:182, 299; logging.h | — | AR1 (T), RP1a (P) |
| `InitRules.mn_init` | P2P-35 | init.cpp:1046-1056, 1413-1431 | init.cpp:1094-1104, 1395-1440, node/mempool_args.cpp (`GetStricterThanDefaultRelayPolicy`) | ND1 | AM1 (P) |
| `InitRules.onion_bind` | P2P-34 | init.cpp:2616-2631 | init.cpp:656-657, 2685-2703 | ND1 | P8 (P) |
| `InitRules.settings_file` | P2P-29 | util/settings.cpp:93, 121-128, util/system.cpp:548, 552, 598-601 | util/settings.cpp:97-99, 131-140, common/init.cpp:95, 105 | — | AR1 (T), IM1a (P), O11 (V) |
| `InitRules.sigcache_sizing` | CONS-23 | script/sigcache.cpp:85; script/sigcache.h:17-19; validation.cpp:1841 | node/validation_cache_args.cpp:28-33; init.cpp:1548-1551; cuckoocache.h:365-376 | ND1 | CS6 (P), AR1 (T), O11 (V) |
| `InitRules.verifydb_require_full` | P2P-25 | validation.cpp:4646 (returns bool), rpc/blockchain.cpp:1432, node/chainstate.cpp:280-285 | validation.cpp:5030 (VerifyDBResult), rpc/blockchain.cpp:1423, 1440, node/chainstate.cpp:436-454, init.cpp:2085 … | ND1 | CH5 (P), RP1a (P), O11 (V) |
| `RpcRules.address_decoding` | RPC-27 | rpc/output_script.cpp:49-68; key_io.cpp:45-67 | rpc/output_script.cpp:57-94; key_io.cpp:46-99,260-293 | RP1a | WL2b (P), AR2 (T), O11 (V), O14 (V), BS5 (P) |
| `RpcRules.arg_type_check` | RPC-11 | rpc/util.cpp:33-56; rpc/server.cpp:533-534; rpc/net.cpp:784,1015 | rpc/util.cpp:537-549,747-758; rpc/net.cpp:799,1029 | RP0 | AR2 (T), O11 (V), O14 (V), WL2b (P) |
| `RpcRules.batch_whitelist` | RPC-10 | httprpc.cpp:103-111,247-261 (`whitelisted(jreq)` :256) | httprpc.cpp:104-112,248-261 (`whitelisted(element)` :256) | RP0 | PL1 (V), O14 (V) |
| `RpcRules.bip9_stats_window` | CONS-12 | versionbits.cpp:140-176; rpc/blockchain.cpp:1483 | versionbits.cpp:140-188; rpc/blockchain.cpp:1510, 1525 | RP1a | CS4 (P), AR2 (T), O14 (V) |
| `RpcRules.composite_args` | RPC-12 | rpc/util.cpp:124-160; rpc/quorums.cpp:278,595,955-965; rpc/masternode.cpp:291,381; rpc/governance.cpp:179-180 … | rpc/util.cpp:114-140; rpc/quorums.cpp:291,618,986-993; rpc/masternode.cpp:316,413; rpc/governance.cpp:179-180 … | RP0, RP2a, RP2b, RP3b | WL5 (P), AR2 (T), O11 (V), PL1 (V) |
| `RpcRules.deployment_json` | RPC-03 | rpc/blockchain.cpp:1453-1503 | rpc/blockchain.cpp:1461-1525,1696-1735 | RP1a | AR2 (T), O14 (V), O11 (V) |
| `RpcRules.dkgstatus_mineable_height` | RPC-36 | rpc/quorums.cpp:418-419 | rpc/quorums.cpp:429-431 | RP3b | DK1 (P), O11 (V) |
| `RpcRules.doccheck_depth` | RPC-15 | rpc/util.cpp:538-540 | rpc/util.cpp:551-572,976-1041 | RP0 | AR2 (T), O11 (V) |
| `RpcRules.getrawtransaction_verbosity` | RPC-23 | rpc/rawtransaction.cpp:286-288,386-390,428 | rpc/rawtransaction.cpp:373-385,466-505,555-563,599 | RP1b | AR2 (T), O14 (V) |
| `RpcRules.index_rpc_errors` | RPC-35 | rpc/index_util.cpp:15-96; rpc/node.cpp:426,498,657,1110-1150; rpc/blockchain.cpp:561-562 | rpc/node.cpp:416-436,504-509,597-602,684-687,839-880,1219-1229; rpc/blockchain.cpp:553-566 | RP1b, RP1a | IX2 (P), AR2 (T), O11 (V) |
| `RpcRules.listdiff_legacy_evo_addresses` | RPC-41 | rpc/evo.cpp:1724-1733; evo/dmnstate.cpp:116-145 | rpc/evo.cpp:1958-1973 | RP2a | PL1 (V), O14 (V) |
| `RpcRules.maxfeerate_cap` | RPC-18 | rpc/mempool.cpp:73-75,155-157 | rpc/util.cpp:80-85; rpc/mempool.cpp:76,157 | RP3a | O11 (V) |
| `RpcRules.networkhashps_strict` | RPC-28 | rpc/mining.cpp:62-80 | rpc/mining.cpp:63-86 | RP3a | MN1 (P), O11 (V) |
| `RpcRules.raw_tx_arg_order` | RPC-49 | rpc/rawtransaction_util.cpp:24-40 | rpc/rawtransaction_util.cpp:24-141 | RP1b | WL4 (P) |
| `RpcRules.rawmempool_order` | RPC-22 | rpc/mempool.cpp:351-358; txmempool.cpp:1308-1333 | rpc/mempool.cpp:333-341 | RP3a | O14 (V) |
| `RpcRules.rest_mempool_query` | RPC-06 | rest.cpp:626-660 (`MempoolToJSON(...,true)` :647) | rest.cpp:698-722 | ZQ1 | O14 (V) |
| `RpcRules.setmocktime_max` | RPC-16 | rpc/node.cpp:265-269 | rpc/node.cpp:274-278 | RP1a, ND1 | O11 (V) |
| `RpcRules.utxoupdatepsbt_txindex` | RPC-25 | rpc/rawtransaction.cpp:1659-1745 | rpc/rawtransaction.cpp:274-336,1801-1842 | RP1b | WL4 (P), O14 (V) |
| `RpcRules.verifychain_strict` | P2P-25 | validation.cpp:4646 (returns bool), rpc/blockchain.cpp:1432, node/chainstate.cpp:280-285 | validation.cpp:5030 (VerifyDBResult), rpc/blockchain.cpp:1423, 1440, node/chainstate.cpp:436-454, init.cpp:2085 … | RP1a, ND1 | CH5 (P), O11 (V) |
| `CliRules.netinfo_transport_column` | RPC-14 | bitcoin-cli.cpp:556 | bitcoin-cli.cpp:554 | AR3 | — |

## 7. Testing and the row check

- **Golden statics.** Each crate's `release` module has a unit test that renders both statics (`{:#?}`) and compares them with a checked-in snapshot. Any change to a profile value then shows up as a reviewed diff. CS0 adds the pattern with `UngatedRules`.
- **Both variants per consumer.** A task that reads a field tests each value: a unit test with each static where the logic is local (CS3's clock, MP1a's size bound, P5's tracker), otherwise a functional test under `-releaseprofile`. A task brief lists its fields from §10.
- **Row check.** `contrib/node/pin-delta/check-profile-rows.py` is a follow-up; the first task that lands a runtime group writes it, and the merge gate runs it. It reads the `profile` rows and homes of `O17-pin-delta.md` §5 and the `/// O17 <ID>` doc comments of every `release.rs`. It fails on a row without a field, a field without a row, or a field whose home disagrees with the report. Rows whose home is `RpcSurface` or `ArgsTable` are checked against AR2's and AR1's generated tables instead.
- **Release-ref grep** (§2 rule 2): `check-release-refs.sh`.
- **Verification layers per profile** (A1 §4):
  - L4/O11: each tag's functional tests against its profile (`node/func-compat-v24.0` default, `-v23.1` with `-releaseprofile=23.1`).
  - L6/O13: probes against v23.1.8 dashd and v24 dashd, one set per profile (+E).
  - L3/O10: the mutation operators for `UngatedRules` rows run per profile, against the live v23.1.8 reference (GBT proposal) and the v24 oracle.
  - O14 DIFF-RPC: per pin, with the same `-deprecatedrpc` set on both sides.
  - O12: mixed networks with v23.1.8 binaries on regtest under `-releaseprofile=23.1`.

## 8. Declared deviations (`v24-all`)

The 22 `v24-all` rows of report §5 are implemented once (the v24 behaviour) on every network. DEC-37 approved them as declared mainnet deviations after moving the 42 rows the GPT r2 review disputed to `profile`. On mainnet each is a known difference from v23.1.8, allowed by one of the report's §2 policies, and each states **why it is unreachable or unobservable** in report §5.11:
- **H**, narrow hardening: only malformed input, an abuse quota or a quorum-signed inconsistency reaches it, and the row states that scope exactly;
- **B**, race or crash fix: v24's outcome is one v23 can also produce, or v23's branch is a non-terminating loop, an unfulfilled future or an unloadable file;
- **U**, unreachable under the mainnet profile: regtest/devnet-only params or a regtest-only RPC guard;
- **R**, bounded reject-path proof (EVO-18 only);
- **I**, operator-only or unspecified: disk faults, PID files, `-loadblock` scheduling, a startup rescan that finds nothing, randomised selection.

Policy A (architecture) is retired: storage freedom (DEC-11) does not cover supported configurations or documented query results, so its rows are `profile` (`IndexRules`).

Every row is assigned to exactly one verification list, and that task is added to the row's consumers:
- **O10**: allowed L3 differences;
- **O13b/O13c**: known fingerprint probes;
- **O14**: `compat/ALLOWED-DIFFS.md`;
- **O11**: functional-test expectations;
- **RL2**: operator migration notes;
- **none**: no healthy peer, client or operator sees the difference.

The partition is generated from `decisions.json` (`deviation`). `merge.py` fails if a `v24-all` row lacks an entry or its argument, or if an entry names a row that is not `v24-all`.

| List | Rows (policy) |
|---|---|
| O10 | EVO-18 (R) |
| O13b | CONS-26 (H), SIG-11 (H) |
| O13c | DKG-04 (H) |
| O14 | CONS-30 (H), RPC-07 (H), RPC-47 (H), WAL-32 (I), WAL-34 (I) |
| O11 | DKG-14 (U) |
| RL2 | CONS-22 (I), WAL-44 (I) |
| none | MPIX-03 (U), P2P-30 (I), RPC-08 (B), RPC-09 (B), RPC-17 (U), SIG-20 (B), WAL-23 (B), WAL-25 (H), WAL-30 (I), WAL-45 (B) |

**Fault fallbacks inside `profile` rows.** Two `profile` rows keep one common sub-behaviour where GPT r2 advised against reproducing v23: P2P-17 disconnects on a genuine block-read fault under both releases (v23 aborts), and DKG-05's v23 observer variant reproduces the external effects of v23's never-drained queues (requests, scores) without keeping the queues.

A `v24-all` row becomes `profile` only by a report amendment: new evidence that an honest or probing party can trigger it under the mainnet profile and that no policy covers it.

## 9. Re-pin

`Release` variants name release *lines*. Each line is pinned to one tag (`V24_0` = v24.0.0-rc.3 today).

1. **New tag in a pinned line** (e.g. rc.3 → 24.0.0 final; SW-V24 weekly watch, DEC-23):
   1. `inventory.py --old v24.0.0-rc.3 --new v24.0.0` (rust-dashcore `contrib/node/pin-delta/`);
   2. audit the packets with `AUDIT-BRIEF.md`, merge with `merge.py` and a new `decisions.json`, confirm zero uncited PRs with `--report`, and regenerate §6 and §10 with `spec_tables.py`;
   3. update the `V24_0` statics in place, regenerate AR1/AR2 tables from the new tag, and update `Identity.ua_version` and `client_version`.

   The old tag's behaviour is not kept: no network runs it.
2. **New line** (e.g. v24.1): add a `Release` variant, then give every group a full literal for it (§2 rule 4 makes the compiler list them). Use the PIN-DELTA of the previous line's tag → the new tag.
3. **A network moves line** (mainnet moves to v24): change `default_for(Mainnet)`, re-run the parity evidence of A1 §2.25 against the new pin, and re-take the P2P fingerprint baseline (O13). Keep `V23_1` for one release while regtest suites still use it, then delete it together with every field value only it used.
4. **A fix that lands in a later pin** of the same line becomes a row of that re-pin's audit, and so a field like any other. That is the mechanism PLAN §7.8 names for lockstep changes.

## 10. Per-task view

What each PLAN task does with profile fields. Task briefs quote their line; a task not listed here touches no profile field. Roles as in §6: **Reads** means the task's crate reads the field and implements both behaviours. **Provides / receives** means the task implements both variants behind a parameter that a reader passes in (or supplies data the reader needs), because its crate cannot depend on the field's home. **Tables / verifies** means a generated per-release table (AR1/AR2) or verification work.

| Task | Reads | Provides / receives a parameter | Tables / verifies |
|---|---|---|---|
| AM1 | `P2pRules.mnauth_inbound_cap` | `InitRules.mn_init` | — |
| AR1 | — | — | `SporkRules.signer_model`, `WalletRules.dust_protection`, `WalletRules.rescan_option`, `InitRules.help_before_config`, `InitRules.ignored_conf_fatal`, `InitRules.invalid_log_option_fatal`, `InitRules.log_scan_category`, `InitRules.settings_file`, `InitRules.sigcache_sizing` |
| AR2 | — | — | `UngatedRules.{au_stable_txid, special_tx_max_type}`, `WalletRules.default_kind`, `WalletRules.descriptor_reject_hybrid`, `WalletRules.hardened_marker`, `WalletRules.import_label_sanitize`, `WalletRules.protx_legacy_key_reencode`, `WalletRules.protx_presign_checks`, `WalletRules.psbt_verify_finalized`, `RpcRules.address_decoding`, `RpcRules.arg_type_check`, `RpcRules.bip9_stats_window`, `RpcRules.composite_args`, `RpcRules.deployment_json`, `RpcRules.doccheck_depth`, `RpcRules.getrawtransaction_verbosity`, `RpcRules.index_rpc_errors` |
| AR3 | `CliRules.netinfo_transport_column` | — | `InitRules.help_before_config` |
| AR4 | `WalletRules.tool_create_default` | — | — |
| BS4a | — | `UngatedRules.protx_payload_decode` | — |
| BS5 | — | `RpcRules.address_decoding` | — |
| BS6a | — | `UngatedRules.cbtx_v4_known`, `UngatedRules.protx_payload_decode` | — |
| BS6b | — | `UngatedRules.au_stable_txid` | — |
| BS7 | — | `P2pRules.msg_asset_unlock` | — |
| BS8 | — | `P2pRules.qrinfo_request`, `LlmqRules.qgetdata_decode` | — |
| BS9a | — | `LlmqRules.dkg_intake` | — |
| BS9b | — | `Identity.protocol_version` | — |
| BS10 | — | `P2pRules.msg_asset_unlock` | — |
| BS11a-t | — | `UngatedRules.bls_scheme_scope` | — |
| CH1 | `UngatedRules.time_source` | — | — |
| CH2 | — | `PolicyRules.blocksonly_max_mempool_mb` | — |
| CH3a | `UngatedRules.bls_scheme_scope`, `UngatedRules.is_gate.connect_block`, `UngatedRules.mandatory_script_flags` | — | — |
| CH3b | `ChainstateRules.persist_cl_conflict_flags` | `PolicyRules.disconnect_pool_accounting` | — |
| CH5 | `UngatedRules.bls_scheme_scope`, `UngatedRules.old_budget_check` | `InitRules.verifydb_require_full`, `RpcRules.verifychain_strict` | — |
| CH6a | — | `IndexRules.dash_index_model` | — |
| CH6b | — | `IndexRules.dash_index_model` | — |
| CH7a | `UngatedRules.bls_scheme_scope`, `UngatedRules.old_budget_check`, `UngatedRules.superblocks_gate` | — | — |
| CH7b | `UngatedRules.is_gate.connect_block`, `UngatedRules.qc_dup_prepass`, `UngatedRules.quorum_root_cache` | — | — |
| CH7c | `UngatedRules.quorum_root_cache` | — | — |
| CH8 | `IndexRules.dash_index_model` | `WalletRules.importwallet_pruned` | — |
| CJ0 | — | `Identity.protocol_version` | — |
| CJ1 | `Identity.protocol_version`, `CoinJoinRules.dstx_check_order`, `CoinJoinRules.server_dsa_check_order`, `CoinJoinRules.server_entry_admission`, `CoinJoinRules.server_reject_duplicate_collateral`, `CoinJoinRules.server_reject_empty_entry` | — | — |
| CS0 | `UngatedRules.au_stable_txid` | — | — |
| CS3 | `UngatedRules.au_stable_txid`, `UngatedRules.cbtx_v4_known`, `UngatedRules.mandatory_script_flags`, `UngatedRules.protx_check_order`, `UngatedRules.reject_strings`, `UngatedRules.special_tx_max_type`, `UngatedRules.time_source` | — | — |
| CS4 | — | `LlmqRules.ehf_sign_window`, `RpcRules.bip9_stats_window` | — |
| CS6 | `UngatedRules.mandatory_script_flags` | `InitRules.sigcache_sizing` | — |
| DK1 | `Identity.min_mn_proto_version`, `P2pRules.unsolicited_scoring`, `LlmqRules.dkg_bad_connection`, `LlmqRules.dkg_intake`, `LlmqRules.dkg_status_phase`, `LlmqRules.observer_dkg_push` | `InitRules.initial_tip_fanout`, `RpcRules.dkgstatus_mineable_height` | — |
| EV1 | `UngatedRules.au_window_failure_class`, `UngatedRules.protx_check_order`, `UngatedRules.protx_payload_decode`, `UngatedRules.proupserv_type_in_special_tx`, `UngatedRules.reject_strings`, `UngatedRules.special_tx_max_type` | — | — |
| EV3 | `UngatedRules.asset_unlock_index_membership`, `UngatedRules.old_budget_check`, `UngatedRules.reject_strings`, `UngatedRules.superblocks_gate` | — | — |
| EV4 | `UngatedRules.cbtx_v4_known`, `UngatedRules.quorum_root_cache` | — | — |
| GS1 | `GovernanceRules.record_rejected_votes` | — | — |
| GV1b | `UngatedRules.old_budget_check`, `P2pRules.object_download`, `P2pRules.request_gov_invs_when_disabled`, `GovernanceRules.invalid_vote_cache`, `GovernanceRules.orphan_vote_requires_valid_sig`, `GovernanceRules.record_rejected_votes` | `SyncRules.gov_vote_sync_peer_bound`, `InitRules.governance_requires_txindex` | — |
| IM1a | — | `PolicyRules.fee_estimates`, `IndexRules.dash_index_model`, `InitRules.settings_file` | — |
| IM1b | — | `GovernanceRules.invalid_vote_cache` | — |
| IM2 | `WalletRules.extra_db_records`, `WalletRules.hardened_marker` | — | — |
| IX1 | `IndexRules.rewind_on_disconnect` | `WalletRules.fast_rescan` | — |
| IX2 | `IndexRules.dash_index_model` | `RpcRules.index_rpc_errors` | — |
| LK1a | `P2pRules.unsolicited_scoring`, `LockRules.coinbase_cl_processing`, `LockRules.seen_clsig_cache` | — | — |
| LK1b | `UngatedRules.is_gate.connect_block`, `P2pRules.object_download`, `LockRules.islock_batch_missing_quorum` | — | — |
| LK1c | `UngatedRules.is_gate.{miner, cl_signer}` | — | — |
| LQ2 | `UngatedRules.qc_dup_prepass`, `UngatedRules.quorum_root_cache` | — | — |
| LQ3 | — | `P2pRules.mnlist_data_unavailable_score`, `P2pRules.qrinfo_request` | — |
| LQ4a | — | `Identity.min_mn_proto_version`, `LlmqRules.dkg_bad_connection` | — |
| LQ5 | — | `LlmqRules.sigshare_recovery_skip_unreachable` | — |
| MN0 | `UngatedRules.time_source` | — | — |
| MN1 | `UngatedRules.bls_scheme_scope`, `UngatedRules.is_gate.{miner, cl_signer}`, `UngatedRules.mandatory_script_flags`, `UngatedRules.proupserv_type_in_special_tx`, `UngatedRules.qc_dup_prepass`, `UngatedRules.superblocks_gate`, `UngatedRules.time_source`, `PolicyRules.protx_collateral_vs_pending`, `MiningRules.package_special_checks` | `RpcRules.networkhashps_strict` | — |
| MP1a | `UngatedRules.au_stable_txid`, `UngatedRules.au_window_failure_class`, `UngatedRules.mandatory_script_flags`, `UngatedRules.proupserv_type_in_special_tx`, `UngatedRules.special_tx_max_type`, `PolicyRules.asset_unlock_mempool`, `PolicyRules.min_relay_fee_checked_first`, `PolicyRules.min_standard_tx_size`, `PolicyRules.protx_collateral_vs_pending`, `PolicyRules.protx_cross_scheme_dup`, `PolicyRules.proupserv_operator_key_binding` | `P2pRules.msg_asset_unlock` | — |
| MP1b | `PolicyRules.asset_unlock_mempool`, `PolicyRules.blocksonly_max_mempool_mb`, `PolicyRules.disconnect_pool_accounting`, `PolicyRules.orphan_processing`, `PolicyRules.proupserv_operator_key_binding` | — | — |
| MP2 | `PolicyRules.fee_estimates` | — | — |
| ND1 | `UngatedRules.time_source`, `PolicyRules.blocksonly_max_mempool_mb`, `SporkRules.signer_model`, `IndexRules.dash_index_model`, `WalletRules.rescan_option`, `InitRules.abort_exit_failure`, `InitRules.bind_failure_fatal`, `InitRules.governance_requires_txindex`, `InitRules.help_before_config`, `InitRules.ignored_conf_fatal`, `InitRules.initial_tip_fanout`, `InitRules.invalid_log_option_fatal`, `InitRules.mn_init`, `InitRules.onion_bind`, `InitRules.sigcache_sizing`, `InitRules.verifydb_require_full`, `RpcRules.setmocktime_max`, `RpcRules.verifychain_strict` | — | — |
| O3 | — | — | `LlmqRules.dkg_intake` |
| O10 | — | — | `UngatedRules.asset_unlock_index_membership`, `UngatedRules.au_stable_txid`, `UngatedRules.au_window_failure_class`, `UngatedRules.bls_scheme_scope`, `UngatedRules.cbtx_v4_known`, `UngatedRules.mandatory_script_flags`, `UngatedRules.old_budget_check`, `UngatedRules.protx_check_order`, `UngatedRules.protx_payload_decode`, `UngatedRules.qc_dup_prepass`, `UngatedRules.quorum_root_cache`, `UngatedRules.reject_strings`, `UngatedRules.special_tx_max_type`, `UngatedRules.time_source` |
| O11 | — | — | `UngatedRules.superblocks_gate`, `P2pRules.qrinfo_request`, `SporkRules.defs`, `SporkRules.signer_model`, `LlmqRules.dkg_status_phase`, `IndexRules.dash_index_model`, `WalletRules.fast_rescan`, `WalletRules.protx_presign_checks`, `WalletRules.resubmission`, `InitRules.abort_exit_failure`, `InitRules.invalid_log_option_fatal`, `InitRules.settings_file`, `InitRules.sigcache_sizing`, `InitRules.verifydb_require_full`, `RpcRules.address_decoding`, `RpcRules.arg_type_check`, `RpcRules.composite_args`, `RpcRules.deployment_json`, `RpcRules.dkgstatus_mineable_height`, `RpcRules.doccheck_depth`, `RpcRules.index_rpc_errors`, `RpcRules.maxfeerate_cap`, `RpcRules.networkhashps_strict`, `RpcRules.setmocktime_max`, `RpcRules.verifychain_strict` |
| O12b | — | — | `Identity.min_mn_proto_version`, `LlmqRules.dkg_bad_connection` |
| O13a | — | — | `Identity.{protocol_version, ua_version, client_version}` |
| O13b | — | — | `UngatedRules.au_window_failure_class`, `UngatedRules.mandatory_script_flags`, `P2pRules.block_read_vs_prune`, `P2pRules.mnlist_data_unavailable_score`, `P2pRules.msg_asset_unlock`, `P2pRules.notfound_limit`, `P2pRules.object_download`, `P2pRules.qrinfo_request`, `P2pRules.tx_inv_before_verack`, `PolicyRules.orphan_processing`, `SporkRules.getsporks_dedup`, `SyncRules.gov_vote_sync_peer_bound`, `LlmqRules.non_mn_recsig_relay`, `LlmqRules.observer_dkg_push`, `LockRules.islock_batch_missing_quorum`, `LockRules.seen_clsig_cache`, `CoinJoinRules.dstx_check_order` |
| O13c | — | — | `Identity.min_mn_proto_version`, `LlmqRules.dkg_bad_connection`, `LlmqRules.dkg_intake`, `LlmqRules.qgetdata_decode`, `LlmqRules.qgetdata_tracking`, `LlmqRules.qgetdata_unregistered_type_score`, `CoinJoinRules.server_dsa_check_order`, `CoinJoinRules.server_entry_admission`, `CoinJoinRules.server_reject_duplicate_collateral`, `CoinJoinRules.server_reject_empty_entry`, `InitRules.initial_tip_fanout` |
| O14 | — | — | `UngatedRules.superblocks_gate`, `ChainstateRules.persist_cl_conflict_flags`, `P2pRules.qrinfo_request`, `MiningRules.package_special_checks`, `SporkRules.defs`, `LlmqRules.dkg_status_phase`, `GovernanceRules.record_rejected_votes`, `IndexRules.rewind_on_disconnect`, `WalletRules.coinjoin_pending_locks`, `WalletRules.coinjoin_rounds_memo_unknown`, `WalletRules.coinjoin_unconfirmable_filter`, `WalletRules.default_kind`, `WalletRules.hardened_marker`, `WalletRules.psbt_sign_with_mixing_unlock`, `WalletRules.reconcile_freed_inputs`, `RpcRules.address_decoding`, `RpcRules.arg_type_check`, `RpcRules.batch_whitelist`, `RpcRules.bip9_stats_window`, `RpcRules.deployment_json`, `RpcRules.getrawtransaction_verbosity`, `RpcRules.listdiff_legacy_evo_addresses`, `RpcRules.rawmempool_order`, `RpcRules.rest_mempool_query`, `RpcRules.utxoupdatepsbt_txindex` |
| O15 | — | — | `UngatedRules.quorum_root_cache`, `PolicyRules.disconnect_pool_accounting` |
| P2a | `Identity.min_mn_proto_version`, `Identity.protocol_version`, `Identity.{protocol_version, ua_version, client_version}` | `LlmqRules.dkg_bad_connection`, `InitRules.bind_failure_fatal` | — |
| P2b | `UngatedRules.time_source`, `Identity.{protocol_version, ua_version, client_version}` | — | — |
| P3 | `UngatedRules.time_source` | — | — |
| P4 | `UngatedRules.au_window_failure_class`, `UngatedRules.mandatory_script_flags`, `UngatedRules.proupserv_type_in_special_tx`, `P2pRules.block_read_vs_prune`, `P2pRules.notfound_limit` | — | — |
| P5 | `UngatedRules.au_window_failure_class`, `UngatedRules.mandatory_script_flags`, `P2pRules.msg_asset_unlock`, `P2pRules.notfound_limit`, `P2pRules.object_download`, `P2pRules.tx_inv_before_verack`, `P2pRules.unsolicited_scoring`, `PolicyRules.min_standard_tx_size`, `PolicyRules.orphan_processing` | `WalletRules.resubmission` | — |
| P5a | `P2pRules.object_download`, `P2pRules.unsolicited_scoring` | `SporkRules.getsporks_dedup`, `LlmqRules.non_mn_recsig_relay` | — |
| P6a | `UngatedRules.au_stable_txid`, `UngatedRules.reject_strings`, `Identity.protocol_version`, `P2pRules.msg_asset_unlock`, `P2pRules.object_download`, `P2pRules.request_gov_invs_when_disabled` | `GovernanceRules.invalid_vote_cache`, `GovernanceRules.orphan_vote_requires_valid_sig`, `CoinJoinRules.dstx_check_order` | — |
| P6b | `P2pRules.mnlist_data_unavailable_score`, `P2pRules.qrinfo_request` | — | — |
| P7 | `P2pRules.mnauth_inbound_cap` | `InitRules.initial_tip_fanout` | — |
| P8 | — | `InitRules.bind_failure_fatal`, `InitRules.onion_bind` | — |
| PL1 | — | — | `PolicyRules.asset_unlock_mempool`, `LlmqRules.platform_recsig_per_msghash`, `WalletRules.platform_addresses`, `RpcRules.batch_whitelist`, `RpcRules.composite_args`, `RpcRules.listdiff_legacy_evo_addresses` |
| QM1b | `LlmqRules.observer_dkg_push`, `LlmqRules.qgetdata_decode`, `LlmqRules.qgetdata_tracking`, `LlmqRules.qgetdata_unregistered_type_score` | `InitRules.initial_tip_fanout` | — |
| RL1 | — | — | `InitRules.abort_exit_failure` |
| RL2 | — | — | `IndexRules.dash_index_model` |
| RP0 | `RpcRules.arg_type_check`, `RpcRules.batch_whitelist`, `RpcRules.composite_args`, `RpcRules.doccheck_depth` | — | — |
| RP1a | `ChainstateRules.persist_cl_conflict_flags`, `Identity.{protocol_version, ua_version, client_version}`, `P2pRules.mnauth_inbound_cap`, `WalletRules.hardened_marker`, `RpcRules.address_decoding`, `RpcRules.bip9_stats_window`, `RpcRules.deployment_json`, `RpcRules.index_rpc_errors`, `RpcRules.setmocktime_max`, `RpcRules.verifychain_strict` | `InitRules.log_scan_category`, `InitRules.verifydb_require_full` | — |
| RP1b | `UngatedRules.au_stable_txid`, `UngatedRules.{au_stable_txid, special_tx_max_type}`, `RpcRules.getrawtransaction_verbosity`, `RpcRules.index_rpc_errors`, `RpcRules.raw_tx_arg_order`, `RpcRules.utxoupdatepsbt_txindex` | — | — |
| RP2a | `UngatedRules.{au_stable_txid, special_tx_max_type}`, `RpcRules.composite_args`, `RpcRules.listdiff_legacy_evo_addresses` | — | — |
| RP2b | `SporkRules.defs`, `GovernanceRules.record_rejected_votes`, `WalletRules.coinjoin_pending_locks`, `RpcRules.composite_args` | — | — |
| RP3a | `UngatedRules.mandatory_script_flags`, `UngatedRules.qc_dup_prepass`, `UngatedRules.superblocks_gate`, `UngatedRules.time_source`, `PolicyRules.asset_unlock_mempool`, `PolicyRules.min_relay_fee_checked_first`, `PolicyRules.min_standard_tx_size`, `PolicyRules.protx_cross_scheme_dup`, `RpcRules.maxfeerate_cap`, `RpcRules.networkhashps_strict`, `RpcRules.rawmempool_order` | — | — |
| RP3b | `UngatedRules.asset_unlock_index_membership`, `P2pRules.qrinfo_request`, `LlmqRules.platform_recsig_per_msghash`, `RpcRules.composite_args`, `RpcRules.dkgstatus_mineable_height` | — | — |
| SG1a | `LlmqRules.ehf_sign_window`, `LlmqRules.non_mn_recsig_relay`, `LlmqRules.platform_recsig_per_msghash` | — | — |
| SG1b | `LlmqRules.platform_recsig_per_msghash`, `LlmqRules.sigshare_recovery_skip_unreachable` | — | — |
| SP0 | `UngatedRules.is_gate.connect_block`, `UngatedRules.is_gate.{miner, cl_signer}`, `UngatedRules.superblocks_gate`, `SporkRules.defs`, `SporkRules.getsporks_dedup`, `SporkRules.signer_model` | — | — |
| SP1 | `P2pRules.object_download`, `SyncRules.gov_vote_sync_peer_bound` | — | — |
| WL1 | — | `WalletRules.empty_hd_chain_counts_as_one` | — |
| WL2a | `WalletRules.coinjoin_pending_locks`, `WalletRules.extra_db_records`, `WalletRules.fast_rescan`, `WalletRules.reconcile_freed_inputs`, `WalletRules.rescan_option`, `WalletRules.rescan_requires_unlock`, `WalletRules.resubmission` | — | — |
| WL2b | `WalletRules.coinjoin_unconfirmable_filter`, `WalletRules.default_kind`, `WalletRules.empty_hd_chain_counts_as_one`, `WalletRules.hardened_marker`, `WalletRules.import_label_sanitize`, `WalletRules.importwallet_pruned`, `WalletRules.label_all_descriptor_scripts`, `WalletRules.platform_addresses`, `WalletRules.platform_recipients`, `WalletRules.psbt_sign_with_mixing_unlock`, `WalletRules.reconcile_freed_inputs`, `WalletRules.rescan_requires_unlock` | `RpcRules.address_decoding`, `RpcRules.arg_type_check` | — |
| WL3 | `WalletRules.empty_hd_chain_counts_as_one`, `WalletRules.extra_db_records`, `WalletRules.hardened_marker` | — | — |
| WL4 | `WalletRules.descriptor_reject_hybrid`, `WalletRules.hardened_marker`, `WalletRules.psbt_sign_with_mixing_unlock`, `WalletRules.psbt_verify_finalized` | `RpcRules.raw_tx_arg_order`, `RpcRules.utxoupdatepsbt_txindex` | — |
| WL5 | `UngatedRules.{au_stable_txid, special_tx_max_type}`, `WalletRules.platform_recipients`, `WalletRules.protx_legacy_key_reencode`, `WalletRules.protx_presign_checks`, `WalletRules.reconcile_freed_inputs` | `RpcRules.composite_args` | — |
| WL6 | `WalletRules.coinjoin_pending_locks`, `WalletRules.coinjoin_rounds_memo_unknown`, `WalletRules.coinjoin_unconfirmable_filter`, `WalletRules.extra_db_records`, `WalletRules.psbt_sign_with_mixing_unlock` | `Identity.protocol_version` | — |
| WL7 | `WalletRules.dust_protection`, `WalletRules.importwallet_pruned`, `WalletRules.rescan_option` | — | — |
| WL8 | `WalletRules.coinjoin_pending_locks`, `WalletRules.coinjoin_rounds_memo_unknown`, `WalletRules.coinjoin_unconfirmable_filter`, `WalletRules.fast_rescan`, `WalletRules.psbt_sign_with_mixing_unlock`, `WalletRules.reconcile_freed_inputs`, `WalletRules.resubmission` | — | — |
| ZQ1 | `PolicyRules.proupserv_operator_key_binding`, `RpcRules.rest_mempool_query` | — | — |

Every RPC task (RP0–RP3b, WL2b, WL5, WL7, ZQ1) also reads `dash_rpc_spec::surface(Release)`, and every option-parsing task (AR1, ND1, WL2a) reads `dash_args::options(Release)`.

## 11. Open points

1. **DEC-02 is amended** (DEC-34a): `WalletRules.default_kind` is `Legacy` for `V23_1` and `Descriptor` for `V24_0`.
2. **Mainnet sporks are hard-coded in both releases** (every spork 0 = active, SPORK_21 = 1; v23 `spork.cpp:265-275`, v24 `:171-181`). SP0 implements that hard-coding under both profiles, so `is_gate`'s `SyncedAndSpork3` means "blockchain-synced" on mainnet. The SPORK_3/SPORK_9 values matter only off mainnet, where the v23.1 functional tests set them. Pinning a mainnet SPORK_3 value of "off" would make a `V23_1` node accept blocks v23.1.8 rejects with `conflict-tx-lock`.
3. **CS0 alignment** (§3), forwarded by the manager because CS0 is being implemented: rename `header_time_source` to `time_source` (and the `ConsensusCtx.header_check_time` comment), and `V23_1_8` to `V23_1` (the `Release` variant and the `UngatedRules` const; CS1's `ScriptPolicy`/`ScriptFlags` names follow, and their mandatory flags come from `UngatedRules.mandatory_script_flags`); add `Hash`, `ALL`, `name()` and `parse()` to `Release`; `default_for` takes `dash_params::Network`; drop the row-3 comment; keep `Effect::BlsSchemeLeak`. Owning tasks add the other `UngatedRules` fields as they land (§4.1 fixes their names now).
4. **State-carrying rules.** `quorum_root_cache` and `bls_scheme_scope` select the semantics of state that `dashd-chainstate` owns (CH7c, CH7a/CH5). Their tests need a fresh and a warmed reference process (O10, O15), because v23.1.8's result depends on what the process saw before.
5. **Low-value `profile` rows** (report §2) could be downgraded to `v24-all` with explicit sign-off; DEC-37's parity rule argues against it.
6. **AR1/AR2 per-release generation** is a new input requirement: both generators read the pinned tag's source, not develop.
