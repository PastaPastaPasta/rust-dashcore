# dash-evo API (EV0): requirement map for the API review

EV0 lands `dash-evo`'s interfaces, types first (PLAN EV0). This file maps each requirement of the
binding designs to the item that serves it, and lists the deviations and the decisions taken.

**Status:** API review round 1 (`node-program/reviews/EV0-api-r1.md`, APPROVE-WITH-CHANGES, no
blockers) is applied: M1, M2, M3 and the review's §4 recommendations on all fourteen open
questions (§7). §8 lists what changed and the minors left for EV2a. The round-2 closure check
(`EV0-api-r2.md`, APPROVE-WITH-NITS) is applied too: r2-m1 and r2-m2 (§5 item 9) and nits
r2-n1 to r2-n3.

**SB0 split:** the EV addendum (`node-program/designs/EV-addendum-SB0.md` rev6, DEC-122) reshapes
EV0 for SB0 (rev6.1, DEC-119): the shared `dash-evo` (this crate) and the node-only `dash-dml`.
§0 maps every EV0 item to its home and names what the addendum changed; §7 records its
decisions. §1-§6 and §8 are EV0's map as reviewed, with EV0's paths (`dml::X` is now `dash_dml::X`,
`store::X` is `dash_dml::store::X` unless §0 says otherwise). `dash-dml/API.md` lists the
node-only items and their tests.

Inputs: A1 §2.5, §2.10.1, §2.10.2 and the O2a errata; `EV2-dml.md` rev4 (DEC-58) with its errata;
`LQ1-quorum-members.md` rev4 (DEC-60); CS0 (merged, `76ce0052`); ST3a §5.6; DEC-50, DEC-71; the EV
addendum rev6 (DEC-122, Opus r7 O1-O5) and SB0 rev6.1 (DEC-119).

The node's compile-only consumers are in `dash-dml/tests/consumers/` (`ev1.rs`, `ev2a.rs`,
`ev2b.rs`, `lq1a.rs`, `ch7a.rs`), and SPV's is `tests/consumers/spv.rs` here; each has a small
runnable check. `dash-dml/tests/store.rs` tests the in-memory store.

## 0. The SB0 split (EV addendum §1.2, §1.3, §2.2)

| EV0 item (`dash_evo::…`) | Home | Change |
|---|---|---|
| `ctx::EvoCtx`, `effect::{Effect, EffectKind, EffectSink}`, `bls::*` | `dash_evo::{ctx, effect, bls}` | none |
| `dml::{ProTxHash, MnType, RawMnType}` | `dash_evo::types` | moved; `dash_dml` re-exports them |
| the rest of `dml::*` | `dash_dml::*` (flattened) | moved |
| `store::{StoreError, DmlReader, EvoStore, EvoDelta, MinedCommitmentRecord, MemEvoStore, MemEvoView}` | `dash_dml::store` | moved; `EvoStore: DmlReader + MinedCommitmentReader<Error = StoreError>` |
| `store::{MinedCommitmentReader, MinedFamily, MinedIndexEntry, MinedCommitment, base_through}` | `dash_evo::llmq` | moved; the reader gains `type Error: From<BrokenInvariant>`, and `base_through` is generic over it |
| `llmq::{QuorumMemberSource, MemberError, QuorumMembers}` | `dash_evo::llmq` | generic: `type Entry: MnEntryView`, `type Error`, no `Send + Sync`; `MemberError<E>` is `Missing(BlockId)`, `View(E)`, `UnknownLlmqType(u8)`; `QuorumMembers<E> = Arc<[E]>`. EV0's two-variant enum is `dash_dml::store::NodeMemberError` |
| `llmq::{QuorumSnapshot, SnapshotSkipMode, LlmqCommitment}` | `dash_evo::llmq` | none |
| `creditpool`, `mnhf` | `dash_dml::{creditpool, mnhf}` | moved |
| `testkit::MemChain`, `testkit::pro_tx` | `dash_evo::testkit` | none |
| `testkit::{cold_state, dmn}`, `MnList::insert_for_tests` | `dash_dml::testkit` | moved |
| `tests/store.rs`, `tests/consumers/{ev1,ev2a,ev2b,ch7a,lq1a}.rs` | `dash-dml/tests/` | moved; `lq1a.rs` keeps only LQ1's `UsedSet` and snapshot rows, over an attempt-owned overlay (§2.6) |
| `imbl = "=7.0.2"` | `dash-dml` | `dash-evo` no longer depends on it; `check-pure.sh` checks that |

New in `dash-evo` (§1.3, §2.2):

