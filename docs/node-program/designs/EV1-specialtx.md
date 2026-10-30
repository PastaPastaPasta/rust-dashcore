# EV1: special-transaction contextual checks (`dash-dml::specialtx`)

> PLAN §7 handling applies to this design; see ~/private/embargo.

| | |
|---|---|
| Status | D design, Opus leg (PLAN §4.4 row EV1). Next: Sol design review, then Opus high |
| Date | 2026-10-13 |
| Author | EV1 designer (Opus 5.5) |
| Task | `CheckSpecialTxInner` and every per-type check for the 12 special types (types 10–12 are v24-only), NetInfo `Validate()`, the shared-collateral covenant (template outputs and spends) |
| Profile | v24 only (DEC-82). No release profile is modelled; the PLAN row's "per profile" and `i:BS6c` are stale (§8 A-6) |
| Binding inputs | A1 §1.4.1, §2.5, §2.10, Appendix A (`EvoCtx`); EV addendum §1.5 (EV1 lives in `dash-dml::specialtx`), §2.6 (attempt facade); EV2 D2 (§1.3, §2.7, §3.2, §3.2.6); CS0 (`ValidationError`, `CoinsView`, `ConsensusCtx`); PRD FR-CON-25, FR-CON-26, FR-EVO-06, NFR-CEQ-02/-04/-07/-12; O17 EVO-14, EVO-16, EVO-17, MPIX-16; DEC-71, DEC-79, DEC-82, DEC-98 Q-1, DEC-184, DEC-186 |
| Core verified at | `v24.0.0-rc.3` in `~/workspace/dash` (`git show v24.0.0-rc.3:src/…`). Every citation below is at that pin. Paths are under `src/`; `specialtxman.cpp` and friends are under `evo/`, `commitment.cpp` under `llmq/` |
| rust-dashcore verified at | `node/integration` f4046b5f5; base-sdk pin dbe245f7 |

---

## 0. Summary

1. **One pure module, one entry point per Core function.** `dash-dml::specialtx` ports `CheckSpecialTxInner` (`specialtxman.cpp:217-274`) and every function it calls, in Core's order, with Core's reject strings and classes (CS0 `TxValidationResult::{Consensus, BadSpecial}`). It also ports the two covenant functions (`:1815-1849`) and NetInfo `Validate()` (`netinfo.cpp:351-381, 588-638`). The callers (CH7a for blocks, MP1a for the mempool) place them as Core does.
2. **Explicit context, no globals.** Every input Core reads from a manager or `Params()` comes through `SpecialTxCtx` (§2.3): the pprev `MnList`, a `CoinsView`, a block-index lookup, the deployments and `SpecialTxRules`, a global-scheme `BlsCtx` that is independent of the check height, a quorum seam, the optional asset-unlock index set, a signature verifier and `check_sigs`.
3. **The block and mempool paths differ only in what the caller binds.** Core's two overloads of `CheckMNHFTx` and `CheckAssetUnlockTx` differ only in `GetQuorum(type, hash)` versus `GetQuorum(type, hash, chain)`, plus the index set being `nullopt`. EV1 takes both through the `QuorumLookup` seam and `Option<&dyn UnlockIndexes>`.
4. **BLS verify is injected** (`SigVerifier`, §2.5). Until BS12's serial verify lands (DEC-79), the pure crate makes no pairing calls. A legacy-scheme verify the injected verifier cannot do is a fail-stop `Unavailable` fault, never a reject. ECDSA compact recovery stays in-crate.
5. **Trivial validity is ported, not borrowed.** base-sdk `Checkable` is context-free and is not Core's `IsTriviallyValid`/`Validate()`, so EV1 never calls it for consensus (D3).
6. **NetInfo is EV1's.** EV1 provides the real `NetInfoValidity` implementation that EV2a's codec takes (`dash-dml/src/codec.rs:48`). It also provides `AddEntry` and `migration_reject`, a read-only predicate for the NetInfo step of Core's state-version migration. EV1 evaluates it at the position where Core's type-2 and type-3 checks run their scratch migration (§3.16.1). EV2b builds its own conversion over `add_entry`, and the two are equality-tested (D24).
7. **Payloads are decoded once.** CH7a builds `BlockPayloads`; EV1 defines the per-tx decode-failure mapping (D6). On the mempool path EV1 decodes for itself.
8. **Verification:**
   - L0: Core's unit vectors, plus trivially-valid/invalid vectors under both BLS encodings.
   - L1: `diff_check_special_tx` through `oracle_check_special_tx` for types 1–6, 8 and 10–12.
   - Types 7 and 9 and the block-path variants join L1 at EV1-5 through `oracle_check_special_tx_block` (A-1, a merge prerequisite), with an L3 negative matrix against the pinned dashd as a second check.
   - A reject-string census (§3.13), register rows 21, 26 and 27, and six fuzz targets under DEC-184/186.
9. **Six slices** (§7), each with a checkpoint. The first slice (NetInfo and the census) needs neither BLS decode nor verify. Every slice that checks version-1 keys merges on BS11a's exact legacy decoder (D10).

---

## 1. Scope and non-goals

### 1.1 In scope

- `check_special_tx_inner`: the dispatcher (`specialtxman.cpp:217-274`), including the DIP3 gate (O17 EVO-17), the coinbase checks for type 5 (`:246-256`), the exception mapping (`:268-271`) and the unhandled-type reject (`:273`).
- `validated_payload::<T>`, Core's `GetValidatedPayload` (`:1097-1122`), and `ProTxVersion::GetMax` (`evo/providertx.h`).
- The per-type checks:
  - ProTx (types 1–4, 10–12): §3.3–§3.9, with ProDis via `CheckProDisTxForList`.
  - CbTx (type 5): `CheckCbTx` only (`cbtx.cpp:24-55`).
  - Quorum commitment (type 6): `CheckLLMQCommitment` (`commitment.cpp:240-310`), wrapping LQ2/SB8's `Verify` (D5).
  - MNHF (type 7): `CheckMNHFTxImpl` (`mnhftx.cpp:107-157`) and `MNHFTxPayload::IsTriviallyValid` (`:94-104`).
  - Asset lock and unlock (types 8 and 9): `CheckAssetLockTx` (`assetlocktx.cpp:29-89`), `CheckAssetUnlockTxImpl` (`:177-232`) and `VerifyAssetUnlockSig` (`:106-153`).
- Every `IsTriviallyValid` of the ProTx payloads (`providertx.cpp:265-589`), the payout and share list checks (`:25-164`), `CheckNetInfo` and `CheckProviderNetworkFields` (`:166-263`), `IsVersionChangeValid` (`specialtxman.cpp:1132-1146`), the signature helpers (`:1068-1095`) and `CheckInputsHash` (`providertx.h:560`).
- NetInfo `Validate()` for both kinds, `ValidateService`, `ValidateDomainPort`, the duplicate checks and `AddEntry` (`netinfo.cpp:69-638`).
- The hashes and strings that are signed:
  - `MakeSignString` (`providertx.cpp:375-395`);
  - `MakeSharedRegConsentHash` (`:335-373`);
  - `CProDisTx::MakeSignHash` (`:506-533`);
  - the MNHF and asset-unlock `SignHash` preimages.
- The covenant: `CheckSharedCollateralSpends` and `CheckSharedCollateralTemplateOutputs` (`specialtxman.cpp:1815-1849`), with the template script from `sharedcollateral.h`.
- O17 rows at v24:
  - EVO-16: the ProTx check order, which is §3 itself.
  - EVO-17: `bad-tx-type-dip3-inactive`.
  - MPIX-16: `TX_BAD_SPECIAL` for `bad-assetunlock-too-old-quorum` and `-too-late`.
  - EVO-14: under v24, a v3 ProTx always decodes; the BS6a layout is the only layout, and EV1 consumes nothing from BS6c.

### 1.2 Non-goals

| Not here | Owner |
|---|---|
| `ProcessSpecialTxsInBlock` orchestration (`specialtxman.cpp:797-999`): CbTx presence, the order with the credit pool, `qblockman.ProcessBlock`, the merkle roots, best-CL, MNHF processing, the BLS flag flip | CH7a (with EV2b, EV3, LQ, SB7) |
| `BuildNewListFromBlock`/`RebuildListFromBlock` and its in-block re-checks (`:309-794`) | EV2b |
| `set_state_version`, `apply_pro_up_serv` (`:51-137`) as functions | EV2b (D9); EV1 ports only their NetInfo step, read-only (§3.16.1) |
| `CheckCbTxBestChainlock` (`:139-215`), `CheckCreditPoolDiffForBlock` (`:1036-1066`), the CbTx merkle roots | CH7a / EV3 / SB7 |
| `RangesSet::contains` (CONS-10), credit-pool eras, `GetAssetUnlockFee` (`assetlocktx.cpp:256-270`) | EV3 |
| The MNHF in-block rules (`mnhftx.cpp:190-260`) | EV3 |
| `CFinalCommitment::Verify` and `VerifyNull` (`commitment.cpp:113-238`) | LQ2 / SB8 |
| `IsStandardSpecialTx` (`specialtxman.cpp:1851-1878`), the mempool driver `CheckSpecialTxForMempool` (`validation.cpp:802-825`), `protx-dup` | MP1a |
| Where the covenant is called and how it is gated (`validation.cpp:1010-1018, 2700-2716`; `specialtxman.cpp:853-862`; `miner.cpp:410`) | CH7a, CH3a, MP1a, the miner (D7) |
| Wallet construction of any special tx | WL5 |

---

## 2. Data model and public API

### 2.1 Module layout

```
dash-dml/src/specialtx/
  mod.rs        check_special_tx_inner, SpecialTxRules, SpecialTxCtx, re-exports
  payload.rs    validated_payload::<T>, version_max, BlockPayloads + PayloadSlot (D6)
  trivial.rs    IsTriviallyValid for the ProTx payloads, payout/share list checks
  network.rs    CheckProviderNetworkFields, CheckNetInfo, IsNetInfoTriviallyValid
  provider.rs   ProReg (1), ProUpServ (2), ProUpReg (3), ProUpRev (4)
  shared.rs     ProDis (10, +ForList), ProUpShare (11), ProUpSharedReg (12), ProRataFloor
  covenant.rs   shared_collateral_spends, shared_collateral_template_outputs, TEMPLATE_SCRIPT
  cbtx.rs       check_cbtx (type 5, CheckCbTx only)
  qc.rs         check_llmq_commitment (type 6) over the LQ2 Verify seam
  mnhf.rs       check_mnhf_tx (type 7), is_valid_mn_activation
  asset.rs      check_asset_lock_tx (8), check_asset_unlock_tx (9), verify_asset_unlock_sig
  sig.rs        ECDSA VerifyHash / VerifyHashCanonical / VerifyMessage, SigVerifier, hash preimages
dash-dml/src/netinfo/
  mod.rs        CoreNetInfoValidity: NetInfoValidity, validate_mn, validate_ext
  entry.rs      ValidateService (both kinds), ValidateDomainPort, IsBadPort, TLD rules
  add.rs        add_entry (MnNetInfo/ExtNetInfo::AddEntry, ProcessCandidate)
  migration.rs  migration_reject (§3.16.1), over a private scratch conversion
```

NetInfo is its own module because EV2a's codec, EV2b's migration and EV1 all use it.

### 2.2 Results

All checks return `Result<(), TxValidationError>` (CS0). Here `TxValidationError = ValidationError<TxValidationResult>`, `reason: Cow<'static, str>` and no DoS score. Each Core reason is a `&'static str` constant in `specialtx::reasons`. The two migration rejects that copy another state's reason (`specialtxman.cpp:1367, 1476`) carry the `'static` string from the migration error, so nothing is allocated.

Faults that are not rejects are a separate type:

```rust
pub enum SpecialTxFault {
    /// The verifier cannot evaluate this scheme (D2). Fail-stop: the caller aborts the attempt.
    VerifyUnavailable,
    /// A seam failed (store, quorum facade). Propagated, never mapped to a reject.
    /// Never carries `SeamError::CoreThrow`, which EV1 maps to a reject (below).
    Seam(SeamError),
}
pub type CheckResult = Result<Result<(), TxValidationError>, SpecialTxFault>;

pub enum SeamError {
    /// The seam's Core counterpart throws here deterministically (D25). EV1 maps it to
    /// TX_CONSENSUS `failed-check-special-tx`.
    CoreThrow,
    /// `scan_quorums` would need more than `MAX_QUORUM_SCAN` hashes (D14). Fault.
    ScanCapacity,
    /// Local storage or I/O. Fault.
    Storage(StoreError),
}
```

Core's `catch (const std::exception&) → TX_CONSENSUS "failed-check-special-tx"` (`specialtxman.cpp:268-271`) is reproduced for every exception a ported function can raise in Core (for example `arith_uint256` division by zero, `std::vector::at`). EV1-1 lists them, with one test each, as `Throw` sites that return that reject. The same reject is returned when a seam returns `SeamError::CoreThrow`: each seam implementer returns it exactly where the Core code behind the seam throws deterministically (LQ2/SB8: `CFinalCommitment::Verify` → `GetAllQuorumMembers`; LQ1a: `GetQuorum` / `BuildQuorumFromCommitment` and `ScanQuorums`), and lists those sites with one test each (D25). `ScanCapacity` and `Storage` are never turned into `failed-check-special-tx`; they are `SpecialTxFault::Seam`.

### 2.3 Context

```rust
/// Core GetSpecialTxRules(pindex) (validation.cpp:6797): V24 and EVO_SHARES, active after `pindex`.
/// Block path: `pindex` = pprev (the same block as `cons.prev`). Mempool path: `pindex` = the tip,
/// also when `cons.prev` is an earlier block (expired-AU retry, validation.cpp:1103, 822, 825;
/// AU replacement, :1400). The caller binds it; EV1 never derives it from `cons.prev` (D21).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpecialTxRules { pub v24: bool, pub evo_shares: bool }

pub struct SpecialTxCtx<'a> {
    /// `cons.prev` is Core's pindexPrev (the check height minus one). `cons.bls` is the global
    /// scheme the verifier uses (D1-c), which is not derived from `cons.prev` on the mempool path.
    pub evo: EvoCtx<'a>,                       // cons + check_sigs
    pub rules: SpecialTxRules,
    pub list: &'a MnList,                       // GetListForBlock(pindexPrev), resolved by the caller
    pub coins: &'a dyn CoinsView,               // block: the view Core passes to ProcessSpecialTxsInBlock;
                                                // mempool: CoinsTip, never the mempool input cache (D13)
    pub index: &'a dyn BlockLookup,             // LookupBlockIndex: any known header
    pub quorums: &'a mut dyn QuorumLookup,      // GetQuorum / ScanQuorums (D1-f)
    pub unlock_indexes: Option<&'a dyn UnlockIndexes>, // block path: credit pool at pprev; mempool: None
    pub verifier: &'a dyn SigVerifier,          // BLS VerifyInsecure (D2)
    pub effects: &'a mut dyn EffectSink,        // quorum-facade effects, in Core's call order
}

