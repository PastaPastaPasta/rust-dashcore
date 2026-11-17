# EV addendum to SB0: the `dash-evo` / `dash-dml` split, error contracts, and the EV0, EV2a and LQ1a deltas

| | |
|---|---|
| **Status** | **rev6 (final)**, answering Opus high r7 (APPROVE-WITH-CHANGES, O1–O5; `reviews/EV-addendum-r7-opus.md`) inside DEC-111, DEC-113, DEC-117 and DEC-119: Core's snapshot-cache critical sections made normative, EV0's commit order fixed, `VContext` stateless with every attempt opener on V, SPV requests bounded by their target, and the model-only suite moved to a new row LQ1m. rev5 726ff9a1 answered Sol r5 (R5-M1, R5-m1) inside DEC-117: both capability minting paths need V's context, so `begin_attempt` is V-only; Sol r6 approved it. rev4 77c76faf answered Sol r4 per **DEC-117**: a closed allow-list of raw getters on the caller's thread, every other external route dispatched whole to V, enforced by a capability type. rev3 17c65276 answered Sol r3 (R3-M1). rev2 df34ea3e answered Sol r2 (R2-M1) per **DEC-113** (serialize attempts with external readers). rev1 37cff995 answered Sol r1 (M1–M5, m1–m3) per **DEC-111**; DEC-109 adopted rev0's five open questions. No further review round: the manager checks rev6 against Opus r7 (DEC-92). §8 maps each finding to its change (§8.1 rev6, rev5 and rev4, §8.2 rev3, §8.3 rev2, §8.4 rev1). rev0: fa09d8d0 |
| **Date** | 2026-10-09 |
| **Scope** | A delta. It changes only what SB0 forces on the approved EV2 (rev4, DEC-58) and LQ1 (rev4, DEC-60) designs and on EV0's approved API, plus EV0 m2's resolution (DEC-111). Everything not named here stands as approved |
| **Inputs** | SB0 rev3 d790b55e and rev4 7d40936c/53091a32 (§2.2, §4.1.1, §5.1–§5.4, §12.1, §13); DEC-58, DEC-60, DEC-71/79, DEC-98 Q-12, DEC-104 (M8), DEC-105 (F4), DEC-107 (M4), DEC-109, DEC-111; Sol's SB0 r1 m3 (`reviews/SB0-design-r1-sol.md:131-139`); EV0 `node/ev0-evo-interfaces` @3a5cb631 (`dash-evo/API.md`, API reviews r1/r2); CH2 rev5 640d37f9 §2.4.4, §2.10 (latch, `AbortSink`) |
| **Citation keys** | `SB0 §x`, `EV2 §x`, `LQ1 §x` are sections of those designs. `EV0 path` is a file on the EV0 branch. `v24 x.cpp:N` / `v23 x.cpp:N` are Dash Core v24.0.0-rc.3 / v23.1.8 |
| **CPM** | `docs/node-program/tools/cpm_ev_addendum.py` (committed; pins `cpm.py` and `cpm_sb0_rev1.py` by blob hash). Scratch: `/work/scratch/ev-add/` |

> **PLAN §7.** This document states what the shared code checks, as design properties. For current SPV-mode validation coverage, see PLAN §7 handling (CANDIDATE-SPV).

---

## 0. Summary

1. **Split.** `dash-evo` (layer P, shared) keeps the vocabulary, the views and the algorithms: `ctx`, `effect`, `bls`, the LQ1 views and member trait, `QuorumSnapshot`, the mined-commitment *reader trait*, and `ProTxHash`/`MnType`/`RawMnType`. Node-only `dash-dml` (layer Pn) takes the authoritative DML (`MnList`, `Dmn`, diffs, records), `StoreError`, `EvoStore`/`DmlReader`, `EvoDelta`, `MemEvoStore`, credit pool, MNHF and `imbl`. `dash-mnlist` (S) consumes only `dash-evo` (§1). The node entry type stays `Arc<Dmn>` through a shared forwarding impl for `Arc<T>` and `impl MnEntryView for Dmn` in `dash-dml` (orphan-rule-safe, §1.3).
2. **Errors.** Shared views carry an associated `Error`; one error type flows through each call. `MemberError<E>` has `Missing(BlockId)`, `View(E)` and `UnknownLlmqType(u8)`. Every LQ1 function that reads ancestry returns `Result`, including the rotation predicate and the enabled-type list; the chain view's `Ok(None)` means only "above the block" or "actual genesis" (§2.2, §2.5). The node keeps LQ1's D4 fail-stop through a node-side mapping to `StoreError`. SPV maps the same variants to a request, `Unavailable`, or "latch already tripped" (§2.3).
3. **One new rule SB0 implies:** a view never answers one of Core's *normal-absence* values (an absent snapshot row, `BlockDataUnavailable`, a `None` ancestor) for input it merely does not hold (§2.1).
4. **EV0 m2 resolved (DEC-111, DEC-113):** one attempt-tagged row overlay, owned by CH7b's attempt handle, shared by every members and precompute call in the attempt; writes are ordered effects applied whatever the outcome. While an attempt is open, every external members, materializer, predictor and snapshot-read route waits until CH7a has applied its log and released it (LQ1 §3.7.2's serialization, restored) (§2.6).
5. **EV0** is reshaped in seven commits on its branch, after a rebase onto the re-pinned integration tip; no behaviour changes (§3).
6. **EV2a** moves to `dash-dml` and keeps LQ1's view contract tests for `MnList`; the impls themselves land in EV0 commit 1 (rev6). `project_sml` leaves EV2a for EV4-serve (DEC-109 (2)) (§4.1).
7. **LQ1a** gets store-independent fallible signatures, the not-held tests, `members_uncached` for consumers without Core's caches, and the `SIGN_HEIGHT_OFFSET` export. A new small row, **LQ1m**, lands the attempt-overlay model and its conformance cases (rev6). mnxcheck compares SPV against the node's *cached* membership (§4.2, §4.3).
8. **PLAN.** No size or tier changes to existing rows; one new S row, LQ1m (deps `LQ1a`; CH7b takes `LQ1m`, LQ1b `i:LQ1m`). EV0 keeps PLAN's `CS0, i:BS6a`; LQ1a's `i:EV4` becomes `i:SB7`; LQ2 takes `SB8`. REL11 stays **30.0**; the merge-edge counterfactual is 31.5 (§5).

---

## 1. The crate split

### 1.1 Rule

- **`dash-evo` (P, shared):** what both products compute from authenticated inputs. Core's vocabulary types, the views LQ1 reads through, the algorithms, verification, the effect channel the algorithms emit into, and the BLS scheme context. No `imbl`, no store type, no DML value.
- **`dash-dml` (Pn, node-only):** Core's authoritative evo state and its store contract. Everything whose correctness depends on a complete chain and a complete evodb: the DML model, `StoreError` and its fail-stop meaning, `EvoStore`, deltas and records, credit pool, MNHF.
- **`dash-mnlist` (S, SPV):** never names a `dash-dml` item (SB0 L2: S → B, P, S). It implements `dash-evo`'s views over SML lists, qrinfo snapshots and `AnchoredAncestry`.

### 1.2 EV0 item map

Names are EV0's; only paths change unless the "Change" column says otherwise.