| Item | Serves |
|---|---|
| `llmq::{MnEntryView, MnListView, ScratchMnList, ListCounts, AddMnRejected}` | LQ1 §2.2.1-§2.2.2, text unchanged; infallible (DEC-109 (3)). `dash_dml` implements them for `Dmn` and `MnList` |
| `impl<T: MnEntryView> MnEntryView for Arc<T>` | `Entry = Arc<T>` for a crate that owns `T` (E0117) |
| `llmq::{LlmqChain, CompleteChain, BrokenInvariant}` | the chain contract (§2.2): `Ok(None)` only above the block or at genesis; the node's complete ancestry never errs |
| `tests/consumers/spv.rs` | SPV's mapping of the error contract (§2.3), compile-only with four checks |

## 1. What EV0 provides, and what it leaves to the owners

| Area | EV0 (this crate today) | Owner adds |
|---|---|---|
| DML types | `ProTxHash`, `MnType`, `RawMnType` (`stored_type`, `is_evo`, `weight`), `MnHot` (Core's ban/revive methods), `MnCold` (its derived-value cache is crate-private; `core_eq`), `Dmn` (raw `mn_type`; `is_evo`, `weight`), `PayeeKey`, `Counts` (Core's six fields), `Provenance`, `UniqueKey` (collateral, key id, platformNodeID), `UniqueSlot`, `UniqueMap`, `SameRecord` | EV2a: `UniqueKey::{service, domain, operator_key}`, `MnCold::{from_pro_reg, reset_operator_fields, update_confirmed_hash}`, BS6a's `payouts`/`shares` fields |
| `MnList` | the value type over `imbl` maps; `default_pre_dip3`, `scratch`, `new`; the lookups (`get`, `get_valid`, `is_valid`, `is_pose_banned`, `get_by_internal_id`, `has_unique`, `unique_owner`, `get_by_collateral`, `get_valid_by_collateral`, `iter`, `iter_valid`, `counts`, `len`, `provenance`, `collision_buckets`); `SameRecord` | EV2a: `try_add`/`update`/`update_hot`/`remove`, the indexes and choke point, `core_order`, `get_by_service`, `get_by_operator_key`, `has_operator_key_under_any_scheme`, `diff_to`/`apply_diff`/`apply_record`, `is_equal`, `check_invariants`, codecs, `resolve_list`, `ListCache`. EV2b: `payee`, `projected_payees`, PoSe |
| Diffs and records | `MnListDiff`, `MnStateDiff` (mask + source state), `MnListDiffRecord` (counter and bucket trailer), `DmlDelta` | EV2a: `MnStateDiff::{between, apply}`, the canonical codecs, `DmlRecordBytes` |
| Store | `StoreError`, `DmlReader`, `MinedCommitmentReader` (index rows `mined_index` with `MinedFamily`/`MinedIndexEntry`, ancestry-resolved reads provided over it, `base_through`), `EvoStore`, `EvoDelta`, `MinedCommitmentRecord`, `MemEvoStore` + `MemEvoView` | CH7a: the chainstate implementation (`EvoReadView`), B1 persistence |
| Effects | `Effect` (A1 inventory + errata; `NotifyMnListChanged { at, undo }`), `EffectKind`, `EffectSink` (+ `impl for Vec<Effect>`) | CH7a/CH7b: routing, the durability classification (an exhaustive `match`), `AttemptRows` |
| BLS | `BlsCtx`/`BlsSchemeKind`/`BlsSchemeSource` re-exports, `bls_ctx_for_block`, `bls_ctx_after`, `EvoCtx::bls` | CH7a: the tip context (`bls_ctx_after(tip)` after each commit); BS6a/BS6c: scheme-free decode |
| LLMQ | `QuorumMemberSource` (generic since the split, §0), `MemberError`, `QuorumMembers`, `QuorumSnapshot`, `SnapshotSkipMode`, `LlmqCommitment` | LQ1a/b: everything in LQ1 §2.1; LQ2: commitments and the block processor |
| EV3 records | `CreditPool`, `RangesSet` (representation only), `MnhfSignals` | EV3: the operations, limit eras, resolvers |
| Test kit | feature `test-utils`: `testkit::MemChain` (a forking block tree implementing `ChainAncestry`), `testkit::{pro_tx, cold_state, dmn}`, `MnList::insert_for_tests` (not Core's `AddMN`; deleted when `try_add` lands, Q14) | EV2a: its list builders on `try_add`, the synthetic block generator |

## 2. EV2 (`EV2-dml.md` rev4) → items

| EV2 requirement | Served by |
|---|---|
| §2.1 pure, no `std` `HashMap`, `imbl = "=7.0.2"` exact, `MnList::height: i32`, Core's int types | `Cargo.toml`, `clippy.toml`, `dml::MnCold` (`i32` heights, `version: i32`), `MnList::height() -> i32` |
| §2.2 `ProTxHash: Ord` = memcmp of internal bytes | `dml::ProTxHash` (newtype over `Hash256`; unit test pins order vs display hex) |
| §2.2 `UniqueKey` = serialization pre-image; `UniqueSlot { owner, refs }`; `UniqueMap` keyed SipHash, no iteration | `dml::{UniqueKey, UniqueSlot, UniqueMap}`; quirk-23 collision of key id and platformNodeID tested |
| §2.3 hot/cold `Dmn`; private `pose_ban_height` with Core's methods; SML-hash slots per validity (private `derived`); equality short-circuit on `Arc::ptr_eq(cold)`; `RawMnType` for `nType` | `dml::{Dmn, MnHot, MnCold, RawMnType, MnType}`; `MnCold::derived` is crate-private, so no state built outside the crate can carry another state's cache; `SameRecord for Arc<_>` short-circuits on `Arc::ptr_eq`; `Dmn::mn_type` is raw (Q13) |
| §2.4 `MnList` fields, `PayeeKey`, `Counts` (Core's `GetCounts`: undefined types count as regular with weight 0), `Provenance { FromGenesis, Decoded }` and the no-panic rule | `dml::{MnList, PayeeKey, Counts, Provenance}`; `MnList::new` is `Decoded`, `default_pre_dip3`/`scratch` are `FromGenesis` |
| §2.5 `default_pre_dip3`, `scratch`, `new` | `MnList::{default_pre_dip3, scratch, new}` (the mutations are EV2a's) |
| §2.6 read API | the lookups listed in §1; the rest is EV2a's |
| §2.7 collision buckets carried by the list and the records | `MnList::collision_buckets()`, `MnListDiffRecord::collision_buckets_after` |
| §2.8 `MnListDiff`, `MnStateDiff`, `MnListDiffRecord { height, total_registered_after, diff, collision_buckets_after }` | `dml::{MnListDiff, MnStateDiff, MnListDiffRecord}` |
| §2.10 `DmlReader: Sync` returning `Result<Option<Arc<_>>, StoreError>`; `EvoStore: DmlReader`; `DmlDelta { block, height, record, snapshot }`; `MemEvoStore`; nothing erased; a differing rewrite is `StoreError::Inconsistent` | `store::{DmlReader, EvoStore}`, `dml::DmlDelta` (inside `EvoDelta::dml`), `MemEvoStore::{apply, disconnect}`, rewrites compared with `SameRecord`; `tests/store.rs::{rewrites_must_equal_the_stored_row_and_fail_atomically, a_rewrite_check_never_changes_the_stored_serialization}` |
| §2.11 `QuorumMemberSource` (LQ1 §2.6 text), `MemberError::{Store, UnknownLlmqType}`, `EffectSink` parameter | `llmq::{QuorumMemberSource, MemberError}`; since the split, `MemberError<E>` with the node's `dash_dml::store::NodeMemberError::{Store, UnknownLlmqType}` (§0); `dash-dml/tests/consumers/ev2b.rs` maps `Store` → node error, `UnknownLlmqType` → exception |
| §2.12 EV0 row: `EvoStore`, `EvoDelta`, `Effect`/`EffectSink`, the member source, type skeletons | all of the above |
| §3.8 `Effect::ResetPlatformBan { pro_tx_hash, height }` (erratum), emitted before `bad-dip3-enf-block` | `effect::Effect::ResetPlatformBan`; `tests/consumers/ev2b.rs::the_reset_survives_the_enforcement_failure` |
| §3.11 item 1, LQ1 D4: store faults are node errors | `StoreError` docs; `NodeMemberError::Store` (since the split; `MemberError::Missing` maps there too) |
| §4 `mn_diff` for every DIP3 block, `mn_snapshot` every 576 and the first DIP3 block, in B1 | `EvoDelta::dml`, `DmlDelta::snapshot`; cadence shown in `tests/consumers/ev2a.rs::dml_delta` |

## 3. LQ1 (`LQ1-quorum-members.md` rev4) → items

| LQ1 requirement | Served by |
|---|---|
| §2.2.1 `MnEntryView` fields: proTxHash, collateral, `is_evo`, `is_banned` (own state), `confirmed_hash_is_null`, stored `confirmed_hash_with_protx` | `Dmn::{pro_tx_hash, collateral, is_evo}`, `MnHot::is_banned`, `MnCold::{confirmed_hash, confirmed_hash_with_pro_tx}`; since the split, the trait is `llmq::MnEntryView` and `dash-dml/src/views.rs` implements it (§0) |
| §2.2.2 `MnListView`: counts (total, enabled), contains, `is_pose_banned` (false when absent), `for_each(only_valid)`, scratch list | `MnList::{counts, get, is_pose_banned, iter, iter_valid, scratch}`; since the split, the trait is `llmq::MnListView` and `dash-dml/src/views.rs` implements it (§0) |
| §2.2.4 lean `UsedSet` from `ColdDerived::unique_keys` | `MnCold`'s crate-private cache holds the slot (EV2a fills it and exposes the keys); the stub probes what EV0 can build |
| §2.3 `LlmqSource::mn_list(at)` | EV2a's `resolve_list(at, chain, …)` over `DmlReader` (CH7b binds it); the record walk is in `tests/consumers/ev2a.rs` |
| §2.3 `LlmqSource::coinbase_facts(at)` | SB7's `cbtx::CoinbaseFacts` (CH7b binds it over the block store; LQ1 applies `cbtx::nonnull_coinbase_chainlock`); a block the view does not hold is `None`, never `BlockDataUnavailable` (EV addendum §2.1) |
| §2.3 `SnapshotRows::{get, store}`: get `Ok(None)` is normal, store is an unconditional overwrite visible to the next read | `EvoStore::quorum_snapshot`, `MemEvoStore::store_quorum_snapshot`, `Effect::StoreQuorumSnapshot`; the attempt overlay is in `tests/consumers/lq1a.rs::AttemptRows` |
| §2.4 `QuorumSnapshot` | `llmq::QuorumSnapshot` (until BS8; Q4) |
| §2.5 `MinedCommitmentIndex`: `mined_until`, `nth_mined_by_index`, `newest_by_index`, `mined_commitment`, every read through `at`'s ancestry | **retired**: `store::MinedCommitmentReader` is the single commitment trait (Q1), with the same read names (`mined_commitment` takes `at`) provided over `mined_index`; `tests/store.rs` covers forks, both families and the height-0 bound |
| §3.8.2 "every call site passes an active-chain block" | not so for qrinfo's request block (M2): `mined_index(type, Rotated { i }, tip, h(S), 1)` + `base_through(S, …)` + `mined_commitment(…, tip)` is Core's read; `tests/store.rs::qrinfo_reads_the_active_index_for_a_stale_request_block` |
| §2.6 the member-source trait, owned by EV0, `MemberError` semantics | `llmq::{QuorumMemberSource, MemberError, QuorumMembers}` (text verbatim in EV0; generic since the split, §0) |
| §2.5 `QuorumPrecompute::precompute(…, &mut dyn EffectSink)` | `EffectSink`, `MemberError` (the trait is LQ1b's) |
| §3.7.2, §4.2 rows written as effects inside attempts, applied in order regardless of the result | `effect` module docs; `tests/consumers/ch7a.rs::effects_survive_a_failed_attempt_and_the_delta_does_not` |
| §3.14 A1 §2.10.1 erratum (overwritten at each members-cache miss, never erased) | `Effect::StoreQuorumSnapshot` docs |

## 4. A1, ST3a, CS0 and the BLS ruling → items

| Requirement | Served by |
|---|---|
| A1 §2.5 `EvoStore` (best block, MN list rows, credit pool, MNHF, mined commitments, quorum snapshot) | `store::EvoStore` (+ `DmlReader`, `MinedCommitmentReader`) |
| A1 §2.5 `EvoDelta { MN diff, snapshot, mined commitments, credit pool, MNHF, BLS flip }`, transactional only | `store::EvoDelta` (`dml`, `mined_commitments`, `credit_pool`, `mnhf_signals`); the BLS flip is derived, not stored (Q7) |
| A1 §2.10.1 effect inventory, emission and application rules, plus VerifyDB level 3's undo notification (M3) | `effect::{Effect, EffectSink}`; `NotifyMnListChanged { at, undo }` |
| A1 §2.10.2 historical reads take `at: BlockId` and resolve through ancestry; never "latest"; records keyed by hash and never mutated | `MinedCommitmentReader` (index rows under a cap, ancestry reads over them; qrinfo is the exception, M2); hash-keyed rows + owner resolvers (§1); `MemEvoView` binds a block index |
| A1 Appendix A `EvoCtx { cons, check_sigs }` (never gates AU quorum or MNHF sigs) | `ctx::EvoCtx` |
| A1 O2a errata (12 special types; no `dos` field) | nothing in EV0 depends on them |
| ST3a §5.6 item 5 / §2.4.4: CH7a declares each effect class presumed or independent | `effect::EffectKind` (+ `ALL`); CH7a's stub classifies with an exhaustive `match` (Q8) |
| CS0: `BlockId`, `ChainAncestry`, `DeploymentView`, `ConsensusCtx`, `BlsCtx`, `bls_ctx_for_block`, `Amount`, validation errors; no duplicated primitives | used directly; `CoreBytes` stays sealed to tx/header/block (EV2 Q9) |
| DEC-71: decode scheme-free, resolve the scheme at use | `bls` module: `bls_ctx_after` (tip, DKG base), `bls_ctx_for_block` (block processing), `EvoCtx::bls`; no decode API takes a `BlsCtx` |

## 5. Deviations from the designs

1. **`MinedCommitmentReader::mined_commitment` takes `at`** (LQ1 §2.5 has none). An ancestry-resolved store keeps every branch's record; `at` picks the one Core would see and folds in v24's mined-in-chain check. Core's own reads pass the tip. Q1.
2. **Per-block "after `at`" resolution is the owners'**, not an `EvoStore` method: resolving a list, pool or signal set needs deployment gates and fall-backs (`GetListForBlockInternal`, `CMNHFManager::GetForBlock`). `EvoStore` exposes the hash-keyed rows, which are ancestry-correct by construction. Q2.
3. **`MnList` holds only the fields EV0's lookups use**; EV2a adds the secondary indexes, `core_order` cache and `fast_payee_ok` privately. No mutation, `with_height`, `is_default` or codec in EV0.
4. **`UniqueKey` is `Box<[u8]>`** (EV2: `SmallVec<[u8; 48]>`), private, and also `Ord` (for PF0's `OrdMap` comparison). EV2a/PF0 may change the representation.
5. **`ProTxHash` is an EV0 newtype**, not `make_hash!`: the macro's expansion calls `BaseCodec::encode`, which pure crates ban at forbid level. Q5.
6. **`MnCold` lacks `payouts` and `shares`** until BS6a's v24 shapes are pinned; `is_shared()` is false until then. Scripts are `ScriptBytes = Vec<u8>`: base-sdk exports no script buffer type. Q6.
7. **`Effect::ReinsertMineableCommitment` holds `Arc<LlmqCommitment>`** (A1: by value) to keep the enum small.
8. **`QuorumSnapshot.skip_list_mode` is Core's raw `i32`**, with `skip_mode()` naming the four values: Core decodes any `int`, and a row must re-serialize as read. Q4.
9. **No `PartialEq` on DML values** (M1): `MnCold`, `Dmn`, `MnList`, `MnListDiff`, `MnStateDiff`, `MnListDiffRecord`, `DmlDelta`, `EvoDelta`.
   - `SameRecord::same_record` compares records as Core serializes them, and requires their operator keys to be equal under Core's `operator==` as well. The key is compared as `key.clone().to_bytes_as(scheme_of(version))`, what the state's `CBLSLazyPublicKeyVersionWrapper` writes (legacy iff `nVersion == 1`), and as `a.clone() == b.clone()`. Both run on clones, so the compared keys are untouched and no later serialization or comparison of them (`core_eq` included) differs (r2-m1). The second condition matters for in-memory states only: serialization alone does not identify an in-memory key. For records decoded from bytes the two coincide. The captured scheme is not compared on its own, so a revoked masternode's default key (captured under the global flag online, under the version's scheme when reconstructed) is the same record, as Core says (r2-m2). That makes it `WriteDerived`'s comparison (`evo/evodb.h` v24:159-205) on stored rows and EV2a's canonical-codec comparison; the store's rewrite check and EV2 §2.8's online-vs-reconstructed check use it. `MnList::same_record` short-circuits on shared maps.
   - `MnCold::core_eq` is Core's comparison with its side effects (the key compares with `CBLSLazyWrapper::operator==`, which may decode), for the places Core compares.
   - Tests: `dml/record.rs::{same_record_never_changes_a_later_serialization, same_record_leaves_a_later_core_eq_as_core, default_keys_compare_as_core_serializes, keys_compare_under_the_version_scheme, core_eq_has_cores_side_effect}` (raw-undecodable vs decoded key, an already-failed key, and the API review r2 probes), `tests/store.rs::a_rewrite_check_never_changes_the_stored_serialization`.
   - `MinedCommitmentRecord` and `Effect` keep `PartialEq` while `LlmqCommitment` has no lazy field; when LQ2 swaps the alias, their equality becomes Core's `SerializedEqual` (`llmq/blockprocessor.cpp` v24:381-382).
10. **The rewrite check is v24's `WriteDerived`** (DEC-82: v24 only). A differing rewrite needs a derivation bug, so we fail-stop on it.
11. **`Counts` is Core's six `size_t` fields as `u64`**, weighted sums included (Q13): an undefined `nType` counts as regular with weight 0, so the weighted sums are not derivable from the four counts.
12. **Mined commitments are read at two levels** (M2): `mined_index` returns the index rows of the chain ending at `chain_end` under a height cap, which is what Core iterates (its index holds the active chain's rows); `mined_until`/`nth_mined_by_index`/`newest_by_index` are provided over it with `chain_end = at`. The reader exposes `chain()` for the base mapping. `newest_by_index` is now one walk.
13. **`EvoDelta` carries no BLS context** (Q7, against A1 §2.5's "BLS flip"): the flag after a commit is `bls_ctx_after(tip)`, so CH7a recomputes it. There is no `BlsSchemeLeak` effect: v24 commits the flag only after a full connect (A1 §2.25, DEC-82).

## 6. Findings for the manager (both confirmed by the API review)

- **`MnListChanged` gating (EV2 §3.8 row 6, DEC-50(12)).** v24.0.0-rc.3 notifies only when the diff has changes: `validation.cpp:2918` is `if (this == &m_chainman.ActiveChainstate() && mnlist_updates.diff.HasChanges())`, and the disconnect paths are gated the same way (v24:2368). EV2's premise that v24 notifies always does not hold at the pin; CH7a raises `MnListChanged` only on change.
- **VerifyDB level 3 notifies the list change with `undo = true`** through `DisconnectBlock` (`validation.cpp` v24:2368-2370); A1 §2.10.1 listed only level 4. `Effect::NotifyMnListChanged` now carries `undo` (M3).
- **Core's qrinfo builder reads the active chain's index for a request block it never checks against the active chain** (`llmq/snapshot.cpp` v24:141, 191-197), so LQ1 §3.8.2's "every call site is active-chain" does not hold there (M2).
- **Purity gate and blst.** `OperatorKey` is BS11a-t's `BlsLazyPk`, which needs `dash-pkc/bls`, which brings blst → threadpool into the pure graph. `check-pure.sh` fails on that one edge until DEC-71's gate change (`wrappers = ["blst"]` plus the blst-without-`std` assertion and the wrapper bans) lands with the combined re-pin. With that exception applied locally, all three pure crates pass. EV0 merges after BS6a (i:BS6a), so after the re-pin.

## 7. Decisions on the open API questions (review §4, all adopted)

| Q | Decision | Where |
|---|---|---|
| Q1 | One commitment trait: EV0's `MinedCommitmentReader`, `mined_commitment` takes `at`; LQ1's `MinedCommitmentIndex` and its `Commitment` generic are retired (LQ1a needs only `quorum_index`; null commitments are never stored). No tip-bound adapter outside tests. M2's index-level read added with it | `llmq/mined.rs`; LQ1a stub; LQ1 §2.5/§3.8.2 erratum (manager) |
| Q2 | Hash-keyed rows in `EvoStore`; owners resolve "after `at`" (`resolve_list`, EV3's resolvers; LQ1's `LlmqSource::mn_list(at)` is CH7b's binding of `resolve_list`) | `dash-dml/src/store/mod.rs` docs |
| Q3 | Keep the one-line `LlmqCommitment` alias; when LQ2 swaps it, rewrite checks compare canonical bytes (`SerializedEqual`) | `llmq/mod.rs`, `dash-dml`'s `MinedCommitmentRecord` docs |
| Q4 | Re-export BS8's `QuorumSnapshot` when it lands **if** it keeps the mode raw (`i32` or `Unknown(i32)`) and round-trips Core's bitset; otherwise EV0 keeps its type plus a conversion | `llmq/snapshot.rs` |
| Q5 | base-sdk owns `ProTxHash` in `dash-primitives` (memcmp `Ord`, EV0's unit test kept against it); `immer_hash` stays here. The purity ban is not weakened. Until then the newtype stays | `types.rs` |
| Q6 | BS6a re-exports a script buffer type and confirms `PayoutShare`/`CollateralShare` at the combined re-pin; `ScriptBytes = Vec<u8>` until then | BS6a |
| Q7 | `EvoDelta::bls_after` dropped; CH7a holds the tip context (`bls_ctx_after(tip)`; no leak effect under DEC-82) | `dash-dml/src/store/delta.rs` |
| Q8 | CH7a keeps the durability table as an exhaustive `match` over `EffectKind`, no wildcard; EV0's docs name the classification as guidance only | `effect.rs`; CH7a stub |
| Q9 | EV1 owns `BlockPayloads` in `dash-dml::specialtx::payload`; CH7a builds it once, EV2b consumes it | EV1 |
| Q10 | `QuorumMemberSource: Send + Sync`. **Superseded** by the EV addendum (rev5, DEC-117): no supertrait, since every runtime implementor carries a V-only capability (§2.6) | `llmq/members.rs` |
| Q11 | EV3's call; per-block values are fine under DEC-11, EV3's resolvers do Core's recompute and fall-back. `RangesSet`'s representation matches Core's `std::set<Range>`; its semantics are per pin (CONS-10) | EV3; the `dash-dml/src/creditpool.rs` doc is m9, deferred |
| Q12 | `ListCache`/`CacheHint` (with LQ1's `CacheHint::WorkBlock`) are EV2a's | EV2a |
| Q13 | `Dmn::mn_type: RawMnType` with `stored_type()`, `is_evo()`, `weight()` (0 for undefined); `Counts` stores Core's weighted sums | `dash-dml/src/types.rs` |
| Q14 | Keep `MnList::insert_for_tests` behind `test-utils` until `try_add` lands, then delete it and move the testkit builders onto `try_add` | `dash-dml/src/list.rs` |

### EV addendum decisions (rev6, DEC-122)

| # | Decision | Where |
|---|---|---|
| A1 | **The split** (§1.1, §1.2): the shared `dash-evo` (layer P; SPV and the node) and the node-only `dash-dml` (layer Pn: the DML, the store and its records, the credit pool, MNHF, `imbl`). SPV names only `dash-evo`. Until SB1's L2/L6 gates exist, `check-pure.sh` fails if `dash-evo`'s normal closure reaches `dash-dml` or `imbl` | §0; both crates' `lib.rs`; `contrib/node/check-pure.sh` |
| A2 | **Generic errors** (§2.2, §2.3): `QuorumMemberSource` has `type Entry` and `type Error`; `MemberError<E>` is `Missing(BlockId)`, `View(E)`, `UnknownLlmqType(u8)`; `MinedCommitmentReader::Error: From<BrokenInvariant>`. The node keeps EV0's two variants as `NodeMemberError`, and `Missing` is a store inconsistency there (D4). SPV's mapping is shown, not shipped (SB11 owns it, in `dash-mnlist`) | `llmq/{members,mined}.rs`; `dash-dml` `store/mod.rs`; `tests/consumers/spv.rs` |
| A3 | **The `Arc<T>` forwarding impl** (M1, §1.3): `impl<T: MnEntryView> MnEntryView for Arc<T>` here, so `dash-dml` (and SPV) write `Entry = Arc<T>`, which they could not implement themselves (E0117) | `llmq/view.rs`; `dash-dml/tests/entry_view.rs` |
| A4 | **The chain contract** (§2.2): `LlmqChain` with `Ok(None)` only above the block or at genesis, and `CompleteChain` over the node's `ChainAncestry` | `llmq/chain.rs` |
| A5 | **m2 resolved** (DEC-111, §2.6): CH7b's `AttemptHandle` owns one overlay per attempt, shared by every members and precompute call; `store` overwrites it and logs `StoreQuorumSnapshot`, `get` reads it before the backing rows; CH7a applies the log on every outcome. EV0's per-call stub is replaced by that shape | `dash-dml/tests/consumers/lq1a.rs` (cases 1 and 2); LQ1m and CH7b carry the full suite |
| A6 | **m5** (rename `iter` to a memcmp name, a dissolved bucket in the trailer) still lands in EV2a's first commit | EV2a |
| A7 | **`ScratchMnList for MnList::try_add` is `unimplemented!`** until EV2a lands `MnList::try_add` (Core's `AddMN`, EV2 §2.5): EV0 has no list mutation, and any other acceptance rule would be wrong where Core swallows `AddMN`'s failures. LQ1a's stub uses its lean `UsedSet` | `dash-dml` `views.rs` |
| A8 | **The CH7a consumer is unchanged**: it never calls the member source, so the generic errors do not reach it | `dash-dml/tests/consumers/ch7a.rs` |
| A9 | **The node's member source is lifetime-generic** (Sol r3 R3-M1): `DmlMemberSource<'a>` is `dyn QuorumMemberSource<…> + 'a`, so callers hold `&DmlMemberSource<'_>` and a source may borrow an attempt's view (CH7b's `AttemptHandle<'v>`, §2.6's capability-scoped adapters). The trait needed no change: only the alias's default `'static` object bound rejected borrowing sources | `dash-dml` `store/mod.rs` (doctest pair); `dash-dml/tests/consumers/ev2b.rs` |
| A10 | **The layering guard reads the resolved graph** (Sol r3 R3-M2): `contrib/node/check-layering.py` walks normal edges in `cargo metadata --all-features` for the whole workspace and every target, by package name, so a forbidden package reached through a feature of a shared dependency, a rename, a re-export or a target-only table fails (also `imbl-sized-chunks`, and a forbidden name no package carries). Dev edges pass, but a feature turned on only through a dev or build edge still counts: Cargo's metadata unifies over every edge kind, so the guard errs on the side of failing. `selftest-checks.sh` plants each bypass, including a non-member's feature only `dash-dml` enables. Each package reached also contributes its declared normal dependencies, optional ones included, so an optional edge nothing in the workspace turns on still fails (Opus-high m5); a declared path or git package missing from the lockfile fails too, as its own dependencies are unknown (a registry one is checked by name) (SB1 replaced `check-layering.py` with `contrib/node/check-layers.py`'s imbl ban, which keeps these cases) | `contrib/node/{check-layering.py,check-pure.sh,selftest-checks.sh}` |

## 8. Round 1 changes, and the minors left for EV2a

Changed: M1 (`SameRecord`, `MnCold::core_eq`, no `PartialEq` on DML values, the store's rewrite check, three tests), M2 (`mined_index`, `MinedFamily`, `MinedIndexEntry`, `base_through`, the reads provided over it, the stale-request fork test), M3 (`NotifyMnListChanged { at, undo }`, CH7a stub), Q1, Q7, Q8 (stub), Q10, Q13 (with m7).

Left for EV2a and the owners, as the review allows: m2 (where the attempt snapshot overlay lives; resolved since by the EV addendum, §7 A5), m4 (`EvoStore::best_block` is recovery-only state), m5 (rename `iter` to a memcmp name until `core_order` lands; specify a dissolved bucket in the trailer), m6 (`MnStateDiff` accessors for RP2a), m9 (`RangesSet` semantics per pin), and nits n1-n4 (duplicated height and hash fields, the BLS test view over `MemChain`, `scratch()` provenance, parent-linkage checks in `MemEvoStore`).

## 9. SB7: the shared SML core (EV4-core; SB0 §5.5, DEC-112 O2)

| Requirement | Served by |
|---|---|
| SML entry and its hash (Core `CSimplifiedMNListEntry`, `CalcHash`) | `sml::SmlEntry::{hash, write_hashed}`, `sml::write_net_info` (legacy `CService` and `ExtNetInfo` bytes, written by hand: pure crates may not call base-sdk's encoder) |
| `merkleRootMNList` (`CalcCbTxMerkleRootMNList`), full and incremental | `sml::merkle_root_mn_list` (reference), `sml::merkle_root_mn_list_from_leaves`; `sml::SmlList` holds the entries in proTxHash memcmp order and caches every tree level, so `merkle_root` rehashes only changed paths (BG rows `sml-root-*`) |
| `merkleRootQuorums` (`CalcCbTxMerkleRootQuorums`) | `sml::merkle_root_quorums` over `QcHashes` (Core's `GetQcHashes`), the block's `BlockQc`s and `QuorumRootParams`; `sml::merkle_root_quorums_from_hashes` for a held set |
| the CbTx checks (`bad-cbtx-mnmerkleroot`, `bad-cbtx-quorummerkleroot`) | `cbtx::{check_mn_list_root, check_quorums_root}` |
| the CbTx partial-merkle check (SB0 §5.2 step 2; #1117) | `cbtx::PartialMerkleTree::extract_matches` (Core's `ExtractMatches`), `cbtx::verify_cbtx_proof` |
| the coinbase-CL predicate (LQ1 §3.3.1) | `cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock}` |
| dashcore `sml::masternode_list::scores_for_quorum` | `sml::score::{QuorumScore, ScoredEntry, quorum_score, scores_for_quorum, confirmed_hash_with_pro_reg_tx}` |

Deviations from LQ1 §3.3.1's proposed signature: there is no `rules: &UngatedRules` (RP-V24 removed
it; the predicate is v24's), `CoinbaseFacts::First` holds the block's first transaction as a
`TxRef` (its payload decodes once, through `Tx::payload`) rather than `(is_coinbase, n_type,
extra_payload)`, a block without transactions is `NoTransactions`, and the signature comes back as
the `BlsEagerSig` Core reads under the caller's `BlsCtx`. `QcHashes` keys types by number, as
Core's `std::map<LLMQType, …>` orders them. `CFinalCommitment`'s `SerializeHash` and the BLS
verification of `quorum_entry/{validation,verify_message}.rs` are not here: the first is the
caller's (LQ2 holds the commitment hashes), the second SB8's, which needs BS12's serial BLS
(blst's pool is banned in this crate).