pub trait BlockLookup {
    /// Core `LookupBlockIndex`: any header in the block index, whatever its chain.
    fn lookup(&self, h: &BlockHash) -> Option<(BlockId, u32)>;
}
pub trait UnlockIndexes { fn contains(&self, index: u64) -> bool; } // Core CRangesSet::Contains
pub trait QuorumLookup {
    fn get_quorum(&mut self, t: LlmqType, h: &QuorumHash, fx: &mut dyn EffectSink)
        -> Result<Option<QuorumKey>, SeamError>;
    /// Hashes of `ScanQuorums(t, at, n)`, newest first, into a caller-owned fixed buffer.
    /// Every hash Core returns is written; if `n > MAX_QUORUM_SCAN` the call returns
    /// `SeamError::ScanCapacity` and writes nothing (never truncates; D14).
    fn scan_quorums(&mut self, t: LlmqType, at: BlockId, n: usize,
                    out: &mut ArrayVec<QuorumHash, MAX_QUORUM_SCAN>,
                    fx: &mut dyn EffectSink) -> Result<(), SeamError>;
}
pub struct QuorumKey { pub quorum_hash: QuorumHash, pub public_key: BlsPublicKeyRef }
/// max over every LLMQ in the params table of `signingActiveQuorumCount` + 1. 33 at the pin
/// (`llmq_60_75`, `llmq/params.h:382-394`, selectable as the Platform type by `-llmqplatform`,
/// `chainparams.cpp:1308-1327`). Computed by a `const fn` over the table, not written as a literal.
pub const MAX_QUORUM_SCAN: usize = llmq::max_signing_active_quorum_count() + 1;
```

`dash-chain`'s `IndexAccess::lookup` + `height` implement `BlockLookup`. The ancestry checks that Core makes on the looked-up block (`commitment.cpp:286-289`, `mnhftx.cpp:133-136`) go through `cons.chain: ChainAncestry` (`ancestor(prev, height) == id`). `ancestor(prev, h)` with `h` above `prev`'s height returns `None`, which compares as not equal; it never panics (Core `GetAncestor` → `nullptr`) (D31).

### 2.4 Entry points

```rust
pub fn check_special_tx_inner(tx: &Tx, payload: PayloadSlot<'_>, ctx: &mut SpecialTxCtx) -> CheckResult;
/// Called outside the inner catch (CH3a, MP1a), so its error is storage only, never `CoreThrow`.
pub fn shared_collateral_spends(tx: &Tx, coins: &dyn CoinsView) -> Result<Result<(), TxValidationError>, StoreError>;
pub fn shared_collateral_template_outputs(tx: &Tx, payload: PayloadSlot<'_>) -> Result<(), TxValidationError>;
/// Core CheckCbTx (cbtx.cpp:24-55). CH7a's pre-loop coinbase call (§3.2 "Block path", D18);
/// `cons.prev` = pindex->pprev.
pub fn check_cbtx(cbtx: &CbTx, cons: &ConsensusCtx, v24: bool) -> Result<(), TxValidationError>;
pub fn check_pro_dis_tx_for_list(tx: &Tx, p: &ProDisTx, list: &MnList, spend_height: u32,
                                 check_sigs: bool) -> Result<(), TxValidationError>;  // EV2b reuses it
pub struct CoreNetInfoValidity<'p> { pub params: &'p ChainParams }       // impl NetInfoValidity
pub fn add_entry(ni: &mut NetInfo, purpose: NIPurpose, input: &str, params: &ChainParams) -> NetInfoStatus;
/// The reject of SetStateVersion's NetInfo step (§3.16.1), or `None`. Read-only (D24).
pub fn migration_reject(ni: &NetInfo, cur_version: u16, target: u16, mn_type: MnType,
                        params: &ChainParams) -> Option<&'static str>;
```

`PayloadSlot<'a>` is `Option<Result<&'a PayloadDecode, &'a PayloadError>>`, the shape `Tx::payload()` already returns. `PayloadDecode::is_complete()` is Core's "stream empty" condition in `GetTxPayload` (`specialtx.h`). For `SpecialPayload::UnknownProTxVersion` it is true exactly when the payload is the 2 version bytes (D6, D20).

### 2.5 Signature verification

```rust
pub trait SigVerifier {
    /// Core CBLSSignature::VerifyInsecure(pk, hash). `scheme` is the verify scheme only: the
    /// global flag at the call (`bls.cpp:296-312`). `sig` and `pk` carry their own decode.
    /// `Ok(false)` for an undecodable point, and for an Exotic point under the basic flag; under
    /// the legacy flag, relic's result (BS11a D16). `Err(VerifyUnavailable)` when this
    /// verifier cannot evaluate `scheme`.
    fn bls_verify_insecure(&self, sig: &BlsSigRef, pk: &BlsPublicKeyRef, msg: &[u8; 32],
                           scheme: VerifyScheme) -> Result<bool, VerifyUnavailable>;
}
```

`VerifyScheme` is a type of its own, built only from V19's activation after the block's parent (`VerifyScheme::after_v19`), so a key's or signature's decode scheme cannot be passed as the verify scheme (EV1-2 r1 m-4).

Three schemes meet in one BLS check (D22):

| Object | Scheme | Core |
|---|---|---|
| Payload signature (ProUpServ, ProUpRev) | decoded eagerly under the payload version (`LegacyBLS` → legacy). If that is not canonical, Core tries the other scheme: canonical there → `Reset()`, an invalid signature that still decodes; canonical in neither → throw, so `GetTxPayload` fails → `bad-protx-payload` | `bls.h:183-201` |
| Operator key | decoded lazily by `Get()` under the stored `bufLegacyScheme`; a failed decode is the invalid object | `bls.h:494-511` |
| Verify | the global `bls_legacy_scheme` at the call | `bls.cpp:296-312` |

`BlsSigRef` and `BlsPublicKeyRef` (BS6a / BS11a-t) carry their decode outcome (valid object, or the invalid object) and the scheme they were decoded under; EV1 passes them through unchanged. The verifier returns `Ok(false)` for the invalid object, as `VerifyInsecure` does.

The ECDSA paths (`CHashSigner::VerifyHash`, `VerifyHashCanonical` and `CMessageSigner::VerifyMessage`) use `dash-pkc` secp256k1 compact recovery in-crate; they are allowed in pure crates today. `VerifyHashCanonical` checks, in order: 65 bytes, header byte 27..34, low-S (S compared big-endian against the half order), then `VerifyHash`.

### 2.6 Neighbours

| Neighbour | Direction | What crosses | Notes |
|---|---|---|---|
| CH7a | caller (block) | `SpecialTxCtx` with `quorums` bound to the attempt facade (`OnValidation`), `unlock_indexes` = credit pool at pprev when V20 is active at pindex, `rules` = `GetSpecialTxRules(pprev)`, `check_sigs = fCheckCbTxMerkleRoots`; `BlockPayloads`; the covenant calls; the coinbase rule (§3.2 "Block path", D18): before the loop, only when `check_sigs` and `vtx[0].nType == 5`, `check_cbtx` (no DIP3 gate), then CH7a's own `bad-cbtx-version` presence check; in the loop, `i == 0` with `nType == 5` skips `check_special_tx_inner` after the template-output check | maps a reject to `BLOCK_CONSENSUS` "Special Transaction check failed (tx hash %s) %s" (`specialtxman.cpp:870-875`); the pre-loop `check_cbtx` reject maps to `BLOCK_CONSENSUS` with the same reason (`:823-828`) |
| CH3a | caller (ConnectBlock) | `shared_collateral_spends` after `CheckTxInputs`, gated on `DeploymentActiveAt(pindex, V24)` | `validation.cpp:2700-2716` |
| MP1a | caller (mempool) | `SpecialTxCtx` with `prev` = tip, or `chain[first_height]`/`chain[last_height]` for the expired-AU retry (`validation.cpp:802-825`), `rules` = `GetSpecialTxRules(tip)` in every case (`:1103`), `bls` = tip's flag, `unlock_indexes = None`, quorums bound under `OnV`, `coins` = `CoinsTip()` (`validation.cpp:802-810`), `check_sigs = true` always (`:805-808`) | `shared_collateral_spends` gets the mempool input cache `m_view` after `CheckTxInputs`, gated on V24 after the tip (`validation.cpp:999-1018`); special check at `:1097-1104`. Second call site: the fresh-instance asset-unlock replacement (`:1370-1404`) calls `CheckSpecialTx(*ptx, tip, GetSpecialTxRules(tip), CoinsTip(), check_sigs = true)` at `:1400`. The two views differ (D13); rules from the tip (D21) |
| EV2a | read API | `MnList::{get, get_by_collateral, has_unique, unique_owner, has_operator_key_under_any_scheme}`; `UniqueKey`; `OperatorKey`'s `==` is the `key_changed` equality (§3.5, D17) | the codec takes `CoreNetInfoValidity` |
| EV2b | sibling | `netinfo::add_entry` (EV2b's `migrate` builds its own NetInfo conversion over it); `netinfo::migration_reject` (equality test in EV2b's acceptance, §3.16.1, D24); `check_pro_dis_tx_for_list`; `BlockPayloads`; `OperatorKey`'s `==` for `key_changed` (D17) | EV2b trusts EV1 ran against pprev (EV2 §1.3) |
| EV3 | provides | `UnlockIndexes` impl (`RangesSet::contains`) | interface dependency |
| LQ1a | provides | `QuorumLookup` binding over `scan_quorums` / `get_quorum` (EV addendum §2.6) | effects in call order |
| LQ2 / SB8 | provides | `CommitmentVerifier` (`Verify(base, checkSigs=false)`, `VerifyNull`) | D5 |
| BS6a / BS11a-t | provides | payload types, `OperatorKey::{is_valid, is_legacy}` and `PartialEq` (base-sdk `BlsLazy::eq`, D17), `BlsSigRef` / `BlsPublicKeyRef` with their decode outcome and scheme (§2.5), `SpecialPayload::UnknownProTxVersion` (D20), decode domain | D2, D10, D22 |
| `dash-netaddr` (new, EV1-1) | provides | Core-exact `NetAddr` / `Service`, `split_host_port`, `is_bad_port`, numeric lookup, `maybe_flip_ipv6_to_cjdns(s, cjdns_reachable)`, extracted from dash-p2p, which re-exports them | D28 |
| BS12 | provides | serial pool-free BLS verify (basic and legacy) | DEC-79 |
| dash-oracle-sys | test | `check_special_tx`, `special_tx_sign_hash` | §6 |

---

## 3. Core equivalence

Columns: **Line** is the Core line of the reject; **Cl** is the class (`C` = `TX_CONSENSUS`, `S` = `TX_BAD_SPECIAL`). Steps run top to bottom; the first failure returns.

### 3.1 Function map

| Core (v24.0.0-rc.3) | Rust |
|---|---|
| `CSpecialTxProcessor::CheckSpecialTxInner` (`specialtxman.cpp:217-274`) | `specialtx::check_special_tx_inner` |
| `GetValidatedPayload<T>` (`:1097-1122`), `ProTxVersion::GetMax` | `payload::validated_payload`, `payload::version_max` |
| `IsVersionChangeValid` (`:1132-1146`), `IsSchemeMigration` (`:46-49`) | `provider::version_change_valid`, `provider::is_scheme_migration` |
| `CheckHashSig` ×2, `CheckStringSig` (`:1068-1095`) | `sig::check_hash_sig_ecdsa`, `sig::check_hash_sig_bls`, `sig::check_string_sig` |
| `CheckProRegTx` (`:1148-1321`) | `provider::check_pro_reg_tx` (D35) |
| `CheckProUpServTx` (`:1323-1426`) | `provider::check_pro_up_serv` |
| `CheckProUpRegTx` (`:1428-1528`) | `provider::check_pro_up_reg_tx` (D35) |
| `CheckProUpRevTx` (`:1530-1561`) | `provider::check_pro_up_rev` |
| `CheckProUpShareTx` (`:1563-1617`) | `shared::check_pro_up_share_tx` (D35) |
| `CheckProUpSharedRegTx` (`:1619-1674`) | `shared::check_pro_up_shared_reg_tx` (D35) |
| `CheckProDisTxForList` (`:1676-1787`), `CheckProDisTx` (`:1789-1813`) | `shared::check_pro_dis_tx_for_list`, `shared::check_pro_dis_tx` (D35) |
| `CheckSharedCollateralSpends`, `…TemplateOutputs` (`:1815-1849`) | `covenant::*` |
| `IsPayoutListTriviallyValid`, `IsShareListTriviallyValid`, `IsShareListVotingKeySafe`, `IsPayoutListKeySafe` (`providertx.cpp:25-164`) | `trivial::*` |
| `IsNetInfoTriviallyValid`, `CheckNetInfo`, `CheckProviderNetworkFields` (`:166-263`) | `network::*` |
| `C*Tx::IsTriviallyValid` (`:265-589`) | `trivial::*` |
| `CheckInputsHash`, `CalcTxInputsHash` (`providertx.h:560`) | `sig::check_inputs_hash`, `sig::inputs_hash` (D35) |
| `CheckCbTx` (`cbtx.cpp:24-55`) | `cbtx::check_cbtx` |
| `CheckLLMQCommitment` (`commitment.cpp:240-310`) | `qc::check_llmq_commitment` |
| `MNHFTxPayload::IsTriviallyValid` (`mnhftx.cpp:94-104`), `CheckMNHFTxImpl` (`:107-157`) | `mnhf::*` |
| `CChainParams::IsValidMNActivation` (`chainparams.cpp:109-130`) | `mnhf::is_valid_mn_activation` |
| `CheckAssetLockTx` (`assetlocktx.cpp:29-89`), `CheckAssetUnlockTxImpl` (`:177-232`), `VerifyAssetUnlockSig` (`:106-153`) | `asset::*` |
| `MnNetInfo::{ValidateService, AddEntry, Validate}` (`netinfo.cpp:286-381`), `ExtNetInfo::{HasAddrPortDuplicates, HasAddrDuplicates, ProcessCandidate, ValidateService, ValidateDomainPort, AddEntry, Validate}` (`:383-638`), `DomainPort::{ValidateDomain, Validate}` (`:90-160`), `NetInfoEntry::IsTriviallyValid` (`:209`), `IsNodeOnMainnet`, `MainParams` (`:69-80`) | `netinfo::*` |

### 3.2 Dispatcher: `CheckSpecialTxInner` (`specialtxman.cpp:217-274`)

| Step | Line | Condition | Cl | Reason / action |
|---|---|---|---|---|
| 1 | 224 | `!tx.HasExtraPayloadField()` | | accept |
| 2 | 228 | `!DeploymentActiveAfter(pindexPrev, DIP0003)` | S | `bad-tx-type-dip3-inactive` |
| 3 | 232-265 | switch on `nType` | | as below |
| 3.1 | | 1 → `CheckProRegTx(…, view, rules, …, check_sigs)` | | §3.3 |
| 3.2 | | 2 → `CheckProUpServTx(…, rules.v24, …)` | | §3.4 |
| 3.3 | | 3 → `CheckProUpRegTx(…, view, rules, …)` | | §3.5 |
| 3.4 | | 4 → `CheckProUpRevTx(…, rules.v24, …)` | | §3.6 |
| 3.5 | | 10 → `CheckProDisTx` | | §3.7 |
| 3.6 | | 11 → `CheckProUpShareTx` | | §3.8 |
| 3.7 | | 12 → `CheckProUpSharedRegTx` | | §3.9 |
| 3.8 | 249 | 5 and `!tx.IsCoinBase()` | C | `bad-cbtx-invalid` |
| 3.9 | 254 | 5 and the payload does not decode | C | `bad-cbtx-payload` |
| 3.10 | | 5 → `CheckCbTx(cbTx, pindexPrev, rules.v24, state)` | | §3.10 |
| 3.11 | | 6 → `llmq::CheckLLMQCommitment` | | §3.11 |
| 3.12 | | 7 → `CheckMNHFTx` (the `chain` overload when `chain != nullptr`) | | §3.12 |
| 3.13 | | 8 → `CheckAssetLockTx(tx, state, rules.v24)` | | §3.12 |
| 3.14 | | 9 → `CheckAssetUnlockTx` (the `chain` overload when `chain != nullptr`), with `indexes` | | §3.12 |
| 4 | 270 | exception thrown | C | `failed-check-special-tx` |
| 5 | 273 | type not handled | S | `bad-tx-type-check` |

`CheckSpecialTx` (`:276-…`) is the mempool entry: `chain = nullptr`, `indexes = std::nullopt`.

**Block path: the coinbase (`ProcessSpecialTxsInBlock`, `specialtxman.cpp:816-837, 853-875`).** On the block path the type-5 coinbase does not reach `CheckSpecialTxInner`, so steps 2 and 3.8–3.10 do not apply to it. CH7a runs, in order (D18):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| B1 | 816 | `fCheckCbTxMerkleRoots && vtx.size() > 0 && vtx[0]->nType == TRANSACTION_COINBASE`; otherwise go to B4 | | |
| B2 | 818-819 | `!vtx[0]->IsCoinBase()` | BLOCK_CONSENSUS | `bad-cbtx-invalid` |
| B3a | 821 | `GetTxPayload<CCbTx>` fails | BLOCK_CONSENSUS | `bad-cbtx-payload` (831) |
| B3b | 823-828 | `!CheckCbTx(cbTx, pindex->pprev, rules.v24)` | BLOCK_CONSENSUS | the `CheckCbTx` reason (§3.10) |
| B4 | 834-837 | `fCheckCbTxMerkleRoots && DeploymentActiveAt(*pindex, DIP0003) && !opt_cbTx` | BLOCK_CONSENSUS | `bad-cbtx-version` (CH7a's) |
| B5 | 853-862 | per tx `i`, from 0: `rules.v24 && !CheckSharedCollateralTemplateOutputs(vtx[i])` | BLOCK_CONSENSUS | the covenant reason (§3.17) |
| B6 | 865 | `i == 0 && vtx[i]->nType == TRANSACTION_COINBASE` → next tx | | |
| B7 | 871-875 | `CheckSpecialTxInner(vtx[i], …)` | BLOCK_CONSENSUS | the inner reason |

There is no DIP3 gate before B1. `fCheckCbTxMerkleRoots = fScriptChecks` (`validation.cpp:2663-2665`), which assumevalid controls; `RollforwardBlock` passes `false` (`validation.cpp:5197-5200`). With `fCheckCbTxMerkleRoots` false, B1–B4 do not run and B6 still skips the coinbase. A type-5 transaction at `i > 0` goes through B7, so steps 3.8–3.10 apply to it; through a block only 3.8 is reachable, because `CheckBlock` rejects a second coinbase (`validation.cpp:4401`), and 3.9 and 3.10 are reached through direct calls. On the mempool path a type-5 transaction always goes through the dispatcher.

For CH7a: B1–B7 run inside `ProcessSpecialTxsInBlock`'s outer `catch (const std::exception&)` → BLOCK_CONSENSUS `failed-procspectxsinblock` (`specialtxman.cpp:993-995`), separate from the inner `failed-check-special-tx`; nothing EV1 exports raises on B1–B6.

**`GetValidatedPayload<T>` (`:1097-1122`)**:

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | 1103 | `tx.nType != T::SPECIALTX_TYPE` | C | `bad-protx-type` |
| 2 | 1109 | `GetTxPayload<T>` fails | C | `bad-protx-payload` |
| 3 | 1114 | `nVersion > GetMax(DeploymentActiveAfter(pindexPrev, V19), is_v24_active)` | C | `bad-protx-version` |
| 4 | | `!IsTriviallyValid(state)` | | that function's reject |

`GetMax(v19, v24)`: both → 3 (`ExtAddr`), `v19` only → 2 (`BasicBLS`), else 1 (`LegacyBLS`).

**`IsVersionChangeValid` (`:1132-1146`)**: `!is_v24_active` → valid; `state_version >= BasicBLS && tx_version == LegacyBLS` → C `bad-protx-version-downgrade` (1142).

### 3.3 Type 1, ProRegTx

`CProRegTx::IsTriviallyValid` (`providertx.cpp:265-333`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | 268 | `nVersion == 0 \|\| nVersion > GetMax(true, true)` | C | `bad-protx-version` |
| 2 | 271 | `nVersion < BasicBLS && nType == Evo` | C | `bad-protx-evo-version` |
| 3 | 274 | `!IsValidMnType(nType)` | C | `bad-protx-type` |
| 4 | 277 | `nMode != 0` | C | `bad-protx-mode` |
| 5a | 283 | shared: external collateral (`!collateralOutpoint.hash.IsNull()`) | S | `bad-protx-shares-external` |
| 5b | 287 | shared: `!keyIDOwner.IsNull()` | S | `bad-protx-shares-owner-key` |
| 5c | 290 | shared: payouts not empty | S | `bad-protx-shares-payouts` |
| 5d | 294 | not shared: early period/penalty fields non-zero | S | `bad-protx-shares-empty-fields` |
| 5e | 297 | not shared: `keyIDOwner.IsNull()` | S | `bad-protx-key-null` |
| 6 | 301 | `!pubKeyOperator.Get().IsValid() \|\| keyIDVoting.IsNull()` | S | `bad-protx-key-null` |
| 7 | 304 | `pubKeyOperator.IsLegacy() != (nVersion == LegacyBLS)` | S | `bad-protx-operator-pubkey` |
| 8 | | shared → `IsShareListTriviallyValid` (§3.14); else `IsPayoutListTriviallyValid(GetOwnerPayouts, keyIDOwner, keyIDVoting)` | | §3.14 |
| 9 | 316 | `netInfo->CanStorePlatform() != (nVersion >= ExtAddr)` | C | `bad-protx-netinfo-version` |
| 10 | | `!netInfo->IsEmpty() && !IsNetInfoTriviallyValid(…)` | | §3.15 |
| 11 | 324 | any entry `!IsTriviallyValid()` | S | `bad-protx-netinfo-bad` |
| 12 | 329 | `nOperatorReward > 10000` | S | `bad-protx-operator-reward` |

`CheckProRegTx` (`specialtxman.cpp:1148-1321`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | | `GetValidatedPayload<CProRegTx>` | | §3.2 |
| 2 | 1160 | `rules.v24 && nVersion < BasicBLS` | C | `bad-protx-version-disallowed` |
| 3a | 1165 | `nType == Evo && !rules.evo_shares && IsShared()` | S | `bad-protx-shares-evo` |
| 3b | 1168 | `nType == Evo && !rules.evo_shares && GetOwnerPayouts().size() > 1` | S | `bad-protx-payouts-evo` |
| 4 | | `CheckProviderNetworkFields(…, allow_empty=true)` | | §3.15 |
| 5 | | `expected = GetMnType(nType).collat_amount` (Regular 1000 DASH, Evo 4000 DASH; `dmn_types.h`) | | |
| 6e1 | 1188 | external (`!collateralOutpoint.hash.IsNull()`): `!view.GetCoin \|\| coin.IsSpent() \|\| value != expected` | S | `bad-protx-collateral` |
| 6e2 | 1192 | external: `!ExtractDestination(coin script)` | S | `bad-protx-collateral-dest` |
| 6e3 | 1199 | external: destination is not `PKHash` | S | `bad-protx-collateral-pkh` |
| 6i1 | 1205 | internal: `n >= tx.vout.size()` | S | `bad-protx-collateral-index` |
| 6i2 | 1208 | internal: `vout[n].nValue != expected` | S | `bad-protx-collateral` |
| 6i3 | 1214 | internal, shared: `!IsSharedCollateralScript(vout[n].scriptPubKey)` | S | `bad-protx-shares-collateral-script` |
| 6i4 | 1217 | internal, not shared: `!ExtractDestination(vout[n])` | S | `bad-protx-collateral-dest` |
| 6i5 | | internal: `collateralOutpoint = (tx.GetHash(), n)` | | |
| 7 | | `!IsShared()` → `IsPayoutListKeySafe(GetOwnerPayouts, dest, keyIDOwner, keyIDVoting, nVersion >= ExtAddr)` | | §3.14 |
| 8a | 1239, 1244 | per `netInfo` entry: AddrPort / DomainPort is a unique property of an MN whose `collateralOutpoint != collateralOutpoint` | S | `bad-protx-dup-netinfo-entry` |
| 8b | 1247 | per entry: neither AddrPort nor DomainPort | S | `bad-protx-netinfo-entry` |
| 9a | 1257 | shared: any `shares[i].keyIDOwner` is a unique property | S | `bad-protx-dup-key` |
| 9b | 1261 | shared: `pubKeyOperator` is a unique property | S | `bad-protx-dup-key` |
| 9c | 1264 | not shared: `keyIDOwner` or `pubKeyOperator` is a unique property | S | `bad-protx-dup-key` |
| 10 | 1273 | `rules.v24 && HasOperatorKeyUnderAnyScheme(pubKeyOperator.Get(), uint256())` | S | `bad-protx-dup-key` |
| 11 | 1279 | `nType == Evo && HasUniqueProperty(platformNodeID)` | S | `bad-protx-dup-platformnodeid` |
| 12 | 1285 | `!DeploymentDIP0003Enforced(pindexPrev->nHeight) && keyIDOwner != keyIDVoting` | S | `bad-protx-key-not-same` |
| 13 | | `CheckInputsHash` | C | `bad-protx-inputs-hash` |
| 14e | 1083 | external: `check_sigs && !CheckStringSig` (`VerifyMessage(keyID, vchSig, MakeSignString())`) | C | `bad-protx-sig` |
| 14i | 1304 | internal: `!vchSig.empty()` | C | `bad-protx-sig` |
| 14s | 1314 | internal, shared, `check_sigs`: any `!VerifyHashCanonical(MakeSharedRegConsentHash(tx), shares[i].keyIDOwner, vchJoinSigs[i])` | S | `bad-protx-shares-sig` |

Steps 8–12 read the list at pindexPrev (`dmnman.GetListForBlock(pindexPrev)`). `DeploymentDIP0003Enforced(h)` is `h >= DIP0003EnforcementHeight`.

### 3.4 Type 2, ProUpServTx

`CProUpServTx::IsTriviallyValid` (`providertx.cpp:412-440`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | 415 | `nVersion == 0 \|\| nVersion > GetMax(true, true)` | C | `bad-protx-version` |
| 2 | 418 | `nVersion < BasicBLS && nType == Evo` | C | `bad-protx-evo-version` |
| 3 | 421 | `!IsValidMnType(nType)` | C | `bad-protx-type` |
| 4 | 424 | `netInfo->CanStorePlatform() != (nVersion >= ExtAddr)` | C | `bad-protx-netinfo-version` |
| 5 | 427 | `netInfo->IsEmpty()` | S | `bad-protx-netinfo-empty` |
| 6 | | `!IsNetInfoTriviallyValid` | | §3.15 |
| 7 | 435 | any entry `!IsTriviallyValid()` | S | `bad-protx-netinfo-bad` |

`CheckProUpServTx` (`specialtxman.cpp:1323-1426`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | | `GetValidatedPayload<CProUpServTx>` | | §3.2 |
| 2 | | `CheckProviderNetworkFields(…, allow_empty=false)` | | §3.15 |
| 3 | 1341 | `mnList.GetMN(proTxHash)` missing | C | `bad-protx-hash` |
| 4 | 1349 | `nType != dmn->nType` | C | `bad-protx-type-mismatch` |
| 5 | | `IsVersionChangeValid(state.nVersion, nVersion, v24)` | | §3.2 |
| 6 | 1360 | `v24 && nType == Evo && nVersion < ExtAddr` | C | `bad-protx-version-disallowed` |
| 7 | 1367 | `v24`: `ApplyProUpServTx(copy of state, ptx, v24)` fails | S | the migration reject (`bad-protx-netinfo-version`) |
| 8 | 1377 | `v24 && IsSchemeMigration(state.nVersion, nVersion) && HasOperatorKeyUnderAnyScheme(state.pubKeyOperator.Get(), proTxHash)` | S | `bad-protx-dup-key` |
| 9a | 1385, 1390 | per entry: AddrPort / DomainPort is a unique property of an MN with `proTxHash != ptx.proTxHash` | S | `bad-protx-dup-netinfo-entry` |
| 9b | 1393 | per entry: neither | S | `bad-protx-netinfo-entry` |
| 10 | 1401 | `nType == Evo`: `platformNodeID` unique and held by another `proTxHash` | S | `bad-protx-dup-platformnodeid` |
| 11a | 1408 | `scriptOperatorPayout != CScript()` and `dmn->nOperatorReward == 0` | S | `bad-protx-operator-payee` |
| 11b | 1411 | `scriptOperatorPayout != CScript()` and not P2PKH/P2SH | S | `bad-protx-operator-payee` |
| 12 | | `CheckInputsHash` | C | `bad-protx-inputs-hash` |
| 13 | 1092 | `check_sigs && !sig.VerifyInsecure(state.pubKeyOperator.Get(), SerializeHash(ptx))` | C | `bad-protx-sig` |

`IsSchemeMigration(old, new)` is `old == LegacyBLS && new > LegacyBLS` (`:46-49`). In `ApplyProUpServTx` (`:110-137`) the copy takes `nVersion = ptx.nVersion` (`:117`) and `netInfo = ptx.netInfo` (`:119`), then calls `SetStateVersion(copy, max(state.nVersion, ptx.nVersion), ptx.nType)` (`:114-115, 136`). Rust: step 7 is `netinfo::migration_reject(&ptx.net_info, ptx.version, max(state.version, ptx.version), ptx.mn_type, params)` (§3.16.1).

### 3.5 Type 3, ProUpRegTx

`CProUpRegTx::IsTriviallyValid` (`providertx.cpp:459-476`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | 462 | `nVersion == 0 \|\| nVersion > GetMax(true, true)` | C | `bad-protx-version` |
| 2 | 465 | `nMode != 0` | C | `bad-protx-mode` |
| 3 | 469 | `!pubKeyOperator.Get().IsValid() \|\| keyIDVoting.IsNull()` | S | `bad-protx-key-null` |
| 4 | 472 | `pubKeyOperator.IsLegacy() != (nVersion == LegacyBLS)` | S | `bad-protx-operator-pubkey` |
| 5 | | `IsPayoutListTriviallyValid(GetOwnerPayouts, CKeyID{}, keyIDVoting)` | | §3.14 |

`CheckProUpRegTx` (`specialtxman.cpp:1428-1528`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | | `GetValidatedPayload<CProUpRegTx>` | | §3.2 |
| 2 | 1441 | MN missing | C | `bad-protx-hash` |
| 3 | 1447 | `dmn->pdmnState->IsShared()` | S | `bad-protx-shared-mn` |
| 4 | | `IsVersionChangeValid` | | §3.2 |
| 5a | 1465 | `v24`, `key_changed = !(ptx.pubKeyOperator == state.pubKeyOperator)`, `migrating = IsSchemeMigration(state.nVersion, nVersion)`: `(key_changed \|\| migrating) && HasOperatorKeyUnderAnyScheme(ptx.pubKeyOperator.Get(), proTxHash)` | S | `bad-protx-dup-key` |
| 5b | 1476 | `v24 && !key_changed`: `SetStateVersion(copy, max(state.nVersion, nVersion), dmn->nType)` fails | S | the migration reject |
| 6 | | `IsPayoutListTriviallyValid(owner_payouts, state.keyIDOwner, ptx.keyIDVoting)` | | §3.14 |
| 7 | 1484 | `dmn->nType == Evo && !rules.evo_shares && owner_payouts.size() > 1` | S | `bad-protx-payouts-evo` |
| 8 | 1490 | `!view.GetCoin(dmn->collateralOutpoint) \|\| coin.IsSpent()` | C | `bad-protx-collateral` |
| 9 | 1496 | `!ExtractDestination(coin script)` | C | `bad-protx-collateral-dest` |
| 10 | | `IsPayoutListKeySafe(owner_payouts, dest, state.keyIDOwner, ptx.keyIDVoting, max(state.nVersion, nVersion) >= ExtAddr)` | | §3.14 |
| 11 | 1506 | `pubKeyOperator` is a unique property held by another `proTxHash` | S | `bad-protx-dup-key` |
| 12 | 1514 | `!DeploymentDIP0003Enforced(pindexPrev->nHeight) && state.keyIDOwner != ptx.keyIDVoting` | S | `bad-protx-key-not-same` |
| 13 | | `CheckInputsHash` | C | `bad-protx-inputs-hash` |
| 14 | 1073 | `check_sigs && !CHashSigner::VerifyHash(SerializeHash(ptx), state.keyIDOwner, vchSig)` | C | `bad-protx-sig` |

Rust: step 5b is `netinfo::migration_reject(&state.net_info, state.version, max(state.version, ptx.version), dmn.mn_type, params)` (§3.16.1).

`key_changed` (step 5a, 1461-1465) is `!(ptx.pubKeyOperator == state.pubKeyOperator)` with `CBLSLazyWrapper::operator==` (`bls.h:514-535`), in order: the same object → equal; both default (`!bufValid && !objInitialized`) → equal; exactly one default → not equal; both `bufValid` with the same `bufLegacyScheme` → `vecBytes` equal; both `objInitialized` → `obj == r.obj`; otherwise `Get() == r.Get()`. Object equality is `CBLSWrapper::operator==` (`bls.h:77-80`): `fValid == r.fValid && impl == r.impl`. Rust: `<OperatorKey as PartialEq>::eq`, which is base-sdk `BlsLazy::eq` (`pkgs/pkc/src/bls/wrap_lazy.rs:252-284` @dbe245f7) over `BlsValue::eq` (`wrap_value.rs:119-124`). EV1 and EV2b call this function and no other (D17). At 5a the ptx key is already decoded (both flags set) by `IsTriviallyValid`'s `Get()`; the state-side cache may or may not be populated; neither affects `key_changed` for inputs reaching 5a.

### 3.6 Type 4, ProUpRevTx

`CProUpRevTx::IsTriviallyValid` (`providertx.cpp:486-498`): version 0 or `> GetMax(true, true)` → C `bad-protx-version` (489); `nReason > REASON_LAST` → C `bad-protx-reason` (495).

`CheckProUpRevTx` (`specialtxman.cpp:1530-1561`): `GetValidatedPayload` → MN missing → C `bad-protx-hash` (1543) → `IsVersionChangeValid` → `CheckInputsHash` → `check_sigs && !sig.VerifyInsecure(state.pubKeyOperator.Get(), SerializeHash(ptx))` → C `bad-protx-sig` (1092).

### 3.7 Type 10, ProDisTx

`CProDisTx::IsTriviallyValid` (`providertx.cpp:535-544`): `nVersion == 0 || nVersion > 1` → C `bad-prodis-version` (538); `vchSigs.empty() || vchSigs.size() > 8` → S `bad-prodis-sig-count` (541).

`CheckProDisTx` (`specialtxman.cpp:1789-1813`): `GetValidatedPayload` → `!v24` → C `bad-prodis-too-early` (1799) → MN missing in the pindexPrev list → S `bad-prodis-hash` (1810) → `CheckProDisTxForList(tx, ptx, mnList, pindexPrev->nHeight + 1, state, check_sigs)`.

`CheckProDisTxForList` (`:1676-1787`). All rejects are S.

| Step | Line | Condition | Reason |
|---|---|---|---|
| 1 | 1681 | MN missing | `bad-prodis-hash` |
| 2 | 1685 | not shared | `bad-prodis-not-shared` |
| 3 | 1692 | `vin.size() != 1 \|\| vin[0].prevout != collateralOutpoint \|\| !vin[0].scriptSig.empty()` | `bad-prodis-input` |
| 4 | 1695 | `actorIndex >= shares.size()` | `bad-prodis-actor` |
| 5 | 1701 | `vchSigs.size() != 1 && vchSigs.size() != shares.size()` | `bad-prodis-sig-count` |
| 6 | | `unanimous = vchSigs.size() == shares.size()`; `early = nSpendHeight - registeredHeight < earlyPeriodBlocks`; `required = (unanimous \|\| !early) ? 0 : earlyPenalty` | |
| 7 | 1714 | `vout.size() != n - 1 && vout.size() != n` | `bad-prodis-payee-count` |
| 8 | 1717-1720 | `non_actor_total` = sum of the non-actor share amounts (before the loop) | |
| 8a | 1728 | one iteration per non-actor share, in share order, taking the next output; in that iteration: output script `!= refundScript` | `bad-prodis-payee` |
| 8b | 1734 | same iteration, after 8a: `bonus = nValue - amount`; `bonus < ProRataFloor(required, amount, non_actor_total)`; then the next share starts at 8a | `bad-prodis-penalty-floor` |
| 9a | 1741 | after the loop, actor output present (`vout.size() == n`): script `!= actor refundScript` | `bad-prodis-payee` |
| 9b | 1744 | actor output present: `nValue == 0` | `bad-prodis-actor-output-zero` |
| 10 | 1750 | `bonus_total < required` | `bad-prodis-penalty-sum` |
| 11 | 1763 | `non_actor_total + actor_amount - out_total > MAX_FEE` (1,000,000) | `bad-prodis-fee` |
| 12 | 1766 | `!unanimous && bonus_total > earlyPenalty` | `bad-prodis-bonus` |
| 13 | 1775, 1781 | `check_sigs`: any `!VerifyHashCanonical(MakeSignHash(tx, vchSigs.size()), signer keyIDOwner, sig)` | `bad-prodis-sig` |

`ProRataFloor(total, weight, weight_total)` (`providertx_util.cpp:88-95`) is `arith_uint256{uint64_t(total)} * arith_uint256{uint64_t(weight)} / arith_uint256{uint64_t(weight_total)}`, returned as `CAmount(v.GetLow64())`. The port keeps the `as u64` casts, computes in `u128` when the operands are below 2^63 and otherwise in `dash-consensus` `U256`, and returns the low 64 bits as `i64`. A zero divisor follows §2.2 (D11); consensus state cannot reach it (at least 2 shares of at least 100 DASH, `providertx.cpp:66-81`), so its test is defensive (§6.5).

### 3.8 Type 11, ProUpShareTx

`CProUpShareTx::IsTriviallyValid` (`providertx.cpp:552-564`): version → C `bad-proupshare-version` (555); `vchSig.size() != 65` → S `bad-proupshare-sig-size` (558); `scriptReward` not P2PKH/P2SH → S `bad-proupshare-payee` (561).

`CheckProUpShareTx` (`specialtxman.cpp:1563-1617`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | | `GetValidatedPayload` | | §3.2 |
| 2 | 1573 | `!v24` | C | `bad-proupshare-too-early` |
| 3 | 1579 | MN missing | S | `bad-proupshare-hash` |
| 4 | 1583 | not shared | S | `bad-proupshare-not-shared` |
| 5 | 1586 | `shareIndex >= shares.size()` | S | `bad-proupshare-index` |
| 6 | 1592 | `!ExtractDestination(scriptReward)` | S | `bad-proupshare-payee-dest` |
| 7a | 1595 | destination == `keyIDVoting` | S | `bad-proupshare-payee-reuse` |
| 7b | 1599 | destination == any share `keyIDOwner` | S | `bad-proupshare-payee-reuse` |
| 8 | | `CheckInputsHash` | C | `bad-protx-inputs-hash` |
| 9 | 1612 | `check_sigs && !VerifyHashCanonical(SerializeHash(ptx), shares[shareIndex].keyIDOwner, vchSig)` | S | `bad-proupshare-sig` |

### 3.9 Type 12, ProUpSharedRegTx

`CProUpSharedRegTx::IsTriviallyValid` (`providertx.cpp:574-589`): version → C `bad-proupsharedreg-version` (577); `!pubKeyOperator.Get().IsValid() || keyIDVoting.IsNull()` → S `bad-protx-key-null` (580); `pubKeyOperator.IsLegacy()` → S `bad-protx-operator-pubkey` (583); `vchSigs.empty() || > 8` → S `bad-proupsharedreg-sig-count` (586).

`CheckProUpSharedRegTx` (`specialtxman.cpp:1619-1674`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | | `GetValidatedPayload` | | §3.2 |
| 2 | 1630 | `!v24` | C | `bad-proupsharedreg-too-early` |
| 3 | 1636 | MN missing | S | `bad-proupsharedreg-hash` |
| 4 | 1640 | not shared | S | `bad-proupsharedreg-not-shared` |
| 5 | 1645 | `vchSigs.size() != shares.size()` | S | `bad-proupsharedreg-sig-count` |
| 6 | 1651 | `HasOperatorKeyUnderAnyScheme(pubKeyOperator.Get(), proTxHash)` | S | `bad-protx-dup-key` |
| 7 | 1656 | `!IsShareListVotingKeySafe(shares, keyIDVoting)` | S | `bad-proupsharedreg-payee-reuse` |
| 8 | | `CheckInputsHash` | C | `bad-protx-inputs-hash` |
| 9 | 1668 | `check_sigs`: `payload_hash = SerializeHash(ptx)`; any `!VerifyHashCanonical(payload_hash, mnState.shares[i].keyIDOwner, vchSigs[i])` | S | `bad-proupsharedreg-sig` |

### 3.10 Type 5, `CheckCbTx` (`cbtx.cpp:24-55`)

All C, all `pindexPrev` non-null on this path:

| Step | Line | Condition | Reason |
|---|---|---|---|
| 1 | 27 | `nVersion == INVALID (0) \|\| nVersion >= UNKNOWN (5)` | `bad-cbtx-version` |
| 2 | 32 | `pindexPrev->nHeight + 1 != cbTx.nHeight` | `bad-cbtx-height` |
| 3 | 37 | `DeploymentActiveAt(*pindexPrev, DIP0008) && nVersion < MERKLE_ROOT_QUORUMS (2)` | `bad-cbtx-version` |
| 4 | 42 | `isV20 = DeploymentActiveAfter(pindexPrev, V20)`; `isV20 != (nVersion >= CLSIG_AND_BALANCE (3))` | `bad-cbtx-version` |
| 5 | 50 | `requires = is_v24_active && isV20`; `requires != (nVersion >= MERKLE_ROOT_ASSETUNLOCKS (4))` | `bad-cbtx-version` |

### 3.11 Type 6, `CheckLLMQCommitment` (`commitment.cpp:240-310`)

All C:

| Step | Line | Condition | Reason |
|---|---|---|---|
| 1 | 246 | payload does not decode | `bad-qc-payload` |
| 2 | 254 | `!Params().GetLLMQ(llmqType)` | `bad-qc-commitment-type` |
| 3 | 272 | `nVersion == 0 \|\| nVersion > CURRENT_VERSION` | `bad-qc-version` |
| 4 | 278 | `qcTx.nHeight != pindexPrev->nHeight + 1` | `bad-qc-height` |
| 5a | 284 | `LookupBlockIndex(quorumHash) == nullptr` | `bad-qc-quorum-hash` |
| 5b | 289 | `pQuorumBaseBlockIndex != pindexPrev->GetAncestor(pQuorumBaseBlockIndex->nHeight)` | `bad-qc-quorum-hash` |
| 6 | 296 | `commitment.IsNull()`: `!VerifyNull()` → reject, else accept | `bad-qc-invalid-null` |
| 7 | 304 | `!commitment.Verify(util_params with base = pQuorumBaseBlockIndex, checkSigs=false)` | `bad-qc-invalid` |

D5 covers how `Verify` is reached.

### 3.12 Types 7, 8, 9

`MNHFTxPayload::IsTriviallyValid` (`mnhftx.cpp:94-104`): version 0 or > 1 → C `bad-mnhf-version` (97); `versionBit >= VERSIONBITS_NUM_BITS` (29) → C `bad-mnhf-nbit-out-of-bounds` (100).

`CheckMNHFTxImpl` (`mnhftx.cpp:107-157`). All C:

| Step | Line | Condition | Reason |
|---|---|---|---|
| 1 | 111 | `!IsSpecialTxVersion() \|\| nType != MNHF_SIGNAL` | `bad-mnhf-type` |
| 2 | 116 | payload does not decode | `bad-mnhf-payload` |
| 3 | | `IsTriviallyValid` | above |
| 4 | 125 | `!Params().IsValidMNActivation(versionBit, pindexPrev->GetMedianTimePast())` | `bad-mnhf-non-ehf` |
| 5a | 130 | `LookupBlockIndex(quorumHash) == nullptr` | `bad-mnhf-quorum-hash` |
| 5b | 135 | `pindexQuorum != pindexPrev->GetAncestor(pindexQuorum->nHeight)` | `bad-mnhf-quorum-hash` |
| 6 | 139-143 | `msgHash = GetHash()` of a tx copy whose payload `signal.sig = CBLSSignature()` (`SetTxPayload`) | |
| 7 | 148 | `get_quorum(llmqTypeMnhf, quorumHash) == nullptr` | `bad-mnhf-missing-quorum` |
| 8 | 153 | `!sig.VerifyInsecure(quorum->qc->quorumPublicKey, SignHash(llmqTypeMnhf, quorum->qc->quorumHash, GetRequestId(), msgHash))` | `bad-mnhf-invalid` |

`GetRequestId()` (`:32-35`) is `SerializeHash(std::make_pair(MNEHF_REQUESTID_PREFIX, int64_t{signal.versionBit}))` with `MNEHF_REQUESTID_PREFIX = "mnhf"` (`:28`). `IsValidMNActivation(bit, mtp)` (`chainparams.cpp:109-130`): for each deployment whose `bit` matches, skip it when `mtp` is outside its window; if it is in the window, return `useEHF`. When nothing matches, return true. The two overloads (`:159-174`) differ only in `qman.GetQuorum(type, hash)` versus `qman.GetQuorum(type, hash, chain)`.

`CheckAssetLockTx` (`assetlocktx.cpp:29-89`). All S:

| Step | Line | Condition | Reason |
|---|---|---|---|
| 1 | 32 | `nType != ASSET_LOCK` | `bad-assetlocktx-type` |
| 2a | 40 | per `OP_RETURN` output: `scriptPubKey.size() != 2 \|\| scriptPubKey[1] != 0` | `bad-assetlocktx-non-empty-return` |
| 2b | 42 | `nValue == 0 \|\| !MoneyRange(nValue)` | `bad-assetlocktx-opreturn-outofrange` |
| 2c | 45 | a second `OP_RETURN` | `bad-assetlocktx-multiple-return` |
| 3 | 49 | none | `bad-assetlocktx-no-return` |
| 4 | 53 | payload does not decode | `bad-assetlocktx-payload` |
| 5 | 57 | version 0 or > `CURRENT_VERSION` (2) | `bad-assetlocktx-version` |
| 6 | 60 | `!is_v24_active && version > 1` | `bad-assetlocktx-version-2` |
| 7 | 64 | `creditOutputs.empty()` | `bad-assetlocktx-emptycreditoutputs` |
| 8a | 70 | per credit output: `nValue == 0 \|\| !MoneyRange(nValue) \|\| !MoneyRange(running sum)` | `bad-assetlocktx-credit-outofrange` |
| 8b | 76 | v2: not P2PKH/P2SH | `bad-assetlocktx-script-pubkey` |
| 8c | 80 | v1: not P2PKH | `bad-assetlocktx-pubKeyHash` |
| 9 | 85 | sum `!=` the return amount | `bad-assetlocktx-creditamount` |

`CheckAssetUnlockTxImpl` (`assetlocktx.cpp:177-232`) and `VerifyAssetUnlockSig` (`:106-153`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | 185 | `nType != ASSET_UNLOCK` | S | `bad-assetunlocktx-type` |
| 2 | 189 | `!vin.empty()` | S | `bad-assetunlocktx-have-input` |
| 3 | 193 | `vout.size() > MAXIMUM_WITHDRAWALS` (32) | S | `bad-assetunlocktx-too-many-outs` |
| 4 | 203 | payload does not decode | S | `bad-assetunlocktx-payload` |
| 5 | 208 | version 0 or > `CURRENT_VERSION` | S | `bad-assetunlocktx-version` |
| 6 | 211 | `!is_v24_active && version > INITIAL_VERSION` | S | `bad-assetunlocktx-version-2` |
| 7 | 215 | `indexes != nullopt && indexes->Contains(index)` | C | `bad-assetunlock-duplicated-index` |
| 8 | 219 | `LookupBlockIndex(quorumHash) == nullptr` | C | `bad-assetunlock-quorum-hash` |
| 9 | | `msgHash = ::SerializeHash(tx copy)` with payload `{version, index, fee, requestedHeight, quorumHash, CBLSSignature{}}` | | |
| 10 | | `type = llmqTypePlatform`; `quorums = scan_quorums(type, pindexPrev, signingActiveQuorumCount + 1)` | | |
| 11 | 129 | no `q->qc->quorumHash == quorumHash` in `quorums` | S | `bad-assetunlock-too-old-quorum` |
| 12 | 135 | `pindexPrev->nHeight < requestedHeight \|\| pindexPrev->nHeight >= getHeightToExpiry()` (`requestedHeight + 48`) | S | `bad-assetunlock-too-late` |
| 13 | 142 | `get_quorum(type, quorumHash) == nullptr` | C | `bad-assetunlock-quorum-internal-error` |
| 14 | 152 | `!quorumSig.VerifyInsecure(quorum->qc->quorumPublicKey, SignHash(type, qc quorumHash, SerializeHash(make_pair(ASSET_UNLOCK_REQUESTID_PREFIX, index)), msgHash))` | C | `bad-assetunlock-not-verified` |

The two overloads (`:234-254`) differ only in `VerifySig(qman, …)` versus `VerifySig(qman, chain, …)`.

### 3.13 Reject-string census

`contrib/node/ev1-census.py` (slice EV1-1) regenerates this list from the pin. It enumerates every `Invalid(` call in the scoped functions of §3.1, whatever its arguments, keyed by `file:line`. A call whose reason argument is not a string literal must appear in a checked-in classification table (`file:line`, class, the set of reasons it can forward); an unclassified one fails the script. The initial entries are `specialtxman.cpp:1367` and `:1476`: `TX_BAD_SPECIAL`, forwarding `migration_state.GetRejectReason()` ∈ {`bad-protx-netinfo-version`} (`:38, 94`). The test `specialtx::reasons::census_matches_pin` asserts that every reason constant appears exactly once in the checked-in census at its class, and that every census row in EV1's functions has a constant and either at least one L0 vector reaching it or an explicit `unreachable` mark with its Core reason. The initial `unreachable` rows are `providertx.cpp:49` `bad-protx-payee-dest` and `:105` `bad-protx-shares-payee-dest`: `IsValidPayoutScript` (`:20-23`) admits only P2PKH and P2SH scripts, and `ExtractDestination` succeeds on both. The pin has 213 literal `TxValidationResult` reject sites in EV1's functions (`specialtxman.cpp:217-274, 1068-1849`; `providertx.cpp`; `providertx.h:560-567`; `cbtx.cpp:24-55`; `mnhftx.cpp:94-157`; `assetlocktx.cpp:29-232`; `commitment.cpp:240-310`): 144 `TX_BAD_SPECIAL` and 69 `TX_CONSENSUS`, carrying 146 distinct strings. The two forwarding sites are the classification table's entries. The `BLOCK_CONSENSUS` sites in the same files (`specialtxman.cpp:309-794, 797-999, 1036-1066`; `cbtx.cpp:60-155`; `mnhftx.cpp:190-260`; `assetlocktx.cpp:256-270`) belong to EV2b, CH7a and EV3 and are listed in the census as not EV1's.

### 3.14 Payout and share lists (`providertx.cpp:25-164`)

All S.

`IsPayoutListTriviallyValid(payouts, keyIDOwner, keyIDVoting)` (`:25-61`). Step 2 is one loop: each payout runs 2a–2e before the next payout starts (D19).

| Step | Line | Condition | Reason |
|---|---|---|---|
| 1 | 29 | `empty() \|\| size() > 8` | `bad-protx-payouts-count` |
| 2 | 34 | for each payout, in list order: | |
| 2a | 36 | `reward < 100 \|\| reward > 10000`; else add `reward` to the total | `bad-protx-payout-reward` |
| 2b | 41 | not P2PKH/P2SH | `bad-protx-payee` |
| 2c | 44 | script repeats an earlier payout | `bad-protx-payee-dup` |
| 2d | 49 | `!ExtractDestination` | `bad-protx-payee-dest` |
| 2e | 53 | destination `== owner` (when owner is not null) or `== voting` | `bad-protx-payee-reuse` |
| 3 | 58 | after the loop: sum `!= 10000` | `bad-protx-payout-reward-sum` |

`IsShareListTriviallyValid(shares, early_period_blocks, early_penalty, required_collateral, keyIDVoting)` (`:63-130`). Step 3 is one loop: each share runs 3a–3e, including both of its scripts, before the next share starts (D19).

| Step | Line | Condition | Reason |
|---|---|---|---|
| 1 | 67 | `size() < 2 \|\| size() > 8` | `bad-protx-shares-count` |
| 2 | 70 | `early_period_blocks > 420480` | `bad-protx-shares-early-period` |
| 3 | 77 | for each share, in list order: | |
| 3a | 80 | `amount < 100 DASH \|\| amount > required_collateral`; else add to the total and update the minimum | `bad-protx-shares-amount` |
| 3b | 86 | `keyIDOwner.IsNull()` | `bad-protx-shares-key-null` |
| 3c | 89 | owner repeats an earlier share | `bad-protx-shares-dup-key` |
| 3d | 92 | refund script repeats an earlier share | `bad-protx-shares-dup-refund` |
| 3e | 95 | for each script of this share, refund then reward (an empty reward is skipped), 3e.i–3e.iv: | |
| 3e.i | 101 | not P2PKH/P2SH | `bad-protx-shares-payee` |
| 3e.ii | 105 | `!ExtractDestination` | `bad-protx-shares-payee-dest` |
| 3e.iii | 108 | destination `== keyIDVoting` | `bad-protx-shares-payee-reuse` |
| 3e.iv | 110-112 | destination `==` the owner of any share in the list, in list order, including later shares | `bad-protx-shares-payee-reuse` |
| 4 | 118 | after the loop: sum `!= required_collateral` | `bad-protx-shares-amount-sum` |
| 5 | 121 | `early_penalty < 0 \|\| early_penalty >= min share amount` | `bad-protx-shares-penalty` |
| 6 | 127 | `early_period_blocks == 0 && early_penalty != 0` | `bad-protx-shares-penalty` |

`IsShareListVotingKeySafe(shares, voting)` (`:132-144`) returns false when any share's refund script or `RewardScript()` has a destination equal to `voting`.

`IsPayoutListKeySafe(payouts, collateral_dest, owner, voting, check_payout_collateral_reuse)` (`:146-164`): `collateral_dest == owner || == voting` → `bad-protx-collateral-reuse` (152); then, if `check_payout_collateral_reuse`, any payout destination `== collateral_dest` → `bad-protx-payee-reuse` (159).

### 3.15 Provider network fields (`providertx.cpp:166-263`)

`IsNetInfoTriviallyValid` (`:166-185`), all S: no `CORE_P2P` entries → `bad-protx-netinfo-empty` (170); `nType == Regular` with platform entries → `bad-protx-netinfo-bad` (175); `CanStorePlatform() && nType == Evo` missing `PLATFORM_HTTPS` or `PLATFORM_P2P` → `bad-protx-netinfo-empty` (181).

`CheckNetInfo` (`:187-210`) maps `netInfo->Validate()` to an S reject:

| Status | Line | Reason |
|---|---|---|
| `BadAddress` | 191 | `bad-protx-netinfo-addr` |
| `BadPort` | 193 | `bad-protx-netinfo-port` |
| `BadType` | 195 | `bad-protx-netinfo-addr-type` |
| `NotRoutable` | 197 | `bad-protx-netinfo-addr-unroutable` |
| `Malformed` | 199 | `bad-protx-netinfo-bad` |
| `BadInput` | 203 | `bad-protx-netinfo-entry` |
| `Duplicate` | 205 | `bad-protx-dup-netinfo-entry` |
| `MaxLimit` | 207 | `bad-protx-netinfo-maxlimit` |

`CheckProviderNetworkFields(net_info, type, version, platformNodeID, p2p, http, allow_empty)` (`:212-263`):

| Step | Line | Condition | Cl | Reason |
|---|---|---|---|---|
| 1 | 217 | `!net_info \|\| CanStorePlatform() != (version >= ExtAddr)` | C | `bad-protx-netinfo-version` |
| 2a | 221 | `IsEmpty() && !allow_empty` | S | `bad-protx-netinfo-empty` |
| 2b | | not empty: `IsNetInfoTriviallyValid`, then `CheckNetInfo` | | above |
| 3 | | `type != Evo` → accept | | |
| 4 | 231 | `platformNodeID.IsNull()` | S | `bad-protx-platform-nodeid` |
| 5a | 235 | `version >= ExtAddr && p2p != 0` | S | `bad-protx-platform-p2p-port` |
| 5b | 238 | `version >= ExtAddr && http != 0` | S | `bad-protx-platform-http-port` |
| 6a | 245, 248 | `version < ExtAddr && IsNodeOnMainnet()`: `p2p != MainParams().GetDefaultPlatformP2PPort()` / `http != …PlatformHTTPPort()` | S | `bad-protx-platform-p2p-port` / `-http-port` |
| 6b | 252, 255 | `version < ExtAddr`: `p2p == MainParams().GetDefaultPort()` / `http == …` | S | `bad-protx-platform-p2p-port` / `-http-port` |
| 6c | 258-260 | `core_port = net_info->GetPrimary().GetPort()` (257); `p2p == http`, or either `== core_port` | S | `bad-protx-platform-dup-ports` |

Step 6c is reached only for `type == Evo` with `version < ExtAddr` (`version >= ExtAddr` returns at 233-241) and with platform ports that pass steps 6a and 6b (243-255). ProReg is the only caller with `allow_empty`; for it, Evo with `version < ExtAddr` means version 2, because a version-1 Evo ProReg is rejected earlier with `bad-protx-evo-version` (`providertx.cpp:270-272`). In that context, when `net_info` is empty, `MnNetInfo::GetPrimary` returns `CService{}` (`netinfo.cpp:343-349`), so `core_port` is 0 and `platformP2PPort == 0` or `platformHTTPPort == 0` gives `bad-protx-platform-dup-ports` (260). On mainnet, step 6a rejects a zero port first (D31).

### 3.16 NetInfo `Validate()` (`netinfo.cpp`)

`IsNodeOnMainnet()` (`:69`) is `Params().NetworkIDString() == MAIN`. `MainParams()` (`:77-80`) is the mainnet `CChainParams`. In Rust, both come from `ChainParams { network_id, default_port, … }`, and `MainParams()` is `ChainParams::mainnet()`, a `const` (CS0).

`MnNetInfo::Validate` (`:351-381`): an entry that is not `IsTriviallyValid()` → `Malformed`. Otherwise `ValidateService` (`:286-305`), in order:
1. `!IsValid()` → `BadAddress`;
2. `!IsIPv4()` → `BadType`;
3. `RequireRoutableExternalIP() && !IsRoutable()` → `NotRoutable`;
4. `IsNodeOnMainnet() != (port == MainParams().GetDefaultPort())` → `BadPort`.

`IsAllowedPlatformHTTPPort(port)` (`:59-66`) is `port == 443`.

`ExtNetInfo::Validate` (`:588-638`):
1. `m_version == 0 || m_version > CURRENT_VERSION || m_data.empty()` → `Malformed`;
2. `HasAddrPortDuplicates()` (`:383-392`, over every entry of every purpose) → `Duplicate`;
3. for each purpose in map order, all of 3a–3e for that purpose before the next purpose:
   - 3a. `!IsValidPurpose(purpose)` → `Malformed`;
   - 3b. `entries.empty()` → `Malformed`;
   - 3c. `entries.size() > MAX_ENTRIES_EXTNETINFO` (4, `netinfo.h:20`) → `MaxLimit`;
   - 3d. `HasAddrDuplicates(entries)` (`:401-418`, by `ToStringAddr()` string) → `Duplicate`;
   - 3e. for each entry of this purpose, in order:
     - `!IsTriviallyValid()` → `Malformed`;
     - a `CService` → `ValidateService(purpose, service)` (`:452-478`), in order: `!IsValid()` → `BadAddress`; not CJDNS/I2P/IPv4/IPv6/Tor → `BadType`; `RequireRoutableExternalIP() && !IsRoutable()` → `NotRoutable`; I2P with port `!= I2P_SAM31_PORT` (0) → `BadPort`; otherwise port 0, or `IsBadPort(port) && !(purpose == PLATFORM_HTTPS && IsAllowedPlatformHTTPPort(port))` → `BadPort`;
     - a `DomainPort` in a purpose other than `PLATFORM_HTTPS` → `BadInput`, else `ValidateDomainPort` (`:480-498`): `!domain.IsValid()` (`DomainPort::Validate`, `:130-…`, over `ValidateDomain` `:90-113`: `BadLen`, `BadChar`, `BadCharPos`, `BadDotless`, `BadLabelLen`, `BadLabelCharPos`) → `BadInput`; port 0, or `IsBadPort(port) && !IsAllowedPlatformHTTPPort(port)` → `BadPort`; `ToStringAddr()` matching a `TLDS_BAD` or `TLDS_PRIVACY` suffix → `BadInput`; the last `.`-label not all `SAFE_CHARS_ALPHA` → `BadInput`;
     - neither → `Malformed`.
4. → `Success`.

`HasAddrPortDuplicates` (`:383-392`) inserts every entry of `m_all_entries` into a `std::set<NetInfoEntry>`, so it compares by `NetInfoEntry` ordering, not strings. `HasAddrDuplicates` compares the full `ToStringAddr()` strings of one purpose's entries.

**String storage (D15).** Every `ToStringAddr()`/`ToStringAddrPort()` value EV1 forms is compared or passed whole, never truncated. A `DomainPort` address is borrowed as `&str` (up to `DOMAIN_MAX_LEN` = 253 bytes, `netinfo.h:25`). Every other form is written into one stack buffer type, `AddrStr = ArrayString<NETINFO_ADDR_MAX>`. Here `NETINFO_ADDR_MAX` = 253 + 2 (IPv6 brackets) + 1 (`:`) + 5 (port) = 261, which covers the domain-and-port form and every service form. A unit test formats the longest form of each network kind (IPv4, IPv6, Tor v3, I2P, CJDNS, internal, domain) and asserts that it fits.

`ExtNetInfo::IsEmpty()` is `m_version == 1 && m_data.empty()`.

The base-sdk `NetInfoV2` keeps the first of a repeated purpose and yields groups sorted by purpose. Core's `m_data` is a `std::map<uint8_t, …>` built by the deserializer. The equivalence of "map order" and "first wins" is pinned by L0 vectors decoded through both (§6.1) before `validate_ext` relies on it.

`AddEntry` (`MnNetInfo :307-349`; `ExtNetInfo :500-586`, with `ProcessCandidate :420-450`) is the string round trip that `SetStateVersion` and `ApplyProUpServTx` use (`specialtxman.cpp:51-137`). Its steps:
- `SplitHostPort` with a default port of 0 (Ext) or the network default (Mn);
- the character filters;
- `SetSpecial` for privacy TLDs;
- `DomainPort::Set`;
- numeric `Lookup`;
- `MaybeFlipIPv6toCJDNS`;
- `ValidateService`;
- `ProcessCandidate`.

The Mn kind accepts only `CORE_P2P` and only when it is empty. A failure surfaces in `SetStateVersion`/`ApplyProUpServTx` as `bad-protx-netinfo-version` (`specialtxman.cpp:38, 94`).

#### 3.16.1 The NetInfo step of the scratch migration

The type-2 and type-3 checks run `ApplyProUpServTx` / `SetStateVersion` on a copy of the state and forward the failure reason as `TX_BAD_SPECIAL` (`specialtxman.cpp:1363-1367, 1471-1476`). `SetStateVersion(state_mn, nVersion, nType)` (`:51-106`), in order:

| Step | Line | Core | Reject |
|---|---|---|---|
| 1 | 54 | `needs_extended = nVersion >= ExtAddr` | |
| 2 | 57-60 | payout-list move | none |
| 3 | 70-73 | operator key `Set(pubkey, nVersion == LegacyBLS)` | none |
| 4 | 74-75 | `state_mn.nVersion == nVersion && state_mn.netInfo->CanStorePlatform() == needs_extended` → return true | |
| 5 | 78 | `converted = MakeNetInfo(nVersion)` (`ExtNetInfo` if `nVersion >= ExtAddr`, else `MnNetInfo`; `netinfo.cpp:277-284`) | |
| 6a | 80-82 | `needs_extended`: `AddNetInfoEntries(converted, CORE_P2P, old CORE_P2P entries)` | `bad-protx-netinfo-version` (`:38`) |
| 6b | 85-89 | `needs_extended && old CanStorePlatform()`: the same for `PLATFORM_P2P`, then `PLATFORM_HTTPS` | `bad-protx-netinfo-version` (`:38`) |
| 6c | 91-94 | `needs_extended && !old CanStorePlatform() && nType == Evo && !old IsEmpty()` | `bad-protx-netinfo-version` (`:94`) |
| 6d | 96-97 | `needs_extended`: zero the two platform ports | none |
| 7 | 98-100 | `!needs_extended`: `AddNetInfoEntries(converted, CORE_P2P, old CORE_P2P entries)` | `bad-protx-netinfo-version` (`:38`) |
| 8 | 103-105 | assign the version and `converted` | |

`AddNetInfoEntries(net_info, purpose, entries)` (`:33-42`): for each entry in order, `net_info->AddEntry(purpose, entry.ToStringAddrPort()) != Success` → `bad-protx-netinfo-version`.

EV1 ports steps 1, 4, 5, 6a–6c and 7 as `netinfo::migration_reject(ni, cur_version, target, mn_type, params)`. Here `cur_version` is the copy's `nVersion` when `SetStateVersion` is entered, and the call sites' inputs are in §3.4 and §3.5. It builds the converted `NetInfo` in a private scratch value inside `migration.rs`, returns the reject or `None`, and mutates nothing outside that local value. The converted value is not exported (D24). Steps 2, 3, 6d and 8 have no reject and are not ported (D9). Every reject on this path comes from the NetInfo steps, so no non-NetInfo reject is left open for EV2b.

**EV2b equality (EV2b's acceptance).** EV2b's `migrate::set_state_version` implements its NetInfo step itself, over `netinfo::add_entry`. The test `migrate_netinfo_agrees_with_ev1_predicate` (property-based, plus every §6.1 NetInfo vector as seed) asserts, for every `(state, target, mn_type)`, that `set_state_version` fails with `bad-protx-netinfo-version` exactly when `migration_reject(&state.net_info, state.version, target, mn_type, params)` is `Some`. That compares two implementations of Core's steps: EV2b's conversion and EV1's predicate. The converted `net_info` EV2b produces on success is checked by EV2b against Core (EV2 acceptance), not against EV1.

### 3.17 Shared-collateral covenant (`specialtxman.cpp:1815-1849`)

`IsSharedCollateralScript(s)` is a byte compare with `04 44534843 75 51` (push4 "DSHC", `OP_DROP`, `OP_TRUE`; `sharedcollateral.h`).

`CheckSharedCollateralSpends(tx, view)`: if the tx is a coinbase, accept. Then for each input whose `view.GetCoin(prevout)` is an unspent coin paying the template, `!IsSpecialTxVersion() || nType != PROVIDER_DISSOLVE` → C `bad-shared-collateral-spend` (1826).

`CheckSharedCollateralTemplateOutputs(tx)`: for each output `i` paying the template:
- `!IsSpecialTxVersion() || nType != PROVIDER_REGISTER` → C `bad-shared-collateral-create` (1840);
- the payload does not decode, `!IsShared()`, `!collateralOutpoint.hash.IsNull()` or `collateralOutpoint.n != i` → C `bad-shared-collateral-create` (1845).

The coinbase is checked too.

Call sites (the callers' obligations, D7):

| Path | Where | Gate | Order |
|---|---|---|---|
| mempool | `validation.cpp:1010-1018` (PreChecks, after `CheckTxInputs`; spends read `m_view`, the mempool input cache) | V24 active after the tip | spends, then template outputs |
| block, template outputs | `specialtxman.cpp:853-862` (`ProcessSpecialTxsInBlock` loop, every tx incl. coinbase) | `rules.v24` | before that tx's `CheckSpecialTxInner` |
| block, spends | `validation.cpp:2700-2716` (ConnectBlock tx loop, after `CheckTxInputs`) | `DeploymentActiveAt(pindex, V24)` | per tx |
| miner | `miner.cpp:410` | as the block | |

### 3.18 Signed hashes and strings

| Preimage | Core | Rust |
|---|---|---|
| ProTx `SerializeHash(ptx)` (the payload, `vchSig`/`sig` excluded by the serializer's `SER_GETHASH`) | `specialtxman.cpp:1070, 1089` | `sig::payload_sig_hash::<T>`, streaming the BS6a encoder in hash mode |
| `MakeSignString` | `providertx.cpp:375-395` | `sig::make_sign_string` renders through a `fmt::Write` sink twice: a counting pass for the compact-size length the message hash needs, then a hashing pass. No buffer, so nothing can truncate (D15). Payouts render through the network's base58 prefixes from `ChainParams` |
| `CMessageSigner` hash | `hash(MESSAGE_MAGIC ‖ str)` with compact-size prefixes, magic `"DarkCoin Signed Message:\n"` | `sig::message_hash` |
| `MakeSharedRegConsentHash(tx)` | `providertx.cpp:335-373` | `sig::shared_reg_consent_hash` |
| `CProDisTx::MakeSignHash(tx, n)` | `providertx.cpp:506-533` | `sig::pro_dis_sign_hash` |
| `CalcTxInputsHash` | `providertx.h` | `sig::inputs_hash` (stream every prevout) |
| MNHF `msgHash` | `mnhftx.cpp:139-143` | `sig::txid_with_payload_override`: txid computed by streaming the tx with the payload re-encoded with a null signature; no tx copy |
| AU `msgHash` | `assetlocktx.cpp:223-229` | `sig::serialize_hash_with_payload_override` (full serialization, not txid) |
| LLMQ `SignHash(type, quorumHash, id, msgHash)` | `llmq/signhash` | `dash_evo::llmq::sign_hash` (LQ's; EV1 consumes) |

L0 cross-checks every row against `oracle special_tx_sign_hash` (`dash-oracle-sys/src/mnlist.rs:492`), or against Core unit vectors where the oracle does not expose it.

---

## 4. Persistence

Not applicable. EV1 is a pure function of its context and writes nothing.

---

## 5. Performance

### 5.1 Hot path

The block path calls `check_special_tx_inner` once per special tx; mainnet blocks carry 0–30 such txs, mostly types 5, 6 and 8/9. Rules:

1. **No per-check heap allocation where Core has none.**
   - Payloads are borrowed from `BlockPayloads`.
   - Reasons are `&'static str`.
   - The ProDis per-share loop, the NetInfo dedupe (≤ 4 entries per purpose, ≤ 3 purposes) and the scan buffer for `ScanQuorums` (`MAX_QUORUM_SCAN` = 33 at the pin, D14) use `ArrayVec`.
   - The address dedupe borrows domain strings and formats service addresses into `AddrStr` (261 bytes, D15). Nothing is truncated.
   - `migration_reject` builds its scratch `NetInfo` only when Core's early return (§3.16.1 step 4) is not taken.
2. **No transaction copies.** Core builds `CMutableTransaction` copies for the MNHF and AU message hashes. EV1 streams the serialization with the payload override straight into the hasher (§3.18); L1 proves the hashes equal.
3. **One list lookup per key.** `has_unique` followed by `unique_owner` (Core's `HasUniqueProperty` + `GetUniquePropertyMN`) collapses to one `unique_owner` call. That changes no outcome, because both read the same immutable list.
4. **Signatures are the cost.** BLS `VerifyInsecure` dominates. Batch verification (A1:1695-1707) is a later slice (EV1-6): the obligations are recorded in block order, and on the first non-signature reject the earlier pending obligations are resolved individually first, so the first reject in block order and its string are preserved. Legacy-scheme signatures are always verified individually.

### 5.2 PF0 workloads and benches

| Bench | Input | Counter |
|---|---|---|
| `ev1_block_mainnet_mix` | the 64 mainnet blocks with the most special txs in 2,100,000–2,140,000 (captured once, `node-bench` fixture) with `check_sigs = false` | `instructions:u` per block |
| `ev1_pro_reg_external` | ProReg v2, external collateral, 8 payouts, `check_sigs = true` (ECDSA message) | per check |
| `ev1_pro_up_serv_ext` | ProUpServ v3 Evo, 3 purposes × 4 entries, v24 migration copy | per check |
| `ev1_netinfo_validate_ext` | `validate_ext` on a 12-entry ExtNetInfo | per call |
| `ev1_pro_dis_8` | ProDis, 8 shares, unanimous, `check_sigs = true` | per check |
| `ev1_asset_unlock` | AU v2 with a 25-quorum scan (model facade) | per check |

The BG row is "crit special-tx checks". The PLAN gives it no ratio, so PF0 records the baseline at merge, and the counter gate guards regressions only.

---

## 6. Verification

### 6.1 L0: vectors and unit tests

- **Core unit vectors.** These are ported from `src/test/evo_trivialvalidation.cpp` (with its JSON data `src/test/data/trivially_invalid.json` and `trivially_valid.json`), `evo_netinfo_tests.cpp`, `evo_assetlocks_tests.cpp`, `evo_mnhf_tests.cpp`, `evo_sharedmn_tests.cpp`, `evo_cbtx_tests.cpp` and the special-tx cases of `evo_deterministicmns_tests.cpp`, at the pin. Each keeps Core's expected reason.
- **Both BLS encodings.** Every trivially-valid/invalid vector that carries an operator key or BLS signature runs twice: once with the key in the legacy encoding and once in the basic encoding. Both must give Core's verdict for that encoding (`bad-protx-operator-pubkey` when the encoding and version disagree). Legacy points use BS11a's exact decoder; the vectors that need it merge with BS11a (D10).
- **Signature decode across schemes (D22).** ProUpServ v2/v3 and ProUpRev v2/v3 with `check_sigs = false`: signature bytes canonical only under the legacy scheme decode to the invalid signature and are **accepted**; bytes canonical under neither scheme give `bad-protx-payload`. The mirrored v1 cases (canonical only under basic; under neither) run too. Expected verdicts from `oracle_check_special_tx` at the pin.
- **Operator-key equality (D17).** States: the same object; default; raw (`bufValid`) basic; raw legacy; decoded (`Set`) basic; decoded legacy; raw+decoded basic; raw+decoded legacy; raw undecodable or malleable, never read; raw undecodable or malleable, then read once (the failed `Get()` clears `bufValid`, so the key then compares and serializes as default, zeros; `bls.h:492-511`, `:502, 506`). The matrix is ptx state × state-side state, each cell with the same and with different key bytes where the states allow it. Expected values are Core's `CBLSLazyWrapper::operator==` at the pin, taken from the BS12 corpus or base-sdk `lazy_equality_follows_core`.
  - **eq half (full cross-product).** Each cell asserts `<OperatorKey as PartialEq>::eq`, each side's flags afterwards, and each side's serialization when a decode failed.
  - **Oracle half (through ProUpReg, `v24` active, against `oracle_check_special_tx`).** The ptx key is a valid raw+decoded key in its version's scheme (`IsTriviallyValid`'s `Get()`, `providertx.cpp` rc.4:458; D42); the state-side key is raw, as deserialized from the `SER_DISK` list. Cases: same key, same scheme; same key, other scheme (`key_changed` false, `migrating` true); different key, same scheme; different key, other scheme; bytes canonical in both schemes that decode to different points; a raw-undecodable state key if EV2a's list codec can hold one, otherwise recorded as unconstructible with the codec's reason. Each case is crossed with the 5a/5b outcomes it allows: another MN holding the key under either scheme (`bad-protx-dup-key`), and the `!key_changed` migration path with a state whose `migration_reject` is `Some`. Cells with any other ptx state are marked unconstructible through ProUpReg, citing `providertx.cpp` rc.4:458.
- **Unknown ProTx versions (D20).** Types 1–4, versions 0, 4 and 0xffff: a payload of only the 2 version bytes gives `bad-protx-version` (C); the same with trailing bytes gives `bad-protx-payload` (C). Expected verdicts from `oracle_check_special_tx`; the template-output covenant result is asserted unchanged.
- **Empty netInfo platform ports (D31).** ProReg Evo with an empty `netInfo` (`allow_empty`) and a non-null `platformNodeID`, all through `oracle_check_special_tx`: (a) testnet or regtest, version 2, `platformP2PPort == 0` and `platformHTTPPort` nonzero, not `MainParams().GetDefaultPort()` → `bad-protx-platform-dup-ports` (260); (b) the same with the two ports swapped → `bad-protx-platform-dup-ports`; (c) mainnet, version 2, `platformP2PPort == 0` → `bad-protx-platform-p2p-port` (245); (d) version 3 (`ExtAddr`, empty `ExtNetInfo`), both ports 0 → no reject from `CheckProviderNetworkFields` (returns at 241); (e) testnet, version 1, ports as in (a) → `bad-protx-evo-version` (`providertx.cpp:270-272`).
- **Census closure.** Every census row (§3.13) is reached by at least one vector with the exact reason and class, except the rows marked `unreachable` in the census with their Core reason (§3.13), which closure accepts explicitly.
- **Signed preimages.** Every preimage in §3.18 is checked against `special_tx_sign_hash` across 1,000 generated payloads per type.
- **Signed preimages over non-canonical wire forms (D23).** The ProUpServ `SerializeHash(ptx)`, the ProReg `MakeSignString` and `MakeSharedRegConsentHash`, each over a wire payload whose ExtNetInfo repeats a purpose and one whose purposes are out of order. The Rust hash is computed from the decoded payload and must equal `special_tx_sign_hash` on the same wire bytes. A contrast value is computed by the same preimage construction with the `netInfo` field's wire bytes spliced in place of its re-encoding (signature excluded as in the real preimage); a vector asserts the Rust hash differs from it.
- **NetInfo order.** One vector per §3.16 status, with `Params()` set to each of mainnet, testnet, regtest and devnet. One vector per repeated-purpose ExtNetInfo encoding pins the base-sdk "first wins, sorted" decode against Core's map.
- **NetInfo nesting (M6).** On a network with `RequireRoutableExternalIP()`: purpose `CORE_P2P` holds one structurally valid unroutable service, and purpose `PLATFORM_P2P` holds five distinct entries. Core returns `NotRoutable` (`bad-protx-netinfo-addr-unroutable`), not `MaxLimit`. The mirrored vector puts the five entries in the lower purpose and expects `MaxLimit`.
- **Long addresses (D15).** Two distinct 253-byte-capable domains that share their first 64 bytes, in one `PLATFORM_HTTPS` purpose, give no `Duplicate`. The same long domain twice gives `Duplicate`. A maximal domain-and-port string goes through `add_entry`. A Tor v3 and an I2P service are dedupe-compared.
- **Direct NetInfo statuses.** `Validate()` and `AddEntry` statuses that no special-tx verdict can expose (statuses preempted by `IsNetInfoTriviallyValid` or the trivial checks; every `AddEntry` status other than the migration's success/failure) get direct vectors from `evo_netinfo_tests.cpp`, plus new cases computed by Core's functions at the pin through the A-3 entry points once they land.
- **Migration predicate.** One vector per §3.16.1 reject row (6a, 6b, 6c, 7) and per early-return case, through ProUpServ and ProUpReg against `oracle_check_special_tx` (the reject is a tx verdict there).
- **Coins views (D13, D26).** (a) Mempool: a ProReg whose external collateral is an output of an unconfirmed mempool tx → `bad-protx-collateral`, because `ctx.coins` is `CoinsTip`. (b) Mempool: a non-ProDis tx spending a shared-collateral template output of an unconfirmed mempool ProReg → `bad-shared-collateral-spend`, because the covenant reads the mempool input cache. In EV1-3 both run against a model `CoinsView` pair (tip view and mempool input cache). The same scenarios with MP1a's real binding, asserted against the pinned dashd (L3), are in MP1a's acceptance.
- **Quorum scan capacity (D14).** A devnet with `-llmqplatform=llmq_60_75` (32 signing-active, so Core scans 33) and at least 34 mined Platform quorums, all retained as known quorums. An asset unlock signed by the 33rd-newest quorum passes step 11. One signed by the 34th-newest gives `bad-assetunlock-too-old-quorum`. Both run against the model facade (L0, EV1-5). The run against the pinned dashd on that devnet (L3) is in the joint certification (§6.3, D27).
- **Types 7 and 9 with `check_sigs = false` (EV1-5).** An MNHF with a bad signature gives `bad-mnhf-invalid`, and an asset unlock with a bad signature gives `bad-assetunlock-not-verified`, with `check_sigs = false` on the block binding and the model facade. The mempool binding cannot pass `false`: `CheckSpecialTxForMempool` hard-codes `check_sigs = true` (`validation.cpp:805-808`). It is tested separately with that fixed value, and a test asserts that MP1a's binding sets it.
- **Coinbase on the block path (D18, with A-1).** Through `oracle_check_special_tx_block` and CH7a's binding: a pre-DIP3 regtest block whose coinbase is a valid v1 or v2 CbTx is accepted. "Valid" includes `merkleRootMNList` = the root of the empty list and, for v2, the computed `merkleRootQuorums` (`specialtxman.cpp:924-955`, gated on `opt_cbTx`, not on DIP3), so the vector holds whether A-1 covers the whole of `ProcessSpecialTxsInBlock` or only the loop; the run report records which; a block checked with `check_sigs = false` whose CbTx height is wrong gives no CbTx reject; a block whose coinbase fails `CheckCbTx` and whose tx 0 also fails the template-output check gives the `CheckCbTx` reason.

### 6.2 L1: `diff_check_special_tx`

- **Harness:** `node-l1/tests/l1_specialtx.rs`, std-only, against `Oracle::check_special_tx(chain, tx, ctx, check_sigs)` (`dash-oracle-sys/src/context.rs:322`).
- **Generator:** builds a synthetic pprev `MnList` (serialized for the oracle as `SER_DISK`), coins (`Unchecked`/`AtTip`), ancestry, version-bits deployments and the BLS flag. It then mutates a valid tx of each type: versions, keys in both encodings, payouts, shares, NetInfo entries in every purpose and kind, collateral modes, ports, `check_sigs` true and false. The default is k ∈ {1, 2, 3} simultaneous mutations per case.
- **Pairwise first reject (D19).** For every table in §3 and every ordered pair of checks in it, the generator emits cases that violate both, including across entities: share k with share k+1, payout k with payout k+1, purpose p with purpose p+1, non-actor share k with k+1 (§3.7). The expected reject is the first one in Core's execution order: (entity index, step) inside a per-element loop, table step order outside loops. Expected values are taken from the oracle, including for L0 pair tests.
- **Pair coverage.** Coverage is measured over feasible pairs with distinct reject strings. `contrib/node/ev1-pairs.toml` (checked in with EV1-2, validated by `ev1-census.py`) lists per table the excluded pairs, each with its Core reason: unreachable steps (§3.14 2d and 3e.ii, see §3.13); pairs whose conditions cannot hold together (for example the two bounds of 3a on one share); pairs with the same reject string, which a verdict cannot tell apart (3e.iii / 3e.iv `bad-protx-shares-payee-reuse`; 5 / 6 `bad-protx-shares-penalty`; §3.7 8a / 9a `bad-prodis-payee`; and every other equal-string pair the script finds). The script fails on an excluded pair that names a missing step or carries no reason. The merge-gate report lists pair coverage per table next to census coverage.
- **Legacy points:** drawn from the BS12 corpus until BS11a lands. This is a measurement limit only: it bounds what the run measured, and it is not the parity claim, which rests on BS11a (D10).
- **Assertion:** verdict, reason and class equal.
- **Coverage:** types 1–6, 8 and 10–12, with type 5 limited to non-coinbase transactions: for those, the oracle's mempool path is Core's block-path code, so this covers both paths. The block-path coinbase rule (§3.2 "Block path") is covered by the D18 vectors through A-1.
- **Legacy verify (D30).** Until A-4 is settled, cases with `check_sigs = true` whose global flag is legacy are excluded from L1 and counted in the run report. Until A-4 resolves, L0 for these cases asserts the fail-stop `SpecialTxFault::VerifyUnavailable` (§0 item 4, D2), not Core's verdict.
- **Types 7 and 9:** `oracle_check_special_tx` returns `E_CONTEXT` for them once DIP3 is active, even for early rejects. They join through `oracle_check_special_tx_block` (A-1) at EV1-5, whose merge it gates (§6.3).

Merge gate: ≥ 24 CPU-h with no disagreement, plus the census rows reached by the generator listed in the run report.

### 6.3 L2/L3: types 7, 9 and the block-path variants

A-1 (`oracle_check_special_tx_block`) is a merge prerequisite for EV1-5. At EV1-5, types 7 and 9 and the `chain`/`indexes` variants join §6.2 with the same gate. In addition, as a second and independent check, L2 and L3 below form a joint certification row with CH7a and MP1a: they run once CH7a and MP1a are in the node under test, and they are not part of EV1-5's merge gate (D27):

- **L2:** replay of mainnet and testnet history through CH7a with EV1 in place. Every MNHF (7) and asset-unlock (9) tx in history must pass, and the block verdicts must equal Core's (O8 StateDigest windows that contain types 7 and 9).
- **L3 negative matrix.** Run on regtest (and on the D14 devnet) against the pinned dashd, both through the mempool (`sendrawtransaction` reject string) and in a block (`submitblock` / `getblocktemplate` proposal), with EV1 in the node under test. The matrix has one row per reachable context branch of §3.12 and one row per reachable first-reject pair; each row asserts the same reject string from both nodes.

| Type | Single branches | First-reject pairs (expected winner first) |
|---|---|---|
| 7 | `bad-mnhf-non-ehf` (the bit of an in-window deployment without `useEHF`); `bad-mnhf-quorum-hash` unknown hash; `bad-mnhf-quorum-hash` known block off the active chain; `bad-mnhf-missing-quorum` (known ancestor that is not an `llmqTypeMnhf` quorum); `bad-mnhf-invalid`; block path below assumevalid (`check_sigs = false`) + bad sig → `bad-mnhf-invalid` | non-ehf > quorum-hash; quorum-hash (unknown) > invalid sig; quorum-hash (off-chain) > missing-quorum; missing-quorum > invalid sig |
| 9 | `bad-assetunlock-duplicated-index` (block path only); `bad-assetunlock-quorum-hash` unknown hash; `bad-assetunlock-too-old-quorum` (known block, not in the scan; and an expired quorum); `bad-assetunlock-too-late` (tip below `requestedHeight`; tip at `requestedHeight + 48`); `bad-assetunlock-not-verified`; block path below assumevalid (`check_sigs = false`) + bad sig → `bad-assetunlock-not-verified`; the expired-AU mempool retry at `first_height`/`last_height` | duplicated-index > quorum-hash; quorum-hash > too-old; too-old > too-late; too-late > not-verified; payload-level rejects (`-version`, `-too-many-outs`) > every context reject |

`bad-assetunlock-quorum-internal-error` is listed as unreachable on a consistent node (the scan found the hash, `get_quorum` did not). It is covered by L0 on the model facade only, and the matrix records it as not reachable.

L0 census closure (§6.1) stays a separate check.

### 6.4 Covenant

There is no oracle entry for `CheckSharedCollateralSpends` alone. Coverage:
- template outputs: through `oracle_dml_block`, plus the `diff_check_special_tx` generator placing template outputs on every type;
- spends: L0 vectors from `evo_sharedmn_tests.cpp` and `feature_masternode_shares.py` (L3).

A-2 asks for an oracle entry for spends.

### 6.5 Fuzz targets (DEC-184 plateau policy, DEC-186 `fuzzctl certify`)

| Target | Oracle | Property |
|---|---|---|
| `specialtx_check_diff` | `check_special_tx` | §6.2 under libFuzzer, structured generator with k ∈ {1, 2, 3} simultaneous mutations and the pairwise first-reject cases (D19), all types the oracle covers |
| `specialtx_trivial` | none | the decode → `validated_payload` chain never panics, is deterministic, and gives one verdict per encoding |
| `netinfo_validate` | until A-3: `check_special_tx` on ProReg v3 / ProUpServ wrappers whose other fields pass every earlier check; after A-3: `oracle_netinfo_validate` | EV1-1: panic-free and deterministic. From EV1-2 (which lands the trivial checks the wrappers reach first, D29) until A-3: **tx verdict** equal (not the status) for inputs that reach `CheckNetInfo`; after A-3: `Validate()` status equal per network |
| `netinfo_add_entry` | until A-3: none, plus the migration predicate through `check_special_tx` (ProUpReg from EV1-3, ProUpServ from EV1-4, whose state migration runs); after A-3: `oracle_netinfo_add_entry` | until A-3: panic-free and deterministic, and `migration_reject` agrees with the tx verdict from the slice that lands the type (D29); after A-3: `add_entry` status and resulting entry equal |
| `prodis_arith` | in-crate `arith_uint256` reference (`dash-consensus` `U256`) | `ProRataFloor` and the fee/bonus sums equal for all `u64` inputs. The zero-divisor path maps to `failed-check-special-tx`; consensus state cannot reach it (§3.7), so this case is defensive |
| `shared_collateral` | `oracle_dml_block` for outputs; A-2 for spends | covenant verdicts equal |

Each target gets a `fuzzctl certify` run at merge and follows the DEC-184 plateau rule (kill or rework a leg once it plateaus). The differential status/entry properties of `netinfo_validate` and `netinfo_add_entry` are claimed only once A-3 lands; until then those targets certify the narrowed properties above, and §6.1's direct vectors cover the statuses they cannot reach.

### 6.6 Conformance register rows

| Row | Test | Input |
|---|---|---|
| 21 | `conformance/tests/quirks.rs::quirk_21_protx_sig_scheme_follows_height` (status scaffold → implemented in EV1-4) | ProUpServ/ProUpRev signatures across the V19 boundary, with the global flag taken from the context |
| 26 | `quirk_26_*` (EV1 half) | ProReg whose external collateral is created earlier in the same block → `bad-protx-collateral` against the pprev coins view |
| 27 | `quirk_27_*` | external-collateral ProReg sign strings on mainnet, testnet and regtest params |

### 6.7 Functional tests (L4, with CH7a and MP1a)

`feature_dip3_deterministicmns.py`, `feature_protx_version.py`, `feature_llmq_evo.py`, `feature_evo_shares_activation.py`, `feature_mnehf.py`, `feature_asset_locks.py`, `feature_masternode_shares.py`, `feature_masternode_payout_shares.py`, `rpc_netinfo.py`. Each must pass with the node under test in place of dashd, wherever the test exercises a special-tx reject.

---

## 7. Implementation plan

Six slices on `node/ev1-*` branches, each reviewed Sol → Opus high, then gated and PF0'd.

| Slice | Content | Depends on | Checkpoint (merge gate) |
|---|---|---|---|
| **EV1-1** | `dash-netaddr`: the pure parts of dash-p2p extracted, dash-p2p re-exporting them, Core vectors for anything missing (D28); `reasons` + census script (with the nonliteral classification table); `netinfo` (validate both kinds, `add_entry`, `migration_reject`, `CoreNetInfoValidity`); `network` (`CheckProviderNetworkFields`, `CheckNetInfo`); EV2a's codec switched to `CoreNetInfoValidity` in node builds | EV0, CS3, BS6a | L0 NetInfo per network, incl. the nesting, long-address and direct-status vectors; `dash-netaddr` vectors; `netinfo_validate`, `netinfo_add_entry` certified panic-free and deterministic (§6.5, D29); census script and classification table |
| **EV1-2** | `payload` (`validated_payload`, `version_max`, `BlockPayloads`/`PayloadSlot`); `trivial` (every `IsTriviallyValid`, payout/share lists); `sig` ECDSA paths and preimages (§3.18 rows except the MNHF/AU ones); `SigVerifier` trait with a test double | EV1-1, **BS11a** (exact legacy decode; merge gate, D10) | L0 trivially valid/invalid, both encodings, exact codes; the D20 and D22 vectors; census closure for `providertx.cpp` at rc.4 (D32); preimages vs `special_tx_sign_hash`, incl. the D23 wire forms rc.4 still decodes (D33); `specialtx_trivial` certified; `netinfo_validate` certified on its tx-verdict property (D29, D34) |
| **EV1-3** | dispatcher; types 1, 3, 10, 11, 12 and ProDis-for-list; covenant; `prodis_arith` | EV1-2 (so BS11a), EV2a | L1 `diff_check_special_tx` for 1, 3, 10–12 with `check_sigs = false` ≥ 24 CPU-h, incl. pairwise first-reject coverage (D19); covenant L0 and the D13 coins-view tests on a model `CoinsView` (D26); the D17 vectors; `netinfo_add_entry` re-certified with the ProUpReg migration agreement (D29); register row 26 |
| **EV1-4** | type 2, type 4; BLS verify through `SigVerifier` bound to BS12 (basic) and the legacy path (D2) | EV1-3, BS12 (serial verify) | L1 for 1–4, 10–12 with `check_sigs = true` ≥ 24 CPU-h, legacy-flag cases excluded and counted until A-4 is settled (D30); `netinfo_add_entry` re-certified with the ProUpServ migration agreement (D29); register rows 21, 27 |
| **EV1-5** | types 5, 6, 8; types 7 and 9 with `QuorumLookup` (`MAX_QUORUM_SCAN`), `BlockLookup`, `UnlockIndexes` | EV1-4, LQ1a (binding), LQ2/SB8 (`CommitmentVerifier`), EV3 (`UnlockIndexes`, interface only), **A-1** (merge prerequisite) | L0 + L1 through A-1: L1 for 5 (non-coinbase), 6, 7, 8, 9 incl. the `chain`/`indexes` variants ≥ 24 CPU-h; `check_cbtx` and the D18 block-mode vectors; `check_sigs = false` vectors for 7 and 9 on the block/model binding, and the mempool binding at its fixed `check_sigs = true`; the D14 vector on the model facade; the D25 `CoreThrow` mapping tests; PF0 baseline for §5.2. The §6.3 L2 replay and L3 matrix are a joint certification row with CH7a/MP1a (D27) |
| **EV1-J** (joint, not a slice) | §6.3 L2 replay and L3 negative matrix; the D13 and D14 L3 runs on the pinned dashd | EV1-5, CH7a, MP1a, a runnable node | certified once, after CH7a and MP1a merge; recorded in the conformance register (D27) |
| **EV1-6** | ordered batch BLS verification (§5.1 item 4) | EV1-5, CH7a | `diff_check_special_tx` block mode equal with batching on and off; PF0 counter improvement on `ev1_block_mainnet_mix` with `check_sigs = true` |

---

## 8. Open questions, decisions and asks

### 8.1 Decisions (D-rows)

| ID | Question | Decision | Why |
|---|---|---|---|
| D1 | Inputs and seams | `SpecialTxCtx` (§2.3): (a) the pprev `&MnList` resolved by the caller via `resolve_list`; (b) `&dyn CoinsView` (D13); (c) `EvoCtx`, whose `cons.prev` is Core's `pindexPrev` and whose `cons.bls` is the global flag, which the mempool caller derives from the tip even when `prev` is an earlier block (`validation.cpp:802-825`); (d) `BlockLookup` with `LookupBlockIndex` semantics (any known header) plus `cons.chain` for ancestry; (e) `SpecialTxRules`; (f) `QuorumLookup`, called in Core's order (scan, window, get) with effects to the sink, bound by CH7a to the attempt facade with `OnValidation` and by MP1a under `OnV`; (g) `Option<&dyn UnlockIndexes>`; (h) `SigVerifier`; (i) `check_sigs` | Every Core global or manager read becomes an explicit seam; the block/mempool split is the binding, as in Core's overloads |
| D2 | BLS verify in a pure crate before BS12 | BLS goes only through `SigVerifier`. Until BS12's serial verify lands, EV1-1 to EV1-3 run with `check_sigs = false` or a test double; EV1-4 is gated on BS12. A legacy-scheme verify the bound verifier cannot do returns `VerifyUnavailable` → `SpecialTxFault`, which aborts the attempt; it is never a reject. Exotic points: `false` under the basic flag; under the legacy flag relic's result, computed by BS11a's engine behind the serial verify (BS11a D16, D22). BS11a owns legacy-flag verify in pure crates | DEC-79; a missing verifier must not turn into a consensus verdict |
| D3 | base-sdk `Checkable` | Never used for consensus. EV1 ports every `IsTriviallyValid` in Core's order | `Checkable` is context-free and not Core's order or strings |
| D4 | NetInfo | EV1 ports `Validate()`, `ValidateService`, `ValidateDomainPort`, `AddEntry` with the network-dependent rules (`IsNodeOnMainnet`, `MainParams()` ports, `RequireRoutableExternalIP`, `IsBadPort`) and `ToStringAddr`-exact, nontruncating dedupe (D15), in `dash-dml::netinfo`; `CoreNetInfoValidity` replaces `ContextFreeNetInfo` in node builds | EV2 §1.3 assigns it to EV1; three consumers |
| D5 | Type 6 | EV1 owns steps 1–5 of `CheckLLMQCommitment` and calls a `CommitmentVerifier` seam for `VerifyNull` and `Verify(base, checkSigs=false)`, implemented by LQ2/SB8. EV1-5 waits for that seam (interface dependency); until then type 6 L1 is limited to steps 1–5 | one `Verify` in the codebase |
| D6 | `BlockPayloads` | `dash-dml::specialtx::payload::BlockPayloads`: `Vec<PayloadSlot>` indexed by tx position, built once by CH7a and shared with EV2b. Mapping: `None` (no extra payload) → dispatcher step 1; `Err` or `!is_complete()` → the per-type "payload" reject at Core's position (`bad-protx-payload`, `bad-cbtx-payload`, `bad-qc-payload`, `bad-mnhf-payload`, `bad-assetlocktx-payload`, `bad-assetunlocktx-payload`); `UnknownProTxVersion` with `is_complete()` → decoded, so `GetValidatedPayload` step 3 (v > max) or `IsTriviallyValid` step 1 (v = 0) gives `bad-protx-version` (C); `UnknownProTxVersion` with trailing bytes → `bad-protx-payload` (C) (D20) | Core decodes at each check; decoding is a pure function of the bytes under v24 (EVO-14) |
| D7 | Covenant placement | EV1 provides both functions; the callers call and gate them as in §3.17. EV3's PLAN row "shared-collateral spends" is read as the ConnectBlock call site, which is CH3a's | one implementation, Core's call sites |
| D8 | `bad-assetunlock-*` ownership | EV1 owns every string inside `CheckSpecialTxInner`, including `bad-assetunlock-*`; EV3 keeps the credit-pool diff and `bad-txns-assetunlock-fee-outofrange` | follows Core's function boundary; LQ1:960 is superseded on this point |
| D9 | `set_state_version` / `apply_pro_up_serv` | EV2b owns `dash-dml::migrate`; EV1 does not call it | |
| D10 | Legacy decode | Core decodes legacy-encoded fields in version-1 payloads (operator keys of ProReg and ProUpReg, `providertx.h:180, 346`; signatures of ProUpServ and ProUpRev, `:301, 419`) and calls `Get().IsValid()` on the keys (`providertx.cpp:301, 469`). The legacy predicate also decides validity at every other version, through the signature decode's opposite-scheme fallback (`bls.h:183-201`, §2.5, D22). Every slice whose verdicts depend on that decode (EV1-2's trivial checks, and through them EV1-3 onward) merges only on BS11a's exact legacy decoder; BS11a-t's types alone are not enough. The parity claim rests on BS11a. Until BS11a lands, the L1 generator draws legacy points only from the BS12 corpus; that is a measurement limit on the run, not the parity claim | received transactions are not constrained by a fuzzer |
| D11 | `failed-check-special-tx` | Reproduced for every exception a ported function can raise in Core, and for `SeamError::CoreThrow` (§2.2, D25); `ScanCapacity` and `Storage` are `SpecialTxFault`. Deliberate deviation: Core maps a `dbwrapper_error` thrown inside `CheckSpecialTxInner` to `failed-check-special-tx`; EV1 aborts the attempt on a storage error instead | a store error must not become a consensus reject |
| D12 | Reject representation | `&'static str` constants and CS0 classes; `SpecialTxRules` re-exported from `dash-dml::specialtx` (it is v24-only, so it does not belong in `dash-consensus`) | DEC-98 Q-1: reject strings are node-side |
| D13 | Coins views | Block: `ctx.coins` is the view Core passes to `ProcessSpecialTxsInBlock`; `shared_collateral_spends` gets ConnectBlock's view after `CheckTxInputs` (`validation.cpp:2700-2716`). Mempool: `ctx.coins` is `CoinsTip()` (`validation.cpp:802-810`), never the mempool input cache; `shared_collateral_spends` gets `m_view` after `CheckTxInputs` (`:999-1018`). Tests in §6.1 | Core binds two different views on the mempool path |
| D14 | Quorum scan capacity | `MAX_QUORUM_SCAN` = max `signingActiveQuorumCount` over the whole LLMQ table + 1 (33 at the pin), as a `const fn`. `scan_quorums` writes every hash or returns `SeamError::ScanCapacity`; it never truncates. A unit test asserts that `MAX_QUORUM_SCAN` ≥ `signingActiveQuorumCount + 1` for every LLMQ type any network or `-llmqplatform` can select. The §6.1 devnet fixture retains at least 34 Platform quorums, so both the 33rd-newest (in the scan) and the 34th-newest (outside it) exist | `-llmqplatform` can select `llmq_60_75` |
| D15 | NetInfo strings | Domain addresses borrowed whole; other forms in `ArrayString<261>`; a test asserts that the longest form of each kind fits (§3.16) | `DOMAIN_MAX_LEN` = 253 |
| D16 | Line 94 of the migration | `SetStateVersion`'s Evo reject at `specialtxman.cpp:91-94` reads only the NetInfo kind and emptiness and the MN type, so it is ported as part of `migration_reject` (§3.16.1 step 6c) | closest to Core: it decides the forwarded `bad-protx-netinfo-version` |
| D17 | `key_changed` equality (Opus r1 M1) | `key_changed` (§3.5 step 5a) is `!(ptx.pubKeyOperator == state.pubKeyOperator)` under `CBLSLazyWrapper::operator==` (`bls.h:514-535`) over `CBLSWrapper::operator==` (`bls.h:77-80`). Rust uses `<OperatorKey as PartialEq>::eq` = base-sdk `BlsLazy::eq` (`wrap_lazy.rs:252-284` @dbe245f7) over `BlsValue::eq` (`wrap_value.rs:119-124`); EV1 and EV2b use only this function. L0 vectors (§6.1): a ptx-state × state-side-state matrix for `eq` (flags and failed-decode serialization asserted), and an oracle half through ProUpReg limited to constructible inputs, with unconstructible cells cited to `providertx.cpp` rc.4:458 (Opus r2 N0; D42) | Core's definition |
| D18 | Coinbase on the block path (Opus r1 M2) | EV1 exports `cbtx::check_cbtx`. CH7a runs the coinbase rule of `specialtxman.cpp:816-837, 865` (§3.2 "Block path"): pre-loop, only under `check_sigs` (`fCheckCbTxMerkleRoots`) and only when `vtx[0].nType == 5`, no DIP3 gate, rejects as `BLOCK_CONSENSUS` with the same reason; the loop skips `i == 0` with `nType == 5` after the template-output check. §6.2's both-paths claim is narrowed to non-coinbase type 5; the D18 vectors (§6.1) run through A-1; the pre-DIP3 vector's "valid" includes the empty-list MN root and, for v2, the quorum root (`specialtxman.cpp:924-955`; Opus r2 n5). B1–B7 run inside `ProcessSpecialTxsInBlock`'s outer catch (`:993-995`, BLOCK_CONSENSUS `failed-procspectxsinblock`), which is CH7a's (Opus r2 n6) | Core's call sites |
| D19 | Per-element order and pairwise first reject (Opus r1 M3) | §3.14 and §3.7 number the per-element loops explicitly (each payout runs 2a–2e, each share 3a–3e with both scripts, each non-actor share 8a–8b, before the next element). L1 and `specialtx_check_diff` default to k ∈ {1, 2, 3} simultaneous mutations and generate, for every ordered pair of checks i < j per table and across adjacent entities (share, payout, purpose, non-actor share), cases whose expected reject is the first in Core's execution order ((entity index, step) inside a loop, table order outside loops), taken from the oracle. Pair coverage is over feasible pairs with distinct reject strings; infeasible, unreachable and equal-string pairs are listed with their Core reasons in `contrib/node/ev1-pairs.toml`, which `ev1-census.py` validates (Opus r2 N2, N3). Pair coverage is reported at each merge gate next to census coverage | single-fault cases cannot detect order |
| D20 | Unknown ProTx version (Opus r1 M4) | D6 amended: `UnknownProTxVersion` with `is_complete()` → `bad-protx-version` (C) at Core's position (`providertx.h:167-171, 274-278, 338-342, 407-411` bail out after `nVersion`; `specialtxman.cpp:1113-1116` or `IsTriviallyValid`); with trailing bytes → `bad-protx-payload` (C). The template-output covenant path is unaffected. L0 vectors for types 1–4 at versions 0, 4 and 0xffff, with and without trailing bytes (§6.1) | Core's decode succeeds on an empty stream |
| D21 | Mempool rules (Opus r1 m1) | `SpecialTxCtx::rules` is bound by the caller: block = `GetSpecialTxRules(pprev)`; mempool = `GetSpecialTxRules(tip)` in every call, including the expired-AU retry at `chain[first_height]` / `chain[last_height]` (`validation.cpp:1103, 822, 825`) and the AU replacement call site (`:1370-1404`, at `:1400`: tip, tip rules, `CoinsTip`, `check_sigs = true`) | Core's arguments |
| D22 | BLS schemes (Opus r1 m2) | `BlsSigRef` / `BlsPublicKeyRef` carry their decode outcome and decode scheme; `SigVerifier`'s `scheme` is the verify scheme only (§2.5). Signature decode follows `bls.h:183-201` (payload-version scheme, then the opposite one: canonical there → invalid object; neither → `bad-protx-payload`); keys decode through `Get()` under the stored flag (`bls.h:494-511`); verify uses the global flag (`bls.cpp:296-312`). D10 reworded. `check_sigs = false` vectors for v2/v3 signatures canonical only under legacy (accepted) and under neither (`bad-protx-payload`) | three schemes meet in one check |
| D23 | Preimages over non-canonical wire forms (Opus r1 m3) | Signed hashes are computed from the decoded payload's re-encoding. §6.1 adds ProUpServ `SerializeHash`, ProReg `MakeSignString` and `MakeSharedRegConsentHash` vectors with repeated and out-of-order ExtNetInfo purposes, against `special_tx_sign_hash`; the contrast value is the same preimage with the `netInfo` wire bytes spliced in, signature excluded (Opus r2 n2) | Core hashes the decoded object |
| D24 | Migration conversion ownership (Opus r1 m4) | EV1 exports only `migration_reject` and the NetInfo primitives (`CoreNetInfoValidity` / validate, `add_entry`). `convert_for_version` is not an EV1 export: EV1's conversion is a private scratch value in `netinfo/migration.rs`, and EV2b implements its own conversion over `add_entry`. The EV2b equality test compares the two implementations' verdicts (§3.16.1) | follows D9; two implementations make the equality test meaningful |
| D25 | Core throws behind seams (Opus r1 m5) | `SeamError::CoreThrow` is the outcome a seam implementer (LQ1a: `GetQuorum` / `BuildQuorumFromCommitment`, `ScanQuorums`; LQ2/SB8: `CFinalCommitment::Verify` → `GetAllQuorumMembers`) returns exactly where its Core counterpart throws deterministically; each implementer lists those sites with one test each. EV1 maps it to TX_CONSENSUS `failed-check-special-tx` (`specialtxman.cpp:267-271`). `ScanCapacity` and `Storage` stay faults; the storage deviation is recorded in D11 | Core's catch covers code behind the seams |
| D26 | EV1-3 coins-view tests (Opus r1 m6) | EV1-3 runs the D13 tests against a model `CoinsView` pair; the real-binding runs and the L3 comparison move to MP1a's acceptance | MP1a is not an EV1-3 dependency |
| D27 | EV1-5 checkpoint (Opus r1 m6) | EV1-5 merges on L0 + L1 through A-1. The §6.3 L2 replay and L3 negative matrix, and the D13/D14 L3 runs, are one joint certification row (EV1-J) with CH7a and MP1a, run once both are in the node under test | removes the EV1-5 ↔ CH7a cycle |
| D28 | Core-exact netaddr provider (Opus r1 m6) | EV1-1 extracts the pure parts of dash-p2p into a new pure crate `dash-netaddr`: `netaddr.rs` (`NetAddr`, `Service`, `Network`, predicates incl. `is_routable`, `set_special`, `Display` = `ToStringAddr[Port]`), `split_host_port` (`netcore.rs:1436`), `is_bad_port` (`dial.rs:235`), the numeric lookup (from `dial.rs:389` `lookup_numeric`) as `lookup_numeric(name, port_default)`, with the port taken from the caller and no CJDNS flip (dash-p2p's wrapper keeps `default_port_for` and the flip), and `maybe_flip_ipv6_to_cjdns` (`local.rs:192`) as a separate function with an explicit `cjdns_reachable: bool` parameter in place of `NetCore::is_reachable` / `g_reachable_nets`. dash-p2p re-exports them and keeps its behaviour. Anything NetInfo needs that is missing gets Core vectors at the pin. `CoreNetInfoValidity` and `add_entry` take `cjdns_reachable` from the caller's config. NetInfo calls the lookup with its own split port: `MnNetInfo` without the flip (`netinfo.cpp:321`), `ExtNetInfo` followed by the flip (`:542-543`) (Opus r2 n1) | dash-p2p depends on tokio and sockets; dash-dml is pure |
| D29 | `netinfo_*` tx-verdict properties (Opus r1 m6) | Moved out of EV1-1. EV1-1 certifies `netinfo_validate` and `netinfo_add_entry` as panic-free and deterministic, with L0 vectors. The `netinfo_validate` tx-verdict property certifies at EV1-2, which lands the trivial checks the wrappers reach first (§3.3 steps 9–11, §3.4 steps 4–7). The `netinfo_add_entry` migration agreement joins with ProUpReg at EV1-3 and ProUpServ at EV1-4, and each of those checkpoints lists the re-certification (§7; Opus r2 n4) | the harness needs the earlier checks |
| D30 | Legacy verify in EV1-4 L1 (Opus r1 m6) | Until A-4 is settled, `check_sigs = true` cases with a legacy global flag are excluded from L1, counted in the run report; until A-4 resolves, L0 asserts `SpecialTxFault::VerifyUnavailable` for them, not Core's verdict. They do not block EV1-4's gate; A-4 gates the parity claim for legacy-flag verification (Opus r2 N4) | the legacy verifier's owner is not yet named |
| D31 | Nits (Opus r1 m7; Sol r3 N1) | §3.15 6c: an empty `netInfo` gives `core_port` 0 (`netinfo.cpp:343-349`); `bad-protx-platform-dup-ports` follows only for a non-mainnet Evo ProReg at version 2 whose ports pass `providertx.cpp:243-255` (version 1 Evo gives `bad-protx-evo-version`, :270-272). The vector runs in that context, plus the mainnet (`bad-protx-platform-p2p-port`, :245), `ExtAddr` (no reject, :233-241) and version-1 contexts with their own expected results (§6.1; Opus r2 N1); §3.14 signature includes `keyIDVoting`; §2.3 `ancestor(prev, h)` with `h` above `prev` returns `None`, compared as not equal; §6.5 `prodis_arith` zero divisor is a defensive case | |
| D32 | Census at rc.4 (EV1-2) | The census reads the pin, v24.0.0-rc.4 (DEC-484): 211 literal sites (142 TX_BAD_SPECIAL, 69 TX_CONSENSUS) and 146 distinct reasons, where §3.13 was counted at rc.3 (213 sites, 144 TX_BAD_SPECIAL). rc.4 drops the `bad-protx-netinfo-bad` entry loops of the ProReg and ProUpServ trivial checks (CE4-03) and moves the `specialtxman.cpp` checks below `CheckSpecialTxInner`. `contrib/node/ev1-census.py --check` asserts the rc.4 totals | the census follows the pin |
| D33 | D23 at rc.4 (EV1-2) | rc.4's `ExtNetInfo` reader throws on an unknown, repeated or out-of-order purpose (`netinfo.h:431-433`), so those forms never decode and never reach a preimage: both sides give `bad-protx-payload`. D23's vectors use the non-canonical forms that still decode, an entry Core reads but `Validate()` refuses (an unknown entry type, `0.0.0.0`), for ProUpServ `SerializeHash` and ProReg `MakeSignString` and `MakeSharedRegConsentHash`, each with the wire-bytes contrast | the rc.4 reader narrowed the domain |
| D34 | D29 harness (EV1-2) | `netinfo_validate`'s tx-verdict leg wraps the object in Core-signed ProRegTx and ProUpServTx templates (versions 1–3, regular and Evo; none shared or with a second owner payout, so `specialtxman.cpp:1223-1230` never fires) on every network, with V24 active where `DeploymentEnabled` allows it (not on rc.4's mainnet, `chainparams.cpp:213`). Where ours rejects, the class and reason are Core's; where ours passes, Core rejects at the next step exactly: `bad-protx-hash` for a ProUpServTx (empty list), `bad-protx-collateral` for a ProRegTx (empty coins view) | until A-3 gives `Validate()` its own entry |
| D35 | Landed names (EV1-3; confirmed DEC-682) | The checks keep the scaffold's landing names: `provider::check_pro_reg_tx`, `check_pro_up_reg_tx`; `shared::check_pro_dis_tx`, `check_pro_dis_tx_for_list`, `check_pro_up_share_tx`, `check_pro_up_shared_reg_tx`; `covenant::shared_collateral_spends`, `shared_collateral_template_outputs`. `check_inputs_hash` lives in `sig.rs` beside `inputs_hash`, so `CalcTxInputsHash` has one definition. §3.1 shows them | the scaffold's markers name them |
| D36 | EV1-3 context (confirmed DEC-682) | `SpecialTxCtx<'a, E>` holds only what types 1, 3 and 10–12 read: `evo`, `rules`, `list`, `coins: &dyn CoinsView<Error = E>`, `net_info`. `SpecialTxFault<E>` is `Unported(TxType)` or `Coins(E)`. `QuorumLookup`, `UnlockIndexes` and the `SigVerifier` of §2.3/§2.5 join with EV1-4 and EV1-5, which land the types that read them. Until then types 2, 4 and 5–9 return the fail-stop `Unported` (DEC-663 A1), each arm removed when its type lands | no seam before its first reader |
| D37 | Core's catch in node-l1's check tables (EV1-3) | Core's `try`/`catch` wraps only the `switch`, after the DIP3 gate (`specialtxman.cpp` rc.4:217-274), so a throw is a step of the type that throws. The throw row moves from the dispatcher (row 4) to the first row of the type 2 and 4 tables, and `failed-check-special-tx` joins the CbTx, QC, MNHF and asset-lock/unlock rows; `missing_checks()` drops to ≥ 10 (from 27) | Core's control flow |
| D38 | QC's landing marker (EV1-3) | The type 6 table's marker is `fn check_qc_tx`, the function LQ2 landed in `qc.rs`; it pointed at a stale name | the marker must exist |
| D39 | node-l1 tables for types 1 and 3 (EV1-3; confirmed DEC-682) | The ProReg and ProUpReg check tables collapse to two rows, `GVP-T` (the payload and its trivial checks) and `C` (the contextual checks), which still compare reasons exactly. Step-level pinning moves to dash-dml's L0 vectors and EV1-3b's D19 first-reject coverage. `l1_dml_block` maps the scaffold's step ids through `step_of`; `PROBED_MISSES` drops from 8 to 5 | one owner per step |
| D40 | Quirk 26, EV1's half (EV1-3; DEC-682 Q1) | The L0 vector `dash-dml/src/specialtx/tests.rs::collateral_from_pprev_utxo` shows that an external collateral missing from the view gives `bad-protx-collateral` (`specialtxman.cpp` rc.4:1245-1249). That the view is pprev's is the caller's: `ProcessSpecialTxsInBlock` runs on `view` (`validation.cpp` rc.4:2669) before `UpdateCoins` (:2773). EV1-3b's L1 same-block case (a block funding the collateral and a ProReg spending it, through `oracle_dml_block`) tests that binding; register row 26 stays `scaffold` until it lands | the L0 vector passes for any caller (Opus EV1-3a r1 n-1) |
| D41 | D17 eq-half corpus (EV1-3; DEC-683) | `data/d17-lazy-eq.txt`: the BS12 corpus's 1008 lazy public-key pairs (base-sdk `bls_wrapper.json5` @f27bc4f55ccc) plus 784 pairs of the same classes with a Set-basic key (`set:BB`, `set:LB`), probed with base-sdk's `contrib/vectors/bls_wrapper/probe.cpp` against v24.0.0-rc.3 (0364ed626e), which reproduces the corpus's eager rows, lazy scripts and 1008 pairs exactly. 1792 pairs, both orders on fresh keys, each side's serialization afterwards. `src/bls/` is unchanged from rc.3 to rc.4 | the corpus had Set only with the legacy flag |
| D42 | D17 oracle half (EV1-3; DEC-683) | `node-l1/tests/l1_specialtx_d17.rs`. Observable: the V24 address migration. Masternode 1 at state version 1 or 2 holds a legacy address on port 22, which ExtNetInfo refuses, and a ProUpRegTx v3 raises it: an unchanged key runs the migration (`bad-protx-netinfo-version`), a changed one skips it. Cells: same bytes, same scheme; same point, legacy state; another key under each scheme; basic bytes read as legacy; undecodable and zero bytes under each scheme; `sub_100`'s bytes (canonical under both schemes, two points; asserted to decode both ways) as the ptx key against the same bytes in a legacy and a basic state; and masternode 2 holding the key, under the basic scheme against a migrating state, under the legacy scheme against an unmigrating one (no probe: the migration rejects), and under each scheme against a changed key (`bad-protx-dup-key`). Each cell asserts ours == Core == expected. Unconstructible: a ptx key other than read and decoded (`IsTriviallyValid`'s `Get()`, `providertx.cpp` rc.4:458; this document cited 468-473, an older pin's line), and state keys that are default, decoded by `Set`, read before step 5a, or the ptx's own object (no disk form); the eq half covers them. L0 adds the call-site pair through `check_pro_up_reg_tx` (P's legacy bytes against P under basic: unchanged; `sub_100` under both: changed) | only constructible cells reach Core |
| D43 | `inputs_hash_of` (EV1-3) | `sig::inputs_hash_of(inputs)` hashes a vin before a `Tx` exists, for builders (node-l1's generated payloads, L0 fixtures); `inputs_hash(tx)` calls it | one `CalcTxInputsHash` |
| D44 | ProUpSharedReg join signatures (EV1-3) | The share/signature `zip` relies on the decoder reading exactly one join signature per share (`providertx.h` rc.4:192-206), which base-sdk's decoder does (decode domain) | the decoded domain fixes the count |
| D45 | Census unreachable rows (EV1-3) | ProReg `bad-protx-netinfo-entry` (`CheckNetInfo` refuses first), ProUpShare `bad-proupshare-payee-dest` (`IsTriviallyValid` admits only P2PKH and P2SH), and ProReg's `bad-protx-type` else-branch (`IsTriviallyValid` refuses an undefined type first) are listed unreachable in the L0 census closure | closure accepts only cited exclusions |
| D46 | `CheckCtx::check_special_tx` (EV1-3; confirmed DEC-682) | node-l1's `CheckCtx::check_special_tx` is public, so the D17 oracle test drives ours through the binding the block checks use | one path for both |
| D47 | Mutation survivors (EV1-3; Opus EV1-3a r1 m-1) | `covenant.rs` `\|\|` → `&&` is equivalent: a non-ProReg or non-special-version tx has no `ProRegTx` payload, so the payload arm rejects with the same reason. The `is_scheme_migration` mutants are not: step 5a's probe (`specialtxman.cpp` rc.4:1520-1527) is observable whenever a later ProUpReg check also fails. Two-fault vectors pin it: a duplicate key under the other encoding with a payout to the owner key, and with a state address the migration refuses; both are `bad-protx-dup-key`. EV1-3b's D19 generator covers dup key × every later ProUpReg fault | a single-fault argument is not an equivalence |

### 8.2 Asks to the manager

| ID | Ask |
|---|---|
| A-1 | **Accepted.** An O2a follow-up: `oracle_check_special_tx_block` (the `chain` overloads, an asset-unlock index set and a quorum-manager context with mined quorums). It must reach types 7 and 9, including their early rejects, and the block-path variants of the rest. It is a merge prerequisite for EV1-5 (§6.3) |
| A-2 | Optional: an `oracle_shared_collateral_spends(tx, coins)` entry, so the spend rule can join L1 and the `shared_collateral` target |
| A-3 | **Required for the NetInfo differential fuzz claim** (staffed with A-1): `oracle_netinfo_validate(bytes, network)` and `oracle_netinfo_add_entry(kind, purpose, string, network)`, returning the `NetInfoStatus` and the resulting entry. Until then the `netinfo_*` targets are narrowed (§6.5) |
| A-4 | Name the owner of legacy-scheme BLS verify in pure crates (BS11a or BS12), and give BS12's serial-verify timing. EV1-4 is not gated on it (D30); it gates the parity claim for legacy-flag verification |
| A-5 | Confirm D5's `CommitmentVerifier` seam with the LQ2/SB8 owners |
| A-6 | PLAN:586 row text: drop "per profile" and `i:BS6c` (DEC-82; BS6c dropped; EVO-14 leaves EV1 nothing to consume); A1:752 `dash-evo::specialtx` → `dash-dml::specialtx`; `dash-evo/API.md` Q9's `BlockPayloads` path → `dash-dml::specialtx::payload`; COVERAGE E7 and EV3's row per D7; LQ1:960 per D8 |