| EV0 item (`dash_evo::…`) | New home | Change |
|---|---|---|
| `ctx::EvoCtx` | `dash_evo::ctx` | none |
| `effect::{Effect, EffectKind, EffectSink}` | `dash_evo::effect` | none (it names only shared types once `ProTxHash` moves) |
| `bls::{BlsCtx, bls_ctx_after, bls_ctx_for_block, …}` | `dash_evo::bls` | none. The DEC-98 Q-12 wrappers follow DEC-109 (1): eager and global holders here, the lazy operator key in `dash-dml`; BS6a's rework, after EV0 |
| `dml::{ProTxHash, MnType, RawMnType}` | `dash_evo::types` | moved; `dash_dml` re-exports them. `ProTxHash` keeps `immer_hash` (EV0 Q5's base-sdk move is unchanged). `MnType`/`RawMnType` are shared because the SML entry's `nType` is the same Core field (`evo/dmn_types.h`, both pins) |
| `dml::{Dmn, MnHot, MnCold, MnList, Counts, PayeeKey, Provenance, KeyId, OperatorKey, ScriptBytes, UniqueKey, UniqueMap, UniqueSlot, SameRecord, MnListDiff, MnStateDiff, MnListDiffRecord, DmlDelta}` | `dash_dml::*` (flattened; `dml::` drops) | moved |
| `store::{StoreError, DmlReader, EvoStore, EvoDelta, MinedCommitmentRecord, MemEvoStore, MemEvoView}` | `dash_dml::store` | moved; `EvoStore: DmlReader + MinedCommitmentReader<Error = StoreError>` |
| `store::{MinedCommitmentReader, MinedFamily, MinedIndexEntry, MinedCommitment, base_through}` | `dash_evo::llmq` | moved; the reader gains `type Error: From<BrokenInvariant>` (§2.2). It keeps `chain() -> &dyn ChainAncestry`: only holders of complete ancestry implement it (the node, test kits). SPV builds its scan from the authenticated set (SB0 §5.2 step 6) and does not implement it |
| `llmq::{QuorumMemberSource, MemberError, QuorumMembers}` | `dash_evo::llmq` | generic (§2.2). EV0's two-variant `MemberError` survives node-side as `dash_dml::NodeMemberError` |
| `llmq::{QuorumSnapshot, SnapshotSkipMode, LlmqCommitment}` | `dash_evo::llmq` | none (BS8 swap per EV0 Q3/Q4 unchanged) |
| `creditpool::{CreditPool, RangesSet}`, `mnhf::MnhfSignals` | `dash_dml::{creditpool, mnhf}` | moved |
| `testkit::MemChain` | `dash_evo::testkit` | none |
| `testkit::{pro_tx, cold_state, dmn}`, `MnList::insert_for_tests` | `dash_dml::testkit` | moved (`pro_tx` stays usable from `dash_evo::testkit`, since `ProTxHash` is shared) |
| `tests/store.rs`, `tests/consumers/{ev1,ev2a,ev2b,ch7a}.rs` | `dash-dml/tests/` | moved |
| `tests/consumers/lq1a.rs` | split | the view traits become `dash_evo` source (§1.3); the `MnList` impls go to `dash-dml` (§4.1); its per-call `AttemptRows` is replaced by the attempt-owned overlay (§2.6, §3 commit 5) |
| `Cargo.toml` deps | | `dash-evo`: `dash-consensus`, `dash-num`, `dash-params`, `dash-pkc` (`bls`), `dash-primitives`, `dash-types`. `dash-dml`: those plus `dash-evo` and `imbl = "=7.0.2"` (DEC-50(6) pin moves with it) |

### 1.3 New in `dash-evo` (landed by EV0 so EV2a and SB8 can implement them before LQ1a)

| Item | Source | Why EV0 lands it |
|---|---|---|
| `llmq::{MnEntryView, MnListView, ScratchMnList, ListCounts, AddMnRejected}` | LQ1 §2.2.1–2.2.2 text, unchanged (today a copy in `EV0 tests/consumers/lq1a.rs`) | EV0 commit 1 lands `impl MnListView for MnList` (rev6, O2; SB0 §12.1 had given it to EV2a), and SB8 adds `impl MnListView for SmlList`. Both precede or parallel LQ1a, so the traits must exist first |
| `impl<T: MnEntryView> MnEntryView for Arc<T>` | new (M1) | forwards every method to `T`. After the split the trait is `dash-evo`'s and `Arc` is `std`'s, so `dash-dml` cannot write `impl MnEntryView for Arc<Dmn>` (E0117: `Arc` is not a fundamental type). With this impl, `dash-dml` writes `impl MnEntryView for Dmn` (legal: local type) and keeps `Entry = Arc<Dmn>` with cheap clones at the algorithm boundary (`MnEntryView: Clone`, LQ1 §2.2.1; `Arc<T>: Clone` always). SB8's SML entry may use it too |
| `llmq::LlmqChain` + `llmq::CompleteChain<'a, E>` | new (§2.2) | SB0 §4.1.1: LQ1 and `dash-mnlist` read bases and work blocks through a view with an associated error; `CompleteChain` wraps the node's infallible `ChainAncestry` |
| `BrokenInvariant` | new (§2.2) | lets the moved reader's provided methods report "base above the queried block" without naming `StoreError` |

Every other impl the split needs is legal where it lands: `MnListView`/`ScratchMnList for MnList` and LQ1b's `UsedSet` in `dash-dml` (local types), `MnListView for SmlList` in `dash-evo` (SB7/SB8, local), `From<BrokenInvariant> for StoreError` and `From<MemberError<StoreError>> for NodeMemberError` in `dash-dml` (local target type).

LQ1a still lands `LlmqCtx`, `LlmqSource`, `SnapshotRows`, `CoinbaseFacts` (with SB7) and the algorithms, with the signatures of §2.2 and §2.5.

### 1.4 What `dash-mnlist` consumes

`dash_evo::{types, llmq (views, LlmqChain, MemberError, QuorumMembers, QuorumSnapshot, LlmqCommitment, members_uncached, select_from_scan, select_start_height, SIGN_HEIGHT_OFFSET), sml (SB7), locks (SB8), bls, effect::EffectSink}`. It implements `LlmqChain`, `LlmqSource<SmlList>` and `SnapshotRows` with its own error type (§2.3), and passes a discarding `EffectSink` (SPV applies no effects). Nothing else from EV0.

### 1.5 Knock-on module homes (paths only; no scope change)

| Task | Was | Now | Why |
|---|---|---|---|
| EV1 | `dash-evo::specialtx` | `dash-dml::specialtx` | reads `&MnList` at `pprev` (EV2 §2.12) |
| EV3 | `dash-evo::{creditpool, mnhf}` | `dash-dml::{creditpool, mnhf}` | the records moved |
| EV4-serve | `dash-evo::{sml, cbtx}` | `dash-dml` (projection incl. `project_sml`, root cache, mnlistdiff builder) | needs `MnList`; EV4-core is SB7 in `dash-evo::sml` |
| LQ2 | `dash-evo::llmq::{commitment, blockprocessor}` | `commitment` checks shared in `dash-evo::llmq` (the one `CFinalCommitment::Verify` is SB8's, DEC-109 (4)); `blockprocessor` (writes `MinedCommitmentRecord`, `EvoDelta`) in `dash-dml` | the block processor writes node state |
| LQ1b `UsedSet` | LQ1 §2.2.4 adapter | `dash-dml` | built from `ColdDerived::unique_keys`, a DML internal |

---

## 2. Error contracts

### 2.1 The two kinds of "no answer", and the not-held rule

Core has two kinds of absence on this path, and they behave differently:
- **Normal absence, a value.** `GetSnapshotForBlock` finding no row breaks the rotation (LQ1 quirks 28a, 28e; `v24 llmq/utils.cpp:496`, `:503`, `v23 :479`, `:485`). `GetNonNullCoinbaseChainlock` failing `ReadBlockFromDisk` returns `nullopt` and selects the hash-form modifier (`v24 evo/cbtx.cpp:181-193`, `v23 :219-231`; consumed at `v24 llmq/utils.cpp:87-89`, `v23 :88-96`). A null `GetAncestor` (height above the block) or a null `pprev` (genesis) ends a walk. All of these change the result. They are not errors.
- **A local fault.** An unreadable record, or a record an established invariant requires (the MN list of a connected block). LQ1 D4 makes it fail-stop on the node.

SPV adds a third: **input the client does not hold** (a list not yet fetched, a snapshot from a qrinfo not yet received, a work block's coinbase it has no diff for, history below the floor). Core never sees this.

**Rule (new; forced by SB0's SPV use of LQ1).** A view returns a normal-absence value only when it *knows* Core would see that absence. Input it merely lacks is `Ok(None)` from `mn_list`/`coinbase_facts` (→ `MemberError::Missing`) or `Err(View(e))`, never an absent snapshot row, never `BlockDataUnavailable`, and never `Ok(None)` from `LlmqChain::ancestor`/`prev` (§2.2's chain contract).
- On the node nothing changes: its views hold everything, so "not held" cannot occur, and D4 covers what is missing.
- In SPV it means a missing qrinfo snapshot can never silently break a rotation, a missing coinbase can never select the hash-form modifier, and missing ancestry can never read as "rotation disabled" or "type not enabled". Core's own qrinfo builder refuses rather than ship an absent row ("Cannot find quorum snapshot", `v24 llmq/snapshot.cpp:60`, `v23 :40`), so a peer can never convey Core's absence anyway. Cost: quirk 28a/28e cycles are `Unavailable` in SPV (liveness only, SB0 §5.1).

Values that stay values, in both products (Sol r1 check 3): genesis/no parent, a disabled or invalid base or index, a known before-genesis previous cycle (§2.2), insufficient enabled members or a failed fill, empty or mode-dependent quarters, a known pre-V20 work block or a known coinbase/payload/signature predicate failure (no CL), and, for the node's scan and materializer only, a missing mined commitment. SPV requests a coinbase only when Core's path actually reads it, and never drops an authenticated scan entry because its own data or verification is missing.

### 2.2 Signatures

```rust
// ---- dash-evo::llmq (EV0) -------------------------------------------------------------------
/// The chain reads LQ1 makes: bases, cycle bases, cycle predecessors, work blocks (SB0 §4.1.1).
/// Contract (M2): `ancestor(id, h)` is `Ok(None)` only when `h > height(id)` (Core's null GetAncestor);
/// `prev(id)` is `Ok(None)` only at actual genesis. A block the view should hold but does not (below
/// an anchored client's floor, not yet fetched) is `Err`, never `Ok(None)`. Heights below zero are never
/// passed: callers compute them in i64 and take Core's null-pointer branch without a view call.
pub trait LlmqChain {
    type Error;
    fn header(&self, id: BlockId) -> Result<HeaderCtx, Self::Error>;
    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, Self::Error>;
    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, Self::Error>;
}
/// The node's complete ancestry. Never errs; `E` is the call's error type (one per call, below).
pub struct CompleteChain<'a, E> { chain: &'a dyn ChainAncestry, _e: PhantomData<fn() -> E> }

/// Members in Core's order; duplicates kept (quirk 28f). EV0's `QuorumMembers`, made generic.
pub type QuorumMembers<E> = Arc<[E]>;

pub enum MemberError<E> {
    /// A per-block input the computation needs is not in the view: `LlmqSource::mn_list` or
    /// `coinbase_facts` returned `Ok(None)` for this block. The meaning is the product's (§2.3).
    Missing(BlockId),
    /// The view failed (a read error, history below an anchored client's floor, a latch already tripped).
    View(E),
    /// Core's `IsQuorumTypeEnabled` throw (EV0 text unchanged).
    UnknownLlmqType(u8),
}

/// EV0's member source, generic. Text and provenance rule as LQ1 §2.6. Attempt callers use CH7b's
/// attempt-bound view (§2.6); `effects` is the attempt's ordered log. No `Send + Sync` supertrait
/// (rev5): every runtime implementor carries a V-only capability (§2.6), so it is `!Send`. EV0 Q10's
/// reason ("shared by validation, QM1, DK1, LQ5 and RPC threads") is superseded by DEC-117.
pub trait QuorumMemberSource {
    type Entry: MnEntryView;
    type Error;
    fn members(&self, llmq_type: LlmqType, base: BlockId, effects: &mut dyn EffectSink)
        -> Result<QuorumMembers<Self::Entry>, MemberError<Self::Error>>;
}
impl<T: MnEntryView> MnEntryView for Arc<T> { /* forwards every method (M1, §1.3) */ }

/// "A mined commitment's base is above the block that mined it": corruption, never a panic.
pub struct BrokenInvariant(pub &'static str);
pub trait MinedCommitmentReader: Sync {
    type Error: From<BrokenInvariant>;
    fn chain(&self) -> &dyn ChainAncestry;                // complete-ancestry holders only (§1.2)
    // mined_commitment, mined_index: EV0's signatures with `StoreError` → `Self::Error`;
    // mined_until, nth_mined_by_index, newest_by_index: provided, as in EV0.
}

// ---- dash-evo::llmq (LQ1a; LQ1 §2.3–2.5 with these changes; full list in §2.5) ---------------
pub struct LlmqCtx<'a, E> { pub params: &'a ChainParams, pub chain: &'a dyn LlmqChain<Error = E>,
                            pub deployments: &'a dyn DeploymentView, pub bls: BlsCtx, pub rules: &'a UngatedRules }
pub trait LlmqSource<L: MnListView> {
    type Error;
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<L>>, Self::Error>;            // None → Missing(at)
    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, Self::Error>; // None → Missing(at); §2.1
}
pub trait SnapshotRows {
    type Error;
    fn get(&self, t: LlmqType, cycle_base: &BlockHash) -> Result<Option<QuorumSnapshot>, Self::Error>; // Ok(None): Core's absence only (§2.1)
    fn store(&mut self, t: LlmqType, cycle_base: &BlockHash, s: &QuorumSnapshot) -> Result<(), Self::Error>; // §2.6
}
pub type LlmqError<E> = MemberError<E>;      // LQ1's LlmqError had the same variants; one enum

// ---- dash-dml (EV0) -------------------------------------------------------------------------
pub enum StoreError { Read { what: Cow<'static, str> }, Inconsistent { what: Cow<'static, str> } } // EV0, unchanged
impl From<BrokenInvariant> for StoreError { /* Inconsistent */ }
impl MnEntryView for Dmn { /* field reads only (M1) */ }
pub type DmlMemberSource = dyn QuorumMemberSource<Entry = Arc<Dmn>, Error = StoreError>;
/// EV0's former `MemberError`: what node callers act on (EV2 §2.11, LQ1 §2.6 mapping table unchanged).
pub enum NodeMemberError { Store(StoreError), UnknownLlmqType(u8) }
impl From<MemberError<StoreError>> for NodeMemberError {
    // Missing(at) → Store(StoreError::inconsistent("MN list missing for a connected block …")): D4
    // View(e)     → Store(e)
    // UnknownLlmqType(t) → UnknownLlmqType(t)
}
```

`MnEntryView`, `MnListView` and `ScratchMnList` stay **infallible** (DEC-109 (3)): both implementations (`MnList`, `SmlList`) are materialized values, and every fallible read happens in the view that produces them (`LlmqSource::mn_list`).

### 2.3 Product mapping

| Outcome | Node (`E = StoreError`) | SPV (`E = dash_mnlist::MnViewError`, SB11) |
|---|---|---|
| `Missing(at)` | **fatal**: `NodeMemberError::Store(Inconsistent)` with Core's text "Found EvoDB inconsistency, you must reindex" → the caller's `Fatal` path → `AbortSink::abort_node` (CH2 §2.10). Never a block verdict, never `failed-procspectxsinblock`, never a P2P request (LQ1 D4) | **request** (rev6, Opus r7 O4): `at` is bounded by the **anchor** only (SB0 §5.2: step 5 reads accepted entries at or above it); a `Missing(at)` below the anchor is `Unavailable` with no request, as `BelowFloor`. The request is for the **commitment being verified**, which SB11's caller names: a `getmnlistd` to a target in the authenticated domain, or the `qrinfo` of the **commitment's** cycle (SB0 §5.2.1 rule 2), whose payload carries the deep inputs. A rotated commitment's inputs reach the work block at cycleBase − 3C − 8 (`v24 llmq/utils.cpp:491-504`), 872 blocks below the cycle on mainnet `llmq_60_75` (C = 288), beyond H_keep = 320. So the H_keep/domain bound applies to the request's **target**, never to `at`, and requests are deduplicated by target. The quorum stays `Unverified` until the input arrives; a failed request makes it `Unavailable` |
| `View(e)` | **fatal**, as above. An engine read error has already tripped the latch inside `GuardedEngine` (CH2 §2.4.4 rule 2, §2.10) | `MnViewError::BelowFloor { height }` → `Unavailable` + SB0 §4.3.1 step 4's diagnostic, **no request** (no backfill, the anchor never moves). `NotHeld { at }` → as `Missing`. `Storage` → the `dash-kv` latch has already tripped; stop, no request |
| `UnknownLlmqType(t)` | `DmlException::Members` → `failed-procspectxsinblock` (EV2 §2.11, unchanged); unreachable after EV1/LQ2's type check | unreachable: the engine runs LQ1 only for types in its configured use (SB0 §5.2 step 5); if reached, `MnReject` for the diff that carried it |
| Absent snapshot row, `BlockDataUnavailable`, `None` ancestor | values, as Core (LQ1 §2.3, §3.7.3) | only when Core would see them (§2.1); a not-held input is `Missing`/`View` |

Shared code never decides between the columns: `dash-evo` returns `MemberError<E>`, and the mapping lives in `dash-dml` (`NodeMemberError`) and `dash-mnlist` (SB11).

### 2.4 Tests: a missing connected-block list, in both products (SB0 §5.1)

- **Shared (EV0, `dash-evo/tests/consumers/spv.rs`, compile + run):** a toy SML list implementing `MnListView`, a source with a node-style `E` and one with an SPV-style `E`; a source whose `mn_list(at)` is `Ok(None)` yields `Missing(at)` through a stub member computation; `NodeMemberError::from(Missing(at))` is `Store(Inconsistent)` with Core's text.
- **Node (LQ1a-B, then CH7b):** LQ1a-B: the map-backed source returns `Ok(None)` for a connected work block → `Missing(work)` → `NodeMemberError::Store`, no verdict, no row written. CH7b: the same injected absence through the production facade trips the latch and calls `abort_node` once, emits no P2P message, and leaves no block marked invalid.
- **SPV (SB11):** the same capture with the list for a base above the floor withheld → exactly one `getmnlistd`, the quorum `Unverified`, no latch trip; then the list arrives → `Verified`. With the base below the floor → `Unavailable`, no request, the diagnostic raised. A withheld qrinfo snapshot → `View(NotHeld)`, **not** a broken rotation (§2.1 negative control: a source that answers `Ok(None)` instead produces different members, and the test asserts the engine never does that). A mainnet-shaped rotated commitment whose cycleBase − 3C − 8 work block is not held → the `qrinfo` of the commitment's cycle is requested once (deduplicated by that target), and the commitment verifies (rev6, O4).

### 2.5 Fallibility of every ancestry-reading LQ1 function (M2)

LQ1 §2.5 had fallible member computations but infallible predicates and iterators over ancestry. Each function below takes `&LlmqCtx<'_, E>` and returns `LlmqError<E>`; an ancestry error surfaces as `View(e)` and is never mapped to `false`, an empty list, a skipped type or a broken rotation.

| LQ1 §2.5 item | Reads | rev0 | rev1 |
|---|---|---|---|
| `is_quorum_type_enabled`, `_with` | deployments at `prev` | `Result<bool, LlmqError>` | `Result<bool, LlmqError<E>>` (same shape) |
| `is_quorum_rotation_enabled` | `ancestor(at, cycleBase − 1)` (`v24 llmq/options.cpp:50-61`, `v23 :45`); `cycleBase < 1` → `Ok(false)` with no read, as Core | `bool` | `Result<bool, LlmqError<E>>` |
| `enabled_quorum_params` | `is_quorum_type_enabled` per configured type | `impl Iterator` | `Result<Vec<&LlmqParams>, LlmqError<E>>`, built eagerly in configured order as Core's `GetEnabledQuorumParams` vector is (`v24 options.cpp:125`, `v23 :119`) |
| `precompute_targets` | the two above | `impl Iterator<Item = LlmqType>` | **replaced** by the caller's loop (below), so Core's interleaving of predicate reads and member computations is kept exactly |
| `members_gate` | `prev(base)`, type enabled at it | `Result` | `Result<bool, LlmqError<E>>` (same shape); `prev == Ok(None)` is genesis → `Ok(false)` |
| `members_route` | the rotation predicate, `ancestor(base, cycleBase)` | infallible | `Result<MembersRoute, LlmqError<E>>` |
| `hash_modifier`, `hash_modifier_from_work_block`, `compute_quorum_members`, `compute_quorum_members_from_work_block`, `scan_store_block` | lists, coinbases, ancestry | `Result` | `LlmqError<E>` (same shape) |
| LQ1b's rotation functions (quarter builders, previous-cycle reads, `compute_by_quarter_rotation`) | `ancestor` at `cycleBase − k·C` for k = 1…3, snapshots, lists | `Result` | `LlmqError<E>`. A previous cycle at a negative height is Core's null `GetAncestor` → the loop break (`v24 llmq/utils.cpp:491-494`), computed in i64 with no view call; a previous cycle at a non-negative height the view does not hold is `View(e)` |
| `QuorumPrecompute::precompute` | the targets + members | `Result<(), MemberError>` | `type Error;` `Result<(), MemberError<Self::Error>>` |
| `QuorumMaterializer::get_quorum` | members via the facade | `Result<Option<_>, LlmqError>` | `type Error;` `Result<Option<Self::Quorum>, LlmqError<Self::Error>>` |
| `scan_quorums_from`, `scan_commitments_from` | the reader, the materializer | `I: MinedCommitmentIndex`, `M: QuorumMaterializer` | `I: MinedCommitmentReader`, `M: QuorumMaterializer<Error = I::Error>`, `ctx: &LlmqCtx<'_, I::Error>` |
| `select_from_scan` | none (takes `rotation_enabled: bool`) | infallible | unchanged; its caller resolves `rotation_enabled` with the fallible predicate first |
| `eval_spork`, `signer_index_bits`, `cycle_base_height`, `calculate_quorum`, `select_start_height` | none | infallible | unchanged |

**Precompute loop (replaces `precompute_targets`).** CH7b's `QuorumPrecompute` implementation is Core's `PreComputeQuorumMembers` (`v24 llmq/blockprocessor.cpp:34-42`, `v23 :30-38`), line for line:

```rust
for p in enabled_quorum_params(ctx, prev)? {                       // GetEnabledQuorumParams(pprev): eager
    if is_quorum_rotation_enabled(ctx, p, block)? && h % p.dkg_interval == 0 {
        facade.members(p.ty, block, reset, effects)?;              // GetAllQuorumMembers; result discarded
    }
}
```

**Negative controls (LQ1a-B; LQ1b-D for the rotated reads).** Each must propagate `View(BelowFloor)` or `View(NotHeld)` through a chain view that withholds the block, and the test asserts the result is not `Ok(false)`, an empty list or a broken rotation: (1) an unavailable cycle predecessor (`cycleBase − 1`) in `is_quorum_rotation_enabled`, `members_route` and the precompute loop; (2) an unavailable previous-cycle ancestor in the rotated composition; (3) `prev` at a non-genesis floor in `members_gate`. The positive controls are Core's values: `cycleBase < 1` → `Ok(false)`; actual genesis → `Ok(false)`; a negative previous-cycle height → the break.

### 2.6 The attempt overlay and its serialization (EV0 m2, resolved per DEC-111 and DEC-113)

**Core.** `StoreSnapshotForBlock` is an unconditional raw write plus a snapshot-cache insert (`v24 llmq/snapshot.cpp:320`, write `:327`; `v23 :283`, `:290`). It bypasses the evodb transaction, so it survives a failed block, `fJustCheck`, a GBT proposal and VerifyDB, and the next `GetSnapshotForBlock` (`v24 :301`, `v23 :264`) in the same attempt sees it, including a later overwrite. Each is one critical section: `GetSnapshotForBlock` holds `LOCK(snapshotCacheCs)` across the cache `get`, the evodb `Read` and the cache `insert` (`v24 :307-315`, `v23 :270-278`), and `StoreSnapshotForBlock` holds `LOCK2(snapshotCacheCs, m_evoDb.cs)` across the raw write and the cache `insert` (`v24 :326-328`, `v23 :289-291`). A read therefore never inserts a value older than a write it overlapped; this addendum keeps both sections (rev6, below).

**Ownership.** CH7b's process-wide `MembersFacade` owns both members caches (and through the runtime, the snapshot cache). For each validation attempt, CH7a obtains an `AttemptHandle` from it:

```rust
// node runtime (CH7b); names indicative
pub struct MembersFacade { caches: Mutex<MembersCaches>, gate: RwLock<()>, /* … */ }   // gate: §2.6 serialization
impl MembersFacade {
    /// Takes the gate exclusively; waits for in-flight external routes to finish. Needs V's context
    /// (rev5, R5-M1), so only V's command loop can open an attempt.
    pub fn begin_attempt<'v>(&'v self, v: &'v mut VContext, id: AttemptId) -> AttemptHandle<'v>;
    /// The only release: consumes the handle once CH7a has applied the attempt's whole log.
    pub fn release_attempt(&self, h: AttemptHandle<'_>, applied: EffectsApplied);
    /// Caller-thread entry (§2.6 allow-list): the shared gate; reaches only `RawRows`.
    pub fn external(&self) -> ExternalToken<'_>;                                            // shared gate
}
impl ExternalToken<'_> { pub fn rows(&self) -> RawRows<'_>; }   // the closed allow-list, and nothing else
/// V's affinity proof (rev5, R5-M1). V's command loop creates exactly one when the V thread starts;
/// the constructor is private to that loop module (not merely `pub(crate)`). `!Send`, `!Sync`, never
/// handed out. Both capabilities are minted from it, and only from it.
/// Zero-sized and stateless (rev6, O3): no field beyond the marker. V's state (chainstate, the evo
/// store writer, the effect ledger) is held beside it, so the `&mut` borrow an open attempt takes
/// never locks that state out (state inside the context is E0499 at log application).
pub struct VContext { _v_only: PhantomData<*const ()> }
impl VContext {
    /// A dispatched command's capability: the shared gate. Private to the loop module. `&mut self`
    /// means an `OnV` and an `AttemptHandle` can never coexist, so V cannot ask for the exclusive gate
    /// while it holds the shared one; that read-to-write self-deadlock is a compile error.
    fn on_v<'v>(&'v mut self, facade: &'v MembersFacade) -> OnV<'v>;
}
/// The capability for every other route (rev4, DEC-117), minted only by `VContext::on_v`. `!Send`, `!Sync`.
pub struct OnV<'f> { token: ExternalToken<'f>, _v_only: PhantomData<*const ()> }
/// Sealed: implemented for `OnV<'_>` and `AttemptHandle<'_>` only.
pub trait OnValidation: sealed::Sealed { /* rows (read and write), caches, facade */ }
// Every runtime `QuorumMemberSource`, `QuorumPrecompute`, `QuorumMaterializer` and writable `SnapshotRows`,
// and every composite builder (scans, `GetQuorum`, the predictor, qrinfo, rotationinfo, mnlistdiff, mnxcheck),
// is constructed from or takes `&C where C: OnValidation`. `ExternalToken` implements none of them.
/// Minted only by `begin_attempt(&mut VContext, …)`. `!Send` and `!Sync`, like `OnV` (rev5).
pub struct AttemptHandle<'f> { facade: &'f MembersFacade, id: AttemptId, _gate: RwLockWriteGuard<'f, ()>,
                               _v_only: PhantomData<*const ()>,
                               overlay: RefCell<BTreeMap<(LlmqType, BlockHash), QuorumSnapshot>> } // last write wins
pub struct EffectsApplied { attempt: AttemptId }   // minted only by CH7a's log application for that attempt
impl QuorumMemberSource for AttemptHandle<'_> { type Entry = Arc<Dmn>; type Error = StoreError; … }
impl QuorumPrecompute   for AttemptHandle<'_> { type Error = StoreError; … }
```

- **One overlay per attempt**, shared by every members and precompute call in it (block connect, the CbTx/MNHF/asset-unlock `GetQuorum`s, LQ2's commitment checks). Each call builds `AttemptRows { overlay: &handle.overlay, backing: runtime rows, sink: effects }` as its `SnapshotRows`. The `RefCell` is there because `members` takes `&self`; the handle is `!Sync` and an attempt is serial (rev5: it was a `Mutex` while sources were `Send + Sync`).
- **`store`** puts the row in the overlay (overwriting) and pushes `Effect::StoreQuorumSnapshot { t, cycle_base, snapshot }` into `effects`, which is the attempt's one ordered log for every call. **`get`** reads the overlay first, then the backing rows. `EffectSink` stays write-only.
- **Caches** are the facade's, not the handle's: a members-cache insert made during an attempt stays made when the attempt fails or is a proposal, as in Core.
- **Apply.** CH7a applies the log in order regardless of the attempt's result (failure, `fJustCheck`, GBT proposal, VerifyDB levels 3/4), before the next attempt's read view opens; each `StoreQuorumSnapshot` overwrites the runtime row synchronously, the write and its cache insert in one snapshot-cache critical section (rev6, O1), and a write failure goes D4 → `abort_node`. CH7a then calls `release_attempt` with the `EffectsApplied` token, which drops the overlay and the gate.
- **`AttemptHandle` stays on V (rev5).** It is `!Send`, and the design does not need it to cross threads. Every call that uses it runs on V (route C): connect, the CbTx/MNHF/asset-unlock `GetQuorum`s, LQ2's checks and precompute. Work CH7a hands to other threads, such as parallel script checks, never takes the handle.
- **Every attempt opener runs on V, after its `VContext` exists (rev6, Opus r7 O3).** The openers are connect and disconnect, reorgs, `TestBlockValidity`/GBT proposal, startup VerifyDB and activation, import and `-reindex`, invalidate/reconsider/precious, and `EnforceChainLock`. There is no second `VContext` constructor, so none of them can run on another thread.
  - **A1/CH7a erratum.** A1 §2.10's `ChainCommand` (`A1-architecture.md:977-984`) has no VerifyDB variant. It gains one, `VerifyDb { check_level, check_blocks, reply }`, and the `verifychain` RPC sends it. A1's init step 6 (`:1494`, load the block index, replay, VerifyDB) and §3.5.5 steps 5–6 (`:1902-1903`, load the tip, VerifyDB, `activate_best_chain`) run on the V thread, either before its command loop starts serving or as `ChainCommand`s. They never run on the init thread: a literal port of Core's `AppInitMain` threading there would need a second constructor, which compile-fail cases 1 and 6 forbid.
  - **Tests** open attempts through a real V loop harness. No `#[cfg(test)]` or feature-gated public constructor of `VContext`, `OnV` or `AttemptHandle` exists: feature unification could leak a feature-gated one into production builds, and `cfg(test)` is invisible to other crates' tests anyway.
- **Non-attempt callers** (RPC, QM1 signing and lock verification, DK1, `qfcommit` intake, mnxcheck) use the facade with a runtime `SnapshotRows`, under the serialization below. Evo rows have one writer, V (A1 §2.1 rule 4; CH2 gives rotation-snapshot writes and their presumed-effect ledger to V). Since rev4 (DEC-117) the rule is an **allow-list**. Only the closed list of raw getters below runs on the caller's thread. **Every other external route is dispatched whole to V** as a `ChainCommand`, before it takes any gate or cache lock, and runs there under an `OnV` capability. V writes rows inline and synchronously, each write and its cache insert in one snapshot-cache critical section, with D4 on failure (the lock order below).

**Serialization (DEC-113; Sol r2 R2-M1).** The attempt publishes its members to the process-wide caches at once (they are process state), but its row writes reach the runtime rows only when the log is applied. A reader outside the attempt could therefore see the new roster M1 from a cache hit and then read the cycle's *old* row S0. Core cannot expose that pair: the rotated computation writes the row before it returns (`StoreSnapshotForBlock`, `v24 llmq/utils.cpp:462` → `snapshot.cpp:326-328`; `v23 utils.cpp:453` → `snapshot.cpp:289-291`), and only then does `GetAllQuorumMembers` publish the indexed entries (`v24 utils.cpp:710-713`, `v23 :639-642`) and the base entry (`v24 :727-728`, `v23 :656-657`). A reader that hits M1 therefore reads S1. rev2 restores LQ1 §3.7.2's serialization with validation rather than adding a shared pending-write view:

- **The gate.** `MembersFacade::gate` is one `RwLock<()>` (writer-preferring, so validation is not starved). An attempt holds it **exclusively** from `begin_attempt` until `release_attempt`. That covers the attempt's body, its return on any outcome, and CH7a's application of its whole log. Every external route holds it **shared** for the whole of its call. External routes therefore run concurrently with each other, as Core's do, but never inside an open attempt. Serializing them with validation is one of Core's schedules (LQ1 §3.7.2, §3.7.4).
- **Where the wait sits (rev4).** There are two kinds of external route, and each waits in one place:
  - **Dispatched routes** (everything not on the allow-list). The caller sends the whole operation as a `ChainCommand` and waits for the reply holding no gate, token or cache lock. If an attempt is open, the command waits in V's queue. When V runs it, between attempts, V's command loop takes the shared gate **first, before any cache lookup, ancestry read or row read**, and wraps it as `OnV`. Every nested call reuses that `&OnV` inline: members, `GetQuorum` and the materializer builds a scan makes, and `BuildSimplifiedMNListDiff` → `BuildQuorumChainlockInfo` → `GetQuorum` inside the composite builders. Nothing re-takes the gate (a recursive shared acquire on a writer-preferring lock deadlocks once a writer queues), and nothing dispatches again (V cannot wait on itself; `dispatch` called on V is a debug assertion).
  - **Allow-listed getters** (`RawRows`, below). The caller's thread calls `facade.external()` first and waits on the gate.
- **Inside the attempt** every call goes through the `AttemptHandle`, which already holds the gate, so none of them waits.
- **Release on every outcome.** CH7a calls `release_attempt` after success, failure, `fJustCheck`, GBT proposal and each VerifyDB attempt. `EffectsApplied` can only be minted by CH7a's log application for that `AttemptId`, so the gate cannot open before the rows are in.
  - If applying the log fails, the latch trips (D4) before release. Routes that were waiting wake to the latch's fatal error and read no rows.
  - A handle dropped without `release_attempt` (an unwind) trips the latch, then drops the gate. Its log was never applied, so the rows Core would hold are missing.
- **Lock order (rev3, R3-M1).** Every route takes these in this order and never backwards: **V dispatch** (a `ChainCommand` sent and its reply awaited) → **the gate** (a shared `ExternalToken` or the exclusive `AttemptHandle`) → **the facade's cache mutex** → **the snapshot cache's lock** (one lock, Core's `snapshotCacheCs`; rev6, O1), held across V's store write for a row and that row's cache insert, and across a getter's whole read-through. No thread waits on V (a `ChainCommand` reply, or any callback V services) while it holds the gate, shared or exclusive, or the cache mutex. Otherwise a token holder waits on V while V waits for the exclusive gate in `begin_attempt`, and both hang.

  **Allow-list (DEC-117; closed).** `ExternalToken::rows()` returns `RawRows`, which has exactly these methods and nothing else. None of them touches the members, quorum-object, scan or memo caches or a lazy production key, computes members, materializes a quorum, persists a write or calls back into V. The list is not cache-free: one getter keeps Core's snapshot-cache bookkeeping (rev5, R5-m1).
  - `snapshot_for_block(t, cycle_base)`: Core's `GetSnapshotForBlock`, a cache-through row read (`v24 llmq/snapshot.cpp:301-318`, `v23 :264-281`). It is the **one permitted cache operation** on this list. On a disk hit it inserts the row into the snapshot cache, and on a warm hit it bumps that cache's recency (`v24 :307-313`, `unordered_lru_cache.h:65`; `v23 :270-276`, `:63`), so it can change later evictions, as Core's getter does. The whole read-through, meaning the cache lookup, the disk read and the insert, is one critical section under the snapshot cache's lock (rev6, O1, below), taken under the shared gate; it waits on nothing else. mnxcheck does not use this getter (§4.3);
  - the runtime store's `MinedCommitmentReader` methods (§2.2): `mined_commitment` and `mined_index`, plus the provided `mined_until`, `nth_mined_by_index` and `newest_by_index` (Core's `GetMinedCommitment` and the `GetMinedCommitments…UntilBlock` family).

  Adding a getter is a design change that needs the same review. A getter that can reach the members, quorum-object, scan or memo caches, a lazy production key, a member computation, a materialization, a persistent write or a V callback can never be added. The production `RawRows` implementation needs the same transitive audit as Core's getters; a reader trait alone does not establish that an implementation is pure (Sol r5).

  | Route | Thread and order |
  |---|---|
  | A. The allow-listed getters | caller's thread; `external()`, then `rows()` |
  | B. Every other external route: runtime members (DK1, `qfcommit` intake, `quorum members` and other RPCs, LQ5); QM1 scans, `select_quorum_for_signing` and `GetQuorum` (incl. `isdlock`/`clsig` verification); the predictor; **the qrinfo and `quorum rotationinfo` builders and `mnlistdiff` serving** (they reach `GetQuorum` → members → `StoreSnapshotForBlock` through `BuildSimplifiedMNListDiff` → `BuildQuorumChainlockInfo`, which walks every `newQuorums` entry: `v24 llmq/snapshot.cpp:135-136`, `:250-252` → `evo/smldiff.cpp:221-226` → `:76-80` → `llmq/quorumsman.cpp:536` → `:71`; `v23 :90-91`, `:205-207` → `:199-203` → `:77-81` → `:414` → `:80`; LQ1 §3.7.1, §6.3.2); mnxcheck (§4.3); **any route added later** | dispatched whole to V **before any gate or cache lock**; V takes the shared gate as `OnV`; nested calls reuse it inline. Rotated or not, hit or miss: the route is not classified by what it might touch |
  | C. Calls inside an attempt (connect, CbTx/MNHF/asset-unlock `GetQuorum`s and the scans and materializations they make, LQ2's checks, precompute; `ChainHelper` back ends) | V, through the `AttemptHandle` (an `OnValidation`). They never dispatch and never take a token |
  | D. Log application and `release_attempt` | V, inline, under the exclusive gate |

- **Snapshot-cache critical sections (rev6, Opus r7 O1; normative, as Core's `snapshotCacheCs` at `v24 llmq/snapshot.cpp:301-329`, `v23 :264-292`).**
  - `snapshot_for_block` holds the snapshot cache's lock across the cache lookup, the disk read and the insert (`v24 :307-315`, `v23 :270-278`).
  - Every row write holds the same lock across the raw write and the cache insert (`v24 :326-328`, `v23 :289-291`): V's inline writes on route B, the log application on route D, and the backing writes `AttemptRows` reaches.
  - No implementation may unlock around the disk read, or between a write and its insert. A `Mutex<LruCache>` that misses, drops the lock for the RocksDB read and re-locks to insert is wrong.

  The gate does not order these: route A and a route-B command both hold only the shared gate. Without the sections, a getter that read S0 from disk could insert it after V overwrote the row with S1 and inserted S1. The disk would then hold S1 and the cache S0, and the next rotated computation for cycle K + C would read S0 as K's previous-cycle snapshot and build members that differ from Core's. Holding the sections adds no cycle to the lock order: the getter waits on nothing else while it holds the lock, and V never waits on the caller. Case 7 tests this.
- **Capability enforcement (DEC-117).** The routes are kept apart by types, not by a table someone must keep complete:
  - every materialization, members, snapshot-write and composite-builder entry point needs an `OnValidation`;
  - only V can construct one: `OnV` from `VContext::on_v`, `AttemptHandle` from `begin_attempt(&mut VContext, …)`. Both minting paths need V's context (rev5, R5-M1), so a public `begin_attempt` gives caller-thread code nothing;
  - `ExternalToken` reaches only `RawRows`.

  A future route that is misclassified therefore fails to compile instead of deadlocking. CH7b's acceptance includes a compile-fail suite (trybuild or equivalent) that must reject each of these:
  1. constructing `OnV` outside V's command loop module;
  2. calling any members, `GetQuorum`/scan/materializer, predictor, qrinfo/rotationinfo/mnlistdiff builder or `SnapshotRows::store` entry point with an `ExternalToken`;
  3. calling anything on `RawRows` beyond the allow-list;
  4. moving `OnV`, `AttemptHandle` (or a reference to either) to another thread;
  5. implementing `OnValidation` outside its crate;
  6. calling `begin_attempt` without V's `VContext` (from caller-thread code, from an `ExternalToken`, or from an `OnV`), or while an `OnV` from the same context is alive (rev5, R5-M1).

  **The V command boundary (Sol r5).**
  - A dispatched command is a synchronous, scoped call: the loop runs `for<'v> FnOnce(&OnV<'v>) -> R` and drops the `OnV` when it returns, so neither the capability nor a reference to it can escape the command.
  - The guarded operation runs no local executor. `!Send` does not forbid holding a capability across an `.await`; it is A1 §3.1's synchronous evo/chainstate/store APIs that rule that out.
  - Runtime adapters are capability-parameterized, command-scoped views with compatible bounds. None validates a capability once in a factory and returns a transferable, ungated members or writer object.
  - The command queue is FIFO and serviced fairly. A command never waits on work that V itself must service.
  - PF measures V's queue wait and service time under cold composite requests, alongside validation and signing load. A long command delays an attempt queued behind it (an allowed Core schedule: `BuildQuorumRotationInfo` runs under `cs_main`, `v24 snapshot.cpp:82`, `v23 :54`), but it cannot block it indefinitely.

  Two consequences follow:
  - V never blocks on the gate inside a dispatched command. V is the only thread that opens attempts (enforced since rev5: `begin_attempt` needs `VContext`), so no writer is queued while V runs a command, and its shared acquire is immediate.
  - Calls that would only have hit a cache also go through V. This is conservative, and the cost is latency for PF to measure. It is not a semantic change, because deferring a call until no attempt is open is one of Core's schedules (Sol r3).
- **Liveness.** No route holds the gate, shared or exclusive, while it waits on V, on P2P, or on any I/O other than the local store reads and V's inline writes named above. An external reader waits until it acquires the gate after an attempt's log application. A writer-preferring lock does not guarantee that the reader gets in before V's next attempt, so its latency and fairness are PF's to measure (Sol r3 R3-m1).

Within one thread nothing changes. A reader outside the attempt sees either the state before the attempt or the state after its log is applied. Both are states Core can expose.

**Conformance cases** (LQ1m lands `reference::ModelAttempt` over LQ1a's `ModelMembersFacade` with cases 1–6; rev6, O5 moved them off LQ1a. CH7b's production handle must pass the same suite, opening its attempts through a real V loop (O3), plus case 7):
1. **Two calls, one attempt** (Sol r1): call 1 stores a row; call 2 in the same attempt reads it and computes from it. Negative control: a per-call overlay (EV0's current stub) reads no row and fails the case.
2. **Overwrite:** two stores to one key; a later read sees the second; both effects are in the log, in order.
3. **Attempt isolation:** a new attempt starts with an empty overlay and reads the first attempt's rows from backing once its log is applied.
4. **Failure and `fJustCheck`:** an attempt that fails after a store, and a check-only attempt, still apply their effects; the members-cache inserts they made survive.
5. **Runtime write failure:** applying a `StoreQuorumSnapshot` that fails trips D4 (`abort_node` once, no verdict).
6. **External reader during an open attempt (DEC-113, Sol r2).**
   - *Fixture:* a connected older cycle K has row S0. Its members-cache entries have been evicted. The attempt recomputes K with changed inputs, producing S1 and roster M1.
   - *Pause:* after the rotated cache insertion (LQ1 §3.7.4 steps 4–5: the indexed entries, then the base entry) and before CH7a applies the log.
   - *External call:* a cache hit on M1, then a read of K's snapshot row. In CH7b this is a dispatched route (B). An allow-listed observer (route A) also reads K's row through `RawRows::snapshot_for_block`.
   - *Requirement:* the call waits. It must not return while the attempt is open. After release it returns (M1, S1), and the observer reads S1.
   - *Where to assert the wait (rev4, R4-m1).* Each kind of route is checked at the place it actually waits:
     - the dispatched call: its command is acknowledged as queued on V, and it makes no lookup, no read and no reply until `release_attempt`;
     - the caller-thread observer: it is parked on the gate, seen through the gate's waiter count.
   - *Matrix:* six runs, attempt ∈ {succeeds, fails, GBT proposal} × hit ∈ {base cache: the attempt's own base; indexed cache: another index `j` of the same cycle, whose base entry is absent}. In the indexed run the external call's own base insert happens only after the wait.
   - *Negative control, scoped (rev4, R4-m1):* it bypasses **both** boundaries on purpose, so that it can expose (M1, S0).
     - In LQ1m's model it is a direct-call facade with neither the gate nor dispatch.
     - In CH7b it is a test-only hook that runs the members hit and the row read on the caller's thread with no dispatch and no gate.

     Removing only the gate from a dispatched command is not a control: V's queue still holds the command behind the attempt.
   - *Waiting, without sleeps:* in LQ1m's model, which is pure and has no threads, the gate is a state machine. An external call made while an attempt is open returns `Blocked`. The case asserts `Blocked`, releases, re-runs the call and asserts (M1, S1). CH7b runs all six combinations with real threads, asserting each wait at its own location (above), and finishes with (M1, S1) and S1 only after `release_attempt`.
   - **6-D, the dispatch deadlock (rev3, Sol r3 R3-M1; CH7b, real threads).** LQ1m's pure model has no V, so this case is CH7b's.
     - *Fixture:* a rotated type with a cold cache for cycle K, so the external call misses and must store K's row.
     - *Run:* an external caller starts a dispatched members call (and, in a second run, a dispatched scan whose materializer misses). It is paused before its synchronous snapshot write. Validation then submits an attempt, and the pause is released.
     - *Requirement:* both calls finish within a bounded wait. The row is written before the call's cache entries are visible: a third reader sees either the state before the call or (M, S) together. The attempt's result is the same as when it runs alone.
     - *Matrix:* three pause positions:
       - (a) the attempt is open when the caller dispatches; its command waits in V's queue until release;
       - (b) the command is running on V, paused before the store, while the attempt waits in V's queue;
       - (c) an allow-listed getter (route A) holds its token on the caller's thread while the attempt queues for the gate.

       These run × attempt ∈ {succeeds, fails, GBT proposal} × route ∈ {members, scan with materializer miss}.
     - **Composite builder, post-V20, cold rotated quorum (rev4, Sol r4 R4-M1).**
       - *Fixture:* the target is post-V20. The `mnlistdiff` inside a qrinfo response adds a rotated quorum whose quorum-object and members caches are both cold, so `BuildSimplifiedMNListDiff` → `BuildQuorumChainlockInfo` → `GetQuorum` → members misses and stores a row.
       - *Run:* the qrinfo builder, `quorum rotationinfo` and a direct `mnlistdiff` request each run as a dispatched route, with an attempt queued at positions (a) and (b), × attempt ∈ {succeeds, fails, GBT proposal}.
       - *Requirement:* both finish. The nested `GetQuorum` and members calls run inline on the command's `OnV`; V's queue receives nothing from inside the command (no self-dispatch); the row is written before its cache entries are visible.
       - *Negative control:* rev3's route-4 arrangement. The builder holds a caller-thread token and its nested rotated `GetQuorum` dispatches to V. With the attempt queued, this hangs. Production code cannot express it, so only a test hook can build it.
     - *Nested validation:* during the attempt, a validation-time scan whose materializer misses finishes through the `AttemptHandle`, and V's command queue receives nothing from inside the attempt.
     - *Negative control (queue-after-token):* the caller takes `external()` on its own thread, computes, then sends its row write to V and waits for the reply. With the attempt queued, this **hangs**. The harness detects it with a bounded reply wait, asserts the hang, and releases it through a test-only escape so both threads can join. Production code cannot express the arrangement: an `ExternalToken` reaches no `store`, members or materialization entry point (the compile-fail suite above).
7. **Snapshot-cache interleaving (rev6, Opus r7 O1; CH7b, real threads).**
   - *Fixture:* a pruned node. Cycle K's row S0 is on disk and not in the snapshot cache, and K's members-cache entries are evicted. K's work block is pruned, so a recomputation takes the hash-form modifier (LQ1 §3.7.3; `v24 evo/cbtx.cpp:181-193`) and produces S1 ≠ S0.
   - *Run:* a route-A `RawRows::snapshot_for_block(t, K)` on the caller's thread is paused by a test hook after its disk read and before its cache insert. A dispatched route-B command (`quorum members` for K) then runs on V: it misses the members caches, recomputes, and overwrites the row with S1. The getter is then resumed.
   - *Requirement:* with Core's section, V's write waits on the snapshot cache's lock until the getter resumes and finishes. The harness asserts that wait with a bounded check, not a sleep. Afterwards the disk holds S1, the snapshot cache holds S1, and the next `snapshot_for_block(t, K)` returns S1.
   - *Negative control:* a getter that drops the lock around its disk read. V's overwrite then completes during the pause, and the resumed getter inserts S0. The case must end with S0 cached while the disk holds S1, so the control fails the requirement.

Until LQ1b-E's rotation exists, a test computation that stores rows and fills the indexed cache stands in for it in cases 1–4 and 6. LQ1b-E reruns them with the real rotation.

---

## 3. The EV0 branch diff (ordered commits)

Base: rebase `node/ev0-evo-interfaces` (3a5cb631, on 7c08530e) onto the `node/integration` tip that carries the BS5+BS6a re-pin (cb504fab/a8755d86). Expected conflicts: `Cargo.toml` members, `.github/ci-groups.yml`, `Cargo.lock`. With DEC-71/79 in tree, `check-pure.sh` must pass with no local exception (EV0 API.md §6 "Purity gate and blst"). Then:

| # | Commit (conventional) | Content | Acceptance |
|---|---|---|---|
| 1 | `feat(dash-evo): add LQ1's list views and a fallible chain view` | `llmq::{MnEntryView, MnListView, ScratchMnList, ListCounts, AddMnRejected}` (LQ1 §2.2 text), the forwarding `impl<T: MnEntryView> MnEntryView for Arc<T>`, `LlmqChain` with §2.2's contract in its doc, `CompleteChain`, `BrokenInvariant`; **`impl MnEntryView for Dmn`, `impl MnListView for MnList` and `impl ScratchMnList for MnList`** in `dash_evo::dml` (field reads only; `type Entry = Arc<Dmn>` through the forwarding impl, `type Scratch = MnList`; rev6, O2); `tests/consumers/lq1a.rs` uses them instead of its copies and **deletes** its own `impl MnEntryView for Arc<Dmn>` and `impl MnListView for MnList` (`lq1a.rs:52`, `:73`), which become E0117 once the traits are `dash-evo`'s | builds; consumers compile; `CompleteChain` never errs (unit test over `MemChain`); `ancestor` above the height is `Ok(None)`; `Arc<Dmn>: MnEntryView` holds, so commit 2's `Entry = Arc<Dmn>` consumers compile |
| 2 | `refactor(dash-evo)!: make the member source generic with store-independent errors` | `MemberError<E>`, `QuorumMembers<E>`, `QuorumMemberSource { type Entry; type Error }` without the `Send + Sync` supertrait (§2.2, rev5; EV0 Q10 superseded); `MinedCommitmentReader { type Error: From<BrokenInvariant> }` moved into `llmq` with its index types and `base_through`; `NodeMemberError` + `From` beside `StoreError`; `ev2b.rs`/`ch7a.rs` consumers map through `NodeMemberError` | every EV0 test passes; `tests/store.rs` unchanged in substance; new mapping test (`Missing` → `Store(Inconsistent)`, Core's text) |
| 3 | `refactor!: split dash-evo into shared dash-evo and node-only dash-dml` | `git mv` per §1.2 (`dml/`, `store/` minus the reader, `creditpool`, `mnhf`, testkit builders, the four consumer tests, `tests/store.rs`); `ProTxHash`/`MnType`/`RawMnType` → `dash_evo::types`, re-exported by `dash_dml`; the `git mv` carries commit 1's `Dmn`/`MnList` view impls into `dash-dml` (legal there: local types; `Entry = Arc<Dmn>` through commit 1's forwarding impl); `dash-dml/{Cargo.toml, clippy.toml, LICENSE}`; workspace member; `dash-dml` in the `node-pure` CI group (so `check-pure.sh` covers it) | both crates pass `check-pure.sh`, clippy (`--all-features` and `--no-default-features`, per the EV0 memory note) and their tests; `cargo tree -p dash-evo -e normal` contains neither `imbl` nor `dash-dml` (a check in `contrib/node/check-pure.sh` or a test script, until SB1's L2/L6 gates exist); a `dash-dml` test (a real cross-crate consumer) calls a generic `fn f<E: MnEntryView>` from `dash-evo` with `Arc<Dmn>` and gets `Dmn`'s field values |
| 4 | `test(dash-evo): add a compile-only SPV consumer for the error contract` | `tests/consumers/spv.rs` (§2.4 shared part); an SPV-style error with `BelowFloor`/`NotHeld`/`Storage`; a toy `LlmqChain` that withholds a block and a stub rotation predicate over it | runs; `Missing`/`View` reach the consumer's mapping unchanged; the withheld predecessor yields `View(NotHeld)`, not `false` (§2.5's shape, ahead of LQ1a) |
| 5 | `test(dash-dml): share one attempt overlay across member calls` | `tests/consumers/lq1a.rs`'s per-call `AttemptRows` becomes a view over an attempt-owned overlay (§2.6) held by a test `AttemptHandle`; §2.6 cases 1 (with the per-call negative control) and 2 over a stub computation that stores rows | both cases pass; the negative control fails as expected |
| 6 | `docs(dash-evo, dash-dml): split API.md and ARCHITECTURE.md for the SB0 split` | `dash-evo/ARCHITECTURE.md`: purity, BLS, effects, the error contract, §2.1's not-held rule and §2.2's chain contract; `dash-dml/ARCHITECTURE.md`: Core's orders, the store, no `PartialEq` on DML values, D4, the attempt overlay's ownership (§2.6); `API.md` in each crate: §1.2's map and this addendum as an input | doc build; links resolve |
| 7 | `docs(dash-evo): record the EV addendum decisions in API.md §7` | EV0 Q-table rows for the split, the generic errors, the `Arc<T>` forwarding impl, and m2 **resolved** (DEC-111, §2.6); m5 still lands in EV2a's first commit | — |

**Order and E0117 (rev6, Opus r7 O2).** Each commit compiles on its own. The view impls for `Dmn` and `MnList` land in commit 1, inside `dash-evo`, while those types are still local there. Commit 3 moves them into `dash-dml`, where the types are local again. The test crate can never hold them: once the traits are `dash-evo`'s, it owns neither the traits nor `Dmn`, `MnList` or `Arc`, and both impls are E0117. The lean `UsedSet` in `lq1a.rs` is a local type, so its impls stay legal. We chose this over wrapping `MnList` in a local newtype until EV2a-1: it is the smaller diff and costs EV2a's zero slack nothing.

No commit changes behaviour: the only semantic edit is that `MemberError::Store` splits into `Missing`/`View` and is re-joined node-side by `NodeMemberError`, so node callers see EV0's two variants as before. Commit 5 changes a test stub only.

**Review (EV0 r3, m1):** Sol then Opus high (DEC-92). It covers the post-rebase code delta, commits 1–5, diffed with `git diff -M` so moved bodies show as moves, **and** the final normative documents of commits 6–7 (`API.md`, `ARCHITECTURE.md`), checked against this addendum, including §2.6's m2 resolution. r2's approval is reused for bodies the move leaves unchanged.

---

## 4. EV2a and LQ1a deltas

### 4.1 EV2a (against EV2 rev4 and the EV2a PLAN row)

| Item | Delta |
|---|---|
| Crate | `dash-dml`. EV2 §2.1's module list applies under `dash-dml/src/` (`store.rs` holds EV0's moved `store/` plus `resolve_list`, `ListCache`). §2.1's dependency list adds `dash-evo` |
| New (SB0 §12.1) | `impl MnListView for MnList` and `impl ScratchMnList for MnList` land in **EV0 commit 1** (rev6, O2; §3), with `type Entry = Arc<Dmn>` (via §1.3's forwarding impl) and `type Scratch = MnList` (`scratch()` + `try_add(_, bump = true)`, which LQ1 §2.2.4 already names as `UsedSet`'s twin). EV2a-1 keeps them compiling against its `MnList`. LQ1b-E later lands `UsedSet` in `dash-dml` as the production scratch, with that twin under `xcheck`. EV2a-1 acceptance adds LQ1 §2.2's contract tests for `MnList` (counts, `contains`, `is_pose_banned` false when absent, `for_each(only_valid)` as a set) |
| Moved out | `MnList::project_sml` (SB0 §12.1/§13.2 put it in EV2a) goes to **EV4-serve** (DEC-109 (2)), which already owns "projection, incremental root cache" (SB0 §12.1) and depends on SB7, which defines the SML entry. Core: `CDeterministicMN::to_sml_entry` (`v24 evo/deterministicmns.cpp:43-46`, `v23 :45-48`), list build (`v24 :255-259`, `v23 :280-284`) |
| §2.11 / §2.12 text | `QuorumMemberSource` → `&DmlMemberSource`; `MemberError::Store` → `NodeMemberError::Store` (via `From<MemberError<StoreError>>`). Mapping and D4 unchanged. §2.12 "dash-spv: none direct" now reads: the projection (EV4-serve) feeds mnxcheck (SB19) |
| Unchanged | the DML model, CHAMP order, payee queue, store, codecs, every acceptance gate |

### 4.2 LQ1a (against LQ1 rev4 and the LQ1a PLAN row)

| Item | Delta |
|---|---|
| Views | §2.2's traits come from EV0 (§1.3), text unchanged. The `MnList` adapter is EV0's (commit 1, rev6), with its contract tests in EV2a; LQ1a-B's "`MnList` adapter" deliverable drops |
| Signatures | §2.2 and §2.5 of this addendum: `LlmqCtx<'a, E>` with `chain: &dyn LlmqChain<Error = E>` (was `&dyn ChainAncestry`); `LlmqSource<L>` and `SnapshotRows` gain `type Error`; `mn_list`/`coinbase_facts` return `Option`; `LlmqError<E>` is `MemberError<E>`; the rotation predicate, `enabled_quorum_params` and `members_route` become fallible; `precompute_targets` gives way to the caller's loop; `QuorumPrecompute`/`QuorumMaterializer` gain `type Error`; one error type per call. §2.5's `MinedCommitmentIndex` was already retired for EV0's reader (EV0 Q1), now at `dash_evo::llmq::MinedCommitmentReader` |
| Not-held rule (new) | §2.1. LQ1a-B's map-backed test source and chain view get a "not held" mode. Tests: a not-held list → `Missing(at)`; a not-held work-block coinbase → `Missing(work)`, **never** the hash-form modifier; §2.5's negative controls (1) and (3); LQ1b-D adds control (2) and a not-held snapshot → `View(e)`, never the 28a/28e break. The existing absent-row vs store-error pair (LQ1 §2.6) stays |
| D4 (node) | unchanged; LQ1a-B adds §2.4's node test |
| Attempt overlay (new; §2.6) | LQ1a codes against §2.6: its `SnapshotRows`, `ModelMembersFacade` and signatures take the attempt shape. LQ1 §3.7.4's facade text gains the attempt handle and the gate; LQ1 §3.7.2's serialization stands as approved (DEC-113). EV0 m2 no longer gates LQ1a's start. **The model and its cases move to LQ1m** (rev6, Opus r7 O5): `reference::ModelAttempt` over `ModelMembersFacade`, §2.6's cases 1–6 with the negative controls of cases 1 and 6, and the state-machine model gate (`Blocked`), since the crate is pure. They are model-only test infrastructure that nothing before CH7b consumes, so they leave LQ1a's zero-slack edge |
| `members_uncached` (new public fn) | LQ1 §2.5 keeps `get_all_quorum_members_uncached` in `reference` (tests only). SB0 needs an uncached composition in production for consumers without Core's caches: `dash-mnlist` (SPV `membersSig` input, SB8/SB11) and mnxcheck's miss path (SB19, §4.3). It is promoted to a public `members_uncached` with the same body. **Phasing (m3):** LQ1a lands it `pub(crate)` with the non-rotated route; LQ1b-E adds the rotated route and makes it `pub`. Until then SB8 composes the public non-rotated steps (`members_gate`, `members_route`, `compute_quorum_members`) and its adapter treats `MembersRoute::Rotated` as unavailable; no build ever accepts against an empty or stub rotated route. Its doc states the rule that node consensus paths reach members only through CH7b's facade (LQ1 §3.7.4): a bypass changes when rows are written, which CH7b's conformance suite detects. `precompute_uncached` stays in `reference` |
| mnxcheck hook (new; SB0 §5.4) | §4.3. LQ1a-B's acceptance adds: on non-rotated captures with a cold cache, `members_uncached` over the model facade's inputs equals `ModelMembersFacade::members` and leaves every model cache, its access order and every row byte unchanged (LQ1b-E repeats it for rotated). The warm-cache → block-data-unavailable case of §4.3 over the model moves to **LQ1m** (rev6, O5) |
| H_keep inputs (new; SB0 §5.2.1) | LQ1a exports `pub const SIGN_HEIGHT_OFFSET: u32 = 8` beside `select_start_height` (`v24 llmq/quorumsman.h:210`, `v23 :184`). §2.7's BS5 list adds `llmq_type_chain_locks` and `pow_target_spacing`, both already in dash-params at the pin (`pkgs/params/src/types.rs:304`, `:330` @a1d4dd3b). `dash-mnlist` computes H_keep (SB11); LQ1 does not |
| Coinbase-CL predicate | `i:EV4` → `i:SB7` (EV4-core is SB7). LQ1's fallback is unchanged: if SB7 has not landed it when LQ1a-B starts, LQ1a-B lands it in `dash-evo::sml`, with SB7's owner reviewing |
| SML adapter (§2.2.4) | becomes production for SPV's `membersSig` input only, **owned by SB8**, not LQ1. LQ1a is unaffected |
| §6.4 | the inverted dash-spv fixture test is replaced by direct use plus mnxcheck (SB0 §12.1); LQ1b-F's dash-spv test PR moves to SB15; its fixtures stay LQ1b-F outputs, as shared vectors. LQ1a is unaffected |

**SB8's acceptance, by phase (m3).** SB8 (deps `SB7, LQ1a, BS12`) gates on: verifier-only tests over supplied rosters (the one `CFinalCommitment::Verify`, DEC-109 (4), with an injected `QuorumMemberSource`), and LQ1's non-rotated fixtures under both views (`MnList` and `SmlList`). The rotated fixtures under both views and the wrong-snapshot-row cases move to **SB11**, which already depends on LQ1b. No edge changes, so the CPM is unaffected.

**What SB0 needs that EV2/LQ1 did not have, in one list:** the views landed in EV0 (§1.3) with an orphan-rule-safe entry impl; store-independent errors and the fallible chain view with its contract (§2.2); fallibility on every ancestry read (§2.5); the not-held rule and its tests (§2.1); the attempt overlay (§2.6) and its model suite (LQ1m); `members_uncached` as a production function; the mnxcheck isolation and cached-membership comparison (§4.3); `SIGN_HEIGHT_OFFSET` and the H_keep role/spacing inputs; `impl MnListView for MnList` in EV0 (rev6), its contract tests in EV2a; the missing-connected-list test in both products (§2.4).

### 4.3 The mnxcheck comparison (M3, m2)

**Choice: compare with the node's cached membership.** mnxcheck (SB19) exists to show that SPV's roster is the one consensus uses. A fresh recomputation is not always that: once a roster is cached, Core returns it on a hit (`v24 llmq/utils.cpp:657`, `v23 :592`) even after its work block is pruned, while a fresh computation then reads `BlockDataUnavailable` and takes the hash-form modifier (`v24 evo/cbtx.cpp:193`, `llmq/utils.cpp:89`) and can choose different members (LQ1 §3.7.4 limits its uncached comparator to misses for this reason). So:

1. **Gate.** `members_gate` false or `MembersRoute::RotatedIndexOutOfRange`: Core returns `{}` before or without a cache hit; mnxcheck compares the empty roster.
2. **Hit.** A new read-only CH7b `MembersFacade::peek(t, base)` follows Core's lookup order: the base cache (`v24 :657`, `v23 :592`); then, for a rotated route, the indexed cache at `(cycle_base, index)` (`v24 :694-695`, `v23 :629`). It does not bump the LRU access counter, does not insert, and does not copy an indexed hit into the base cache (Core's `:697`). It must be its own const lookup primitive. It is not built from `get` or `exists`, both of which bump the counter (`v24 unordered_lru_cache.h:61-69`, `:72-76`; `v23 :59-67`, `:70-74`). It is not a normal facade call followed by an attempted rollback either. A hit is the roster the node uses, so mnxcheck compares it. It stays comparable even after the work block is pruned.
3. **Miss.** The facade would compute now, so mnxcheck computes the same thing without its side effects: `members_uncached` over CH7b's read-only `LlmqSource`, `ShadowRows` and `NoMemo` (or a private memo), with a discarding `EffectSink`. LQ1's twin claim holds on misses, so this equals what the facade would return.
4. **Not comparable.** If a miss's computation read `BlockDataUnavailable` (the source wrapper records it), the node's answer depends on its pruning, which Core defines and SPV cannot share (SPV never produces that value, §2.1). mnxcheck reports `NotComparable(PrunedWorkBlock)`, never a mismatch. Asserts and alarms are only on `Equal`/`Mismatch`.

**Read boundary (DEC-113).** One mnxcheck comparison, meaning the gate check, `peek`, and on a miss the `members_uncached` computation, runs whole as one V command under one `OnV` (§2.6 route B: it is not on the allow-list, because `peek` reads the members caches and a miss computes members). It therefore never runs inside an open attempt, and a miss can never combine members the attempt has already published with rows it has not yet applied. `ShadowRows`' backing reads happen under the same capability.

**Isolation boundary (m2).** Everything mnxcheck reads leaves production state as it was:
- `ShadowRows` reads its overlay first, then a backing row reader that reads `llmq_S` bytes directly and **does not touch the production snapshot cache** (Core's `GetSnapshotForBlock` inserts on a disk hit, `v24 llmq/snapshot.cpp:309-313`, `v23 :272-276`); its writes go to its overlay only.
- `NoMemo` or a private memo; never the runtime `SortedListMemo`.
- **Lazy BLS fields:** an `Arc<Dmn>` shares `Arc<MnCold>`, whose lazy operator key caches its decode. Every observer-only serialization or comparison works on a clone **of each lazy key wrapper**, as EV0 r2's `SameRecord` rule does (`key.clone().to_bytes_as(scheme_of(state.version))`), so mnxcheck never fills a production key's cache. Cloning the roster (`Arc<Dmn>`) or a `Dmn` is not enough, because a shallow `Dmn` clone still shares `Arc<MnCold>` (Sol r2's negative control). Cloning the `Arc` roster for a peek decodes nothing.
- **Observer digests** are computed from clones, so measuring cannot cause the mutation it checks for.

**Tests (SB19, with the model cases in LQ1a-B, LQ1m and LQ1b-E):**
- warm cache → block data unavailable: the node computes a non-rotated roster while the work block carries a non-null coinbase CL, then the block is pruned; mnxcheck's hit path compares the cached roster and reports `Equal` against SPV. Discrimination control: in the fixture, a fresh `members_uncached` gives a different roster;
- the same with a cold cache: `NotComparable(PrunedWorkBlock)`, not `Mismatch`;
- before and after each mnxcheck run: both members caches' full state including access order and counters, the snapshot cache's state and order, `llmq_S` row bytes and the runtime memo are byte-identical;
- with an undecodable operator key in the list: a later Core comparison (`core_eq`) and serialization of the production entries give the same results as without mnxcheck having run. Negative control: an observer that clones only the `Dmn` changes them;
- read boundary: §2.6 case 6's fixture, with mnxcheck as the external call. Started while the attempt is paused, it waits for release, then compares against (M1, S1). Run both the hit and the miss path.

---

## 5. PLAN row updates and CPM

| Row | Description change | Deps | Size | Tier | Acceptance change |
|---|---|---|---|---|---|
| **EV0** | Drop **HELD**. "`dash-evo` (shared) + `dash-dml` (node) interfaces, types first, reshaped per the EV addendum §3" | `CS0, i:BS6a` (**PLAN's row, unchanged**; see below) | S | impl O-m; rev Sol→O-h (EV0 r3 per §3, m1) | §3's commit acceptances; both crates in `node-pure`; `dash-evo`'s normal closure free of `imbl`/`dash-dml`; the shared §2.4 test; the cross-crate entry test; the two-call overlay test |
| **EV2a** | Drop **HELD**. Crate `dash-dml`; keeps EV0's `impl MnListView`/`ScratchMnList for MnList` compiling (they land in EV0 commit 1, rev6) | `EV0, i:O8, i:O2a-EV` (unchanged) | L | unchanged (D O-h done; impl O-h; rev Sol→O-h) | + LQ1 §2.2 view contract tests for `MnList` |
| **LQ1a** | Drop **HELD**. `dash-evo::llmq` with the addendum's signatures (§2.2, §2.5), the attempt overlay contract (§2.6), `members_uncached` (non-rotated, crate-internal), `SIGN_HEIGHT_OFFSET` | `EV2a, BS5, O2a-LQ1, i:SB7` (was `i:EV4` in LQ1 §9; LQ1 §9's other proposed edges unchanged by SB0) | M | unchanged | + not-held tests and §2.5's negative controls; node missing-list test; `members_uncached` ≡ model facade on cold captures, state untouched (the §2.6 model suite and §4.3's warm-cache model case moved to LQ1m, rev6) |
| **LQ1m** (new, rev6) | the attempt-overlay model suite, moved off LQ1a (Opus r7 O5): `reference::ModelAttempt` over LQ1a's `ModelMembersFacade` | `LQ1a` | S | LLMQ; impl O-m; rev Sol→O-h | §2.6 cases 1–6 on `ModelAttempt` (case 6: the 3 × 2 matrix, state-machine gate) with the negative controls of cases 1 and 6; §4.3's warm-cache case on the model |
| LQ1b | as SB0 §13.2 | + `i:LQ1m` (its §2.6 rerun uses the model, rev6) | L | unchanged | + not-held snapshot and previous-cycle controls; `UsedSet` in `dash-dml`; rotated `members_uncached` ≡ facade and made `pub`; §2.6 cases rerun with real rotation |
| LQ2 | blockprocessor in `dash-dml` (§1.5); consumes SB8's `Verify` (DEC-109 (4)) | + `SB8` | unchanged | unchanged | — |
| EV2b | `&DmlMemberSource`, `NodeMemberError` | unchanged | L | unchanged | — |
| EV4 (-serve) | + `MnList::project_sml` (DEC-109 (2)) | `EV2b, BS8, SB7` (SB0) | M | unchanged | + `project_sml` equals Core's `to_sml_entry` bytes on imported lists |
| CH7b | its `LlmqSource` and facade map `MemberError` per §2.3; the attempt handle and overlay, the gate, `release_attempt(EffectsApplied)`, the `VContext`/`OnV`/`OnValidation` capability with whole-route V dispatch for everything except the `RawRows` allow-list, and `begin_attempt` reachable only with `VContext` (§2.6); a zero-sized, stateless `VContext` (rev6, O3); Core's snapshot-cache critical sections (rev6, O1); the precompute loop (§2.5); read-only `peek`, `LlmqSource` and `llmq_S` row reader for mnxcheck (§4.3) | + `LQ1m` (rev6) | L | unchanged | + §2.4's node latch test; §2.6 cases 1–6 on the production handle (case 6 with real threads, each wait asserted at its own location, plus 6-D with the composite-builder case) and case 7 (snapshot-cache interleaving, O1); every attempt opened through a real V loop harness, with no test constructor (O3); the §2.6 compile-fail suite; `peek` leaves cache state and access order unchanged |
| CH7a | A1/CH7a erratum (rev6, Opus r7 O3): `ChainCommand::VerifyDb`, which the `verifychain` RPC also sends; startup load, VerifyDB and activation (A1 init step 6, §3.5.5 steps 5–6) run on V after its `VContext` exists (§2.6) | unchanged | unchanged | unchanged | + VerifyDB levels 3/4 open their attempts on V |
| SB8 | uses the non-rotated steps over `impl MnListView for SmlList`; lands the one `Verify` | unchanged | M | unchanged | verifier-only and non-rotated fixtures only (m3, §4.2) |
| SB11 | `MnViewError`; §2.3's SPV column (requests bounded by their target, rev6, O4); wires rotated `members_uncached` | unchanged | L | unchanged | + §2.4's SPV tests, including the rotated commitment with a deep input that is not held (O4); the rotated fixtures and wrong-snapshot-row cases moved from SB8 (m3) |
| SB19 | §4.3: one V command (`OnV`) per comparison; peek on a hit, `ShadowRows` + `NoMemo` + discarding sink on a miss, `NotComparable` | unchanged | S | unchanged | + §4.3's tests |
| EV1, EV3 | module homes per §1.5 | unchanged | — | — | — |

**EV0's edge (M4).** rev0 proposed `CS0, BS6a` (dropping the `i:`) and claimed no CPM change, but its sensitivity script kept `i:BS6a`; Sol r1 is right that the two disagree. rev1 keeps PLAN's `CS0, i:BS6a`, and the two meanings are kept apart:
- **Model edge** `i:BS6a`: EV0 was coded against BS6a's types while BS6a was in review (the historical overlap), so in the week-0 model EV0's merge waits only for BS6a + 0.5.
- **Real-world merge gate:** BS6a is merged (the cb504fab/a8755d86 re-pin), and §3 rebases EV0 onto that tip, so the gate is already satisfied. It cannot delay EV0 and is not modelled as a merge edge.

**CPM** (`python3 docs/node-program/tools/cpm_ev_addendum.py`; SB0's committed run A plus this addendum's edges; cpm.py's conventions; rev6 adds the LQ1m row and column and run P4):

| Run | EV0 | EV2a | LQ1a | LQ1m | SB7 | SB8 | SB11 | SB19 | CH7b | REL11 |
|---|---|---|---|---|---|---|---|---|---|---|
| **P**: plan of record (EV0 `CS0, i:BS6a`; LQ1a + `i:SB7`; LQ2 + `SB8`; LQ1m S `LQ1a`, CH7b + `LQ1m`, LQ1b + `i:LQ1m`) | 7.0 | 11.0 | 13.5 | 15.5 | 9.5 | 16.0 | 21.5 | 23.5 | 28.0 | **30.0** |
| P1: P with EV0 S → M | 7.0 | 11.0 | 13.5 | 15.5 | 9.5 | 16.0 | 21.5 | 23.5 | 28.0 | 30.0 |
| P2: P with EV0 S → L | 8.5 | 12.5 | 15.0 | 17.0 | 11.0 | 17.5 | 23.0 | 25.0 | 29.5 | 31.5 |
| P3: P with LQ1a M → L | 7.0 | 11.0 | 15.0 | 17.0 | 9.5 | 17.5 | 23.0 | 25.0 | 29.5 | 31.5 |
| P4: P with LQ1m S → M (rev6) | 7.0 | 11.0 | 13.5 | 16.0 | 9.5 | 16.0 | 21.5 | 23.5 | 28.0 | 30.0 |
| X: counterfactual, EV0 `CS0, BS6a` as a merge edge (rev0's row text) | 8.5 | 12.5 | 15.0 | 17.0 | 11.0 | 17.5 | 23.0 | 25.0 | 29.5 | 31.5 |
| X1: X with EV0 S → M | 9.0 | 13.0 | 15.5 | 17.5 | 11.5 | 18.0 | 23.5 | 25.5 | 30.0 | 32.0 |

- **REL11 holds at 30.0** on the interface edge. Limiting chain: REL11 (30.0) ← CH7b (28.0) ← LQ3 (24.0) ← LQ2 (21.5) ← LQ1b (17.5) ← LQ1a (13.5) ← EV2a (11.0) ← EV0 (7.0) ← BS6a (6.5) ← BS4a (4.0) ← BS3 (1.5).
- **Counterfactual X gives 31.5** (Sol's numbers), with the same chain but EV0 (8.5) bound by the BS6a merge. It is not adopted: the edge it models is satisfied in fact, and coding EV0 against BS6a's types is what the interface edge stands for. If the manager prefers merge edges throughout, X is the number for PLAN.
- **Slack.** With `i:BS6a`, EV0 absorbs +0.5 week (S → M free; S → L costs +1.5). rev1 adds commit 5 (a test stub) and slightly wider commit 1/3/4 acceptances, which stay within S. EV2a and LQ1a have zero slack. rev1 added §2.5's signature changes, §2.6's model and cases, and the model side of §4.3 to LQ1a. **rev6 (Opus r7 O5) moves the model-only part, `ModelAttempt`, §2.6's cases 1–6 and §4.3's model case, to the new S row LQ1m.** LQ1a keeps the signatures, the fallibility, the not-held tests, `members_uncached` and `SIGN_HEIGHT_OFFSET`, which LQ1b and SB8 consume, and stays M; P3 still shows the cost if it outgrows that (LQ1a M → L: 31.5). LQ1m finishes at 15.5. CH7b starts at 24.0 (28.0 − L − review), so LQ1m has 8.5 weeks of float, and its `i:` edge into LQ1b (merge ≥ 16.0 against LQ1b's 17.5) does not bind. LQ1m S → M (P4) still gives 30.0.
- **Serialization (rev2, DEC-113) costs no CPM time.** It adds no edge, and the script's output is unchanged from rev1. Its scope was already sized:
  - CH7b: one `RwLock`, a token threaded through the external entry points, and case 6 with threads. LQ1 §3.7.2's approved text already required serialization with validation, so this is inside CH7b's L, not new scope.
  - LQ1m (rev6; LQ1a in rev2–rev5): one more model case with a state-machine gate, inside S.
  - CH7b sits on the zero-slack chain (LQ1m does not, rev6). If CH7b outgrows L, REL11 moves one for one; the model has no size above L. LQ1a M → L costs 31.5 (P3).
  - Runtime cost (an external call waits until it acquires the gate after some attempt's log application; with a writer-preferring lock that can span more than one attempt, per Sol R3-m1) is a latency question for PF/bench work, not a schedule one.
- `LQ1a + i:SB7` and `LQ2 + SB8` move nothing, and the m3 phasing adds no edge. The CH7b additions (`peek`, the attempt handle) are within its L; CH7b sits on the critical chain, so any growth there moves REL11 one for one.
- The model has no progress state (merged rows are not finished at week 0). A remaining-work model was not built: it needs every row's state, which is STATUS's, and X already bounds the merge-edge reading.

---

## 6. Open questions

rev0's five questions were all decided by **DEC-109** (BLS wrapper homes; `project_sml` in EV4-serve; infallible `MnListView`; one `Verify` in SB8 with LQ2 taking `SB8`; `LlmqConfig`). SB0's rev4 erratum 53091a32 aligns SB0's text. EV0 m2 is resolved by DEC-111 and DEC-113 (§2.6).

Still open and not this addendum's: SPV's `DeploymentView` for versionbits-gated types (`TESTDUMMY` for `llmq_test_v17`, regtest only) is SB11's.

No new questions.

---

## 7. Departures from SB0's text

| SB0 | Here | Why |
|---|---|---|
| §5.1: `MnListView` gets an associated `Error` | `MnListView` stays infallible; `LlmqChain` gets one (§2.2) | DEC-109 (3); the list values are materialized; SB0 §4.1.1 itself asks for the chain view with an error |
| §5.1: `MemberError<E>` with `Missing` and `View` only | adds `UnknownLlmqType(u8)` (EV0's existing variant) | Core's throw must stay distinct from both (EV2 §2.11) |
| §12.1 EV0: `llmq` *traits* shared; `store` node-only | `MinedCommitmentReader` moves from `store` to shared `llmq` | LQ1's scan (shared `dash-evo::llmq`) names it; its error becomes associated |
| §12.1/§13.2 EV2a: `project_sml` | EV4-serve (DEC-109 (2)) | keeps SB7 off EV2a's zero slack |
| — | the not-held rule (§2.1), extended to the chain view | implied by SB0 §5.1's "`Missing` becomes `Unavailable`": without it, an SPV view could return a Core absence value and change members silently |
| §5.4: mnxcheck compares with the node's members | compares with the node's *cached* members on a hit (§4.3) | a fresh recompute after pruning is not the roster consensus uses |

---

## 8. Change maps

### 8.1 rev6 changes (Opus high r7 → rev6, final; inside DEC-111, DEC-113, DEC-117 and DEC-119)

| Finding | Ruling | Change | Where |
|---|---|---|---|
| **O1** (major): the caller-thread `snapshot_for_block` raced V's inline row writes, because both hold only the shared gate and rev5 did not put the disk read, or V's write plus insert, inside one snapshot-cache section | as Opus suggests | Core's `snapshotCacheCs` sections are normative (`v24 snapshot.cpp:301-329`, `v23 :264-292`, checked at both pins). `snapshot_for_block` holds the snapshot cache's lock across the lookup, the disk read and the insert. Every row write holds it across the raw write and the insert: route B's inline writes, route D's log application and `AttemptRows`' backing writes. No implementation may unlock around the disk read. The lock order names one lock, Core's, and adds no cycle. New CH7b case 7 forces the stale-row interleaving; its negative control, a getter that unlocks around the read, ends with S0 cached | §2.6 (Core paragraph, apply bullet, non-attempt callers, lock order, allow-list entry, new critical-sections bullet, case 7), §5 (CH7b) |
| **O2**: EV0 commits 1–2 did not compile. The test-crate impls for `Arc<Dmn>`/`MnList` become E0117 once the traits are `dash-evo`'s, and `Dmn: MnEntryView` landed only in commit 3 | Opus's option (a), the smaller diff | Commit 1 lands `impl MnEntryView for Dmn` and `impl MnListView`/`ScratchMnList for MnList` in `dash_evo::dml` and deletes `lq1a.rs`'s two impls. Commit 3's `git mv` carries them into `dash-dml`. The E0117 constraint is stated. EV2a-1 keeps only the `MnList` contract tests | §0 item 6, §1.2, §3 (commits 1 and 3, order note), §4.1, §4.2, §5 (EV2a) |
| **O3**: `VContext`'s `/* … */` invited state, which E0499 would lock out during an attempt, and the init-time attempt openers had no route onto V | as Opus suggests | `VContext` is zero-sized and stateless; V's state sits beside it. Every attempt opener runs on V after the context exists. **A1/CH7a erratum:** `ChainCommand::VerifyDb`, which `verifychain` also sends; A1 init step 6 and §3.5.5 steps 5–6 run on V. Tests open attempts through a real V loop, never a `cfg(test)` or feature-gated constructor | §2.6 (code block, new opener bullet), §5 (CH7b, new CH7a row) |
| **O4**: §2.3 applied H_keep to `at`, but rotated inputs reach cycleBase − 3C − 8 (872 blocks on mainnet), so SPV would never request them | as Opus suggests | `at` is bounded by the anchor only. The H_keep/domain bound applies to the request's target, a `getmnlistd` target or the qrinfo of the commitment's cycle (SB0 §5.2.1 rule 2), and deduplication is keyed by target. One SB11 test covers a deep rotated input that is not held | §2.3, §2.4, §5 (SB11) |
| **O5**: the model-only suite sat on LQ1a's zero-slack edge | manager's ruling: a separate row | New row **LQ1m** (S, LLMQ, deps `LQ1a`). It carries `reference::ModelAttempt`, §2.6 cases 1–6 and §4.3's model case. CH7b takes `LQ1m`, and LQ1b takes `i:LQ1m` for its rerun. The CPM script adds the row and run P4. P gives LQ1m 15.5 and REL11 **30.0** (unchanged, same limiting chain), with 8.5 weeks of float to CH7b's start at 24.0; P4 (LQ1m M) is still 30.0 | §0 items 7–8, §2.6 (conformance header, model references), §4.2, §4.3, §5 (LQ1a, LQ1m, LQ1b, CH7b, CPM table and bullets); `tools/cpm_ev_addendum.py` |

Opus's non-gating implementation notes (an object-safe `OnValidation` behind `dyn`, the four-argument facade `members` in the precompute loop, a writer-preferring lock with a documented policy) are not applied; the reviewer marked them not gating, and the ruling was "change nothing else".

### 8.1a rev5 changes (Sol r5 → rev5, inside DEC-117)

| Finding | Ruling | Change | Where |
|---|---|---|---|
| **R5-M1**: `begin_attempt` was public, so caller-thread code could mint an `AttemptHandle`, which implements `OnValidation`, and skip V dispatch | fix inside DEC-117's capability scheme | Both minting paths now need `VContext`, V's affinity proof. The command loop creates it once at thread start; its constructor is private to the loop module, and it is `!Send`/`!Sync`. `OnV` comes from `VContext::on_v`, and `begin_attempt` takes `&mut VContext`. We did not use `&OnV` as the prerequisite: Sol r5 warns that taking the exclusive gate while holding the shared one self-deadlocks, and the `&mut` borrow makes that combination a compile error. `AttemptHandle` is `!Send`/`!Sync`, because no attempt work crosses threads; its overlay becomes a `RefCell`. `QuorumMemberSource` drops its `Send + Sync` supertrait, since EV0 Q10's cross-thread reason is superseded and a capability-carrying source cannot be `Send`. Compile-fail case 6 is added, and case 4 now covers `AttemptHandle` | §2.2, §2.6 (code block, `AttemptHandle` bullet, capability enforcement), §3 commit 2, §5 (CH7b) |
| **R5-m1**: `snapshot_for_block` is cache-through, which contradicted "no getter may reach a cache" | Sol's fix | The snapshot-cache read-through and recency bump is the one permitted cache operation (both pins cited). Members, quorum-object, scan and memo caches, lazy production keys, computation, persistent writes and V callbacks stay prohibited. The list is no longer described as cache-free | §2.6 allow-list |
| Sol r5 check notes | as written | The production `RawRows` implementation needs a transitive audit. The V command is a synchronous scoped `for<'v> FnOnce(&OnV<'v>)` with no local executor, and `!Send` does not imply no-await (A1 §3.1 does). Adapters are capability-parameterized and command-scoped; no factory returns a transferable ungated object. The command queue is FIFO and fair, and no command waits on V. PF measures V queue wait and service time under cold composite load | §2.6 |

DEC-111, DEC-113 and DEC-117 are unchanged; the CPM is unchanged (REL11 30.0).

### 8.1b rev4 changes (Sol r4 → rev4, per DEC-117)

| Finding | Ruling | Change | Where |
|---|---|---|---|
| **R4-M1**: the qrinfo/rotationinfo builders and `mnlistdiff` serving reach rotated materialization (`BuildSimplifiedMNListDiff` → `BuildQuorumChainlockInfo` → `GetQuorum` → members → `StoreSnapshotForBlock`), so rev3's route 4 put a writer on the caller's thread. It is the second route the deny-list missed | DEC-117: allow-list plus a capability type | The deny-list table is replaced by a closed allow-list: `RawRows` = `snapshot_for_block` plus the `MinedCommitmentReader` getters, the only things reachable from `external()` on the caller's thread. Every other external route dispatches **whole** to V before any gate or cache lock: members, scans, `GetQuorum`, materializers, the predictor, qrinfo, rotationinfo, `mnlistdiff` serving, mnxcheck, and any later route. Nested calls reuse the command's `OnV` inline, with no self-dispatch. Rev3's static `use_rotation` split is gone | §2.6 (code block, non-attempt callers, where the wait sits, lock order), §4.3, §5 (CH7b, SB19) |
| DEC-117 enforcement | — | `OnV` (V's command loop only; `!Send`/`!Sync`) and `AttemptHandle` implement a sealed `OnValidation`. Every materialization, members, snapshot-write and composite entry point needs one; `ExternalToken` reaches only `RawRows`. CH7b acceptance adds a five-item compile-fail suite | §2.6, §5 (CH7b) |
| R4-M1 acceptance | Sol's fix | 6-D adds the post-V20 cold-rotated composite case: qrinfo, rotationinfo and direct `mnlistdiff` dispatched with an attempt queued; nested `GetQuorum` and members run inline; no self-dispatch; a rev3-route-4 negative control (test hook only) that hangs. 6-D(c) now uses an allow-listed getter | §2.6 case 6-D |
| **R4-m1**: case 6 required rotated callers to be parked on a gate they no longer take | Sol's fix | The wait is asserted where each route waits: a dispatched call shows a queued command and no lookup, read or reply until release; an allow-listed caller-thread observer keeps the gate-waiter assertion. All six warm-hit combinations, the delayed indexed-hit base insert and LQ1a's `Blocked` model are kept. The negative control is scoped: it bypasses both dispatch and the gate (model: direct-call facade; CH7b: a test-only caller-thread hook), and removing only the gate is stated not to be a control | §2.6 case 6 |

DEC-109, DEC-111 and DEC-113 are unchanged; the CPM is unchanged (REL11 30.0).

### 8.2 rev3 changes (Sol r3 → rev3)

| Finding | Ruling | Change | Where |
|---|---|---|---|
| **R3-M1**: an outside route holding the shared gate while it waits for V to write its snapshot row deadlocks against V waiting for the exclusive gate in `begin_attempt` | adopt Sol's fix as written | A route that can write a row (rotated-type members and QM1 scans/`GetQuorum`, classified by the static `use_rotation`) is dispatched to V as a `ChainCommand` **before** it takes any token. V takes the token inside the command, between attempts, and writes inline. Read-only routes (predictor, direct snapshot reads, mnxcheck, non-rotated members and scans) take the token on the caller's thread. A lock-order rule in §2.6 lists every route: V dispatch → gate → cache mutex → snapshot cache/V's writer, and never a wait on V while holding the gate or the cache mutex. Attempt-internal calls never dispatch. The liveness bullet no longer says "local reads only" | §2.6 (non-attempt callers, entry points 1–2, lock order, liveness) |
| R3-M1 acceptance | as written | Case 6-D (CH7b, real threads): a cold rotated external members call or scan, paused before its store, plus an attempt. Three pause positions × {success, failure, proposal} × {members, scan}. Both must finish with the row/cache ordering intact. Validation-time nested scans finish through the handle without dispatching. A queue-after-token negative control must hang, detected by a bounded wait | §2.6 case 6 |
| R3-m1 (minor): writer preference does not give a one-attempt bound | — | In §2.6's liveness bullet, which R3-M1 also cites, the wait now lasts until the reader acquires the gate after an attempt's log application, with latency and fairness left to PF. The same claim in §5's CPM bullet is unchanged in rev3 | §2.6 |

DEC-109, DEC-111 and DEC-113 are unchanged; the CPM is unchanged (REL11 30.0).

### 8.3 rev2 (Sol r2 → rev2, per DEC-113)

| Finding | Ruling | Change | Where |
|---|---|---|---|
| **R2-M1**: an external reader could see the attempt's published roster (M1) together with its unapplied old row (S0), a pair Core cannot expose (`v24 utils.cpp:462` → `snapshot.cpp:326-328` writes the row before `utils.cpp:710-713`/`:727-728` publish) | DEC-113: serialize, restoring LQ1 §3.7.2; no shared pending-write view | The facade gets a writer-preferring `RwLock` gate. An attempt holds it exclusively from `begin_attempt` to `release_attempt(EffectsApplied)`, after CH7a has applied its log on every outcome. Every external route takes it shared at its entry, before any cache, ancestry or row read: members, materializer/scans, predictor, direct snapshot reads, mnxcheck. The token is passed down, never re-acquired. A log-application failure or an unwound handle trips the latch before release. The DEC-111 overlay, ordered log and cache semantics are unchanged | §0, §2.6 |
| R2-M1 acceptance | DEC-113 | Case 6: pause after the rotated cache insertion and before the log is applied; an external cache hit followed by a snapshot read must wait, then see (M1, S1). Matrix of {success, failure, GBT proposal} × {base-cache hit, indexed-cache hit}, with a gate-less negative control. LQ1a uses a state-machine gate (`Blocked`); CH7b uses real threads, parked on the gate. mnxcheck reruns the fixture on its hit and miss paths | §2.6, §4.2, §4.3, §5 |
| mnxcheck read boundary | DEC-113 | One `ExternalToken` per comparison (gate check, `peek`, miss computation) | §4.3, §5 (SB19) |
| CPM | — | Serialization adds no edge, and the script's output is unchanged: REL11 **30.0**. CH7b's L already covered LQ1 §3.7.2's serialization; LQ1a's one model case fits in M | §5 |
| Sol r2's check notes (no minors raised) | as written | `peek` is its own const lookup, not built from `get`/`exists` (both bump the counter) and not a facade call plus rollback. A hit stays comparable after pruning. Observers clone each lazy key wrapper, because a shallow `Dmn` clone shares `Arc<MnCold>`; a negative-control test is added | §4.3 |

### 8.4 rev1 (Sol r1 → rev1, per DEC-111)

| Finding | Ruling | Change | Where |
|---|---|---|---|
| **M1** orphan rule: `impl MnEntryView for Arc<Dmn>` in `dash-dml` is E0117 | fix as Sol proposes | shared forwarding `impl<T: MnEntryView> MnEntryView for Arc<T>` (EV0 commit 1) + `impl MnEntryView for Dmn` in `dash-dml` (commit 3); `Entry = Arc<Dmn>` kept; a cross-crate consumer test; the other impls listed as legal | §1.3, §2.2, §3 commits 1/3, §4.1 |
| **M2** ancestry-reading predicates and iterators had no error channel | fix as Sol proposes | every ancestry-reading LQ1 function enumerated and made fallible; `precompute_targets` replaced by Core's loop; `QuorumPrecompute`/`QuorumMaterializer` get `type Error`; the chain view's `Ok(None)` contract; known before-genesis kept as Core's value; three negative controls | §2.1, §2.2, §2.5, §3 commit 4, §4.2 |
| **M3** fresh recompute is not always the node's cached membership | compare on the node's cached-membership semantics | chosen: CH7b `peek` (no bump, no insert) on a hit; on a miss, `members_uncached` with `ShadowRows` + `NoMemo` + discarding sink; `NotComparable` when a miss read `BlockDataUnavailable`. Why: mnxcheck must check what consensus uses | §4.3, §4.2, §5 (CH7b, SB19), §7 |
| **M4** the CPM kept `i:BS6a` while the row dropped it | recompute with the true edges; hold 30.0 with interface edges if possible | EV0 keeps PLAN's `CS0, i:BS6a`, the model edge and the real-world gate (BS6a merged) labelled; pinned script committed; REL11 **30.0**; merge-edge counterfactual 31.5 recorded with its chain; S → M absorption stated only under `i:BS6a`; LQ1a stress added | §5, `tools/cpm_ev_addendum.py` |
| **M5** EV0 m2 left open while releasing LQ1a | DEC-111: one attempt-tagged overlay owned by CH7b's attempt handle | §2.6 contract (ownership, `store`/`get`, ordered effects applied on every outcome, runtime write-through, D4); five conformance cases incl. Sol's two-call case on `ModelAttempt`; EV0 commit 5 replaces the per-call stub; LQ1a start gate dropped | §2.6, §3 commits 5/7, §4.2, §5 |
| m1 review scope | as written | EV0 r3 covers the post-rebase code delta with move detection **and** the final `API.md`/`ARCHITECTURE.md` against this addendum, incl. m2; r2's approval reused for unchanged bodies | §3 |
| m2 shadow-read isolation | as written | overlay-first reads; backing row reader that does not touch the snapshot cache; `NoMemo`/private memo; lazy BLS fields cloned for observer serialization/comparison; later Core comparison/serialization tested; digests from clones | §4.3 |
| m3 SB8's rotated acceptance phase | as written | SB8 gates on verifier-only + non-rotated fixtures; rotated fixtures and wrong-row cases move to SB11 (has LQ1b); `members_uncached` `pub(crate)` non-rotated in LQ1a, `pub` with rotation in LQ1b-E; no stub rotated route accepted; no CPM change | §4.2, §5 |
| rev0 §6 | DEC-109 | marked decided; §1.2/§1.5/§4.1/§7 cite DEC-109 | §6 |
